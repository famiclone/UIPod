/***************************************************************************
 * UIPod: Last.fm Scrobbling 2.0 & Offline Scrobble Cache integration.
 ****************************************************************************/
#include "config.h"

#include <ctype.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <time.h>

#include "action.h"
#include "audio.h"
#include "cuesheet.h"
#include "file.h"
#include "gui/list.h"
#include "icon.h"
#include "kernel.h"
#include "lang.h"
#include "lcd.h"
#include "metadata.h"
#include "misc.h"
#include "rbpaths.h"
#include "settings.h"
#include "splash.h"
#include "string-extra.h"
#include "system.h"
#include "window-sdl.h"

#include "uipod_downloads.h"
#include "uipod_jellyfin.h"
#include "uipod_keyboard.h"
#include "uipod_lastfm.h"
#include "uipod_navidrome.h"
#include "uipod_page.h"
#include "uipod_rs_lastfm.h"
#include "uipod_rs_ui.h"
#include "uipod_transition.h"
#include "uipod_ui.h"

#ifdef RETRO_HANDHELD
#include "uipod-http-target.h"
#endif

#define LFM_CFG_RB    ROCKBOX_DIR "/lastfm.cfg"
#define LFM_CFG_SD_1  UIPOD_SDCARD_ROOT "/lastfm.cfg"
#define LFM_CFG_SD_2  UIPOD_SDCARD_ROOT "/Tools/tg5040/UIPod.pak/lastfm.cfg"

#define LFM_QR_MAX_DIM  64

static pthread_mutex_t s_lfm_mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t  s_lfm_cv  = PTHREAD_COND_INITIALIZER;
static pthread_t       s_lfm_thread;
static bool            s_worker_started = false;
static volatile int    s_shutdown       = 0;
static bool            s_want_sync      = false;
static bool            s_reset_backoff  = false;

static char            s_queue_path[512];

/* ---- Clock Helpers ---------------------------------------------------- */

static uint64_t lfm_unix_now(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_REALTIME, &ts) == 0 && ts.tv_sec > 0)
        return (uint64_t)ts.tv_sec;
    return 0;
}

static uint64_t lfm_mono_ms(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) == 0)
        return (uint64_t)ts.tv_sec * 1000ULL + (uint64_t)(ts.tv_nsec / 1000000L);
    return (uint64_t)current_tick * (1000ULL / HZ);
}

/* ---- HTTP Transport Wiring -------------------------------------------- */

#ifdef RETRO_HANDHELD
static int32_t lfm_http_request_adapter(
    uint8_t is_https, const char *host, uint16_t port,
    const char *method, const char *pq, const char *hdrs,
    const uint8_t *body, size_t body_len, int32_t timeout_ms,
    int32_t *out_status, uint8_t **out_data, size_t *out_len,
    char *err, size_t cap)
{
    return retrohh_http_request(method, is_https, host, port, pq, hdrs,
                                body, body_len, 0 /* verify CA */, timeout_ms,
                                out_status, out_data, out_len, err, cap);
}
#else
static int32_t stub_lfm_req(
    uint8_t is_https, const char *host, uint16_t port,
    const char *method, const char *pq, const char *hdrs,
    const uint8_t *body, size_t body_len, int32_t timeout_ms,
    int32_t *out_status, uint8_t **out_data, size_t *out_len,
    char *err, size_t cap)
{
    (void)is_https; (void)host; (void)port; (void)method; (void)pq; (void)hdrs;
    (void)body; (void)body_len; (void)timeout_ms;
    (void)out_status; (void)out_data; (void)out_len;
    snprintf(err, cap, "HTTP unsupported on this target");
    return -1;
}
static void stub_lfm_free(uint8_t *p) { free(p); }
#endif

static const TpLfmHttpVtable s_lfm_http_vt = {
#ifdef RETRO_HANDHELD
    .http_request = lfm_http_request_adapter,
    .http_free    = retrohh_nav_http_free,
#else
    .http_request = stub_lfm_req,
    .http_free    = stub_lfm_free,
#endif
};

/* ---- Config & Queue Path Persistence ---------------------------------- */

static void resolve_queue_path(void)
{
    const char *home = getenv("HOME");
    if (home && home[0])
    {
        char dir1[512], dir2[512];
        snprintf(dir1, sizeof(dir1), "%s/.config", home);
        mkdir(dir1, 0777);
        snprintf(dir2, sizeof(dir2), "%s/.config/rockbox.org", home);
        mkdir(dir2, 0777);
        snprintf(s_queue_path, sizeof(s_queue_path),
                 "%s/.config/rockbox.org/lastfm_queue.tsv", home);
    }
    else
    {
        strlcpy(s_queue_path, UIPOD_TMP_ROOT "/uipod_lastfm_queue.tsv",
                sizeof(s_queue_path));
    }
}

