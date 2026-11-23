/***************************************************************************
 * UIPod: ListenBrainz Scrobbling & Offline Scrobble Cache integration.
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

#include "uipod_downloads.h"
#include "uipod_jellyfin.h"
#include "uipod_keyboard.h"
#include "uipod_listenbrainz.h"
#include "uipod_navidrome.h"
#include "uipod_page.h"
#include "uipod_rs_listenbrainz.h"
#include "uipod_rs_ui.h"
#include "uipod_transition.h"
#include "uipod_ui.h"

#ifdef RETRO_HANDHELD
#include "uipod-http-target.h"
#endif

#define LB_CFG_RB    ROCKBOX_DIR "/listenbrainz.cfg"
#define LB_CFG_SD_1  UIPOD_SDCARD_ROOT "/listenbrainz.cfg"
#define LB_CFG_SD_2  UIPOD_SDCARD_ROOT "/Tools/tg5040/UIPod.pak/listenbrainz.cfg"

#define LB_DEFAULT_URL "https://api.listenbrainz.org"

static pthread_mutex_t s_lb_mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t  s_lb_cv  = PTHREAD_COND_INITIALIZER;
static pthread_t       s_lb_thread;
static bool            s_worker_started = false;
static volatile int    s_shutdown       = 0;
static bool            s_want_sync      = false;
static bool            s_reset_backoff  = false;

static char            s_queue_path[512];

/* ---- Clock Helpers ---------------------------------------------------- */

static uint64_t lb_unix_now(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_REALTIME, &ts) == 0 && ts.tv_sec > 0)
        return (uint64_t)ts.tv_sec;
    return 0;
}

static uint64_t lb_mono_ms(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) == 0)
        return (uint64_t)ts.tv_sec * 1000ULL + (uint64_t)(ts.tv_nsec / 1000000L);
    return (uint64_t)current_tick * (1000ULL / HZ);
}

/* ---- HTTP Transport Wiring -------------------------------------------- */

#ifdef RETRO_HANDHELD
static int32_t lb_http_request_adapter(
    uint8_t is_https, const char *host, uint16_t port,
    const char *method, const char *pq, const char *hdrs,
    const uint8_t *body, size_t body_len, uint8_t allow_self_signed,
    int32_t timeout_ms, int32_t *out_status, uint8_t **out_data,
    size_t *out_len, char *err, size_t cap)
{
    return retrohh_http_request(method, is_https, host, port, pq, hdrs,
                                body, body_len, allow_self_signed, timeout_ms,
                                out_status, out_data, out_len, err, cap);
}
#else
static int32_t stub_lb_req(
    uint8_t is_https, const char *host, uint16_t port,
    const char *method, const char *pq, const char *hdrs,
    const uint8_t *body, size_t body_len, uint8_t allow_self_signed,
    int32_t timeout_ms, int32_t *out_status, uint8_t **out_data,
    size_t *out_len, char *err, size_t cap)
{
    (void)is_https; (void)host; (void)port; (void)method; (void)pq; (void)hdrs;
    (void)body; (void)body_len; (void)allow_self_signed; (void)timeout_ms;
    (void)out_status; (void)out_data; (void)out_len;
    snprintf(err, cap, "HTTP unsupported on this target");
    return -1;
}
static void stub_lb_free(uint8_t *p) { free(p); }
#endif

static const TpLbHttpVtable s_lb_http_vt = {
#ifdef RETRO_HANDHELD
    .http_request = lb_http_request_adapter,
    .http_free    = retrohh_nav_http_free,
#else
    .http_request = stub_lb_req,
    .http_free    = stub_lb_free,
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
                 "%s/.config/rockbox.org/listenbrainz_queue.tsv", home);
    }
    else
    {
        strlcpy(s_queue_path, UIPOD_TMP_ROOT "/uipod_listenbrainz_queue.tsv",
                sizeof(s_queue_path));
    }
}

static bool lb_read_text_file(const char *rb_path, char *buf, size_t cap)
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

static bool lb_save_config_to_disk(void)
{
    char ini[2048];
    size_t n = tp_listenbrainz_format_config_text(ini, sizeof(ini));
    if (n == 0)
        return false;
    int fd = open(LB_CFG_RB ".new", O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0)
        return false;
    bool ok = (write(fd, ini, n) == (ssize_t)n);
    close(fd);
    if (ok)
        rename(LB_CFG_RB ".new", LB_CFG_RB);
    return ok;
}

