/***************************************************************************
 * UIPod: Persistent Non-Blocking Background Download Queue & Offline Mode.
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

#include "action.h"
#include "file.h"
#include "gui/list.h"
#include "icon.h"
#include "kernel.h"
#include "lang.h"
#include "menu.h"
#include "misc.h"
#include "rbpaths.h"
#include "settings.h"
#include "splash.h"
#include "string-extra.h"
#include "system.h"

#include "uipod_downloads.h"
#include "uipod_library.h"
#include "uipod_page.h"
#include "uipod_rs_jellyfin.h"
#include "uipod_rs_navidrome.h"
#include "uipod_rs_ui.h"
#include "uipod_svg.h"
#include "uipod_transition.h"
#include "uipod_ui.h"

#define OFFLINE_MODE_FILE  ROCKBOX_DIR "/offline_mode.txt"
#define DL_MAX_ITEMS       4096

struct dl_entry
{
    uint32_t uid;
    uint8_t  kind;       /* enum uipod_dl_kind */
    uint8_t  state;      /* enum uipod_dl_state */
    char     title[192];
    char     local_path[MAX_PATH];
    char     id[128];
    char     sub_id[128];
    char     cover_id[128];
    char     suffix[32];
    char     vcodec[32];
    char     acodec[32];
    char     channel_name[128];
    uint32_t width;
    uint32_t height;
    uint32_t bit_depth;
    uint32_t bitrate;
};

static volatile int     s_offline_mode = 0;

static pthread_mutex_t  s_dl_mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t   s_dl_cv  = PTHREAD_COND_INITIALIZER;
static pthread_t        s_worker_thread;
static bool             s_worker_started = false;
static bool             s_worker_busy    = false;
static volatile int     s_shutdown       = 0;
static volatile int     s_cancel_active  = 0;
static uint32_t         s_active_uid     = 0;
static uint32_t         s_next_uid       = 1;

static struct dl_entry *s_items          = NULL;
static int              s_count          = 0;
static int              s_cap            = 0;

static uint32_t         s_batch_done     = 0;
static uint32_t         s_batch_total    = 0;
static volatile uint32_t s_completed_seq = 0;

static char             s_store_path[512];

/* ---- Small Helpers ---------------------------------------------------- */

static void sanitize_tsv_field(const char *src, char *dst, size_t cap)
{
    if (cap == 0)
        return;
    if (!src)
    {
        dst[0] = '\0';
        return;
    }
    size_t j = 0;
    for (size_t i = 0; src[i] && j + 1 < cap; i++)
    {
        char c = src[i];
        if (c == '\t' || c == '\r' || c == '\n')
            c = ' ';
        dst[j++] = c;
    }
    dst[j] = '\0';
}

static bool os_file_present(const char *p)
{
    if (!p || !p[0])
        return false;
    struct stat st;
    return stat(p, &st) == 0 && S_ISREG(st.st_mode) && st.st_size > 0;
}

static bool entry_target_exists_on_disk(const struct dl_entry *e)
{
    if (!e || !e->local_path[0])
        return false;
    if (e->kind == UIPOD_DL_JF_VIDEO)
    {
        char resolved[MAX_PATH];
        return tp_jellyfin_resolve_downloaded_video(
                   e->local_path, resolved, sizeof(resolved)) > 0;
    }
    return os_file_present(e->local_path);
}

static int count_queued_locked(void)
{
    int n = 0;
    for (int i = 0; i < s_count; i++)
    {
        if (s_items[i].state == UIPOD_DL_STATE_QUEUED)
            n++;
    }
    return n;
}

/* ---- Persistent TSV Serialization ------------------------------------- */

static void resolve_store_path(void)
{
    const char *home = getenv("HOME");
    if (home && home[0])
    {
        char dir1[512], dir2[512];
        snprintf(dir1, sizeof(dir1), "%s/.config", home);
        mkdir(dir1, 0777);
        snprintf(dir2, sizeof(dir2), "%s/.config/rockbox.org", home);
        mkdir(dir2, 0777);
        snprintf(s_store_path, sizeof(s_store_path),
                 "%s/.config/rockbox.org/downloads.tsv", home);
    }
    else
    {
        strlcpy(s_store_path, UIPOD_TMP_ROOT "/uipod_downloads.tsv", sizeof(s_store_path));
    }
}