static bool lfm_read_text_file(const char *rb_path, char *buf, size_t cap)
{
    int fd = open(rb_path, O_RDONLY);
    if (fd < 0)
        return false;
    ssize_t n = read(fd, buf, cap - 1);
    close(fd);
    if (n < 0)
        return false;
    buf[n] = '\0';
    return true;
}

static bool lfm_save_config_to_disk(void)
{
    char ini[2048];
    size_t n = tp_lastfm_format_config_text(ini, sizeof(ini));
    if (n == 0)
        return false;
    int fd = open(LFM_CFG_RB ".new", O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0)
        return false;
    bool ok = (write(fd, ini, n) == (ssize_t)n);
    close(fd);
    if (ok)
        rename(LFM_CFG_RB ".new", LFM_CFG_RB);
    return ok;
}

static void lfm_load_or_import_config(void)
{
    char ini[2048];
    if (lfm_read_text_file(LFM_CFG_RB, ini, sizeof(ini)))
    {
        tp_lastfm_load_config_text(ini);
        if (tp_lastfm_has_api_credentials())
            return;
    }

    if (lfm_read_text_file(LFM_CFG_SD_1, ini, sizeof(ini)) ||
        lfm_read_text_file(LFM_CFG_SD_2, ini, sizeof(ini)))
    {
        if (tp_lastfm_load_config_text(ini) && tp_lastfm_has_api_credentials())
            lfm_save_config_to_disk();
    }
}

/* ---- Background Worker Thread ----------------------------------------- */

static void lfm_wake_worker(bool reset_backoff)
{
    pthread_mutex_lock(&s_lfm_mtx);
    s_want_sync = true;
    if (reset_backoff)
        s_reset_backoff = true;
    pthread_cond_broadcast(&s_lfm_cv);
    pthread_mutex_unlock(&s_lfm_mtx);
}

static void *lfm_worker_main(void *arg)
{
    (void)arg;

    uint64_t retry_after_mono = 0;
    uint64_t backoff_ms       = 15000ULL;

    pthread_mutex_lock(&s_lfm_mtx);
    while (!__atomic_load_n(&s_shutdown, __ATOMIC_ACQUIRE))
    {
        bool has_active = tp_lastfm_has_active_track();
        uint64_t now_m  = lfm_mono_ms();

        if (s_reset_backoff)
        {
            s_reset_backoff  = false;
            retry_after_mono = 0;
            backoff_ms       = 15000ULL;
        }

        bool can_sync_now = s_want_sync &&
                            !uipod_offline_mode_get() &&
                            (now_m >= retry_after_mono);

        if (!can_sync_now)
        {
            if (has_active || (s_want_sync && !uipod_offline_mode_get()))
            {
                uint64_t wait_ms = has_active ? 2000ULL : 10000ULL;
                if (s_want_sync && !uipod_offline_mode_get() && retry_after_mono > now_m)
                {
                    uint64_t rem = retry_after_mono - now_m;
                    if (!has_active && rem < 60000ULL)
                        wait_ms = rem;
                }
                struct timespec ts;
                if (clock_gettime(CLOCK_REALTIME, &ts) == 0)
                {
                    ts.tv_sec  += (time_t)(wait_ms / 1000ULL);
                    ts.tv_nsec += (long)((wait_ms % 1000ULL) * 1000000L);
                    if (ts.tv_nsec >= 1000000000L)
                    {
                        ts.tv_sec  += 1;
                        ts.tv_nsec -= 1000000000L;
                    }
                    pthread_cond_timedwait(&s_lfm_cv, &s_lfm_mtx, &ts);
                }
            }
            else
            {
                pthread_cond_wait(&s_lfm_cv, &s_lfm_mtx);
            }

            if (__atomic_load_n(&s_shutdown, __ATOMIC_ACQUIRE))
                break;

            /* Advance unpaused listening timer; commits to `lastfm_queue.tsv` at 50% / 4:00 */
            int st = audio_status();
            bool playing = ((st & AUDIO_STATUS_PLAY) != 0) &&
                           ((st & AUDIO_STATUS_PAUSE) == 0);
            if (tp_lastfm_on_playback_tick(lfm_mono_ms(), playing))
                s_want_sync = true;

            continue;
        }

        s_want_sync = false;
        pthread_mutex_unlock(&s_lfm_mtx);

        /* Run network sync outside mutex so audio/UI threads never block */
        bool cfg_updated = false;
        char err[160];
        err[0] = '\0';
        int32_t rc = tp_lastfm_sync_now(lfm_unix_now(), &cfg_updated, err, sizeof(err));
        if (cfg_updated)
            lfm_save_config_to_disk();

        pthread_mutex_lock(&s_lfm_mtx);
        if (rc >= 0)
        {
            retry_after_mono = 0;
            backoff_ms       = 15000ULL;
            if (tp_lastfm_cached_count() > 0)
                s_want_sync = true;
        }
        else if (rc == -1)
        {
            /* Transient offline / Wi-Fi / service failure: keep queue & back off */
            s_want_sync      = (tp_lastfm_cached_count() > 0);
            retry_after_mono = lfm_mono_ms() + backoff_ms;
            backoff_ms *= 2ULL;
            if (backoff_ms > 300000ULL)
                backoff_ms = 300000ULL;
        }
        else
        {
            /* Not authenticated or disabled: sleep until user signs in or config changes */
            retry_after_mono = 0;
        }
    }
    pthread_mutex_unlock(&s_lfm_mtx);
    return NULL;
}