static void lb_load_or_import_config(void)
{
    char ini[2048];
    if (lb_read_text_file(LB_CFG_RB, ini, sizeof(ini)))
    {
        tp_listenbrainz_load_config_text(ini);
        if (tp_listenbrainz_has_token())
            return;
    }

    if (lb_read_text_file(LB_CFG_SD_1, ini, sizeof(ini)) ||
        lb_read_text_file(LB_CFG_SD_2, ini, sizeof(ini)))
    {
        if (tp_listenbrainz_load_config_text(ini) && tp_listenbrainz_has_token())
            lb_save_config_to_disk();
    }
}

/* ---- Background Worker Thread ----------------------------------------- */

static void lb_wake_worker(bool reset_backoff)
{
    pthread_mutex_lock(&s_lb_mtx);
    s_want_sync = true;
    if (reset_backoff)
        s_reset_backoff = true;
    pthread_cond_broadcast(&s_lb_cv);
    pthread_mutex_unlock(&s_lb_mtx);
}

static void *lb_worker_main(void *arg)
{
    (void)arg;

    uint64_t retry_after_mono = 0;
    uint64_t backoff_ms       = 15000ULL;

    pthread_mutex_lock(&s_lb_mtx);
    while (!__atomic_load_n(&s_shutdown, __ATOMIC_ACQUIRE))
    {
        bool has_active = tp_listenbrainz_has_active_track();
        uint64_t now_m  = lb_mono_ms();

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
                    pthread_cond_timedwait(&s_lb_cv, &s_lb_mtx, &ts);
                }
            }
            else
            {
                pthread_cond_wait(&s_lb_cv, &s_lb_mtx);
            }

            if (__atomic_load_n(&s_shutdown, __ATOMIC_ACQUIRE))
                break;

            /* Advance unpaused listening timer; commits to `listenbrainz_queue.tsv` at 50% / 4:00 */
            int st = audio_status();
            bool playing = ((st & AUDIO_STATUS_PLAY) != 0) &&
                           ((st & AUDIO_STATUS_PAUSE) == 0);
            if (tp_listenbrainz_on_playback_tick(lb_mono_ms(), playing))
                s_want_sync = true;

            continue;
        }

        s_want_sync = false;
        pthread_mutex_unlock(&s_lb_mtx);

        /* Run network sync outside mutex so audio/UI/Last.fm threads never block */
        bool cfg_updated = false;
        char err[160];
        err[0] = '\0';
        int32_t rc = tp_listenbrainz_sync_now(lb_unix_now(), &cfg_updated, err, sizeof(err));
        if (cfg_updated)
            lb_save_config_to_disk();

        pthread_mutex_lock(&s_lb_mtx);
        if (rc >= 0)
        {
            retry_after_mono = 0;
            backoff_ms       = 15000ULL;
            if (tp_listenbrainz_cached_count() > 0)
                s_want_sync = true;
        }
        else if (rc == -1)
        {
            /* Transient offline / Wi-Fi / 429 / 5xx failure: keep queue & back off */
            s_want_sync      = (tp_listenbrainz_cached_count() > 0);
            retry_after_mono = lb_mono_ms() + backoff_ms;
            backoff_ms *= 2ULL;
            if (backoff_ms > 300000ULL)
                backoff_ms = 300000ULL;
        }
        else
        {
            /* Not authenticated / disabled / 401 invalid token: wait until config changes */
            retry_after_mono = 0;
        }
    }
    pthread_mutex_unlock(&s_lb_mtx);
    return NULL;
}

/* ---- Lifecycle & Playback Hooks --------------------------------------- */

void uipod_listenbrainz_init(void)
{
    resolve_queue_path();
    tp_listenbrainz_init(s_queue_path, &s_lb_http_vt);
    lb_load_or_import_config();

    TpLbConfigC cfg;
    memset(&cfg, 0, sizeof(cfg));
    tp_listenbrainz_get_config(&cfg);
    bool needs_username_sync = cfg.enabled && tp_listenbrainz_has_token() && !cfg.username[0];

    pthread_mutex_lock(&s_lb_mtx);
    __atomic_store_n(&s_shutdown, 0, __ATOMIC_RELEASE);
    if (!s_worker_started)
    {
        if (pthread_create(&s_lb_thread, NULL, lb_worker_main, NULL) == 0)
            s_worker_started = true;
    }
    if ((tp_listenbrainz_cached_count() > 0 || needs_username_sync) &&
        !uipod_offline_mode_get())
    {
        s_want_sync = true;
        pthread_cond_broadcast(&s_lb_cv);
    }
    pthread_mutex_unlock(&s_lb_mtx);
}