static void save_queue_locked(void)
{
    if (!s_store_path[0])
        return;

    char tmp_path[540];
    snprintf(tmp_path, sizeof(tmp_path), "%s.new", s_store_path);

    FILE *f = fopen(tmp_path, "wb");
    if (!f)
        return;

    fprintf(f, "#UIPOD_DL_V1\t%u\t%u\n",
            (unsigned)s_batch_done, (unsigned)s_batch_total);

    for (int i = 0; i < s_count; i++)
    {
        const struct dl_entry *e = &s_items[i];
        /* Persist in-flight items as QUEUED so an abrupt power-off resumes them */
        uint8_t disk_state = (e->state == UIPOD_DL_STATE_DOWNLOADING)
                                 ? (uint8_t)UIPOD_DL_STATE_QUEUED
                                 : e->state;
        fprintf(f, "%u\t%u\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%u\t%u\t%u\t%u\n",
                (unsigned)e->kind,
                (unsigned)disk_state,
                e->title,
                e->local_path,
                e->id,
                e->sub_id,
                e->cover_id,
                e->suffix,
                e->vcodec,
                e->acodec,
                e->channel_name,
                (unsigned)e->width,
                (unsigned)e->height,
                (unsigned)e->bit_depth,
                (unsigned)e->bitrate);
    }

    fflush(f);
    fclose(f);
    rename(tmp_path, s_store_path);
}

static bool ensure_cap_locked(int needed)
{
    if (needed <= s_cap)
        return true;
    int ncap = s_cap ? s_cap * 2 : 64;
    while (ncap < needed)
        ncap *= 2;
    struct dl_entry *nv = realloc(s_items, (size_t)ncap * sizeof(*nv));
    if (!nv)
        return false;
    s_items = nv;
    s_cap   = ncap;
    return true;
}

static void load_queue_locked(void)
{
    s_count       = 0;
    s_batch_done  = 0;
    s_batch_total = 0;

    if (!s_store_path[0])
        return;

    FILE *f = fopen(s_store_path, "rb");
    if (!f)
        return;

    char line[2048];
    while (fgets(line, sizeof(line), f))
    {
        size_t len = strlen(line);
        while (len > 0 && (line[len - 1] == '\n' || line[len - 1] == '\r'))
            line[--len] = '\0';
        if (len == 0)
            continue;

        if (strncmp(line, "#UIPOD_DL_V1\t", 13) == 0)
        {
            unsigned bd = 0, bt = 0;
            if (sscanf(line + 13, "%u\t%u", &bd, &bt) == 2)
            {
                s_batch_done  = bd;
                s_batch_total = bt;
            }
            continue;
        }
        if (line[0] == '#')
            continue;

        if (s_count >= DL_MAX_ITEMS || !ensure_cap_locked(s_count + 1))
            break;

        char *cols[15];
        int ncol = 0;
        char *cur = line;
        while (ncol < 15)
        {
            cols[ncol++] = cur;
            char *tab = strchr(cur, '\t');
            if (!tab)
                break;
            *tab = '\0';
            cur = tab + 1;
        }
        if (ncol < 15)
            continue;

        struct dl_entry *e = &s_items[s_count];
        memset(e, 0, sizeof(*e));
        e->uid   = s_next_uid++;
        e->kind  = (uint8_t)strtoul(cols[0], NULL, 10);
        e->state = (uint8_t)strtoul(cols[1], NULL, 10);
        if (e->state == UIPOD_DL_STATE_DOWNLOADING)
            e->state = UIPOD_DL_STATE_QUEUED;
        if (e->state > UIPOD_DL_STATE_FAILED)
            e->state = UIPOD_DL_STATE_QUEUED;

        strlcpy(e->title,        cols[2],  sizeof(e->title));
        strlcpy(e->local_path,   cols[3],  sizeof(e->local_path));
        strlcpy(e->id,           cols[4],  sizeof(e->id));
        strlcpy(e->sub_id,       cols[5],  sizeof(e->sub_id));
        strlcpy(e->cover_id,     cols[6],  sizeof(e->cover_id));
        strlcpy(e->suffix,       cols[7],  sizeof(e->suffix));
        strlcpy(e->vcodec,       cols[8],  sizeof(e->vcodec));
        strlcpy(e->acodec,       cols[9],  sizeof(e->acodec));
        strlcpy(e->channel_name, cols[10], sizeof(e->channel_name));
        e->width     = (uint32_t)strtoul(cols[11], NULL, 10);
        e->height    = (uint32_t)strtoul(cols[12], NULL, 10);
        e->bit_depth = (uint32_t)strtoul(cols[13], NULL, 10);
        e->bitrate   = (uint32_t)strtoul(cols[14], NULL, 10);

        s_count++;
    }

    fclose(f);

    int q = count_queued_locked();
    if (q == 0)
    {
        s_batch_done  = 0;
        s_batch_total = 0;
    }
    else if (s_batch_total < s_batch_done + (uint32_t)q)
    {
        s_batch_total = s_batch_done + (uint32_t)q;
    }
}