/* ---- Lifecycle & Playback Hooks --------------------------------------- */

void uipod_lastfm_init(void)
{
    resolve_queue_path();
    tp_lastfm_init(s_queue_path, &s_lfm_http_vt);
    lfm_load_or_import_config();

    pthread_mutex_lock(&s_lfm_mtx);
    __atomic_store_n(&s_shutdown, 0, __ATOMIC_RELEASE);
    if (!s_worker_started)
    {
        if (pthread_create(&s_lfm_thread, NULL, lfm_worker_main, NULL) == 0)
            s_worker_started = true;
    }
    if (tp_lastfm_cached_count() > 0 && !uipod_offline_mode_get())
    {
        s_want_sync = true;
        pthread_cond_broadcast(&s_lfm_cv);
    }
    pthread_mutex_unlock(&s_lfm_mtx);
}

void uipod_lastfm_shutdown(void)
{
    /* Commit any in-progress track that already crossed the scrobble threshold */
    tp_lastfm_on_track_finish(lfm_mono_ms());

    pthread_mutex_lock(&s_lfm_mtx);
    if (!s_worker_started)
    {
        pthread_mutex_unlock(&s_lfm_mtx);
        return;
    }
    __atomic_store_n(&s_shutdown, 1, __ATOMIC_RELEASE);
    pthread_cond_broadcast(&s_lfm_cv);
    pthread_mutex_unlock(&s_lfm_mtx);

    pthread_join(s_lfm_thread, NULL);
    s_worker_started = false;
}

void uipod_lastfm_notify_online(void)
{
    if (tp_lastfm_cached_count() > 0)
        lfm_wake_worker(true);
}

static void extract_stem(const char *path, char *out, size_t sz)
{
    if (!path || !path[0])
    {
        out[0] = '\0';
        return;
    }
    const char *base = strrchr(path, '/');
    base = base ? base + 1 : path;
    strlcpy(out, base, sz);
    char *dot = strrchr(out, '.');
    if (dot && dot != out)
        *dot = '\0';
}

void uipod_lastfm_on_track_change(const struct mp3entry *id3)
{
    if (!id3)
        return;

    char stem[MAX_PATH];
    const char *title  = "";
    const char *artist = "";
    const char *album  = "";
    const char *aa     = "";
    uint32_t track_num = 0;
    uint64_t dur_ms    = (uint64_t)id3->length;

    if (id3->cuesheet && id3->cuesheet->curr_track &&
        id3->cuesheet->curr_track->title[0])
    {
        title = id3->cuesheet->curr_track->title;
    }
    else if (id3->title && id3->title[0])
    {
        title = id3->title;
    }
    else
    {
        extract_stem(id3->path, stem, sizeof(stem));
        title = stem;
    }

    if (id3->cuesheet && id3->cuesheet->curr_track &&
        id3->cuesheet->curr_track->performer[0])
        artist = id3->cuesheet->curr_track->performer;
    else if (id3->artist && id3->artist[0])
        artist = id3->artist;
    else if (id3->albumartist && id3->albumartist[0])
        artist = id3->albumartist;

    if (id3->album && id3->album[0])
        album = id3->album;
    else if (id3->cuesheet && id3->cuesheet->title[0])
        album = id3->cuesheet->title;

    if (id3->albumartist && id3->albumartist[0])
        aa = id3->albumartist;

    if (id3->tracknum > 0)
        track_num = (uint32_t)id3->tracknum;

    bool is_streaming = uipod_navidrome_is_track_path(id3->path) ||
                        uipod_jellyfin_is_track_path(id3->path);

    if (tp_lastfm_on_track_start(id3->path, artist, title, album, aa,
                                 track_num, dur_ms, is_streaming,
                                 lfm_unix_now(), lfm_mono_ms()))
    {
        /* Reset backoff only moderately (if last retry already expired, wake immediately) */
        pthread_mutex_lock(&s_lfm_mtx);
        s_want_sync = true;
        pthread_cond_broadcast(&s_lfm_cv);
        pthread_mutex_unlock(&s_lfm_mtx);
    }
}