void uipod_listenbrainz_shutdown(void)
{
    /* Commit any in-progress track that already crossed the scrobble threshold */
    tp_listenbrainz_on_track_finish(lb_mono_ms());

    pthread_mutex_lock(&s_lb_mtx);
    if (!s_worker_started)
    {
        pthread_mutex_unlock(&s_lb_mtx);
        return;
    }
    __atomic_store_n(&s_shutdown, 1, __ATOMIC_RELEASE);
    pthread_cond_broadcast(&s_lb_cv);
    pthread_mutex_unlock(&s_lb_mtx);

    pthread_join(s_lb_thread, NULL);
    s_worker_started = false;
}

void uipod_listenbrainz_notify_online(void)
{
    TpLbConfigC cfg;
    memset(&cfg, 0, sizeof(cfg));
    tp_listenbrainz_get_config(&cfg);
    bool needs_username = cfg.enabled && tp_listenbrainz_has_token() && !cfg.username[0];

    if (tp_listenbrainz_cached_count() > 0 || needs_username)
        lb_wake_worker(true);
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
    char *hash = strrchr(out, '#');
    if (hash && hash != out)
        *hash = '\0';
    char *dot = strrchr(out, '.');
    if (dot && dot != out)
        *dot = '\0';
}

void uipod_listenbrainz_on_track_change(const struct mp3entry *id3)
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

    if (tp_listenbrainz_on_track_start(id3->path, artist, title, album, aa,
                                       track_num, dur_ms, is_streaming,
                                       lb_unix_now(), lb_mono_ms()))
    {
        pthread_mutex_lock(&s_lb_mtx);
        s_want_sync = true;
        pthread_cond_broadcast(&s_lb_cv);
        pthread_mutex_unlock(&s_lb_mtx);
    }
}

void uipod_listenbrainz_on_track_finish(const struct mp3entry *id3)
{
    (void)id3;
    if (tp_listenbrainz_on_track_finish(lb_mono_ms()))
    {
        pthread_mutex_lock(&s_lb_mtx);
        s_want_sync = true;
        pthread_cond_broadcast(&s_lb_cv);
        pthread_mutex_unlock(&s_lb_mtx);
    }
}

/* ---- Status Summary for `Settings -> Accounts` ------------------------ */

char *uipod_listenbrainz_menu_label(int selected_item, void *data,
                                    char *buffer, size_t buffer_len)
{
    (void)selected_item;
    (void)data;

    TpLbConfigC cfg;
    memset(&cfg, 0, sizeof(cfg));
    tp_listenbrainz_get_config(&cfg);

    const char *title = str(LANG_UIPOD_LISTENBRAINZ);
    char status[96];

    if (!cfg.enabled)
        strlcpy(status, str(LANG_OFF), sizeof(status));
    else if (tp_listenbrainz_is_token_invalid())
        strlcpy(status, str(LANG_UIPOD_LB_INVALID_TOKEN), sizeof(status));
    else if (tp_listenbrainz_is_authenticated() && cfg.username[0])
        snprintf(status, sizeof(status), "@%s", cfg.username);
    else if (tp_listenbrainz_is_authenticated())
        strlcpy(status, str(LANG_UIPOD_LFM_SIGNED_IN), sizeof(status));
    else
        strlcpy(status, str(LANG_UIPOD_LFM_NOT_SIGNED_IN), sizeof(status));

    snprintf(buffer, buffer_len, "%s%s%s",
             title, UIPOD_INDIC_VALUE_CHEVRON, status);
    return buffer;
}

/* ---- Token Validation & Sign-In Flow ---------------------------------- */