/* ---- Offline Mode Implementation -------------------------------------- */

void uipod_offline_mode_init(void)
{
    int val = 0;
    int fd = open(OFFLINE_MODE_FILE, O_RDONLY);
    if (fd >= 0)
    {
        char buf[16];
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        close(fd);
        if (n > 0)
        {
            buf[n] = '\0';
            if (buf[0] == '1')
                val = 1;
        }
    }
    __atomic_store_n(&s_offline_mode, val, __ATOMIC_RELEASE);
}

bool uipod_offline_mode_get(void)
{
    return __atomic_load_n(&s_offline_mode, __ATOMIC_ACQUIRE) != 0;
}

void uipod_offline_mode_set(bool enabled)
{
    int nv = enabled ? 1 : 0;
    int prev = __atomic_exchange_n(&s_offline_mode, nv, __ATOMIC_ACQ_REL);
    if (prev == nv)
        return;

    int fd = open(OFFLINE_MODE_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd >= 0)
    {
        const char *txt = enabled ? "1\n" : "0\n";
        write(fd, txt, 2);
        close(fd);
    }

    /* Rebuild merged library table immediately (offline cached+local vs online) */
    uipod_library_bump_remote_gen();

    pthread_mutex_lock(&s_dl_mtx);
    if (enabled)
    {
        /* Abort any in-flight HTTP download cleanly; worker keeps it QUEUED */
        __atomic_store_n(&s_cancel_active, 1, __ATOMIC_RELEASE);
    }
    else
    {
        /* Wake worker to resume queued downloads now that we're back online */
        pthread_cond_broadcast(&s_dl_cv);
    }
    pthread_mutex_unlock(&s_dl_mtx);

    uipod_gpu_render_header();
}

static const char *offline_toggle_get(void *ctx, char *buf, int len)
{
    (void)ctx; (void)buf; (void)len;
    return uipod_toggle_str(uipod_offline_mode_get());
}

static void offline_toggle_cycle(void *ctx, int dir)
{
    (void)ctx; (void)dir;
    uipod_offline_mode_set(!uipod_offline_mode_get());
}

const struct menu_value_cb uipod_offline_mode_value_cb = {
    offline_toggle_get,
    offline_toggle_cycle,
    NULL
};

/* ---- Background Download Worker Thread -------------------------------- */

static uint8_t dl_worker_cancel_cb(void *ctx)
{
    (void)ctx;
    if (__atomic_load_n(&s_shutdown, __ATOMIC_ACQUIRE) != 0)
        return 1;
    if (__atomic_load_n(&s_cancel_active, __ATOMIC_ACQUIRE) != 0)
        return 1;
    if (uipod_offline_mode_get())
        return 1;
    return 0;
}

static int find_index_by_uid_locked(uint32_t uid)
{
    for (int i = 0; i < s_count; i++)
    {
        if (s_items[i].uid == uid)
            return i;
    }
    return -1;
}