void uipod_lastfm_on_track_finish(const struct mp3entry *id3)
{
    (void)id3;
    if (tp_lastfm_on_track_finish(lfm_mono_ms()))
    {
        pthread_mutex_lock(&s_lfm_mtx);
        s_want_sync = true;
        pthread_cond_broadcast(&s_lfm_cv);
        pthread_mutex_unlock(&s_lfm_mtx);
    }
}

/* ---- QR Code Browser Authorization Screen ----------------------------- */

struct qr_poll_ctx
{
    char         token[96];
    char         err[160];
    volatile int cancel;
    volatile int status; /* 0 = waiting, 1 = authorized, -1 = error */
};

static void *qr_poll_thread_fn(void *arg)
{
    struct qr_poll_ctx *ctx = (struct qr_poll_ctx *)arg;
    while (!__atomic_load_n(&ctx->cancel, __ATOMIC_ACQUIRE))
    {
        for (int i = 0; i < 25; i++)
        {
            if (__atomic_load_n(&ctx->cancel, __ATOMIC_ACQUIRE))
                return NULL;
            struct timespec ts = { 0, 100000000L }; /* 100 ms */
            nanosleep(&ts, NULL);
        }
        if (__atomic_load_n(&ctx->cancel, __ATOMIC_ACQUIRE))
            return NULL;

        char err[160];
        err[0] = '\0';
        int32_t rc = tp_lastfm_poll_auth_session(ctx->token, err, sizeof(err));
        if (rc == 1)
        {
            __atomic_store_n(&ctx->status, 1, __ATOMIC_RELEASE);
            return NULL;
        }
        else if (rc < 0)
        {
            strlcpy(ctx->err, err, sizeof(ctx->err));
            __atomic_store_n(&ctx->status, -1, __ATOMIC_RELEASE);
            return NULL;
        }
    }
    return NULL;
}

static uint32_t rgb_luma(uint32_t rgb)
{
    uint32_t r = (rgb >> 16) & 0xFF;
    uint32_t g = (rgb >> 8)  & 0xFF;
    uint32_t b = rgb         & 0xFF;
    return r * 299U + g * 587U + b * 114U;
}

static void draw_qr_auth_gpu(const uint8_t *modules, size_t qr_dim)
{
    if (!uipod_gpu_ui_ensure_ready())
        return;

    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);

    uint32_t bg  = (uint32_t)global_settings.bg_color  & 0xFFFFFF;
    uint32_t fg  = (uint32_t)global_settings.fg_color  & 0xFFFFFF;
    uint32_t fg1 = (uint32_t)global_settings.fg1_color & 0xFFFFFF;
    uint32_t fg2 = (uint32_t)global_settings.fg2_color & 0xFFFFFF;

    struct UIPodRsHeader hdr;
    uipod_gpu_fill_header(&hdr, str(LANG_UIPOD_LASTFM));

    float lw = 1024.0f, lh = 768.0f;
    if (!uipod_rs_ui_plugin_begin_ex(bg, fg, fg1, fg2, bg, &hdr,
                                     (uint32_t)win_w, (uint32_t)win_h,
                                     true, &lw, &lh))
        return;

    /* Ensure camera-friendly dark-on-light QR card regardless of dark/light theme */
    bool dark_theme  = rgb_luma(bg) < rgb_luma(fg);
    uint32_t card_bg = dark_theme ? fg : bg;
    uint32_t mod_fg  = dark_theme ? bg : fg;

    const float header_h = 80.0f;
    const float body_y   = header_h + 18.0f;
    const float body_h   = lh - body_y - 24.0f;

    uipod_rs_ui_plugin_draw_text(lw * 0.5f, body_y + 28.0f,
                                 str(LANG_UIPOD_LFM_QR_SCAN),
                                 32.0f, 1, 1, fg);

    if (qr_dim > 0)
    {
        float avail = body_h - 140.0f;
        if (avail > 420.0f) avail = 420.0f;
        if (avail < 200.0f) avail = 200.0f;

        size_t total_cells = qr_dim + 8; /* 4-module quiet zone on each side */
        int cell_px = (int)(avail / (float)total_cells);
        if (cell_px < 4) cell_px = 4;

        float card_sz = (float)(total_cells * (size_t)cell_px);
        float card_x  = (float)(int)((lw - card_sz) * 0.5f);
        float card_y  = (float)(int)(body_y + 68.0f + (avail - card_sz) * 0.5f);

        uipod_rs_ui_plugin_fill_rect(card_x, card_y, card_sz, card_sz, 20.0f, card_bg);
        if (!dark_theme)
            uipod_rs_ui_plugin_stroke_rect(card_x, card_y, card_sz, card_sz, 20.0f, 3.0f, fg2);

        float qr_x0 = card_x + (float)(4 * cell_px);
        float qr_y0 = card_y + (float)(4 * cell_px);
        float msz   = (float)cell_px;

        for (size_t ry = 0; ry < qr_dim; ry++)
        {
            size_t rx = 0;
            while (rx < qr_dim)
            {
                if (modules[ry * qr_dim + rx] == 0)
                {
                    rx++;
                    continue;
                }
                /* Merge horizontal runs of dark modules for crisp seam-free edges */
                size_t run_end = rx + 1;
                while (run_end < qr_dim && modules[ry * qr_dim + run_end] != 0)
                    run_end++;
                uipod_rs_ui_plugin_fill_rect(
                    qr_x0 + (float)rx * msz,
                    qr_y0 + (float)ry * msz,
                    (float)(run_end - rx) * msz,
                    msz,
                    0.0f,
                    mod_fg);
                rx = run_end;
            }
        }
    }

    uipod_rs_ui_plugin_draw_text(lw * 0.5f, lh - 52.0f,
                                 str(LANG_UIPOD_LFM_QR_WAIT),
                                 26.0f, 0, 1, fg1);

    if (uipod_rs_ui_plugin_present_gpu((uint32_t)win_w, (uint32_t)win_h))
        uipod_gpu_note_rendered_screen(true);
}