static bool lb_validate_and_persist(TpLbConfigC *cfg, bool show_splash)
{
    if (!tp_listenbrainz_has_token())
    {
        if (show_splash)
            splash(HZ * 3 / 2, ID2P(LANG_UIPOD_LB_SET_TOKEN_FIRST));
        return false;
    }
    if (uipod_offline_mode_get())
    {
        lb_save_config_to_disk();
        if (show_splash)
            splash(HZ, ID2P(LANG_UIPOD_OFFLINE_MODE));
        return false;
    }

    uipod_fullscreen_message(str(LANG_UIPOD_LB_SYNCING), NULL);

    char err[160];
    err[0] = '\0';
    int32_t rc = tp_listenbrainz_validate_token(err, sizeof(err));
    tp_listenbrainz_get_config(cfg);
    lb_save_config_to_disk();

    if (rc == 0)
    {
        lb_wake_worker(true);
        if (show_splash)
        {
            if (cfg->username[0])
                splashf(HZ * 3 / 2, "%s: @%s", str(LANG_UIPOD_LFM_SIGNED_IN), cfg->username);
            else
                splash(HZ, ID2P(LANG_UIPOD_LFM_SIGNED_IN));
        }
        return true;
    }

    if (show_splash)
        splashf(HZ * 3 / 2, "%s", err[0] ? err : (const char *)str(LANG_FAILED));
    return false;
}

static bool lb_run_token_signin(TpLbConfigC *cfg)
{
    char tok[128];
    strlcpy(tok, cfg->user_token, sizeof(tok));
    if (uipod_kbd_input_ex(tok, sizeof(tok), true) != 0 || !tok[0])
        return false;

    strlcpy(cfg->user_token, tok, sizeof(cfg->user_token));
    tp_listenbrainz_set_config(cfg);
    tp_listenbrainz_get_config(cfg);
    lb_save_config_to_disk();

    return lb_validate_and_persist(cfg, true);
}

/* ---- Settings -> Accounts -> ListenBrainz Page ------------------------ */

enum
{
    LB_ROW_ENABLED = 0,
    LB_ROW_ACCOUNT,
    LB_ROW_SIGN_IN_TOKEN,
    LB_ROW_NOW_PLAYING,
    LB_ROW_STREAMING,
    LB_ROW_SYNC_CACHE,
    LB_ROW_CLEAR_CACHE,
    LB_ROW_USER_TOKEN,
    LB_ROW_API_URL,
    LB_ROW_COUNT
};

struct lb_settings_page
{
    struct uipod_page   base;
    struct gui_synclist lists;
    TpLbConfigC         cfg;
    char                indic_buf[LB_ROW_COUNT][128];
};

static const char *lb_get_name(int sel, void *data, char *buf, size_t len)
{
    (void)data; (void)buf; (void)len;
    switch (sel)
    {
        case LB_ROW_ENABLED:       return str(LANG_UIPOD_LFM_SCROBBLING);
        case LB_ROW_ACCOUNT:       return str(LANG_UIPOD_LFM_ACCOUNT);
        case LB_ROW_SIGN_IN_TOKEN: return str(LANG_UIPOD_LB_SIGN_IN_TOKEN);
        case LB_ROW_NOW_PLAYING:   return str(LANG_UIPOD_LB_NOW_PLAYING);
        case LB_ROW_STREAMING:     return str(LANG_UIPOD_LFM_SCROBBLE_STREAMING);
        case LB_ROW_SYNC_CACHE:    return str(LANG_UIPOD_LFM_SYNC_CACHE);
        case LB_ROW_CLEAR_CACHE:   return str(LANG_UIPOD_LFM_CLEAR_CACHE);
        case LB_ROW_USER_TOKEN:    return str(LANG_UIPOD_LB_USER_TOKEN);
        case LB_ROW_API_URL:       return str(LANG_UIPOD_NAV_SERVER_URL);
        default:                   return "";
    }
}

static void lb_format_masked_token(const char *val, char *out, size_t cap)
{
    if (!val || !val[0])
    {
        snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN,
                 (const char *)str(LANG_UIPOD_NAV_NOT_SET));
        return;
    }
    size_t n = strlen(val);
    if (n <= 8)
        snprintf(out, cap, "%s••••••••", UIPOD_INDIC_PLAIN);
    else
        snprintf(out, cap, "%s%.4s…%s", UIPOD_INDIC_PLAIN, val, val + n - 4);
}

static void lb_format_url_indicator(const char *url, char *out, size_t cap)
{
    if (!url || !url[0])
    {
        snprintf(out, cap, "%sapi.listenbrainz.org", UIPOD_INDIC_PLAIN);
        return;
    }
    const char *disp = url;
    if (strncasecmp(disp, "https://", 8) == 0)
        disp += 8;
    else if (strncasecmp(disp, "http://", 7) == 0)
        disp += 7;
    snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN, disp);
}