static void *dl_worker_main(void *arg)
{
    (void)arg;
    pthread_mutex_lock(&s_dl_mtx);

    while (!__atomic_load_n(&s_shutdown, __ATOMIC_ACQUIRE))
    {
        int next_idx = -1;
        if (!uipod_offline_mode_get())
        {
            for (int i = 0; i < s_count; i++)
            {
                if (s_items[i].state == UIPOD_DL_STATE_QUEUED)
                {
                    next_idx = i;
                    break;
                }
            }
        }

        if (next_idx < 0)
        {
            pthread_cond_wait(&s_dl_cv, &s_dl_mtx);
            continue;
        }

        struct dl_entry job = s_items[next_idx];
        s_items[next_idx].state = UIPOD_DL_STATE_DOWNLOADING;
        s_worker_busy           = true;
        s_active_uid            = job.uid;
        __atomic_store_n(&s_cancel_active, 0, __ATOMIC_RELEASE);
        pthread_mutex_unlock(&s_dl_mtx);

        int32_t rc = -1;
        char err[160];
        err[0] = '\0';

        switch (job.kind)
        {
            case UIPOD_DL_NAV_AUDIO:
            {
                rc = tp_navidrome_ensure_track_cached(
                    job.local_path,
                    job.id,
                    job.cover_id,
                    dl_worker_cancel_cb,
                    NULL,
                    err,
                    sizeof(err));
                if (rc == 0 && os_file_present(job.local_path))
                {
                    if (job.cover_id[0] && !dl_worker_cancel_cb(NULL))
                    {
                        char cov_buf[MAX_PATH];
                        tp_navidrome_ensure_cover_cached(
                            job.local_path, job.cover_id, cov_buf, sizeof(cov_buf));
                    }
                    tp_navidrome_pin_track(job.local_path);
                }
                else if (rc == 0)
                {
                    rc = -1;
                }
                break;
            }
            case UIPOD_DL_JF_AUDIO:
            {
                rc = tp_jellyfin_ensure_audio_cached(
                    job.local_path,
                    job.id,
                    job.suffix,
                    job.cover_id,
                    1,
                    dl_worker_cancel_cb,
                    NULL,
                    err,
                    sizeof(err));
                if (rc == 0 && !os_file_present(job.local_path))
                    rc = -1;
                break;
            }
            case UIPOD_DL_JF_VIDEO:
            {
                if (entry_target_exists_on_disk(&job))
                {
                    rc = 0;
                }
                else
                {
                    rc = tp_jellyfin_download_video(
                        job.local_path,
                        job.id,
                        job.sub_id,
                        job.suffix,
                        job.vcodec,
                        job.acodec,
                        job.width,
                        job.height,
                        job.bit_depth,
                        job.bitrate,
                        dl_worker_cancel_cb,
                        NULL,
                        err,
                        sizeof(err));
                }
                break;
            }
            default:
                rc = -1;
                break;
        }

        bool was_paused_or_shutdown =
            (__atomic_load_n(&s_shutdown, __ATOMIC_ACQUIRE) != 0) ||
            uipod_offline_mode_get();

        pthread_mutex_lock(&s_dl_mtx);
        s_worker_busy = false;
        s_active_uid  = 0;

        int cur_idx = find_index_by_uid_locked(job.uid);
        if (cur_idx >= 0)
        {
            if (was_paused_or_shutdown && rc != 0)
            {
                /* Paused by Offline Mode or clean app exit -> keep QUEUED for resume */
                s_items[cur_idx].state = UIPOD_DL_STATE_QUEUED;
            }
            else if (rc == 0)
            {
                s_items[cur_idx].state = UIPOD_DL_STATE_DONE;
                if (s_batch_done < s_batch_total)
                    s_batch_done++;
                __atomic_fetch_add(&s_completed_seq, 1, __ATOMIC_RELEASE);
            }
            else
            {
                s_items[cur_idx].state = UIPOD_DL_STATE_FAILED;
            }
        }

        if (count_queued_locked() == 0)
        {
            /* Batch finished; reset counters once idle so next enqueue starts at 0/N */
            s_batch_done  = 0;
            s_batch_total = 0;
        }

        save_queue_locked();
    }

    pthread_mutex_unlock(&s_dl_mtx);
    return NULL;
}

void uipod_downloads_init(void)
{
    resolve_store_path();

    pthread_mutex_lock(&s_dl_mtx);
    __atomic_store_n(&s_shutdown, 0, __ATOMIC_RELEASE);
    __atomic_store_n(&s_cancel_active, 0, __ATOMIC_RELEASE);
    load_queue_locked();
    if (!s_worker_started)
    {
        if (pthread_create(&s_worker_thread, NULL, dl_worker_main, NULL) == 0)
            s_worker_started = true;
    }
    if (count_queued_locked() > 0 && !uipod_offline_mode_get())
        pthread_cond_broadcast(&s_dl_cv);
    pthread_mutex_unlock(&s_dl_mtx);
}

void uipod_downloads_shutdown(void)
{
    pthread_mutex_lock(&s_dl_mtx);
    if (!s_worker_started)
    {
        save_queue_locked();
        pthread_mutex_unlock(&s_dl_mtx);
        return;
    }
    __atomic_store_n(&s_shutdown, 1, __ATOMIC_RELEASE);
    __atomic_store_n(&s_cancel_active, 1, __ATOMIC_RELEASE);
    pthread_cond_broadcast(&s_dl_cv);
    pthread_mutex_unlock(&s_dl_mtx);

    pthread_join(s_worker_thread, NULL);

    pthread_mutex_lock(&s_dl_mtx);
    s_worker_started = false;
    save_queue_locked();
    pthread_mutex_unlock(&s_dl_mtx);
}