static bool lfm_run_qr_auth(void)
{
    if (!tp_lastfm_has_api_credentials())
    {
        splash(HZ * 3 / 2, ID2P(LANG_UIPOD_LFM_SET_KEYS_FIRST));
        return false;
    }
    if (uipod_offline_mode_get())
    {
        splash(HZ, ID2P(LANG_UIPOD_OFFLINE_MODE));
        return false;
    }

    uipod_fullscreen_message(str(LANG_UIPOD_LFM_SYNCING), NULL);

    char token[96];
    char url[256];
    char err[160];
    token[0] = '\0';
    url[0]   = '\0';
    err[0]   = '\0';

    if (tp_lastfm_request_auth_token(token, sizeof(token),
                                     url, sizeof(url),
                                     err, sizeof(err)) != 0)
    {
        splashf(HZ * 3 / 2, "%s", err[0] ? err : (const char *)str(LANG_FAILED));
        return false;
    }

    uint8_t qr_modules[LFM_QR_MAX_DIM * LFM_QR_MAX_DIM];
    memset(qr_modules, 0, sizeof(qr_modules));
    size_t qr_dim = tp_lastfm_encode_qr(url, qr_modules, sizeof(qr_modules));

    struct qr_poll_ctx poll_ctx;
    memset(&poll_ctx, 0, sizeof(poll_ctx));
    strlcpy(poll_ctx.token, token, sizeof(poll_ctx.token));

    pthread_t poll_tid;
    bool tid_started = (pthread_create(&poll_tid, NULL, qr_poll_thread_fn, &poll_ctx) == 0);

    bool authorized = false;
    draw_qr_auth_gpu(qr_modules, qr_dim);

    for (;;)
    {
        int st = __atomic_load_n(&poll_ctx.status, __ATOMIC_ACQUIRE);
        if (st == 1)
        {
            authorized = true;
            break;
        }
        else if (st < 0)
        {
            break;
        }

        int tmo = uipod_gpu_is_volume_popup_active() ? (HZ / 50) : (HZ / 15);
        if (tmo < 1) tmo = 1;
        int action = get_action(CONTEXT_STD, tmo);

        if (action == ACTION_STD_CANCEL)
            break;

        if (action == ACTION_NONE && uipod_gpu_is_volume_popup_active())
        {
            uipod_gpu_tick_volume_popup();
            continue;
        }

        draw_qr_auth_gpu(qr_modules, qr_dim);
    }

    __atomic_store_n(&poll_ctx.cancel, 1, __ATOMIC_RELEASE);
    if (tid_started)
        pthread_join(poll_tid, NULL);

    if (authorized)
    {
        lfm_save_config_to_disk();
        lfm_wake_worker(true);
        TpLfmConfigC cfg;
        if (tp_lastfm_get_config(&cfg) && cfg.username[0])
            splashf(HZ * 3 / 2, "%s: @%s", str(LANG_UIPOD_LFM_SIGNED_IN), cfg.username);
        else
            splash(HZ, ID2P(LANG_UIPOD_LFM_SIGNED_IN));
        return true;
    }

    if (__atomic_load_n(&poll_ctx.status, __ATOMIC_ACQUIRE) < 0 && poll_ctx.err[0])
        splashf(HZ * 3 / 2, "%s", poll_ctx.err);

    return false;
}