static const char *lb_get_indicator(int sel, void *data)
{
    struct lb_settings_page *p = (struct lb_settings_page *)data;
    if (!p || sel < 0 || sel >= LB_ROW_COUNT)
        return NULL;

    tp_listenbrainz_get_config(&p->cfg);
    char *out = p->indic_buf[sel];
    size_t cap = sizeof(p->indic_buf[sel]);

    switch (sel)
    {
        case LB_ROW_ENABLED:
            return uipod_toggle_str(p->cfg.enabled != 0);
        case LB_ROW_ACCOUNT:
            if (tp_listenbrainz_is_token_invalid())
                snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN, str(LANG_UIPOD_LB_INVALID_TOKEN));
            else if (tp_listenbrainz_is_authenticated() && p->cfg.username[0])
                snprintf(out, cap, "%s@%s", UIPOD_INDIC_PLAIN, p->cfg.username);
            else if (tp_listenbrainz_is_authenticated())
                snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN, str(LANG_UIPOD_LFM_SIGNED_IN));
            else
                snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN, str(LANG_UIPOD_LFM_NOT_SIGNED_IN));
            return out;
        case LB_ROW_SIGN_IN_TOKEN:
            return NULL;
        case LB_ROW_NOW_PLAYING:
            return uipod_toggle_str(p->cfg.now_playing != 0);
        case LB_ROW_STREAMING:
            return uipod_toggle_str(p->cfg.scrobble_streaming != 0);
        case LB_ROW_SYNC_CACHE:
        case LB_ROW_CLEAR_CACHE:
            snprintf(out, cap, "%s%u", UIPOD_INDIC_PLAIN,
                     (unsigned)tp_listenbrainz_cached_count());
            return out;
        case LB_ROW_USER_TOKEN:
            lb_format_masked_token(p->cfg.user_token, out, cap);
            return out;
        case LB_ROW_API_URL:
            lb_format_url_indicator(p->cfg.api_url, out, cap);
            return out;
        default:
            return NULL;
    }
}

static bool lb_get_chevron(int sel, void *data)
{
    (void)data;
    return sel == LB_ROW_SIGN_IN_TOKEN;
}

static void lb_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct lb_settings_page *)self)->lists);
}

static int lb_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct lb_settings_page *)self)->lists, &action);
    return action;
}

static void lb_apply_and_persist(struct lb_settings_page *p)
{
    tp_listenbrainz_set_config(&p->cfg);
    tp_listenbrainz_get_config(&p->cfg);
    lb_save_config_to_disk();
    if (p->cfg.enabled && tp_listenbrainz_cached_count() > 0 && !uipod_offline_mode_get())
        lb_wake_worker(true);
}

static enum uipod_page_result lb_on_action(struct uipod_page *self, int action)
{
    struct lb_settings_page *p = (struct lb_settings_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);

    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;

    tp_listenbrainz_get_config(&p->cfg);

    if (action == ACTION_SETTINGS_DEC || action == ACTION_SETTINGS_INC)
    {
        if (sel == LB_ROW_ENABLED)
        {
            p->cfg.enabled = p->cfg.enabled ? 0 : 1;
            lb_apply_and_persist(p);
        }
        else if (sel == LB_ROW_NOW_PLAYING)
        {
            p->cfg.now_playing = p->cfg.now_playing ? 0 : 1;
            lb_apply_and_persist(p);
        }
        else if (sel == LB_ROW_STREAMING)
        {
            p->cfg.scrobble_streaming = p->cfg.scrobble_streaming ? 0 : 1;
            lb_apply_and_persist(p);
        }
        return UIPOD_PAGE_STAY;
    }

    if (action != ACTION_STD_OK)
        return UIPOD_PAGE_STAY;