void uipod_downloads_header_status(bool *out_active,
                                   uint32_t *out_done,
                                   uint32_t *out_total)
{
    pthread_mutex_lock(&s_dl_mtx);
    int q = count_queued_locked();
    bool active = (s_worker_busy || q > 0) && !uipod_offline_mode_get();
    uint32_t done  = active ? s_batch_done  : 0;
    uint32_t total = active ? s_batch_total : 0;
    if (active && total == 0)
        total = done + (uint32_t)q + (s_worker_busy ? 1U : 0U);
    pthread_mutex_unlock(&s_dl_mtx);

    if (out_active) *out_active = active;
    if (out_done)   *out_done   = done;
    if (out_total)  *out_total  = total;
}

uint32_t uipod_downloads_completed_seq(void)
{
    return __atomic_load_n(&s_completed_seq, __ATOMIC_ACQUIRE);
}

/* ---- Non-Blocking Enqueue APIs ---------------------------------------- */

static bool enqueue_item(const struct dl_entry *proto)
{
    if (!proto)
        return false;

    bool changed = false;
    pthread_mutex_lock(&s_dl_mtx);

    bool was_idle = (count_queued_locked() == 0 && !s_worker_busy);

    /* Deduplicate against existing entries */
    int existing = -1;
    for (int i = 0; i < s_count; i++)
    {
        if (s_items[i].kind != proto->kind)
            continue;
        if (proto->local_path[0] && s_items[i].local_path[0] &&
            strcmp(s_items[i].local_path, proto->local_path) == 0)
        {
            existing = i;
            break;
        }
        if (proto->id[0] && s_items[i].id[0] &&
            strcmp(s_items[i].id, proto->id) == 0)
        {
            existing = i;
            break;
        }
    }

    if (existing >= 0)
    {
        struct dl_entry *e = &s_items[existing];
        if (e->state == UIPOD_DL_STATE_FAILED ||
            (e->state == UIPOD_DL_STATE_DONE && !entry_target_exists_on_disk(e)))
        {
            if (was_idle)
            {
                s_batch_done  = 0;
                s_batch_total = 0;
            }
            e->state = UIPOD_DL_STATE_QUEUED;
            s_batch_total++;
            changed = true;
        }
        else if (e->state == UIPOD_DL_STATE_DONE && e->kind == UIPOD_DL_NAV_AUDIO)
        {
            tp_navidrome_pin_track(e->local_path);
        }
    }
    else if (s_count < DL_MAX_ITEMS && ensure_cap_locked(s_count + 1))
    {
        if (was_idle)
        {
            s_batch_done  = 0;
            s_batch_total = 0;
        }
        struct dl_entry *dst = &s_items[s_count++];
        *dst       = *proto;
        dst->uid   = s_next_uid++;
        dst->state = UIPOD_DL_STATE_QUEUED;
        s_batch_total++;
        changed = true;
    }

    if (changed)
    {
        save_queue_locked();
        if (!uipod_offline_mode_get())
            pthread_cond_broadcast(&s_dl_cv);
    }

    pthread_mutex_unlock(&s_dl_mtx);

    if (changed)
        uipod_gpu_render_header();
    return changed;
}

bool uipod_downloads_enqueue_nav_track(const char *local_path,
                                       const char *song_id,
                                       const char *cover_id,
                                       const char *title)
{
    if (!local_path || !local_path[0] || !song_id || !song_id[0])
        return false;

    struct dl_entry e;
    memset(&e, 0, sizeof(e));
    e.kind = UIPOD_DL_NAV_AUDIO;
    const char *fallback_title = strrchr(local_path, '/');
    fallback_title = fallback_title ? fallback_title + 1 : local_path;
    sanitize_tsv_field((title && title[0]) ? title : fallback_title,
                       e.title, sizeof(e.title));
    sanitize_tsv_field(local_path, e.local_path, sizeof(e.local_path));
    sanitize_tsv_field(song_id,    e.id,         sizeof(e.id));
    sanitize_tsv_field(cover_id,   e.cover_id,   sizeof(e.cover_id));
    return enqueue_item(&e);
}