/* ---- Direct Username + Password Login --------------------------------- */

static bool lfm_run_password_login(TpLfmConfigC *cfg)
{
    if (!tp_lastfm_has_api_credentials())
    {
        splash(HZ * 3 / 2, ID2P(LANG_UIPOD_LFM_SET_KEYS_FIRST));
        return false;
    }
    if (uipod_offline_mode_get())
    {
        splash(HZ, ID2P(LANG_UIPOD_OFFLINE_MODE));
        return false;
    }

    char user[96];
    char pass[96];
    strlcpy(user, cfg->username, sizeof(user));
    pass[0] = '\0';

    if (uipod_kbd_input_ex(user, sizeof(user), true) != 0 || !user[0])
        return false;
    if (uipod_kbd_input_ex(pass, sizeof(pass), true) != 0 || !pass[0])
        return false;

    uipod_fullscreen_message(str(LANG_UIPOD_LFM_SYNCING), NULL);

    char err[160];
    err[0] = '\0';
    if (tp_lastfm_login_password(user, pass, err, sizeof(err)) != 0)
    {
        splashf(HZ * 3 / 2, "%s", err[0] ? err : (const char *)str(LANG_FAILED));
        return false;
    }

    tp_lastfm_get_config(cfg);
    lfm_save_config_to_disk();
    lfm_wake_worker(true);
    splashf(HZ * 3 / 2, "%s: @%s", str(LANG_UIPOD_LFM_SIGNED_IN),
            cfg->username[0] ? cfg->username : user);
    return true;
}

/* ---- Settings -> Accounts -> Last.fm Page ----------------------------- */

enum
{
    LFM_ROW_ENABLED = 0,
    LFM_ROW_ACCOUNT,
    LFM_ROW_SIGN_IN_QR,
    LFM_ROW_SIGN_IN_PASS,
    LFM_ROW_STREAMING,
    LFM_ROW_SYNC_CACHE,
    LFM_ROW_CLEAR_CACHE,
    LFM_ROW_API_KEY,
    LFM_ROW_API_SECRET,
    LFM_ROW_COUNT
};

struct lfm_settings_page
{
    struct uipod_page   base;
    struct gui_synclist lists;
    TpLfmConfigC        cfg;
    char                indic_buf[LFM_ROW_COUNT][128];
};

static const char *lfm_get_name(int sel, void *data, char *buf, size_t len)
{
    (void)data; (void)buf; (void)len;
    switch (sel)
    {
        case LFM_ROW_ENABLED:      return str(LANG_UIPOD_LFM_SCROBBLING);
        case LFM_ROW_ACCOUNT:      return str(LANG_UIPOD_LFM_ACCOUNT);
        case LFM_ROW_SIGN_IN_QR:   return str(LANG_UIPOD_LFM_SIGN_IN_QR);
        case LFM_ROW_SIGN_IN_PASS: return str(LANG_UIPOD_LFM_SIGN_IN_PASS);
        case LFM_ROW_STREAMING:    return str(LANG_UIPOD_LFM_SCROBBLE_STREAMING);
        case LFM_ROW_SYNC_CACHE:   return str(LANG_UIPOD_LFM_SYNC_CACHE);
        case LFM_ROW_CLEAR_CACHE:  return str(LANG_UIPOD_LFM_CLEAR_CACHE);
        case LFM_ROW_API_KEY:      return str(LANG_UIPOD_LFM_API_KEY);
        case LFM_ROW_API_SECRET:   return str(LANG_UIPOD_LFM_API_SECRET);
        default:                   return "";
    }
}

static void format_masked_key(const char *val, char *out, size_t cap)
{
    if (!val || !val[0])
    {
        snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN,
                 (const char *)str(LANG_UIPOD_NAV_NOT_SET));
        return;
    }
    size_t n = strlen(val);
    if (n <= 8)
    {
        snprintf(out, cap, "%s••••••••", UIPOD_INDIC_PLAIN);
    }
    else
    {
        snprintf(out, cap, "%s%.4s…%s", UIPOD_INDIC_PLAIN, val, val + n - 4);
    }
}