    switch (sel)
    {
        case LB_ROW_ENABLED:
            p->cfg.enabled = p->cfg.enabled ? 0 : 1;
            lb_apply_and_persist(p);
            break;

        case LB_ROW_ACCOUNT:
            if (tp_listenbrainz_has_token() && !tp_listenbrainz_is_token_invalid())
            {
                if (uipod_confirm(str(LANG_UIPOD_LB_SIGN_OUT_Q),
                                  p->cfg.username[0] ? p->cfg.username : NULL))
                {
                    tp_listenbrainz_logout();
                    tp_listenbrainz_get_config(&p->cfg);
                    lb_save_config_to_disk();
                }
            }
            else
            {
                lb_run_token_signin(&p->cfg);
            }
            break;

        case LB_ROW_SIGN_IN_TOKEN:
            lb_run_token_signin(&p->cfg);
            break;

        case LB_ROW_NOW_PLAYING:
            p->cfg.now_playing = p->cfg.now_playing ? 0 : 1;
            lb_apply_and_persist(p);
            break;

        case LB_ROW_STREAMING:
            p->cfg.scrobble_streaming = p->cfg.scrobble_streaming ? 0 : 1;
            lb_apply_and_persist(p);
            break;

        case LB_ROW_SYNC_CACHE:
        {
            if (uipod_offline_mode_get())
            {
                splash(HZ, ID2P(LANG_UIPOD_OFFLINE_MODE));
                break;
            }
            if (!tp_listenbrainz_has_token())
            {
                splash(HZ, ID2P(LANG_UIPOD_LFM_NOT_SIGNED_IN));
                break;
            }
            if (tp_listenbrainz_cached_count() == 0)
            {
                splash(HZ, ID2P(LANG_UIPOD_LFM_SYNC_EMPTY));
                break;
            }

            uipod_fullscreen_message(str(LANG_UIPOD_LB_SYNCING), NULL);
            bool cfg_upd = false;
            char err[160];
            err[0] = '\0';
            int32_t flushed = tp_listenbrainz_sync_now(lb_unix_now(), &cfg_upd,
                                                       err, sizeof(err));
            if (cfg_upd)
                lb_save_config_to_disk();
            if (flushed >= 0)
                splashf(HZ * 3 / 2, str(LANG_UIPOD_LFM_SYNC_DONE), (int)flushed);
            else
                splashf(HZ * 3 / 2, "%s", err[0] ? err : (const char *)str(LANG_FAILED));
            break;
        }

        case LB_ROW_CLEAR_CACHE:
            if (tp_listenbrainz_cached_count() == 0)
            {
                splash(HZ, ID2P(LANG_UIPOD_LFM_SYNC_EMPTY));
                break;
            }
            if (uipod_confirm(str(LANG_UIPOD_LFM_CLEAR_CACHE_Q), NULL))
                tp_listenbrainz_clear_cache();
            break;

        case LB_ROW_USER_TOKEN:
            if (uipod_kbd_input_ex(p->cfg.user_token, sizeof(p->cfg.user_token), true) == 0)
            {
                lb_apply_and_persist(p);
                if (p->cfg.user_token[0] && !uipod_offline_mode_get())
                    lb_validate_and_persist(&p->cfg, true);
            }
            break;

        case LB_ROW_API_URL:
            if (uipod_kbd_input_ex(p->cfg.api_url, sizeof(p->cfg.api_url), true) == 0)
            {
                if (!p->cfg.api_url[0])
                    strlcpy(p->cfg.api_url, LB_DEFAULT_URL, sizeof(p->cfg.api_url));
                lb_apply_and_persist(p);
                if (p->cfg.user_token[0] && !uipod_offline_mode_get())
                    lb_validate_and_persist(&p->cfg, true);
            }
            break;

        default:
            break;
    }

    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable s_lb_set_vt = {
    .legend    = NULL,
    .draw      = lb_draw,
    .poll      = lb_poll,
    .on_action = lb_on_action,
};

static const int s_lb_set_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL,
    ACTION_SETTINGS_DEC, ACTION_SETTINGS_INC, -1
};

int uipod_listenbrainz_menu_page(void)
{
    struct lb_settings_page p;
    memset(&p, 0, sizeof(p));
    tp_listenbrainz_get_config(&p.cfg);

    p.base.vt       = &s_lb_set_vt;
    p.base.context  = CONTEXT_SETTINGS_RECTRIGGER;
    p.base.allowed  = s_lb_set_allowed;
    p.base.animated = true;
    p.base.title    = str(LANG_UIPOD_LISTENBRAINZ);

    gui_synclist_init(&p.lists, lb_get_name, &p, false, 1, NULL);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_UIPOD_LISTENBRAINZ), Icon_NOICON);
    gui_synclist_set_nb_items(&p.lists, LB_ROW_COUNT);
    gui_synclist_set_indicator_callback(&p.lists, lb_get_indicator);
    gui_synclist_set_chevron_callback(&p.lists, lb_get_chevron);
    gui_synclist_select_item(&p.lists, 0);

    uipod_page_run(&p.base);
    return 0;
}