bool uipod_downloads_enqueue_jf_audio(const char *local_path,
                                      const char *song_id,
                                      const char *suffix,
                                      const char *cover_id,
                                      const char *title)
{
    if (!local_path || !local_path[0] || !song_id || !song_id[0])
        return false;

    struct dl_entry e;
    memset(&e, 0, sizeof(e));
    e.kind = UIPOD_DL_JF_AUDIO;
    const char *fallback_title = strrchr(local_path, '/');
    fallback_title = fallback_title ? fallback_title + 1 : local_path;
    sanitize_tsv_field((title && title[0]) ? title : fallback_title,
                       e.title, sizeof(e.title));
    sanitize_tsv_field(local_path, e.local_path, sizeof(e.local_path));
    sanitize_tsv_field(song_id,    e.id,         sizeof(e.id));
    sanitize_tsv_field(suffix,     e.suffix,     sizeof(e.suffix));
    sanitize_tsv_field(cover_id,   e.cover_id,   sizeof(e.cover_id));
    return enqueue_item(&e);
}

bool uipod_downloads_enqueue_jf_video(const struct uipod_dl_jf_video_req *req)
{
    if (!req || !req->local_path || !req->local_path[0] ||
        !req->item_id || !req->item_id[0])
    {
        return false;
    }

    struct dl_entry e;
    memset(&e, 0, sizeof(e));
    e.kind = UIPOD_DL_JF_VIDEO;
    const char *fallback_title = strrchr(req->local_path, '/');
    fallback_title = fallback_title ? fallback_title + 1 : req->local_path;
    sanitize_tsv_field((req->title && req->title[0]) ? req->title : fallback_title,
                       e.title, sizeof(e.title));
    sanitize_tsv_field(req->local_path,      e.local_path, sizeof(e.local_path));
    sanitize_tsv_field(req->item_id,         e.id,         sizeof(e.id));
    sanitize_tsv_field(req->media_source_id, e.sub_id,     sizeof(e.sub_id));
    sanitize_tsv_field(req->container,       e.suffix,     sizeof(e.suffix));
    sanitize_tsv_field(req->video_codec,     e.vcodec,     sizeof(e.vcodec));
    sanitize_tsv_field(req->audio_codec,     e.acodec,     sizeof(e.acodec));
    e.width     = req->width;
    e.height    = req->height;
    e.bit_depth = req->bit_depth;
    e.bitrate   = req->bitrate;
    return enqueue_item(&e);
}

/* ---- Settings -> Downloads Screen ------------------------------------- */

struct dl_view_row
{
    uint32_t uid;
    uint8_t  kind;
    uint8_t  state;
    char     title[192];
    char     local_path[MAX_PATH];
    char     indic[48];
};

struct downloads_page
{
    struct uipod_page   base;
    struct gui_synclist lists;
    struct dl_view_row *rows;
    int                 count;
    int                 cap;
};

static void dl_page_refresh_snapshot(struct downloads_page *p)
{
    bool offline = uipod_offline_mode_get();

    pthread_mutex_lock(&s_dl_mtx);
    int n = s_count;
    if (n > p->cap)
    {
        int ncap = p->cap ? p->cap * 2 : 64;
        while (ncap < n)
            ncap *= 2;
        struct dl_view_row *nv = realloc(p->rows, (size_t)ncap * sizeof(*nv));
        if (nv)
        {
            p->rows = nv;
            p->cap  = ncap;
        }
        else
        {
            n = p->cap;
        }
    }

    /* Show newest / active downloads in reverse-enqueue order or queue order?
     * Queue order (top-to-bottom) matches execution order so user sees progress advance down. */
    p->count = n;
    for (int i = 0; i < n; i++)
    {
        const struct dl_entry *src = &s_items[i];
        struct dl_view_row *dst = &p->rows[i];
        dst->uid   = src->uid;
        dst->kind  = src->kind;
        dst->state = src->state;
        strlcpy(dst->title,      src->title,      sizeof(dst->title));
        strlcpy(dst->local_path, src->local_path, sizeof(dst->local_path));

        const char *st_label = "";
        switch (src->state)
        {
            case UIPOD_DL_STATE_DOWNLOADING:
                st_label = str(LANG_UIPOD_DL_STATE_ACTIVE);
                break;
            case UIPOD_DL_STATE_DONE:
                st_label = str(LANG_UIPOD_DL_STATE_DONE);
                break;
            case UIPOD_DL_STATE_FAILED:
                st_label = str(LANG_UIPOD_DL_STATE_FAILED);
                break;
            case UIPOD_DL_STATE_QUEUED:
            default:
                st_label = offline ? str(LANG_UIPOD_DL_STATE_PAUSED)
                                   : str(LANG_UIPOD_DL_STATE_QUEUED);
                break;
        }
        snprintf(dst->indic, sizeof(dst->indic), "%s%s",
                 UIPOD_INDIC_PLAIN, st_label);
    }
    pthread_mutex_unlock(&s_dl_mtx);

    int ui_rows = p->count > 0 ? p->count : 1;
    int prev_sel = gui_synclist_get_sel_pos(&p->lists);
    gui_synclist_set_nb_items(&p->lists, ui_rows);
    if (prev_sel >= ui_rows)
        gui_synclist_select_item(&p->lists, ui_rows - 1);
}