static const char *lfm_get_indicator(int sel, void *data)
{
    struct lfm_settings_page *p = (struct lfm_settings_page *)data;
    if (!p || sel < 0 || sel >= LFM_ROW_COUNT)
        return NULL;

    tp_lastfm_get_config(&p->cfg);
    char *out = p->indic_buf[sel];
    size_t cap = sizeof(p->indic_buf[sel]);

    switch (sel)
    {
        case LFM_ROW_ENABLED:
            return uipod_toggle_str(p->cfg.enabled != 0);
        case LFM_ROW_ACCOUNT:
            if (tp_lastfm_is_authenticated() && p->cfg.username[0])
                snprintf(out, cap, "%s@%s", UIPOD_INDIC_PLAIN, p->cfg.username);
            else if (tp_lastfm_is_authenticated())
                snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN, str(LANG_UIPOD_LFM_SIGNED_IN));
            else
                snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN, str(LANG_UIPOD_LFM_NOT_SIGNED_IN));
            return out;
        case LFM_ROW_SIGN_IN_QR:
        case LFM_ROW_SIGN_IN_PASS:
            return NULL;
        case LFM_ROW_STREAMING:
            return uipod_toggle_str(p->cfg.scrobble_streaming != 0);
        case LFM_ROW_SYNC_CACHE:
        case LFM_ROW_CLEAR_CACHE:
            snprintf(out, cap, "%s%u", UIPOD_INDIC_PLAIN,
                     (unsigned)tp_lastfm_cached_count());
            return out;
        case LFM_ROW_API_KEY:
            format_masked_key(p->cfg.api_key, out, cap);
            return out;
        case LFM_ROW_API_SECRET:
            snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN,
                     p->cfg.api_secret[0] ? "••••••••"
                                          : (const char *)str(LANG_UIPOD_NAV_NOT_SET));
            return out;
        default:
            return NULL;
    }
}

static bool lfm_get_chevron(int sel, void *data)
{
    (void)data;
    return sel == LFM_ROW_SIGN_IN_QR || sel == LFM_ROW_SIGN_IN_PASS;
}

static void lfm_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct lfm_settings_page *)self)->lists);
}

static int lfm_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct lfm_settings_page *)self)->lists, &action);
    return action;
}

static void lfm_apply_and_persist(struct lfm_settings_page *p)
{
    tp_lastfm_set_config(&p->cfg);
    tp_lastfm_get_config(&p->cfg);
    lfm_save_config_to_disk();
    if (p->cfg.enabled && tp_lastfm_cached_count() > 0 && !uipod_offline_mode_get())
        lfm_wake_worker(true);
}

static enum uipod_page_result lfm_on_action(struct uipod_page *self, int action)
{
    struct lfm_settings_page *p = (struct lfm_settings_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);

    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;

    tp_lastfm_get_config(&p->cfg);

    if (action == ACTION_SETTINGS_DEC || action == ACTION_SETTINGS_INC)
    {
        if (sel == LFM_ROW_ENABLED)
        {
            p->cfg.enabled = p->cfg.enabled ? 0 : 1;
            lfm_apply_and_persist(p);
        }
        else if (sel == LFM_ROW_STREAMING)
        {
            p->cfg.scrobble_streaming = p->cfg.scrobble_streaming ? 0 : 1;
            lfm_apply_and_persist(p);
        }
        return UIPOD_PAGE_STAY;
    }

    if (action != ACTION_STD_OK)
        return UIPOD_PAGE_STAY;