static const char *dl_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    struct downloads_page *p = (struct downloads_page *)data;
    (void)buf; (void)buf_len;
    if (p->count == 0)
        return str(LANG_UIPOD_DL_EMPTY);
    if (sel < 0 || sel >= p->count)
        return "";
    return p->rows[sel].title;
}

static const char *dl_get_indicator(int sel, void *data)
{
    struct downloads_page *p = (struct downloads_page *)data;
    if (p->count == 0 || sel < 0 || sel >= p->count)
        return NULL;
    return p->rows[sel].indic;
}

static const struct uipod_svg_icon *dl_get_svg_icon(int sel, void *data)
{
    struct downloads_page *p = (struct downloads_page *)data;
    if (p->count == 0 || sel < 0 || sel >= p->count)
        return NULL;
    const struct dl_view_row *r = &p->rows[sel];
    if (r->kind == UIPOD_DL_JF_VIDEO)
        return uipod_svg_tag("mp4");
    if (r->local_path[0])
        return uipod_svg_for_path(r->local_path, false);
    return uipod_svg_tag("mp3");
}

static void dl_draw(struct uipod_page *self)
{
    struct downloads_page *p = (struct downloads_page *)self;
    dl_page_refresh_snapshot(p);
    gui_synclist_draw(&p->lists);
}

static int dl_poll(struct uipod_page *self, int timeout)
{
    struct downloads_page *p = (struct downloads_page *)self;
    int action;
    list_do_action(self->context, timeout, &p->lists, &action);
    return action;
}

static void dl_retry_one(uint32_t uid)
{
    pthread_mutex_lock(&s_dl_mtx);
    int idx = find_index_by_uid_locked(uid);
    if (idx >= 0 && s_items[idx].state == UIPOD_DL_STATE_FAILED)
    {
        if (count_queued_locked() == 0 && !s_worker_busy)
        {
            s_batch_done  = 0;
            s_batch_total = 0;
        }
        s_items[idx].state = UIPOD_DL_STATE_QUEUED;
        s_batch_total++;
        save_queue_locked();
        if (!uipod_offline_mode_get())
            pthread_cond_broadcast(&s_dl_cv);
    }
    pthread_mutex_unlock(&s_dl_mtx);
}

static void dl_remove_one(uint32_t uid)
{
    pthread_mutex_lock(&s_dl_mtx);
    int idx = find_index_by_uid_locked(uid);
    if (idx >= 0)
    {
        uint8_t st = s_items[idx].state;
        if (st == UIPOD_DL_STATE_DOWNLOADING || s_active_uid == uid)
            __atomic_store_n(&s_cancel_active, 1, __ATOMIC_RELEASE);
        if ((st == UIPOD_DL_STATE_QUEUED || st == UIPOD_DL_STATE_DOWNLOADING) &&
            s_batch_total > s_batch_done)
        {
            s_batch_total--;
        }
        memmove(&s_items[idx], &s_items[idx + 1],
                (size_t)(s_count - 1 - idx) * sizeof(s_items[0]));
        s_count--;
        if (count_queued_locked() == 0 && !s_worker_busy)
        {
            s_batch_done  = 0;
            s_batch_total = 0;
        }
        save_queue_locked();
    }
    pthread_mutex_unlock(&s_dl_mtx);
}

static void dl_retry_all_failed(void)
{
    pthread_mutex_lock(&s_dl_mtx);
    bool was_idle = (count_queued_locked() == 0 && !s_worker_busy);
    int retried = 0;
    for (int i = 0; i < s_count; i++)
    {
        if (s_items[i].state == UIPOD_DL_STATE_FAILED)
        {
            if (was_idle && retried == 0)
            {
                s_batch_done  = 0;
                s_batch_total = 0;
            }
            s_items[i].state = UIPOD_DL_STATE_QUEUED;
            s_batch_total++;
            retried++;
        }
    }
    if (retried > 0)
    {
        save_queue_locked();
        if (!uipod_offline_mode_get())
            pthread_cond_broadcast(&s_dl_cv);
    }
    pthread_mutex_unlock(&s_dl_mtx);
}

static void dl_clear_completed(void)
{
    pthread_mutex_lock(&s_dl_mtx);
    int w = 0;
    for (int i = 0; i < s_count; i++)
    {
        if (s_items[i].state != UIPOD_DL_STATE_DONE)
        {
            if (w != i)
                s_items[w] = s_items[i];
            w++;
        }
    }
    if (w != s_count)
    {
        s_count = w;
        save_queue_locked();
    }
    pthread_mutex_unlock(&s_dl_mtx);
}

static void dl_clear_all(void)
{
    pthread_mutex_lock(&s_dl_mtx);
    __atomic_store_n(&s_cancel_active, 1, __ATOMIC_RELEASE);
    s_count       = 0;
    s_batch_done  = 0;
    s_batch_total = 0;
    save_queue_locked();
    pthread_mutex_unlock(&s_dl_mtx);
}

static enum uipod_page_result dl_on_action(struct uipod_page *self, int action)
{
    struct downloads_page *p = (struct downloads_page *)self;
    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;

    if ((action == ACTION_STD_OK || action == ACTION_STD_CONTEXT) && p->count > 0)
    {
        int sel = gui_synclist_get_sel_pos(&p->lists);
        if (sel < 0 || sel >= p->count)
            return UIPOD_PAGE_STAY;

        struct dl_view_row row = p->rows[sel];
        bool any_failed = false, any_done = false;
        for (int i = 0; i < p->count; i++)
        {
            if (p->rows[i].state == UIPOD_DL_STATE_FAILED) any_failed = true;
            if (p->rows[i].state == UIPOD_DL_STATE_DONE)   any_done   = true;
        }

        const char *opts[5];
        int opt_codes[5];
        int n = 0;

        if (row.state == UIPOD_DL_STATE_FAILED)
        {
            opts[n] = ID2P(LANG_UIPOD_DL_RETRY);
            opt_codes[n++] = 1;
        }
        opts[n] = (row.state == UIPOD_DL_STATE_QUEUED ||
                   row.state == UIPOD_DL_STATE_DOWNLOADING)
                      ? ID2P(LANG_CANCEL)
                      : ID2P(LANG_REMOVE);
        opt_codes[n++] = 2;

        if (any_failed)
        {
            opts[n] = ID2P(LANG_UIPOD_DL_RETRY_FAILED);
            opt_codes[n++] = 3;
        }
        if (any_done)
        {
            opts[n] = ID2P(LANG_UIPOD_DL_CLEAR_DONE);
            opt_codes[n++] = 4;
        }
        opts[n] = ID2P(LANG_UIPOD_DL_CLEAR_ALL);
        opt_codes[n++] = 5;

        int pick = uipod_context_menu(row.title, opts, n);
        if (pick >= 0 && pick < n)
        {
            switch (opt_codes[pick])
            {
                case 1: dl_retry_one(row.uid);   break;
                case 2: dl_remove_one(row.uid);  break;
                case 3: dl_retry_all_failed();   break;
                case 4: dl_clear_completed();    break;
                case 5: dl_clear_all();          break;
            }
            dl_page_refresh_snapshot(p);
            uipod_gpu_render_header();
        }
    }

    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable dl_vtable = {
    .legend    = NULL,
    .draw      = dl_draw,
    .poll      = dl_poll,
    .on_action = dl_on_action,
};

static const int dl_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CONTEXT, ACTION_STD_CANCEL, -1
};

int uipod_downloads_page(void)
{
    struct downloads_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt       = &dl_vtable;
    p.base.context  = CONTEXT_LIST;
    p.base.allowed  = dl_allowed;
    p.base.animated = true;
    p.base.title    = str(LANG_UIPOD_DOWNLOADS);

    gui_synclist_init(&p.lists, dl_get_name, &p, false, 1, NULL);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_UIPOD_DOWNLOADS), Icon_NOICON);
    gui_synclist_set_indicator_callback(&p.lists, dl_get_indicator);
    gui_synclist_set_svg_icon_callback(&p.lists, dl_get_svg_icon);
    dl_page_refresh_snapshot(&p);
    gui_synclist_select_item(&p.lists, 0);

    uipod_page_run(&p.base);
    free(p.rows);
    return 0;
}