    switch (sel)
    {
        case LFM_ROW_ENABLED:
            p->cfg.enabled = p->cfg.enabled ? 0 : 1;
            lfm_apply_and_persist(p);
            break;

        case LFM_ROW_ACCOUNT:
            if (tp_lastfm_is_authenticated())
            {
                if (uipod_confirm(str(LANG_UIPOD_LFM_SIGN_OUT_Q),
                                  p->cfg.username[0] ? p->cfg.username : NULL))
                {
                    tp_lastfm_logout();
                    tp_lastfm_get_config(&p->cfg);
                    lfm_save_config_to_disk();
                }
            }
            else
            {
                lfm_run_qr_auth();
                tp_lastfm_get_config(&p->cfg);
            }
            break;

        case LFM_ROW_SIGN_IN_QR:
            lfm_run_qr_auth();
            tp_lastfm_get_config(&p->cfg);
            break;

        case LFM_ROW_SIGN_IN_PASS:
            lfm_run_password_login(&p->cfg);
            break;

        case LFM_ROW_STREAMING:
            p->cfg.scrobble_streaming = p->cfg.scrobble_streaming ? 0 : 1;
            lfm_apply_and_persist(p);
            break;

        case LFM_ROW_SYNC_CACHE:
        {
            if (uipod_offline_mode_get())
            {
                splash(HZ, ID2P(LANG_UIPOD_OFFLINE_MODE));
                break;
            }
            if (!tp_lastfm_is_authenticated())
            {
                splash(HZ, ID2P(LANG_UIPOD_LFM_NOT_SIGNED_IN));
                break;
            }
            if (tp_lastfm_cached_count() == 0)
            {
                splash(HZ, ID2P(LANG_UIPOD_LFM_SYNC_EMPTY));
                break;
            }

            uipod_fullscreen_message(str(LANG_UIPOD_LFM_SYNCING), NULL);
            bool cfg_upd = false;
            char err[160];
            err[0] = '\0';
            int32_t flushed = tp_lastfm_sync_now(lfm_unix_now(), &cfg_upd,
                                                 err, sizeof(err));
            if (cfg_upd)
                lfm_save_config_to_disk();
            if (flushed >= 0)
                splashf(HZ * 3 / 2, str(LANG_UIPOD_LFM_SYNC_DONE), (int)flushed);
            else
                splashf(HZ * 3 / 2, "%s", err[0] ? err : (const char *)str(LANG_FAILED));
            break;
        }

        case LFM_ROW_CLEAR_CACHE:
            if (tp_lastfm_cached_count() == 0)
            {
                splash(HZ, ID2P(LANG_UIPOD_LFM_SYNC_EMPTY));
                break;
            }
            if (uipod_confirm(str(LANG_UIPOD_LFM_CLEAR_CACHE_Q), NULL))
                tp_lastfm_clear_cache();
            break;

        case LFM_ROW_API_KEY:
            if (uipod_kbd_input_ex(p->cfg.api_key, sizeof(p->cfg.api_key), true) == 0)
                lfm_apply_and_persist(p);
            break;

        case LFM_ROW_API_SECRET:
            if (uipod_kbd_input_ex(p->cfg.api_secret, sizeof(p->cfg.api_secret), true) == 0)
                lfm_apply_and_persist(p);
            break;

        default:
            break;
    }

    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable s_lfm_set_vt = {
    .legend    = NULL,
    .draw      = lfm_draw,
    .poll      = lfm_poll,
    .on_action = lfm_on_action,
};

static const int s_lfm_set_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL,
    ACTION_SETTINGS_DEC, ACTION_SETTINGS_INC, -1
};

char *uipod_lastfm_menu_label(int selected_item, void *data,
                              char *buffer, size_t buffer_len)
{
    (void)selected_item;
    (void)data;

    TpLfmConfigC cfg;
    memset(&cfg, 0, sizeof(cfg));
    tp_lastfm_get_config(&cfg);

    const char *title = str(LANG_UIPOD_LASTFM);
    char status[96];

    if (!cfg.enabled)
        strlcpy(status, str(LANG_OFF), sizeof(status));
    else if (tp_lastfm_is_authenticated() && cfg.username[0])
        snprintf(status, sizeof(status), "@%s", cfg.username);
    else if (tp_lastfm_is_authenticated())
        strlcpy(status, str(LANG_UIPOD_LFM_SIGNED_IN), sizeof(status));
    else
        strlcpy(status, str(LANG_UIPOD_LFM_NOT_SIGNED_IN), sizeof(status));

    snprintf(buffer, buffer_len, "%s%s%s",
             title, UIPOD_INDIC_VALUE_CHEVRON, status);
    return buffer;
}

int uipod_lastfm_menu_page(void)
{
    struct lfm_settings_page p;
    memset(&p, 0, sizeof(p));
    tp_lastfm_get_config(&p.cfg);

    p.base.vt       = &s_lfm_set_vt;
    p.base.context  = CONTEXT_SETTINGS_RECTRIGGER;
    p.base.allowed  = s_lfm_set_allowed;
    p.base.animated = true;
    p.base.title    = str(LANG_UIPOD_LASTFM);

    gui_synclist_init(&p.lists, lfm_get_name, &p, false, 1, NULL);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_UIPOD_LASTFM), Icon_NOICON);
    gui_synclist_set_nb_items(&p.lists, LFM_ROW_COUNT);
    gui_synclist_set_indicator_callback(&p.lists, lfm_get_indicator);
    gui_synclist_set_chevron_callback(&p.lists, lfm_get_chevron);
    gui_synclist_select_item(&p.lists, 0);

    uipod_page_run(&p.base);
    return 0;
}
