/***************************************************************************
 * UIPod: Jellyfin integration bridge.
 ****************************************************************************/
#include "config.h"

#include <ctype.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>

#include "sqlite3.h"

#include "action.h"
#include "audio.h"
#include "backlight.h"
#include "dir.h"
#include "file.h"
#include "filetypes.h"
#include "gui/list.h"
#include "icon.h"
#include "kernel.h"
#include "lang.h"
#include "metadata.h"
#include "misc.h"
#include "pathfuncs.h"
#include "playlist.h"
#include "rbpaths.h"
#include "root_menu.h"
#include "settings.h"
#include "splash.h"
#include "string-extra.h"
#include "strnatcmp.h"
#include "system.h"

#include "trimpod_folders.h"
#include "trimpod_jellyfin.h"
#include "trimpod_keyboard.h"
#include "trimpod_library.h"
#include "trimpod_navidrome.h"
#include "trimpod_page.h"
#include "trimpod_playlists.h"
#include "trimpod_rs_jellyfin.h"
#include "trimpod_svg.h"
#include "trimpod_transition.h"
#include "trimpod_ui.h"
#include "trimpod_video.h"

#ifdef RETRO_HANDHELD
#include "navidrome-http-target.h"
#include "video-target.h"
#endif

#define JF_CFG_RB         ROCKBOX_DIR "/jellyfin.cfg"
#define JF_CFG_SD_1       "/mnt/SDCARD/jellyfin.cfg"
#define JF_CFG_SD_2       "/tmp/trimpod/jellyfin.cfg"

#define JF_DB_SD          ROCKBOX_DIR "/jellyfin.db"
#define JF_DB_RAM         "/tmp/uipod-jellyfin.db"
#define JF_PLAYLISTS_DIR  ROCKBOX_DIR "/Playlists_Jellyfin"
#define JF_COPY_BUF       (256 * 1024)
#define JF_SCHEMA_VERSION 1

static bool         s_jf_db_in_ram;
static sqlite3     *s_jf_db;
static struct mutex s_jf_db_mutex;
static uint32_t     s_session_seq;

/* ---- Config file persistence ------------------------------------------ */

static bool jf_read_text_file(const char *rb_path, char *buf, size_t cap)
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

static bool jf_save_config_to_disk(void)
{
    char ini[3072];
    size_t n = tp_jellyfin_format_config_text(ini, sizeof(ini));
    if (n == 0)
        return false;
    int fd = open(JF_CFG_RB ".new", O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0)
        return false;
    bool ok = (write(fd, ini, n) == (ssize_t)n);
    close(fd);
    if (ok)
        rename(JF_CFG_RB ".new", JF_CFG_RB);
    return ok;
}

static void jf_load_or_import_config(void)
{
    char ini[4096];
    if (jf_read_text_file(JF_CFG_RB, ini, sizeof(ini)))
    {
        tp_jellyfin_load_config_text(ini);
        if (tp_jellyfin_is_configured())
            return;
    }

    if (jf_read_text_file(JF_CFG_SD_1, ini, sizeof(ini)) ||
        jf_read_text_file(JF_CFG_SD_2, ini, sizeof(ini)))
    {
        if (tp_jellyfin_load_config_text(ini) && tp_jellyfin_is_configured())
            jf_save_config_to_disk();
    }
}

/* ---- SQLite DB (`/tmp/uipod-jellyfin.db` <-> `jellyfin.db`) ----------- */

static const char JF_SCHEMA_SQL[] =
    "CREATE TABLE IF NOT EXISTS tracks("
    "  id INTEGER PRIMARY KEY,"
    "  song_id TEXT NOT NULL UNIQUE,"
    "  path TEXT NOT NULL UNIQUE,"
    "  cover_id TEXT,"
    "  server_path TEXT,"
    "  suffix TEXT,"
    "  bit_depth INTEGER NOT NULL DEFAULT 0,"
    "  category INTEGER NOT NULL DEFAULT 0,"
    "  size INTEGER NOT NULL DEFAULT 0,"
    "  title TEXT,"
    "  artist TEXT,"
    "  albumartist TEXT,"
    "  album TEXT,"
    "  genre TEXT,"
    "  year INTEGER,"
    "  tracknum INTEGER,"
    "  discnum INTEGER,"
    "  duration_ms INTEGER,"
    "  bitrate INTEGER,"
    "  frequency INTEGER,"
    "  browse_artist TEXT,"
    "  sort_artist TEXT);"
    "CREATE INDEX IF NOT EXISTS idx_jf_artist ON tracks(category, sort_artist, album, discnum, tracknum);"
    "CREATE INDEX IF NOT EXISTS idx_jf_album  ON tracks(category, album, discnum, tracknum);"
    "CREATE INDEX IF NOT EXISTS idx_jf_browse ON tracks(category, browse_artist, album, discnum, tracknum);"
    "CREATE INDEX IF NOT EXISTS idx_jf_title  ON tracks(category, title, path);"
    "CREATE INDEX IF NOT EXISTS idx_jf_genre  ON tracks(category, genre);"
    "CREATE TABLE IF NOT EXISTS jf_views("
    "  ord INTEGER PRIMARY KEY AUTOINCREMENT,"
    "  id TEXT NOT NULL UNIQUE,"
    "  name TEXT NOT NULL,"
    "  collection_type TEXT,"
    "  view_kind INTEGER NOT NULL DEFAULT 0);"
    "CREATE TABLE IF NOT EXISTS jf_videos("
    "  row_id INTEGER PRIMARY KEY AUTOINCREMENT,"
    "  id TEXT NOT NULL,"
    "  view_id TEXT NOT NULL,"
    "  item_type INTEGER NOT NULL,"
    "  played INTEGER NOT NULL DEFAULT 0,"
    "  name TEXT NOT NULL,"
    "  series_id TEXT,"
    "  series_name TEXT,"
    "  season_id TEXT,"
    "  season_name TEXT,"
    "  season_number INTEGER NOT NULL DEFAULT 0,"
    "  episode_number INTEGER NOT NULL DEFAULT 0,"
    "  year INTEGER NOT NULL DEFAULT 0,"
    "  duration_ms INTEGER NOT NULL DEFAULT 0,"
    "  resume_ms INTEGER NOT NULL DEFAULT 0,"
    "  media_source_id TEXT,"
    "  container TEXT,"
    "  video_codec TEXT,"
    "  audio_codec TEXT,"
    "  width INTEGER NOT NULL DEFAULT 0,"
    "  height INTEGER NOT NULL DEFAULT 0,"
    "  ratio_num INTEGER NOT NULL DEFAULT 0,"
    "  ratio_den INTEGER NOT NULL DEFAULT 0,"
    "  bit_depth INTEGER NOT NULL DEFAULT 0,"
    "  bitrate INTEGER NOT NULL DEFAULT 0,"
    "  chapters_csv TEXT,"
    "  local_path TEXT,"
    "  UNIQUE(view_id, id));"
    "CREATE INDEX IF NOT EXISTS idx_jfv_view   ON jf_videos(view_id, item_type, name);"
    "CREATE INDEX IF NOT EXISTS idx_jfv_type   ON jf_videos(item_type, name);"
    "CREATE INDEX IF NOT EXISTS idx_jfv_series ON jf_videos(series_id, item_type, season_number, episode_number);"
    "CREATE INDEX IF NOT EXISTS idx_jfv_season ON jf_videos(season_id, item_type, episode_number);"
    "CREATE INDEX IF NOT EXISTS idx_jfv_id     ON jf_videos(id);";

static bool jf_copy_file(const char *src, const char *dst)
{
    bool ok = false;
    int in = open(src, O_RDONLY);
    if (in < 0)
        return false;
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    char *buf = malloc(JF_COPY_BUF);
    if (out >= 0 && buf)
    {
        ssize_t n;
        while ((n = read(in, buf, JF_COPY_BUF)) > 0)
            if (write(out, buf, n) != n)
                break;
        ok = (n == 0);
    }
    free(buf);
    if (out >= 0)
        close(out);
    close(in);
    return ok;
}

static void jf_db_writeback_locked(void)
{
    if (s_jf_db_in_ram && jf_copy_file(JF_DB_RAM, JF_DB_SD ".new"))
        rename(JF_DB_SD ".new", JF_DB_SD);
}

static int jf_db_exec(const char *sql)
{
    return sqlite3_exec(s_jf_db, sql, NULL, NULL, NULL);
}

static void jf_make_sort_artist(const char *ba, char *out, size_t n)
{
    if (!ba)
        ba = "";
    if (strncasecmp(ba, "the ", 4) == 0)
        ba += 4;
    size_t i = 0;
    for (; ba[i] && i + 1 < n; i++)
        out[i] = (char)tolower((unsigned char)ba[i]);
    out[i] = '\0';
}

static void jf_bind_opt_str(sqlite3_stmt *st, int idx, const char *s)
{
    if (s && s[0])
        sqlite3_bind_text(st, idx, s, -1, SQLITE_TRANSIENT);
    else
        sqlite3_bind_null(st, idx);
}

static bool jf_db_setup_locked(void)
{
    jf_db_exec("PRAGMA journal_mode=TRUNCATE;");
    jf_db_exec("PRAGMA synchronous=NORMAL;");

    int ver = 0;
    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(s_jf_db, "PRAGMA user_version;", -1, &st, NULL) == SQLITE_OK)
    {
        if (sqlite3_step(st) == SQLITE_ROW)
            ver = sqlite3_column_int(st, 0);
        sqlite3_finalize(st);
    }

    if (ver != JF_SCHEMA_VERSION)
    {
        jf_db_exec("DROP TABLE IF EXISTS tracks;");
        jf_db_exec("DROP TABLE IF EXISTS jf_views;");
        jf_db_exec("DROP TABLE IF EXISTS jf_videos;");
        if (jf_db_exec(JF_SCHEMA_SQL) != SQLITE_OK)
            return false;
        char sql[48];
        snprintf(sql, sizeof(sql), "PRAGMA user_version=%d;", JF_SCHEMA_VERSION);
        jf_db_exec(sql);
    }
    else
    {
        if (jf_db_exec(JF_SCHEMA_SQL) != SQLITE_OK)
            return false;
    }
    return true;
}

struct sqlite3 *trimpod_jellyfin_db_lock(void)
{
    mutex_lock(&s_jf_db_mutex);
    return s_jf_db;
}

void trimpod_jellyfin_db_unlock(void)
{
    mutex_unlock(&s_jf_db_mutex);
}

bool trimpod_jellyfin_has_tracks(void)
{
    bool has = false;
    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(s_jf_db, "SELECT 1 FROM tracks LIMIT 1;", -1, &st, NULL) == SQLITE_OK)
        {
            has = (sqlite3_step(st) == SQLITE_ROW);
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_jf_db_mutex);
    return has;
}

bool trimpod_jellyfin_has_media(void)
{
    bool has = false;
    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(
                s_jf_db,
                "SELECT 1 FROM jf_views LIMIT 1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            has = (sqlite3_step(st) == SQLITE_ROW);
            sqlite3_finalize(st);
        }
        if (!has && sqlite3_prepare_v2(
                s_jf_db,
                "SELECT 1 FROM tracks LIMIT 1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            has = (sqlite3_step(st) == SQLITE_ROW);
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_jf_db_mutex);
    return has;
}

/* ---- HTTP Vtable Wiring ----------------------------------------------- */

#ifdef RETRO_HANDHELD
static int32_t jf_http_request_adapter(
    uint8_t is_https, const char *host, uint16_t port,
    const char *method, const char *pq, const char *hdrs,
    const uint8_t *body, size_t body_len, uint8_t self_signed,
    int32_t timeout_ms, int32_t *out_status,
    uint8_t **out_data, size_t *out_len, char *err, size_t cap)
{
    return retrohh_http_request(method, is_https, host, port, pq, hdrs,
                                body, body_len, self_signed, timeout_ms,
                                out_status, out_data, out_len, err, cap);
}
#else
static int32_t stub_jf_req(uint8_t is_https, const char *host, uint16_t port,
                           const char *method, const char *pq, const char *hdrs,
                           const uint8_t *body, size_t body_len, uint8_t self_signed,
                           int32_t timeout_ms, int32_t *out_status,
                           uint8_t **out_data, size_t *out_len, char *err, size_t cap)
{
    (void)is_https; (void)host; (void)port; (void)method; (void)pq; (void)hdrs;
    (void)body; (void)body_len; (void)self_signed; (void)timeout_ms;
    (void)out_status; (void)out_data; (void)out_len;
    snprintf(err, cap, "HTTP unsupported on this target");
    return -1;
}
static void stub_jf_free(uint8_t *p) { free(p); }
static int32_t stub_jf_dl(uint8_t is_https, const char *host, uint16_t port,
                          const char *pq, const char *hdrs, uint8_t self_signed,
                          const char *dest, int32_t timeout_ms,
                          tp_jf_cancel_fn ccb, void *cctx, char *err, size_t cap)
{
    (void)is_https; (void)host; (void)port; (void)pq; (void)hdrs;
    (void)self_signed; (void)dest; (void)timeout_ms; (void)ccb; (void)cctx;
    snprintf(err, cap, "HTTP unsupported on this target");
    return -1;
}
#endif

static const TpJfHttpVtable s_jf_http_vt = {
#ifdef RETRO_HANDHELD
    .http_request     = jf_http_request_adapter,
    .http_free        = retrohh_nav_http_free,
    .http_download_ex = retrohh_http_download_ex,
#else
    .http_request     = stub_jf_req,
    .http_free        = stub_jf_free,
    .http_download_ex = stub_jf_dl,
#endif
};

void trimpod_jellyfin_init(void)
{
    mutex_init(&s_jf_db_mutex);

    const char *video_root = "/tmp/uipod_jellyfin_videos";
    const char *audio_root = "/tmp/uipod_jellyfin_music";
    struct stat st;
    if (stat("/mnt/SDCARD", &st) == 0 && S_ISDIR(st.st_mode))
    {
        video_root = "/mnt/SDCARD/Media/Videos/jellyfin";
        audio_root = "/mnt/SDCARD/Media/Music/jellyfin";
    }

    tp_jellyfin_init(video_root, audio_root, &s_jf_http_vt);
    jf_load_or_import_config();

    if (!dir_exists(JF_PLAYLISTS_DIR))
        mkdir(JF_PLAYLISTS_DIR);

    mutex_lock(&s_jf_db_mutex);
    remove(JF_DB_RAM);
    s_jf_db_in_ram = !file_exists(JF_DB_SD) || jf_copy_file(JF_DB_SD, JF_DB_RAM);
    const char *work = s_jf_db_in_ram ? JF_DB_RAM : JF_DB_SD;

    if (sqlite3_open(work, &s_jf_db) != SQLITE_OK || !jf_db_setup_locked())
    {
        sqlite3_close(s_jf_db);
        s_jf_db = NULL;
        remove(JF_DB_SD);
        remove(JF_DB_RAM);
        s_jf_db_in_ram = true;
        if (sqlite3_open(JF_DB_RAM, &s_jf_db) == SQLITE_OK && jf_db_setup_locked())
            jf_db_writeback_locked();
        else
        {
            sqlite3_close(s_jf_db);
            s_jf_db = NULL;
        }
    }
    mutex_unlock(&s_jf_db_mutex);
}

void trimpod_jellyfin_close(void)
{
    tp_jellyfin_shutdown();
    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_close(s_jf_db);
        s_jf_db = NULL;
    }
    remove(JF_DB_RAM);
    mutex_unlock(&s_jf_db_mutex);
}

/* ---- UI-thread cooperative cancel check ------------------------------- */

struct jf_cancel_state
{
    bool cancelled;
};

static uint8_t jf_cancel_check_cb(void *ctx)
{
    struct jf_cancel_state *st = (struct jf_cancel_state *)ctx;
    if (st->cancelled)
        return 1;
    yield();
    int act = get_action(CONTEXT_STD, TIMEOUT_NOBLOCK);
    if (act == ACTION_STD_CANCEL || act == ACTION_TP_HOME)
    {
        if (act == ACTION_TP_HOME)
            trimpod_home_pending = true;
        st->cancelled = true;
        return 1;
    }
    return 0;
}

/* ---- Catalog Sync (`tp_jellyfin_sync_catalog` -> SQLite + Playlists) -- */

struct jf_sync_ctx
{
    sqlite3_stmt          *view_stmt;
    sqlite3_stmt          *video_stmt;
    sqlite3_stmt          *song_stmt;
    struct jf_cancel_state cancel;
};

static void jf_sanitize_playlist_filename(const char *name, char *out, size_t cap)
{
    size_t j = 0;
    for (size_t i = 0; name && name[i] && j + 1 < cap; i++)
    {
        char c = name[i];
        if (c == '/' || c == '\\' || c == ':' || c == '*' ||
            c == '?' || c == '"'  || c == '<' || c == '>' || c == '|')
            c = '_';
        out[j++] = c;
    }
    while (j > 0 && (out[j - 1] == ' ' || out[j - 1] == '.'))
        j--;
    out[j] = '\0';
    if (j == 0)
        strlcpy(out, "Playlist", cap);
}

static void jf_clear_server_playlists_dir(void)
{
    DIR *d = opendir(JF_PLAYLISTS_DIR);
    if (!d)
        return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL)
    {
        const char *dot = strrchr(e->d_name, '.');
        if (!dot || (strcasecmp(dot, ".m3u8") != 0 && strcasecmp(dot, ".m3u") != 0))
            continue;
        char full[MAX_PATH];
        snprintf(full, sizeof(full), "%s/%s", JF_PLAYLISTS_DIR, e->d_name);
        remove(full);
    }
    closedir(d);
}

static void jf_sync_on_view(void *vctx, const TpJfViewC *v)
{
    struct jf_sync_ctx *ctx = (struct jf_sync_ctx *)vctx;
    if (!ctx->view_stmt || !v || !v->id || !v->name)
        return;
    sqlite3_stmt *st = ctx->view_stmt;
    sqlite3_bind_text(st, 1, v->id, -1, SQLITE_TRANSIENT);
    sqlite3_bind_text(st, 2, v->name, -1, SQLITE_TRANSIENT);
    jf_bind_opt_str  (st, 3, v->collection_type);
    sqlite3_bind_int (st, 4, (int)v->view_kind);
    sqlite3_step(st);
    sqlite3_reset(st);
}

static void jf_sync_on_video(void *vctx, const TpJfVideoItemC *it)
{
    struct jf_sync_ctx *ctx = (struct jf_sync_ctx *)vctx;
    if (!ctx->video_stmt || !it || !it->id || !it->view_id || !it->name)
        return;
    sqlite3_stmt *st = ctx->video_stmt;
    sqlite3_bind_text (st,  1, it->id, -1, SQLITE_TRANSIENT);
    sqlite3_bind_text (st,  2, it->view_id, -1, SQLITE_TRANSIENT);
    sqlite3_bind_int  (st,  3, (int)it->item_type);
    sqlite3_bind_int  (st,  4, (int)it->played);
    sqlite3_bind_text (st,  5, it->name, -1, SQLITE_TRANSIENT);
    jf_bind_opt_str   (st,  6, it->series_id);
    jf_bind_opt_str   (st,  7, it->series_name);
    jf_bind_opt_str   (st,  8, it->season_id);
    jf_bind_opt_str   (st,  9, it->season_name);
    sqlite3_bind_int  (st, 10, (int)it->season_number);
    sqlite3_bind_int  (st, 11, (int)it->episode_number);
    sqlite3_bind_int  (st, 12, (int)it->year);
    sqlite3_bind_int64(st, 13, (sqlite3_int64)it->duration_ms);
    sqlite3_bind_int64(st, 14, (sqlite3_int64)it->resume_ms);
    jf_bind_opt_str   (st, 15, it->media_source_id);
    jf_bind_opt_str   (st, 16, it->container);
    jf_bind_opt_str   (st, 17, it->video_codec);
    jf_bind_opt_str   (st, 18, it->audio_codec);
    sqlite3_bind_int  (st, 19, (int)it->width);
    sqlite3_bind_int  (st, 20, (int)it->height);
    sqlite3_bind_int  (st, 21, (int)it->ratio_num);
    sqlite3_bind_int  (st, 22, (int)it->ratio_den);
    sqlite3_bind_int  (st, 23, (int)it->bit_depth);
    sqlite3_bind_int64(st, 24, (sqlite3_int64)it->bitrate);
    jf_bind_opt_str   (st, 25, it->chapters_csv);
    jf_bind_opt_str   (st, 26, it->local_path);
    sqlite3_step(st);
    sqlite3_reset(st);
}

static void jf_sync_on_song(void *vctx, const TpJfAudioSongC *s)
{
    struct jf_sync_ctx *ctx = (struct jf_sync_ctx *)vctx;
    if (!ctx->song_stmt || !s || !s->id || !s->local_track_path)
        return;

    const char *ba = (s->album_artist && s->album_artist[0]) ? s->album_artist
                   : (s->artist ? s->artist : "");
    char sortbuf[128];
    jf_make_sort_artist(ba, sortbuf, sizeof(sortbuf));

    sqlite3_stmt *st = ctx->song_stmt;
    sqlite3_bind_text (st,  1, s->id, -1, SQLITE_TRANSIENT);
    sqlite3_bind_text (st,  2, s->local_track_path, -1, SQLITE_TRANSIENT);
    jf_bind_opt_str   (st,  3, s->cover_id);
    jf_bind_opt_str   (st,  4, s->server_path);
    jf_bind_opt_str   (st,  5, s->suffix);
    sqlite3_bind_int  (st,  6, (int)s->bit_depth);
    sqlite3_bind_int  (st,  7, TP_CAT_MUSIC);
    sqlite3_bind_int64(st,  8, (sqlite3_int64)s->size);
    jf_bind_opt_str   (st,  9, s->title);
    jf_bind_opt_str   (st, 10, s->artist);
    jf_bind_opt_str   (st, 11, s->album_artist);
    jf_bind_opt_str   (st, 12, s->album);
    jf_bind_opt_str   (st, 13, s->genre);
    sqlite3_bind_int  (st, 14, (int)s->year);
    sqlite3_bind_int  (st, 15, (int)s->track);
    sqlite3_bind_int  (st, 16, (int)s->disc_number);
    sqlite3_bind_int64(st, 17, (sqlite3_int64)s->duration_secs * 1000LL);
    sqlite3_bind_int  (st, 18, (int)s->bit_rate);
    sqlite3_bind_int64(st, 19, (sqlite3_int64)s->sampling_rate);
    sqlite3_bind_text (st, 20, ba, -1, SQLITE_TRANSIENT);
    sqlite3_bind_text (st, 21, sortbuf, -1, SQLITE_TRANSIENT);
    sqlite3_step(st);
    sqlite3_reset(st);
}

static void jf_sync_on_playlist(void *vctx, const char *playlist_name,
                                const char *m3u8_content)
{
    (void)vctx;
    if (!playlist_name || !playlist_name[0] || !m3u8_content)
        return;
    if (!dir_exists(JF_PLAYLISTS_DIR))
        mkdir(JF_PLAYLISTS_DIR);

    char safe_name[128];
    jf_sanitize_playlist_filename(playlist_name, safe_name, sizeof(safe_name));

    char path[MAX_PATH];
    snprintf(path, sizeof(path), "%s/%s.m3u8", JF_PLAYLISTS_DIR, safe_name);

    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd >= 0)
    {
        size_t len = strlen(m3u8_content);
        if (len > 0)
            write(fd, m3u8_content, len);
        close(fd);
    }
}

static void jf_sync_on_progress(void *vctx, uint32_t vids, uint32_t trks)
{
    (void)vctx;
    char widest[80], msg[80];
    snprintf(widest, sizeof(widest), "%s 9999 / 99999", str(LANG_TRIMPOD_NAV_SYNCING));
    if (trks > 0)
        snprintf(msg, sizeof(msg), "%s %u + %u", str(LANG_TRIMPOD_NAV_SYNCING),
                 (unsigned)vids, (unsigned)trks);
    else
        snprintf(msg, sizeof(msg), "%s %u", str(LANG_TRIMPOD_NAV_SYNCING),
                 (unsigned)vids);
    trimpod_fullscreen_message(msg, widest);
    backlight_on();
    yield();
}

static uint8_t jf_sync_cancel_bridge(void *vctx)
{
    struct jf_sync_ctx *ctx = (struct jf_sync_ctx *)vctx;
    return jf_cancel_check_cb(&ctx->cancel);
}

bool trimpod_jellyfin_sync(bool show_summary_splash)
{
    if (!tp_jellyfin_is_configured())
    {
        splash(HZ * 3 / 2, ID2P(LANG_TRIMPOD_NAV_CONFIG_NEEDED));
        return false;
    }

    trimpod_fullscreen_message(str(LANG_TRIMPOD_NAV_CONNECTING), NULL);
    button_clear_queue();

    mutex_lock(&s_jf_db_mutex);
    if (!s_jf_db)
    {
        mutex_unlock(&s_jf_db_mutex);
        splash(HZ, ID2P(LANG_FAILED));
        return false;
    }

    jf_db_exec("BEGIN IMMEDIATE;");
    jf_db_exec("DELETE FROM jf_views;");
    jf_db_exec("DELETE FROM jf_videos;");
    jf_db_exec("DELETE FROM tracks;");

    struct jf_sync_ctx ctx;
    memset(&ctx, 0, sizeof(ctx));

    sqlite3_prepare_v2(
        s_jf_db,
        "INSERT OR REPLACE INTO jf_views(id, name, collection_type, view_kind)"
        " VALUES(?1, ?2, ?3, ?4);",
        -1, &ctx.view_stmt, NULL);

    sqlite3_prepare_v2(
        s_jf_db,
        "INSERT OR REPLACE INTO jf_videos("
        "  id, view_id, item_type, played, name, series_id, series_name,"
        "  season_id, season_name, season_number, episode_number, year,"
        "  duration_ms, resume_ms, media_source_id, container, video_codec,"
        "  audio_codec, width, height, ratio_num, ratio_den, bit_depth,"
        "  bitrate, chapters_csv, local_path)"
        " VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,"
        "        ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23,"
        "        ?24, ?25, ?26);",
        -1, &ctx.video_stmt, NULL);

    sqlite3_prepare_v2(
        s_jf_db,
        "INSERT OR REPLACE INTO tracks("
        "  song_id, path, cover_id, server_path, suffix, bit_depth, category,"
        "  size, title, artist, albumartist, album, genre, year, tracknum,"
        "  discnum, duration_ms, bitrate, frequency, browse_artist, sort_artist)"
        " VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,"
        "        ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21);",
        -1, &ctx.song_stmt, NULL);

    jf_clear_server_playlists_dir();

    uint32_t out_videos = 0;
    uint32_t out_tracks = 0;
    char err_msg[128];
    err_msg[0] = '\0';

    int32_t rc = tp_jellyfin_sync_catalog(
        jf_sync_on_view,
        jf_sync_on_video,
        jf_sync_on_song,
        jf_sync_on_playlist,
        jf_sync_on_progress,
        &ctx,
        jf_sync_cancel_bridge,
        &ctx,
        &out_videos,
        &out_tracks,
        err_msg,
        sizeof(err_msg));

    if (ctx.view_stmt)  sqlite3_finalize(ctx.view_stmt);
    if (ctx.video_stmt) sqlite3_finalize(ctx.video_stmt);
    if (ctx.song_stmt)  sqlite3_finalize(ctx.song_stmt);

    if (rc < 0)
    {
        jf_db_exec("ROLLBACK;");
        mutex_unlock(&s_jf_db_mutex);
        if (!ctx.cancel.cancelled)
            splashf(HZ * 3 / 2, "%s", err_msg[0] ? err_msg : (const char *)str(LANG_FAILED));
        return false;
    }

    jf_db_exec("COMMIT;");
    jf_db_writeback_locked();
    mutex_unlock(&s_jf_db_mutex);

    /* Persist freshly acquired `access_token` + `user_id` to `jellyfin.cfg` */
    jf_save_config_to_disk();

    if (show_summary_splash)
        splashf(HZ * 3 / 2, str(LANG_TRIMPOD_JF_SYNCED_FMT),
                (int)out_videos, (int)out_tracks);

    return true;
}

/* ---- Audio Track Lookup, Buffering, Download & ID3 -------------------- */

static bool jf_lookup_song_by_path(const char *path,
                                   char *out_song_id, size_t song_cap,
                                   char *out_suffix, size_t suf_cap,
                                   char *out_cover_id, size_t cov_cap,
                                   int *out_bit_depth)
{
    if (out_song_id && song_cap > 0) out_song_id[0] = '\0';
    if (out_suffix && suf_cap > 0)   out_suffix[0] = '\0';
    if (out_cover_id && cov_cap > 0) out_cover_id[0] = '\0';
    if (out_bit_depth)               *out_bit_depth = 0;
    if (!path || !path[0])
        return false;

    bool found = false;
    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(
                s_jf_db,
                "SELECT song_id, COALESCE(suffix, ''), COALESCE(cover_id, ''), bit_depth"
                " FROM tracks WHERE path=?1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
            if (sqlite3_step(st) == SQLITE_ROW)
            {
                const char *sid = (const char *)sqlite3_column_text(st, 0);
                const char *suf = (const char *)sqlite3_column_text(st, 1);
                const char *cid = (const char *)sqlite3_column_text(st, 2);
                if (sid && sid[0])
                {
                    if (out_song_id && song_cap > 0)
                        strlcpy(out_song_id, sid, song_cap);
                    if (out_suffix && suf_cap > 0 && suf)
                        strlcpy(out_suffix, suf, suf_cap);
                    if (out_cover_id && cov_cap > 0 && cid)
                        strlcpy(out_cover_id, cid, cov_cap);
                    if (out_bit_depth)
                        *out_bit_depth = sqlite3_column_int(st, 3);
                    found = true;
                }
            }
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_jf_db_mutex);
    return found;
}

bool trimpod_jellyfin_is_track_path(const char *path)
{
    return path && path[0] && tp_jellyfin_is_audio_track_path(path);
}

bool trimpod_jellyfin_prepare_track(const char *path, bool ui_blocking)
{
    if (!trimpod_jellyfin_is_track_path(path))
        return true;

    if (file_exists(path))
        return true;

    if (!trimpod_source_is_jellyfin())
        return false;

    char song_id[128], suffix[32], cover_id[128];
    if (!jf_lookup_song_by_path(path, song_id, sizeof(song_id),
                                suffix, sizeof(suffix),
                                cover_id, sizeof(cover_id), NULL))
        return false;

    char err[128];
    err[0] = '\0';

    if (ui_blocking)
    {
        trimpod_fullscreen_message(str(LANG_TRIMPOD_NAV_BUFFERING), NULL);
        button_clear_queue();
        struct jf_cancel_state cancel = { .cancelled = false };
        int32_t rc = tp_jellyfin_ensure_audio_cached(
            path, song_id, suffix, cover_id, 0,
            jf_cancel_check_cb, &cancel, err, sizeof(err));
        if (rc != 0)
        {
            if (!cancel.cancelled && err[0])
                splashf(HZ * 3 / 2, "%s", err);
            return false;
        }
        return true;
    }

    return tp_jellyfin_ensure_audio_cached(
               path, song_id, suffix, cover_id, 0,
               NULL, NULL, err, sizeof(err)) == 0;
}

bool trimpod_jellyfin_download_tracks(char *const *paths, int count)
{
    if (!paths || count <= 0)
        return false;

    button_clear_queue();
    struct jf_cancel_state cancel = { .cancelled = false };
    int ok_count = 0;
    char first_err[128];
    first_err[0] = '\0';

    char widest[64], msg[64];
    snprintf(widest, sizeof(widest), str(LANG_TRIMPOD_DOWNLOADING_FMT), count, count);

    for (int i = 0; i < count; i++)
    {
        if (jf_cancel_check_cb(&cancel))
            break;

        const char *p = paths[i];
        if (!p || !trimpod_jellyfin_is_track_path(p))
            continue;

        if (count == 1)
            trimpod_fullscreen_message(str(LANG_TRIMPOD_DOWNLOADING), NULL);
        else
        {
            snprintf(msg, sizeof(msg), str(LANG_TRIMPOD_DOWNLOADING_FMT), i + 1, count);
            trimpod_fullscreen_message(msg, widest);
        }
        backlight_on();

        char song_id[128], suffix[32], cover_id[128];
        if (!jf_lookup_song_by_path(p, song_id, sizeof(song_id),
                                    suffix, sizeof(suffix),
                                    cover_id, sizeof(cover_id), NULL))
            continue;

        char err[128];
        err[0] = '\0';
        int32_t rc = tp_jellyfin_ensure_audio_cached(
            p, song_id, suffix, cover_id, 1,
            jf_cancel_check_cb, &cancel, err, sizeof(err));
        if (cancel.cancelled)
            break;
        if (rc == 0)
            ok_count++;
        else if (!first_err[0] && err[0])
            strlcpy(first_err, err, sizeof(first_err));
    }

    if (ok_count > 0)
        trimpod_library_mark_stale();

    if (cancel.cancelled)
        return false;

    if (ok_count == 0)
    {
        splashf(HZ * 3 / 2, "%s",
                first_err[0] ? first_err : (const char *)str(LANG_FAILED));
        return false;
    }

    if (count == 1)
        splash(HZ, ID2P(LANG_TRIMPOD_DOWNLOADED_ONE));
    else
        splashf(HZ * 3 / 2, str(LANG_TRIMPOD_DOWNLOADED_FMT), ok_count);
    return true;
}

bool trimpod_jellyfin_download_track(const char *path)
{
    if (!path || !path[0])
        return false;
    char *arr[1] = { (char *)path };
    return trimpod_jellyfin_download_tracks(arr, 1);
}

static char *jf_append_id3_buf(char *base, size_t cap, size_t *used, const char *src)
{
    if (!src || !src[0])
        return NULL;
    size_t len = strlen(src) + 1;
    if (*used + len > cap)
        return NULL;
    char *dst = base + *used;
    memcpy(dst, src, len);
    *used += len;
    return dst;
}

static size_t jf_current_id3v2buf_used(const struct mp3entry *id3)
{
    const char *start = id3->id3v2buf;
    const char *end   = id3->id3v2buf + ID3V2_BUF_SIZE;
    const char *ptrs[] = {
        id3->title, id3->artist, id3->album, id3->genre_string,
        id3->disc_string, id3->track_string, id3->year_string,
        id3->composer, id3->comment, id3->albumartist, id3->grouping,
        id3->mb_track_id
    };
    size_t max_off = 0;
    for (size_t i = 0; i < ARRAYLEN(ptrs); i++)
    {
        const char *p = ptrs[i];
        if (p && p >= start && p < end)
        {
            size_t off = (size_t)(p - start) + strlen(p) + 1;
            if (off > max_off && off <= ID3V2_BUF_SIZE)
                max_off = off;
        }
    }
    return max_off;
}

bool trimpod_jellyfin_fill_id3(const char *path, struct mp3entry *id3)
{
    if (!path || !id3 || !trimpod_jellyfin_is_track_path(path))
        return false;

    wipe_mp3entry(id3);
    strlcpy(id3->path, path, sizeof(id3->path));
    id3->codectype = probe_file_format(path);

    bool ok = false;
    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(
                s_jf_db,
                "SELECT title, artist, albumartist, album, genre,"
                " year, tracknum, discnum, duration_ms, bitrate, frequency, size"
                " FROM tracks WHERE path=?1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
            if (sqlite3_step(st) == SQLITE_ROW)
            {
                size_t used = 0;
                id3->title        = jf_append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                      (const char *)sqlite3_column_text(st, 0));
                id3->artist       = jf_append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                      (const char *)sqlite3_column_text(st, 1));
                id3->albumartist  = jf_append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                      (const char *)sqlite3_column_text(st, 2));
                id3->album        = jf_append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                      (const char *)sqlite3_column_text(st, 3));
                id3->genre_string = jf_append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                      (const char *)sqlite3_column_text(st, 4));
                id3->year         = sqlite3_column_int(st, 5);
                id3->tracknum     = sqlite3_column_int(st, 6);
                id3->discnum      = sqlite3_column_int(st, 7);
                id3->length       = (unsigned long)sqlite3_column_int64(st, 8);
                id3->bitrate      = (unsigned int)sqlite3_column_int(st, 9);
                id3->frequency    = (unsigned long)sqlite3_column_int64(st, 10);
                id3->filesize     = (unsigned long)sqlite3_column_int64(st, 11);
                ok = true;
            }
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_jf_db_mutex);
    return ok;
}

void trimpod_jellyfin_enrich_id3(struct mp3entry *id3)
{
    if (!id3 || !trimpod_jellyfin_is_track_path(id3->path))
        return;

    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(
                s_jf_db,
                "SELECT title, artist, albumartist, album, genre,"
                " year, tracknum, discnum, duration_ms, bitrate, frequency"
                " FROM tracks WHERE path=?1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, id3->path, -1, SQLITE_STATIC);
            if (sqlite3_step(st) == SQLITE_ROW)
            {
                size_t used = jf_current_id3v2buf_used(id3);
                if (!id3->title || !id3->title[0])
                    id3->title = jf_append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                   (const char *)sqlite3_column_text(st, 0));
                if (!id3->artist || !id3->artist[0])
                    id3->artist = jf_append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                    (const char *)sqlite3_column_text(st, 1));
                if (!id3->albumartist || !id3->albumartist[0])
                    id3->albumartist = jf_append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                         (const char *)sqlite3_column_text(st, 2));
                if (!id3->album || !id3->album[0])
                    id3->album = jf_append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                   (const char *)sqlite3_column_text(st, 3));
                if (!id3->genre_string || !id3->genre_string[0])
                    id3->genre_string = jf_append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                          (const char *)sqlite3_column_text(st, 4));
                if (id3->year <= 0)
                    id3->year = sqlite3_column_int(st, 5);
                if (id3->tracknum <= 0)
                    id3->tracknum = sqlite3_column_int(st, 6);
                if (id3->discnum <= 0)
                    id3->discnum = sqlite3_column_int(st, 7);
                if (id3->length == 0)
                    id3->length = (unsigned long)sqlite3_column_int64(st, 8);
                if (id3->bitrate == 0)
                    id3->bitrate = (unsigned int)sqlite3_column_int(st, 9);
                if (id3->frequency == 0)
                    id3->frequency = (unsigned long)sqlite3_column_int64(st, 10);
            }
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_jf_db_mutex);
}

bool trimpod_jellyfin_get_cover_info(const char *path,
                                     char *cover_path_out,
                                     size_t cover_cap,
                                     int *out_bit_depth)
{
    if (cover_path_out && cover_cap > 0)
        cover_path_out[0] = '\0';
    if (out_bit_depth)
        *out_bit_depth = 0;

    if (!trimpod_jellyfin_is_track_path(path))
        return false;

    char song_id[128], suffix[32], cover_id[128];
    int bit_depth = 0;
    if (!jf_lookup_song_by_path(path, song_id, sizeof(song_id),
                                suffix, sizeof(suffix),
                                cover_id, sizeof(cover_id), &bit_depth))
        return false;

    TpJfConfigC cfg;
    if (out_bit_depth && tp_jellyfin_get_config(&cfg) &&
        cfg.audio_quality == TP_JF_AQ_ORIGINAL)
    {
        *out_bit_depth = bit_depth;
    }

    if (!cover_id[0] || !cover_path_out || cover_cap == 0)
        return false;

    return tp_jellyfin_cover_path_for(path, cover_id, cover_path_out, cover_cap) > 0;
}

void trimpod_jellyfin_on_track_change(const char *cur_path)
{
    if (!trimpod_source_is_jellyfin())
        return;

    if (trimpod_jellyfin_is_track_path(cur_path))
    {
        char sid[128], suf[32], cid[128];
        if (jf_lookup_song_by_path(cur_path, sid, sizeof(sid),
                                   suf, sizeof(suf), cid, sizeof(cid), NULL))
        {
            tp_jellyfin_report_session(0, sid, sid, "uipod-audio", 0, 0);
        }
    }

    /* Prefetch next 2 upcoming Jellyfin audio tracks in the background */
    for (int step = 1; step <= 2; step++)
    {
        char pbuf[MAX_PATH];
        const char *pn = playlist_peek(step, pbuf, sizeof(pbuf));
        if (pn && trimpod_jellyfin_is_track_path(pn))
        {
            char sid[128], suf[32], cid[128];
            if (jf_lookup_song_by_path(pn, sid, sizeof(sid),
                                       suf, sizeof(suf), cid, sizeof(cid), NULL))
            {
                tp_jellyfin_prefetch_audio(pn, sid, suf, cid);
            }
        }
    }
}

void trimpod_jellyfin_on_track_finish(const struct mp3entry *id3)
{
    if (!trimpod_source_is_jellyfin() || !id3 ||
        !trimpod_jellyfin_is_track_path(id3->path))
        return;

    char sid[128];
    if (jf_lookup_song_by_path(id3->path, sid, sizeof(sid), NULL, 0, NULL, 0, NULL))
    {
        uint32_t pos_ms = (uint32_t)id3->elapsed;
        tp_jellyfin_report_session(2, sid, sid, "uipod-audio", pos_ms, 0);
    }
}

/* ---- Jellyfin Video Playback & Offline Video Download ----------------- */

struct jf_video_row
{
    char     id[96];
    char     name[256];
    uint32_t duration_ms;
    uint32_t resume_ms;
    char     media_source_id[96];
    char     container[32];
    char     video_codec[32];
    char     audio_codec[32];
    uint32_t width;
    uint32_t height;
    uint32_t ratio_num;
    uint32_t ratio_den;
    uint32_t bit_depth;
    uint32_t bitrate;
    char     chapters_csv[768];
    char     local_path[MAX_PATH];
};

static bool jf_load_video_row(const char *item_id, struct jf_video_row *out)
{
    memset(out, 0, sizeof(*out));
    if (!item_id || !item_id[0])
        return false;

    bool ok = false;
    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(
                s_jf_db,
                "SELECT id, name, duration_ms, resume_ms,"
                " COALESCE(media_source_id, ''), COALESCE(container, ''),"
                " COALESCE(video_codec, ''), COALESCE(audio_codec, ''),"
                " width, height, ratio_num, ratio_den, bit_depth, bitrate,"
                " COALESCE(chapters_csv, ''), COALESCE(local_path, '')"
                " FROM jf_videos WHERE id=?1 LIMIT 1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, item_id, -1, SQLITE_STATIC);
            if (sqlite3_step(st) == SQLITE_ROW)
            {
                strlcpy(out->id,              (const char *)sqlite3_column_text(st, 0), sizeof(out->id));
                strlcpy(out->name,            (const char *)sqlite3_column_text(st, 1), sizeof(out->name));
                out->duration_ms = (uint32_t)sqlite3_column_int64(st, 2);
                out->resume_ms   = (uint32_t)sqlite3_column_int64(st, 3);
                strlcpy(out->media_source_id, (const char *)sqlite3_column_text(st, 4), sizeof(out->media_source_id));
                strlcpy(out->container,       (const char *)sqlite3_column_text(st, 5), sizeof(out->container));
                strlcpy(out->video_codec,     (const char *)sqlite3_column_text(st, 6), sizeof(out->video_codec));
                strlcpy(out->audio_codec,     (const char *)sqlite3_column_text(st, 7), sizeof(out->audio_codec));
                out->width       = (uint32_t)sqlite3_column_int(st, 8);
                out->height      = (uint32_t)sqlite3_column_int(st, 9);
                out->ratio_num   = (uint32_t)sqlite3_column_int(st, 10);
                out->ratio_den   = (uint32_t)sqlite3_column_int(st, 11);
                out->bit_depth   = (uint32_t)sqlite3_column_int(st, 12);
                out->bitrate     = (uint32_t)sqlite3_column_int64(st, 13);
                strlcpy(out->chapters_csv,    (const char *)sqlite3_column_text(st, 14), sizeof(out->chapters_csv));
                strlcpy(out->local_path,      (const char *)sqlite3_column_text(st, 15), sizeof(out->local_path));
                ok = true;
            }
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_jf_db_mutex);
    return ok;
}

static void jf_update_video_resume_db(const char *item_id, uint32_t resume_ms)
{
    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(
                s_jf_db,
                "UPDATE jf_videos SET resume_ms=?1 WHERE id=?2;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_int64(st, 1, (sqlite3_int64)resume_ms);
            sqlite3_bind_text (st, 2, item_id, -1, SQLITE_STATIC);
            sqlite3_step(st);
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_jf_db_mutex);
}

static int jf_parse_chapters_csv(const char *csv, int *out, int max_out)
{
    if (!csv || !csv[0] || !out || max_out <= 0)
        return 0;
    int count = 0;
    const char *p = csv;
    while (*p && count < max_out)
    {
        char *endp = NULL;
        long v = strtol(p, &endp, 10);
        if (endp == p)
            break;
        if (v >= 0)
            out[count++] = (int)v;
        if (*endp == ',')
            p = endp + 1;
        else
            break;
    }
    return count;
}

struct jf_live_play_ctx
{
    struct jf_video_row row;
    char                play_session_id[64];
    bool                reported_start;
    bool                is_transcoded;
};

static bool jf_video_reopen_at_cb(void *vctx, int start_ms)
{
#ifdef RETRO_HANDHELD
    struct jf_live_play_ctx *ctx = (struct jf_live_play_ctx *)vctx;
    if (!ctx->is_transcoded)
        return true;

    tp_jellyfin_stop_encoding(ctx->play_session_id);

    s_session_seq++;
    snprintf(ctx->play_session_id, sizeof(ctx->play_session_id),
             "uipod-%u-%lu", (unsigned)s_session_seq, (unsigned long)current_tick);

    TpJfPlannedStreamC plan;
    char err[128];
    err[0] = '\0';
    if (tp_jellyfin_plan_video_stream(
            ctx->row.id,
            ctx->row.media_source_id,
            ctx->row.container,
            ctx->row.video_codec,
            ctx->row.audio_codec,
            ctx->row.width,
            ctx->row.height,
            ctx->row.bit_depth,
            ctx->row.bitrate,
            start_ms > 0 ? (uint32_t)start_ms : 0,
            ctx->play_session_id,
            &plan,
            err,
            sizeof(err)) != 0)
    {
        return false;
    }

    retrohh_http_proxy_set_path(plan.path_and_query);
    return true;
#else
    (void)vctx; (void)start_ms;
    return false;
#endif
}

static void jf_video_progress_cb(void *vctx, int pos_ms, bool paused, bool stopped)
{
    struct jf_live_play_ctx *ctx = (struct jf_live_play_ctx *)vctx;
    uint32_t pms = pos_ms > 0 ? (uint32_t)pos_ms : 0;

    if (stopped)
    {
        jf_update_video_resume_db(ctx->row.id, pms);
        tp_jellyfin_report_session(
            2, ctx->row.id, ctx->row.media_source_id,
            ctx->play_session_id, pms, 0);
        return;
    }

    if (!ctx->reported_start)
    {
        ctx->reported_start = true;
        tp_jellyfin_report_session(
            0, ctx->row.id, ctx->row.media_source_id,
            ctx->play_session_id, pms, paused ? 1 : 0);
    }
    else
    {
        tp_jellyfin_report_session(
            1, ctx->row.id, ctx->row.media_source_id,
            ctx->play_session_id, pms, paused ? 1 : 0);
    }
}

static void jf_play_video_by_id(const char *item_id, bool force_from_start)
{
    struct jf_live_play_ctx ctx;
    memset(&ctx, 0, sizeof(ctx));
    if (!jf_load_video_row(item_id, &ctx.row))
        return;

    int chapters[64];
    int n_chapters = jf_parse_chapters_csv(ctx.row.chapters_csv, chapters, 64);

    char resume_key[160];
    snprintf(resume_key, sizeof(resume_key), "jellyfin://video/%s", ctx.row.id);

    s_session_seq++;
    snprintf(ctx.play_session_id, sizeof(ctx.play_session_id),
             "uipod-%u-%lu", (unsigned)s_session_seq, (unsigned long)current_tick);

    /* 1) Play local offline SD download directly if present! */
    char offline_path[MAX_PATH];
    if (ctx.row.local_path[0] &&
        tp_jellyfin_resolve_downloaded_video(
            ctx.row.local_path, offline_path, sizeof(offline_path)) > 0)
    {
        struct trimpod_video_stream_opts opts;
        memset(&opts, 0, sizeof(opts));
        opts.url              = offline_path;
        opts.resume_id        = resume_key;
        opts.duration_ms      = (int)ctx.row.duration_ms;
        opts.server_resume_ms = (int)ctx.row.resume_ms;
        opts.force_from_start = force_from_start;
        opts.is_transcoded    = false;
        opts.width            = (int)ctx.row.width;
        opts.height           = (int)ctx.row.height;
        opts.ratio_num        = (int)ctx.row.ratio_num;
        opts.ratio_den        = (int)ctx.row.ratio_den;
        opts.chapter_ms       = n_chapters > 0 ? chapters : NULL;
        opts.chapters         = n_chapters;
        opts.cb_ctx           = &ctx;
        opts.on_progress      = jf_video_progress_cb;

        trimpod_video_play_stream(&opts);
        return;
    }

#ifdef RETRO_HANDHELD
    /* 2) Stream over network via local loopback HTTP/TLS proxy + Jellyfin */
    trimpod_fullscreen_message(str(LANG_TRIMPOD_NAV_BUFFERING), NULL);

    TpJfPlannedStreamC plan;
    char err[128];
    err[0] = '\0';
    if (tp_jellyfin_plan_video_stream(
            ctx.row.id,
            ctx.row.media_source_id,
            ctx.row.container,
            ctx.row.video_codec,
            ctx.row.audio_codec,
            ctx.row.width,
            ctx.row.height,
            ctx.row.bit_depth,
            ctx.row.bitrate,
            0,
            ctx.play_session_id,
            &plan,
            err,
            sizeof(err)) != 0)
    {
        splashf(HZ * 3 / 2, "%s", err[0] ? err : (const char *)str(LANG_FAILED));
        return;
    }

    ctx.is_transcoded = (plan.is_transcoded != 0);

    uint16_t local_port = 0;
    err[0] = '\0';
    if (retrohh_http_proxy_start(
            plan.is_https,
            plan.host,
            plan.port,
            plan.path_and_query,
            plan.extra_headers,
            plan.allow_self_signed,
            &local_port,
            err,
            sizeof(err)) != 0)
    {
        splashf(HZ * 3 / 2, "%s", err[0] ? err : (const char *)str(LANG_FAILED));
        return;
    }

    char proxy_url[128];
    snprintf(proxy_url, sizeof(proxy_url), "http://127.0.0.1:%u/stream.%s",
             (unsigned)local_port,
             plan.download_ext[0] ? plan.download_ext : "ts");

    struct trimpod_video_stream_opts opts;
    memset(&opts, 0, sizeof(opts));
    opts.url              = proxy_url;
    opts.resume_id        = resume_key;
    opts.duration_ms      = (int)ctx.row.duration_ms;
    opts.server_resume_ms = (int)ctx.row.resume_ms;
    opts.force_from_start = force_from_start;
    opts.is_transcoded    = ctx.is_transcoded;
    opts.width            = (int)ctx.row.width;
    opts.height           = (int)ctx.row.height;
    opts.ratio_num        = (int)ctx.row.ratio_num;
    opts.ratio_den        = (int)ctx.row.ratio_den;
    opts.chapter_ms       = n_chapters > 0 ? chapters : NULL;
    opts.chapters         = n_chapters;
    opts.cb_ctx           = &ctx;
    opts.on_reopen_at     = ctx.is_transcoded ? jf_video_reopen_at_cb : NULL;
    opts.on_progress      = jf_video_progress_cb;

    trimpod_video_play_stream(&opts);

    retrohh_http_proxy_stop();
    if (ctx.is_transcoded)
        tp_jellyfin_stop_encoding(ctx.play_session_id);
#else
    splash(HZ, ID2P(LANG_FAILED));
#endif
}

static bool jf_download_video_ids(char **ids, int count)
{
    if (!ids || count <= 0)
        return false;

    button_clear_queue();
    struct jf_cancel_state cancel = { .cancelled = false };
    int ok_count = 0;
    char first_err[128];
    first_err[0] = '\0';

    char widest[64], msg[64];
    snprintf(widest, sizeof(widest), str(LANG_TRIMPOD_DOWNLOADING_FMT), count, count);

    for (int i = 0; i < count; i++)
    {
        if (jf_cancel_check_cb(&cancel))
            break;

        struct jf_video_row row;
        if (!jf_load_video_row(ids[i], &row) || !row.local_path[0])
            continue;

        if (count == 1)
            trimpod_fullscreen_message(str(LANG_TRIMPOD_DOWNLOADING), NULL);
        else
        {
            snprintf(msg, sizeof(msg), str(LANG_TRIMPOD_DOWNLOADING_FMT), i + 1, count);
            trimpod_fullscreen_message(msg, widest);
        }
        backlight_on();

        char err[128];
        err[0] = '\0';
        int32_t rc = tp_jellyfin_download_video(
            row.local_path,
            row.id,
            row.media_source_id,
            row.container,
            row.video_codec,
            row.audio_codec,
            row.width,
            row.height,
            row.bit_depth,
            row.bitrate,
            jf_cancel_check_cb,
            &cancel,
            err,
            sizeof(err));
        if (cancel.cancelled)
            break;
        if (rc == 0)
            ok_count++;
        else if (!first_err[0] && err[0])
            strlcpy(first_err, err, sizeof(first_err));
    }

    if (cancel.cancelled)
        return false;

    if (ok_count == 0)
    {
        splashf(HZ * 3 / 2, "%s",
                first_err[0] ? first_err : (const char *)str(LANG_FAILED));
        return false;
    }

    if (count == 1)
        splash(HZ, ID2P(LANG_TRIMPOD_DOWNLOADED_ONE));
    else
        splashf(HZ * 3 / 2, str(LANG_TRIMPOD_DOWNLOADED_VIDEOS_FMT), ok_count);
    return true;
}

/* ---- Jellyfin Settings Page ------------------------------------------- */

enum
{
    JF_ROW_URL = 0,
    JF_ROW_USER,
    JF_ROW_PASS,
    JF_ROW_VIDEO_QUAL,
    JF_ROW_AUDIO_QUAL,
    JF_ROW_CACHE_LIMIT,
    JF_ROW_SELF_SIGNED,
    JF_ROW_SYNC,
    JF_ROW_CLEAR_CACHE,
    JF_ROW_COUNT
};

static const uint32_t s_jf_cache_presets_mb[] = { 512, 1024, 2048, 4096, 0 };

struct jf_settings_page
{
    struct trimpod_page base;
    struct gui_synclist lists;
    TpJfConfigC         cfg;
    bool                from_source_picker;
    bool                synced_ok;
    char                indic_buf[JF_ROW_COUNT][96];
};

static const char *jf_video_quality_label(uint8_t q)
{
    switch (q)
    {
        case TP_JF_VQ_TRANSCODE_720P: return str(LANG_TRIMPOD_JF_VQ_720P);
        case TP_JF_VQ_TRANSCODE_480P: return str(LANG_TRIMPOD_JF_VQ_480P);
        case TP_JF_VQ_ORIGINAL_1080P: return str(LANG_TRIMPOD_JF_VQ_ORIGINAL);
        case TP_JF_VQ_AUTO_720P:
        default:                      return str(LANG_TRIMPOD_JF_VQ_AUTO);
    }
}

static const char *jf_audio_quality_label(uint8_t q)
{
    switch (q)
    {
        case TP_JF_AQ_MP3_320:  return str(LANG_TRIMPOD_NAV_QUAL_320);
        case TP_JF_AQ_MP3_192:  return str(LANG_TRIMPOD_NAV_QUAL_192);
        case TP_JF_AQ_ORIGINAL:
        default:                return str(LANG_TRIMPOD_NAV_QUAL_ORIGINAL);
    }
}

static void jf_format_cache_limit(uint32_t mb, char *out, size_t cap)
{
    if (mb == 0)
        strlcpy(out, str(LANG_TRIMPOD_NAV_UNLIMITED), cap);
    else if (mb >= 1024 && (mb % 1024) == 0)
        snprintf(out, cap, "%u GB", (unsigned)(mb / 1024));
    else
        snprintf(out, cap, "%u MB", (unsigned)mb);
}

static const char *jf_set_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    (void)data; (void)buf; (void)buf_len;
    switch (sel)
    {
        case JF_ROW_URL:         return str(LANG_TRIMPOD_NAV_SERVER_URL);
        case JF_ROW_USER:        return str(LANG_TRIMPOD_NAV_USERNAME);
        case JF_ROW_PASS:        return str(LANG_TRIMPOD_NAV_PASSWORD);
        case JF_ROW_VIDEO_QUAL:  return str(LANG_TRIMPOD_JF_VIDEO_QUALITY);
        case JF_ROW_AUDIO_QUAL:  return str(LANG_TRIMPOD_NAV_QUALITY);
        case JF_ROW_CACHE_LIMIT: return str(LANG_TRIMPOD_NAV_CACHE_LIMIT);
        case JF_ROW_SELF_SIGNED: return str(LANG_TRIMPOD_NAV_SELF_SIGNED);
        case JF_ROW_SYNC:        return str(LANG_TRIMPOD_NAV_SYNC);
        case JF_ROW_CLEAR_CACHE: return str(LANG_TRIMPOD_NAV_CLEAR_CACHE);
        default:                 return "";
    }
}

static const char *jf_set_get_indicator(int sel, void *data)
{
    struct jf_settings_page *p = (struct jf_settings_page *)data;
    if (sel < 0 || sel >= JF_ROW_COUNT)
        return NULL;
    char *out = p->indic_buf[sel];
    size_t cap = sizeof(p->indic_buf[sel]);

    switch (sel)
    {
        case JF_ROW_URL:
            if (!p->cfg.url[0])
                return str(LANG_TRIMPOD_NAV_NOT_SET);
            {
                const char *u = p->cfg.url;
                if (strncasecmp(u, "http://", 7) == 0)
                    u += 7;
                else if (strncasecmp(u, "https://", 8) == 0)
                    u += 8;
                strlcpy(out, u, cap);
                return out;
            }
        case JF_ROW_USER:
            return p->cfg.username[0] ? p->cfg.username
                                      : (const char *)str(LANG_TRIMPOD_NAV_NOT_SET);
        case JF_ROW_PASS:
            return p->cfg.password[0] ? "••••••••"
                                      : (const char *)str(LANG_TRIMPOD_NAV_NOT_SET);
        case JF_ROW_VIDEO_QUAL:
            return jf_video_quality_label(p->cfg.video_quality);
        case JF_ROW_AUDIO_QUAL:
            return jf_audio_quality_label(p->cfg.audio_quality);
        case JF_ROW_CACHE_LIMIT:
            jf_format_cache_limit(p->cfg.cache_limit_mb, out, cap);
            return out;
        case JF_ROW_SELF_SIGNED:
            return trimpod_toggle_str(p->cfg.allow_self_signed != 0);
        case JF_ROW_SYNC:
            return NULL;
        case JF_ROW_CLEAR_CACHE:
        {
            uint64_t bytes = tp_jellyfin_cache_usage_bytes();
            uint32_t mb = (uint32_t)((bytes + (512ULL * 1024ULL)) / (1024ULL * 1024ULL));
            snprintf(out, cap, "%u MB", (unsigned)mb);
            return out;
        }
        default:
            return NULL;
    }
}

static bool jf_set_get_chevron(int sel, void *data)
{
    (void)data;
    return sel == JF_ROW_SYNC;
}

static void jf_rewrite_playlists_paths_locked(sqlite3_stmt *lookup_stmt)
{
    DIR *d = opendir(JF_PLAYLISTS_DIR);
    if (!d)
        return;

    struct dirent *ent;
    while ((ent = readdir(d)) != NULL)
    {
        const char *nm = ent->d_name;
        size_t nlen = strlen(nm);
        if (nlen < 6 || strcasecmp(nm + nlen - 5, ".m3u8") != 0)
            continue;

        char pl_path[MAX_PATH], tmp_path[MAX_PATH];
        snprintf(pl_path, sizeof(pl_path), "%s/%s", JF_PLAYLISTS_DIR, nm);
        snprintf(tmp_path, sizeof(tmp_path), "%s/%s.tmp", JF_PLAYLISTS_DIR, nm);

        int in = open(pl_path, O_RDONLY);
        if (in < 0)
            continue;
        off_t fsz = filesize(in);
        if (fsz <= 0 || fsz > 2 * 1024 * 1024)
        {
            close(in);
            continue;
        }
        char *content = malloc((size_t)fsz + 1);
        if (!content)
        {
            close(in);
            continue;
        }
        ssize_t n = read(in, content, (size_t)fsz);
        close(in);
        if (n <= 0)
        {
            free(content);
            continue;
        }
        content[n] = '\0';

        int out = open(tmp_path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
        if (out < 0)
        {
            free(content);
            continue;
        }

        char *cursor = content;
        while (*cursor)
        {
            char *nl = strchr(cursor, '\n');
            size_t len = nl ? (size_t)(nl - cursor) : strlen(cursor);
            size_t clean_len = len;
            if (clean_len > 0 && cursor[clean_len - 1] == '\r')
                clean_len--;

            char line[MAX_PATH];
            if (clean_len > 0 && clean_len < sizeof(line) && cursor[0] != '#')
            {
                memcpy(line, cursor, clean_len);
                line[clean_len] = '\0';

                const char *mapped = line;
                sqlite3_bind_text(lookup_stmt, 1, line, -1, SQLITE_STATIC);
                if (sqlite3_step(lookup_stmt) == SQLITE_ROW)
                {
                    const char *np = (const char *)sqlite3_column_text(lookup_stmt, 0);
                    if (np && np[0])
                        mapped = np;
                }
                fdprintf(out, "%s\n", mapped);
                sqlite3_reset(lookup_stmt);
            }
            else if (clean_len > 0 || cursor[0] == '#')
            {
                write(out, cursor, clean_len);
                write(out, "\n", 1);
            }

            if (!nl)
                break;
            cursor = nl + 1;
        }

        close(out);
        free(content);
        rename(tmp_path, pl_path);
    }
    closedir(d);
}

static void jf_swap_audio_suffix(const char *old_path,
                                 const char *orig_suffix,
                                 uint8_t new_quality,
                                 char *out,
                                 size_t cap)
{
    const char *slash = strrchr(old_path, '/');
    const char *fname = slash ? slash + 1 : old_path;
    size_t fname_len = strlen(fname);
    size_t stem_end = fname_len;

    if (fname_len > 9 &&
        (strcasecmp(fname + fname_len - 9, ".320k.mp3") == 0 ||
         strcasecmp(fname + fname_len - 9, ".192k.mp3") == 0))
    {
        stem_end = fname_len - 9;
    }
    else
    {
        const char *dot = strrchr(fname, '.');
        if (dot && dot > fname)
            stem_end = (size_t)(dot - fname);
    }

    char new_suf[24];
    if (new_quality == TP_JF_AQ_MP3_320)
        strlcpy(new_suf, ".320k.mp3", sizeof(new_suf));
    else if (new_quality == TP_JF_AQ_MP3_192)
        strlcpy(new_suf, ".192k.mp3", sizeof(new_suf));
    else
    {
        char ext[12];
        size_t ei = 0;
        const char *s = orig_suffix ? orig_suffix : "";
        while (*s == '.')
            s++;
        for (; *s && ei + 1 < sizeof(ext) && ei < 8; s++)
        {
            if (isalnum((unsigned char)*s))
                ext[ei++] = (char)tolower((unsigned char)*s);
        }
        ext[ei] = '\0';
        snprintf(new_suf, sizeof(new_suf), ".%s", ei > 0 ? ext : "mp3");
    }

    size_t prefix_len = (size_t)(fname - old_path) + stem_end;
    if (prefix_len + strlen(new_suf) + 1 > cap)
    {
        strlcpy(out, old_path, cap);
        return;
    }
    memcpy(out, old_path, prefix_len);
    strcpy(out + prefix_len, new_suf);
}

static void jf_repath_audio_for_quality(uint8_t new_quality)
{
    mutex_lock(&s_jf_db_mutex);
    if (!s_jf_db)
    {
        mutex_unlock(&s_jf_db_mutex);
        return;
    }

    jf_db_exec("BEGIN IMMEDIATE;");
    jf_db_exec("DROP TABLE IF EXISTS temp.jf_repath;");
    jf_db_exec("CREATE TEMP TABLE jf_repath("
               "id INTEGER PRIMARY KEY, old_path TEXT UNIQUE, new_path TEXT NOT NULL UNIQUE);");

    sqlite3_stmt *sel = NULL, *ins = NULL;
    if (sqlite3_prepare_v2(
            s_jf_db,
            "SELECT id, path, COALESCE(suffix, '') FROM tracks;",
            -1, &sel, NULL) == SQLITE_OK &&
        sqlite3_prepare_v2(
            s_jf_db,
            "INSERT OR IGNORE INTO temp.jf_repath(id, old_path, new_path) VALUES(?1, ?2, ?3);",
            -1, &ins, NULL) == SQLITE_OK)
    {
        while (sqlite3_step(sel) == SQLITE_ROW)
        {
            int id           = sqlite3_column_int(sel, 0);
            const char *oldp = (const char *)sqlite3_column_text(sel, 1);
            const char *sfx  = (const char *)sqlite3_column_text(sel, 2);
            if (!oldp || !oldp[0])
                continue;
            char newp[MAX_PATH];
            jf_swap_audio_suffix(oldp, sfx ? sfx : "mp3", new_quality, newp, sizeof(newp));
            sqlite3_bind_int (ins, 1, id);
            sqlite3_bind_text(ins, 2, oldp, -1, SQLITE_TRANSIENT);
            sqlite3_bind_text(ins, 3, newp, -1, SQLITE_TRANSIENT);
            sqlite3_step(ins);
            sqlite3_reset(ins);
        }
    }
    sqlite3_finalize(sel);
    sqlite3_finalize(ins);

    jf_db_exec("UPDATE tracks SET path = ("
               "SELECT new_path FROM temp.jf_repath WHERE temp.jf_repath.id = tracks.id)"
               " WHERE id IN (SELECT id FROM temp.jf_repath);");

    sqlite3_stmt *lookup = NULL;
    if (sqlite3_prepare_v2(s_jf_db,
                           "SELECT new_path FROM temp.jf_repath WHERE old_path = ?1;",
                           -1, &lookup, NULL) == SQLITE_OK)
    {
        jf_rewrite_playlists_paths_locked(lookup);
        sqlite3_finalize(lookup);
    }

    jf_db_exec("DROP TABLE IF EXISTS temp.jf_repath;");
    jf_db_exec("COMMIT;");
    jf_db_writeback_locked();
    mutex_unlock(&s_jf_db_mutex);
}

static void jf_apply_and_persist_cfg(struct jf_settings_page *p, uint8_t prev_audio_qual)
{
    tp_jellyfin_set_config(&p->cfg);
    tp_jellyfin_get_config(&p->cfg);
    jf_save_config_to_disk();
    if (p->cfg.audio_quality != prev_audio_qual)
        jf_repath_audio_for_quality(p->cfg.audio_quality);
}

static void jf_set_draw(struct trimpod_page *self)
{
    gui_synclist_draw(&((struct jf_settings_page *)self)->lists);
}

static int jf_set_poll(struct trimpod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct jf_settings_page *)self)->lists, &action);
    return action;
}

static enum trimpod_page_result jf_set_on_action(struct trimpod_page *self, int action)
{
    struct jf_settings_page *p = (struct jf_settings_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);

    if (action == ACTION_STD_CANCEL)
        return TRIMPOD_PAGE_DONE;

    if (action != ACTION_STD_OK)
        return TRIMPOD_PAGE_STAY;

    uint8_t prev_audio_qual = p->cfg.audio_quality;

    switch (sel)
    {
        case JF_ROW_URL:
        {
            char buf[sizeof(p->cfg.url)];
            if (p->cfg.url[0])
                strlcpy(buf, p->cfg.url, sizeof(buf));
            else
                strlcpy(buf, "http://", sizeof(buf));
            if (trimpod_kbd_input_ex(buf, sizeof(buf), true) == 0)
            {
                strlcpy(p->cfg.url, buf, sizeof(p->cfg.url));
                jf_apply_and_persist_cfg(p, prev_audio_qual);
            }
            break;
        }

        case JF_ROW_USER:
            if (trimpod_kbd_input_ex(p->cfg.username, sizeof(p->cfg.username), true) == 0)
                jf_apply_and_persist_cfg(p, prev_audio_qual);
            break;

        case JF_ROW_PASS:
            if (trimpod_kbd_input_ex(p->cfg.password, sizeof(p->cfg.password), true) == 0)
                jf_apply_and_persist_cfg(p, prev_audio_qual);
            break;

        case JF_ROW_VIDEO_QUAL:
        {
            const char *opts[4] = {
                str(LANG_TRIMPOD_JF_VQ_AUTO),
                str(LANG_TRIMPOD_JF_VQ_720P),
                str(LANG_TRIMPOD_JF_VQ_480P),
                str(LANG_TRIMPOD_JF_VQ_ORIGINAL),
            };
            int pick = trimpod_context_menu(str(LANG_TRIMPOD_JF_VIDEO_QUALITY), opts, 4);
            if (pick >= 0 && pick <= 3)
            {
                p->cfg.video_quality = (uint8_t)pick;
                jf_apply_and_persist_cfg(p, prev_audio_qual);
            }
            break;
        }

        case JF_ROW_AUDIO_QUAL:
        {
            const char *opts[3] = {
                str(LANG_TRIMPOD_NAV_QUAL_ORIGINAL),
                str(LANG_TRIMPOD_NAV_QUAL_320),
                str(LANG_TRIMPOD_NAV_QUAL_192),
            };
            int pick = trimpod_context_menu(str(LANG_TRIMPOD_NAV_QUALITY), opts, 3);
            if (pick >= 0 && pick <= 2)
            {
                p->cfg.audio_quality = (uint8_t)pick;
                jf_apply_and_persist_cfg(p, prev_audio_qual);
            }
            break;
        }

        case JF_ROW_CACHE_LIMIT:
        {
            char labels[ARRAYLEN(s_jf_cache_presets_mb)][32];
            const char *opts[ARRAYLEN(s_jf_cache_presets_mb)];
            for (size_t i = 0; i < ARRAYLEN(s_jf_cache_presets_mb); i++)
            {
                jf_format_cache_limit(s_jf_cache_presets_mb[i], labels[i], sizeof(labels[i]));
                opts[i] = labels[i];
            }
            int pick = trimpod_context_menu(str(LANG_TRIMPOD_NAV_CACHE_LIMIT),
                                            opts, (int)ARRAYLEN(s_jf_cache_presets_mb));
            if (pick >= 0 && (size_t)pick < ARRAYLEN(s_jf_cache_presets_mb))
            {
                p->cfg.cache_limit_mb = s_jf_cache_presets_mb[pick];
                jf_apply_and_persist_cfg(p, prev_audio_qual);
            }
            break;
        }

        case JF_ROW_SELF_SIGNED:
            p->cfg.allow_self_signed = p->cfg.allow_self_signed ? 0 : 1;
            jf_apply_and_persist_cfg(p, prev_audio_qual);
            break;

        case JF_ROW_SYNC:
            jf_apply_and_persist_cfg(p, prev_audio_qual);
            if (trimpod_jellyfin_sync(true))
            {
                p->synced_ok = true;
                if (p->from_source_picker)
                {
                    trimpod_source_set(TRIMPOD_SOURCE_JELLYFIN);
                    p->base.no_arm_back = true;
                    return TRIMPOD_PAGE_DONE;
                }
            }
            break;

        case JF_ROW_CLEAR_CACHE:
            if (trimpod_confirm(str(LANG_TRIMPOD_JF_CLEAR_CACHE_Q), NULL))
            {
                tp_jellyfin_clear_cache(0);
                splash(HZ, ID2P(LANG_TRIMPOD_NAV_CACHE_CLEARED));
            }
            break;
    }

    return TRIMPOD_PAGE_STAY;
}

static const struct trimpod_page_vtable jf_set_vtable = {
    .legend    = NULL,
    .draw      = jf_set_draw,
    .poll      = jf_set_poll,
    .on_action = jf_set_on_action,
};

static const int jf_set_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL, -1
};

int trimpod_jellyfin_settings_page(bool from_source_picker)
{
    struct jf_settings_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt          = &jf_set_vtable;
    p.base.context     = CONTEXT_LIST;
    p.base.allowed     = jf_set_allowed;
    p.base.title       = str(LANG_TRIMPOD_SOURCE_JELLYFIN);
    p.from_source_picker = from_source_picker;

    tp_jellyfin_get_config(&p.cfg);

    gui_synclist_init(&p.lists, jf_set_get_name, &p, false, 1, NULL);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_TRIMPOD_SOURCE_JELLYFIN),
                           Icon_Submenu_Entered);
    gui_synclist_set_indicator_callback(&p.lists, jf_set_get_indicator);
    gui_synclist_set_chevron_callback(&p.lists, jf_set_get_chevron);
    gui_synclist_set_nb_items(&p.lists, JF_ROW_COUNT);
    gui_synclist_select_item(&p.lists, tp_jellyfin_is_configured() ? JF_ROW_SYNC
                                                                   : JF_ROW_URL);

    trimpod_page_run(&p.base);
    return p.synced_ok ? 1 : 0;
}

int trimpod_jellyfin_menu_page(void)
{
    trimpod_jellyfin_settings_page(false);
    return 0;
}

/* ---- Jellyfin Hierarchical Browser (`Videos`, `Television`, `Files`) -- */

enum jf_browse_mode
{
    JF_MODE_VIDEOS = 0,
    JF_MODE_TV     = 1,
    JF_MODE_FILES  = 2,
};

enum jf_level_kind
{
    JF_LVL_VIEWS = 0,
    JF_LVL_ALL_SERIES,
    JF_LVL_VIEW_ITEMS,
    JF_LVL_SERIES_SEASONS,
    JF_LVL_SEASON_EPISODES,
    JF_LVL_AUDIO_ARTISTS,
    JF_LVL_AUDIO_ALBUMS,
    JF_LVL_AUDIO_TRACKS,
};

enum jf_entry_kind
{
    JF_ENT_VIEW = 0,
    JF_ENT_SERIES,
    JF_ENT_SEASON,
    JF_ENT_VIDEO,
    JF_ENT_AUDIO_ARTIST,
    JF_ENT_AUDIO_ALBUM,
    JF_ENT_AUDIO_TRACK,
};

struct jf_browse_entry
{
    uint8_t kind;        /* enum jf_entry_kind */
    uint8_t view_kind;   /* for JF_ENT_VIEW */
    int32_t num;         /* season_number for JF_ENT_SEASON */
    char    label[256];
    char    id[128];     /* view_id / series_id / season_id / video_id */
    char    aux[256];    /* series_id / artist / local_path / container */
};

struct jf_nav_level
{
    enum jf_level_kind kind;
    char               title[128];
    char               id[128];
    char               aux[256];
    int32_t            num;
    int                saved_sel;
};

#define JF_MAX_STACK 8

struct jf_browser_page
{
    struct trimpod_page     base;
    struct gui_synclist     lists;
    enum jf_browse_mode     mode;
    struct jf_nav_level     stack[JF_MAX_STACK];
    int                     depth;
    struct jf_browse_entry *entries;
    int                     count;
    int                     cap;
    int                     result;
};

static void jf_browser_clear_entries(struct jf_browser_page *p)
{
    free(p->entries);
    p->entries = NULL;
    p->count   = 0;
    p->cap     = 0;
}

static struct jf_browse_entry *jf_browser_push_entry(struct jf_browser_page *p)
{
    if (p->count == p->cap)
    {
        int ncap = p->cap ? p->cap * 2 : 32;
        struct jf_browse_entry *ne =
            realloc(p->entries, (size_t)ncap * sizeof(*ne));
        if (!ne)
            return NULL;
        p->entries = ne;
        p->cap = ncap;
    }
    struct jf_browse_entry *e = &p->entries[p->count++];
    memset(e, 0, sizeof(*e));
    return e;
}

static int jf_entry_cmp(const void *a, const void *b)
{
    const struct jf_browse_entry *ea = (const struct jf_browse_entry *)a;
    const struct jf_browse_entry *eb = (const struct jf_browse_entry *)b;
    bool da = (ea->kind != JF_ENT_VIDEO && ea->kind != JF_ENT_AUDIO_TRACK);
    bool db = (eb->kind != JF_ENT_VIDEO && eb->kind != JF_ENT_AUDIO_TRACK);
    if (da != db)
        return da ? -1 : 1;
    return strnatcasecmp(ea->label, eb->label);
}

static void jf_browser_load_level(struct jf_browser_page *p)
{
    jf_browser_clear_entries(p);
    if (p->depth <= 0)
        return;

    struct jf_nav_level *lvl = &p->stack[p->depth - 1];

    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_stmt *st = NULL;
        switch (lvl->kind)
        {
            case JF_LVL_VIEWS:
            {
                const char *sql = (p->mode == JF_MODE_VIDEOS)
                    ? "SELECT id, name, view_kind FROM jf_views WHERE view_kind != 2 ORDER BY ord;"
                    : "SELECT id, name, view_kind FROM jf_views ORDER BY ord;";
                if (sqlite3_prepare_v2(s_jf_db, sql, -1, &st, NULL) == SQLITE_OK)
                {
                    while (sqlite3_step(st) == SQLITE_ROW)
                    {
                        struct jf_browse_entry *e = jf_browser_push_entry(p);
                        if (!e) break;
                        e->kind      = JF_ENT_VIEW;
                        strlcpy(e->id,    (const char *)sqlite3_column_text(st, 0), sizeof(e->id));
                        strlcpy(e->label, (const char *)sqlite3_column_text(st, 1), sizeof(e->label));
                        e->view_kind = (uint8_t)sqlite3_column_int(st, 2);
                    }
                    sqlite3_finalize(st);
                }
                break;
            }

            case JF_LVL_ALL_SERIES:
                if (sqlite3_prepare_v2(
                        s_jf_db,
                        "SELECT id, name, year FROM jf_videos"
                        " WHERE item_type=?1 GROUP BY id ORDER BY name COLLATE NOCASE;",
                        -1, &st, NULL) == SQLITE_OK)
                {
                    sqlite3_bind_int(st, 1, TP_JF_ITEM_SERIES);
                    while (sqlite3_step(st) == SQLITE_ROW)
                    {
                        struct jf_browse_entry *e = jf_browser_push_entry(p);
                        if (!e) break;
                        e->kind = JF_ENT_SERIES;
                        strlcpy(e->id, (const char *)sqlite3_column_text(st, 0), sizeof(e->id));
                        const char *nm = (const char *)sqlite3_column_text(st, 1);
                        int yr = sqlite3_column_int(st, 2);
                        if (yr > 0)
                            snprintf(e->label, sizeof(e->label), "%s (%d)", nm, yr);
                        else
                            strlcpy(e->label, nm, sizeof(e->label));
                    }
                    sqlite3_finalize(st);
                    if (p->count > 1)
                        qsort(p->entries, (size_t)p->count, sizeof(*p->entries), jf_entry_cmp);
                }
                break;

            case JF_LVL_VIEW_ITEMS:
                if (sqlite3_prepare_v2(
                        s_jf_db,
                        "SELECT id, item_type, name, year, COALESCE(container, 'mp4')"
                        " FROM jf_videos"
                        " WHERE view_id=?1 AND item_type IN (?2, ?3, ?4)"
                        " ORDER BY item_type, name COLLATE NOCASE;",
                        -1, &st, NULL) == SQLITE_OK)
                {
                    sqlite3_bind_text(st, 1, lvl->id, -1, SQLITE_STATIC);
                    sqlite3_bind_int (st, 2, TP_JF_ITEM_SERIES);
                    sqlite3_bind_int (st, 3, TP_JF_ITEM_MOVIE);
                    sqlite3_bind_int (st, 4, TP_JF_ITEM_VIDEO);
                    while (sqlite3_step(st) == SQLITE_ROW)
                    {
                        struct jf_browse_entry *e = jf_browser_push_entry(p);
                        if (!e) break;
                        const char *id  = (const char *)sqlite3_column_text(st, 0);
                        int itype       = sqlite3_column_int(st, 1);
                        const char *nm  = (const char *)sqlite3_column_text(st, 2);
                        int yr          = sqlite3_column_int(st, 3);
                        const char *cnt = (const char *)sqlite3_column_text(st, 4);

                        e->kind = (itype == TP_JF_ITEM_SERIES) ? JF_ENT_SERIES : JF_ENT_VIDEO;
                        strlcpy(e->id, id, sizeof(e->id));
                        snprintf(e->aux, sizeof(e->aux), "v.%s", cnt[0] ? cnt : "mp4");
                        if (yr > 0)
                            snprintf(e->label, sizeof(e->label), "%s (%d)", nm, yr);
                        else
                            strlcpy(e->label, nm, sizeof(e->label));
                    }
                    sqlite3_finalize(st);
                    if (p->count > 1)
                        qsort(p->entries, (size_t)p->count, sizeof(*p->entries), jf_entry_cmp);
                }
                break;

            case JF_LVL_SERIES_SEASONS:
                if (sqlite3_prepare_v2(
                        s_jf_db,
                        "SELECT id, name, season_number FROM jf_videos"
                        " WHERE item_type=?1 AND series_id=?2"
                        " GROUP BY id ORDER BY season_number, name COLLATE NOCASE;",
                        -1, &st, NULL) == SQLITE_OK)
                {
                    sqlite3_bind_int (st, 1, TP_JF_ITEM_SEASON);
                    sqlite3_bind_text(st, 2, lvl->id, -1, SQLITE_STATIC);
                    while (sqlite3_step(st) == SQLITE_ROW)
                    {
                        struct jf_browse_entry *e = jf_browser_push_entry(p);
                        if (!e) break;
                        e->kind = JF_ENT_SEASON;
                        strlcpy(e->id,    (const char *)sqlite3_column_text(st, 0), sizeof(e->id));
                        strlcpy(e->label, (const char *)sqlite3_column_text(st, 1), sizeof(e->label));
                        e->num = sqlite3_column_int(st, 2);
                        strlcpy(e->aux, lvl->id, sizeof(e->aux));
                    }
                    sqlite3_finalize(st);
                }
                /* Fallback for flat/seasonless shows: list episodes directly! */
                if (p->count == 0 && sqlite3_prepare_v2(
                        s_jf_db,
                        "SELECT id, name, season_number, episode_number, COALESCE(container, 'mp4')"
                        " FROM jf_videos"
                        " WHERE item_type=?1 AND series_id=?2"
                        " GROUP BY id ORDER BY season_number, episode_number, name COLLATE NOCASE;",
                        -1, &st, NULL) == SQLITE_OK)
                {
                    sqlite3_bind_int (st, 1, TP_JF_ITEM_EPISODE);
                    sqlite3_bind_text(st, 2, lvl->id, -1, SQLITE_STATIC);
                    while (sqlite3_step(st) == SQLITE_ROW)
                    {
                        struct jf_browse_entry *e = jf_browser_push_entry(p);
                        if (!e) break;
                        e->kind = JF_ENT_VIDEO;
                        strlcpy(e->id, (const char *)sqlite3_column_text(st, 0), sizeof(e->id));
                        const char *nm  = (const char *)sqlite3_column_text(st, 1);
                        int sn          = sqlite3_column_int(st, 2);
                        int en          = sqlite3_column_int(st, 3);
                        const char *cnt = (const char *)sqlite3_column_text(st, 4);
                        snprintf(e->aux, sizeof(e->aux), "v.%s", cnt[0] ? cnt : "mp4");
                        if (sn > 0 && en > 0)
                            snprintf(e->label, sizeof(e->label), "S%02dE%02d - %s", sn, en, nm);
                        else if (en > 0)
                            snprintf(e->label, sizeof(e->label), "%02d. %s", en, nm);
                        else
                            strlcpy(e->label, nm, sizeof(e->label));
                    }
                    sqlite3_finalize(st);
                }
                break;

            case JF_LVL_SEASON_EPISODES:
                if (sqlite3_prepare_v2(
                        s_jf_db,
                        "SELECT id, name, episode_number, COALESCE(container, 'mp4')"
                        " FROM jf_videos"
                        " WHERE item_type=?1 AND (season_id=?2 OR (COALESCE(season_id,'')='' AND series_id=?3 AND season_number=?4))"
                        " GROUP BY id ORDER BY episode_number, name COLLATE NOCASE;",
                        -1, &st, NULL) == SQLITE_OK)
                {
                    sqlite3_bind_int (st, 1, TP_JF_ITEM_EPISODE);
                    sqlite3_bind_text(st, 2, lvl->id, -1, SQLITE_STATIC);
                    sqlite3_bind_text(st, 3, lvl->aux, -1, SQLITE_STATIC);
                    sqlite3_bind_int (st, 4, lvl->num);
                    while (sqlite3_step(st) == SQLITE_ROW)
                    {
                        struct jf_browse_entry *e = jf_browser_push_entry(p);
                        if (!e) break;
                        e->kind = JF_ENT_VIDEO;
                        strlcpy(e->id, (const char *)sqlite3_column_text(st, 0), sizeof(e->id));
                        const char *nm  = (const char *)sqlite3_column_text(st, 1);
                        int en          = sqlite3_column_int(st, 2);
                        const char *cnt = (const char *)sqlite3_column_text(st, 3);
                        snprintf(e->aux, sizeof(e->aux), "v.%s", cnt[0] ? cnt : "mp4");
                        if (en > 0)
                            snprintf(e->label, sizeof(e->label), "%02d. %s", en, nm);
                        else
                            strlcpy(e->label, nm, sizeof(e->label));
                    }
                    sqlite3_finalize(st);
                }
                break;

            case JF_LVL_AUDIO_ARTISTS:
                if (sqlite3_prepare_v2(
                        s_jf_db,
                        "SELECT DISTINCT COALESCE(NULLIF(browse_artist, ''), 'Unknown Artist'), sort_artist"
                        " FROM tracks ORDER BY sort_artist, browse_artist;",
                        -1, &st, NULL) == SQLITE_OK)
                {
                    while (sqlite3_step(st) == SQLITE_ROW)
                    {
                        struct jf_browse_entry *e = jf_browser_push_entry(p);
                        if (!e) break;
                        e->kind = JF_ENT_AUDIO_ARTIST;
                        const char *art = (const char *)sqlite3_column_text(st, 0);
                        strlcpy(e->label, art, sizeof(e->label));
                        strlcpy(e->aux,   art, sizeof(e->aux));
                    }
                    sqlite3_finalize(st);
                }
                break;

            case JF_LVL_AUDIO_ALBUMS:
                if (sqlite3_prepare_v2(
                        s_jf_db,
                        "SELECT DISTINCT COALESCE(NULLIF(album, ''), 'Unknown Album'), year"
                        " FROM tracks"
                        " WHERE COALESCE(NULLIF(browse_artist, ''), 'Unknown Artist')=?1"
                        " ORDER BY year, album COLLATE NOCASE;",
                        -1, &st, NULL) == SQLITE_OK)
                {
                    sqlite3_bind_text(st, 1, lvl->aux, -1, SQLITE_STATIC);
                    while (sqlite3_step(st) == SQLITE_ROW)
                    {
                        struct jf_browse_entry *e = jf_browser_push_entry(p);
                        if (!e) break;
                        e->kind = JF_ENT_AUDIO_ALBUM;
                        const char *alb = (const char *)sqlite3_column_text(st, 0);
                        strlcpy(e->label, alb, sizeof(e->label));
                        strlcpy(e->id,    alb, sizeof(e->id));
                        strlcpy(e->aux,   lvl->aux, sizeof(e->aux));
                    }
                    sqlite3_finalize(st);
                }
                break;

            case JF_LVL_AUDIO_TRACKS:
                if (sqlite3_prepare_v2(
                        s_jf_db,
                        "SELECT path, COALESCE(NULLIF(title, ''), song_id), tracknum"
                        " FROM tracks"
                        " WHERE COALESCE(NULLIF(browse_artist, ''), 'Unknown Artist')=?1"
                        "   AND COALESCE(NULLIF(album, ''), 'Unknown Album')=?2"
                        " ORDER BY discnum, tracknum, title COLLATE NOCASE;",
                        -1, &st, NULL) == SQLITE_OK)
                {
                    sqlite3_bind_text(st, 1, lvl->aux, -1, SQLITE_STATIC);
                    sqlite3_bind_text(st, 2, lvl->id, -1, SQLITE_STATIC);
                    while (sqlite3_step(st) == SQLITE_ROW)
                    {
                        struct jf_browse_entry *e = jf_browser_push_entry(p);
                        if (!e) break;
                        e->kind = JF_ENT_AUDIO_TRACK;
                        const char *tp  = (const char *)sqlite3_column_text(st, 0);
                        const char *tit = (const char *)sqlite3_column_text(st, 1);
                        int tno         = sqlite3_column_int(st, 2);
                        strlcpy(e->aux, tp, sizeof(e->aux));
                        if (tno > 0)
                            snprintf(e->label, sizeof(e->label), "%02d. %s", tno, tit);
                        else
                            strlcpy(e->label, tit, sizeof(e->label));
                    }
                    sqlite3_finalize(st);
                }
                break;
        }
    }
    mutex_unlock(&s_jf_db_mutex);

    gui_synclist_set_title(&p->lists, lvl->title, Icon_NOICON);
    gui_synclist_set_nb_items(&p->lists, p->count);
    int sel = lvl->saved_sel;
    if (sel < 0 || sel >= p->count)
        sel = 0;
    gui_synclist_select_item(&p->lists, sel);
}

static struct jf_browser_page *s_jf_slide_page;
static void jf_browser_slide_render(void *ctx)
{
    (void)ctx;
    jf_browser_load_level(s_jf_slide_page);
    gui_synclist_draw(&s_jf_slide_page->lists);
}

static void jf_browser_slide(struct jf_browser_page *p, enum trimpod_transition_dir dir)
{
    s_jf_slide_page = p;
    trimpod_transition_animate(dir, jf_browser_slide_render, NULL);
}

static const char *jf_browser_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    (void)buf; (void)buf_len;
    struct jf_browser_page *p = (struct jf_browser_page *)data;
    if (sel < 0 || sel >= p->count)
        return "";
    return p->entries[sel].label;
}

static const struct trimpod_svg_icon *jf_browser_get_svg(int sel, void *data)
{
    struct jf_browser_page *p = (struct jf_browser_page *)data;
    if (sel < 0 || sel >= p->count)
        return NULL;
    const struct jf_browse_entry *e = &p->entries[sel];
    if (e->kind == JF_ENT_VIDEO || e->kind == JF_ENT_AUDIO_TRACK)
        return trimpod_svg_for_path(e->aux, false);
    return trimpod_svg_for_path(e->label, true);
}

static bool jf_browser_get_chevron(int sel, void *data)
{
    struct jf_browser_page *p = (struct jf_browser_page *)data;
    return sel >= 0 && sel < p->count;
}

struct jf_str_vec
{
    char **v;
    int    n;
    int    cap;
};

static void jf_str_vec_push(struct jf_str_vec *vec, const char *s)
{
    if (!s || !s[0] || vec->n >= 4096)
        return;
    if (vec->n == vec->cap)
    {
        int ncap = vec->cap ? vec->cap * 2 : 32;
        char **nv = realloc(vec->v, (size_t)ncap * sizeof(*nv));
        if (!nv)
            return;
        vec->v = nv;
        vec->cap = ncap;
    }
    char *dup = strdup(s);
    if (dup)
        vec->v[vec->n++] = dup;
}

static void jf_str_vec_free(struct jf_str_vec *vec)
{
    for (int i = 0; i < vec->n; i++)
        free(vec->v[i]);
    free(vec->v);
    vec->v = NULL;
    vec->n = vec->cap = 0;
}

static void jf_collect_video_ids_for_entry(const struct jf_browse_entry *e,
                                           struct jf_str_vec *vec)
{
    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_stmt *st = NULL;
        if (e->kind == JF_ENT_VIEW)
        {
            if (sqlite3_prepare_v2(
                    s_jf_db,
                    "SELECT id FROM jf_videos WHERE view_id=?1 AND item_type IN (?2,?3,?4)"
                    " ORDER BY series_name, season_number, episode_number, name;",
                    -1, &st, NULL) == SQLITE_OK)
            {
                sqlite3_bind_text(st, 1, e->id, -1, SQLITE_STATIC);
                sqlite3_bind_int (st, 2, TP_JF_ITEM_MOVIE);
                sqlite3_bind_int (st, 3, TP_JF_ITEM_EPISODE);
                sqlite3_bind_int (st, 4, TP_JF_ITEM_VIDEO);
            }
        }
        else if (e->kind == JF_ENT_SERIES)
        {
            if (sqlite3_prepare_v2(
                    s_jf_db,
                    "SELECT id FROM jf_videos WHERE series_id=?1 AND item_type=?2"
                    " GROUP BY id ORDER BY season_number, episode_number, name;",
                    -1, &st, NULL) == SQLITE_OK)
            {
                sqlite3_bind_text(st, 1, e->id, -1, SQLITE_STATIC);
                sqlite3_bind_int (st, 2, TP_JF_ITEM_EPISODE);
            }
        }
        else if (e->kind == JF_ENT_SEASON)
        {
            if (sqlite3_prepare_v2(
                    s_jf_db,
                    "SELECT id FROM jf_videos WHERE item_type=?1"
                    " AND (season_id=?2 OR (COALESCE(season_id,'')='' AND series_id=?3 AND season_number=?4))"
                    " GROUP BY id ORDER BY episode_number, name;",
                    -1, &st, NULL) == SQLITE_OK)
            {
                sqlite3_bind_int (st, 1, TP_JF_ITEM_EPISODE);
                sqlite3_bind_text(st, 2, e->id, -1, SQLITE_STATIC);
                sqlite3_bind_text(st, 3, e->aux, -1, SQLITE_STATIC);
                sqlite3_bind_int (st, 4, e->num);
            }
        }
        if (st)
        {
            while (sqlite3_step(st) == SQLITE_ROW)
                jf_str_vec_push(vec, (const char *)sqlite3_column_text(st, 0));
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_jf_db_mutex);
}

static void jf_collect_audio_paths_for_entry(const struct jf_browse_entry *e,
                                             struct jf_str_vec *vec)
{
    mutex_lock(&s_jf_db_mutex);
    if (s_jf_db)
    {
        sqlite3_stmt *st = NULL;
        if (e->kind == JF_ENT_VIEW)
        {
            sqlite3_prepare_v2(
                s_jf_db,
                "SELECT path FROM tracks ORDER BY sort_artist, album, discnum, tracknum;",
                -1, &st, NULL);
        }
        else if (e->kind == JF_ENT_AUDIO_ARTIST)
        {
            if (sqlite3_prepare_v2(
                    s_jf_db,
                    "SELECT path FROM tracks"
                    " WHERE COALESCE(NULLIF(browse_artist, ''), 'Unknown Artist')=?1"
                    " ORDER BY year, album, discnum, tracknum;",
                    -1, &st, NULL) == SQLITE_OK)
            {
                sqlite3_bind_text(st, 1, e->aux, -1, SQLITE_STATIC);
            }
        }
        else if (e->kind == JF_ENT_AUDIO_ALBUM)
        {
            if (sqlite3_prepare_v2(
                    s_jf_db,
                    "SELECT path FROM tracks"
                    " WHERE COALESCE(NULLIF(browse_artist, ''), 'Unknown Artist')=?1"
                    "   AND COALESCE(NULLIF(album, ''), 'Unknown Album')=?2"
                    " ORDER BY discnum, tracknum, title;",
                    -1, &st, NULL) == SQLITE_OK)
            {
                sqlite3_bind_text(st, 1, e->aux, -1, SQLITE_STATIC);
                sqlite3_bind_text(st, 2, e->id, -1, SQLITE_STATIC);
            }
        }
        if (st)
        {
            while (sqlite3_step(st) == SQLITE_ROW)
                jf_str_vec_push(vec, (const char *)sqlite3_column_text(st, 0));
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_jf_db_mutex);
}

static void jf_handle_video_context(const struct jf_browse_entry *e)
{
    if (e->kind == JF_ENT_VIDEO)
    {
        struct jf_video_row row;
        if (!jf_load_video_row(e->id, &row))
            return;
        char dl_path[MAX_PATH];
        bool is_downloaded = row.local_path[0] &&
            tp_jellyfin_resolve_downloaded_video(row.local_path, dl_path, sizeof(dl_path)) > 0;

        const char *items[3] = {
            ID2P(LANG_PLAY),
            ID2P(LANG_TRIMPOD_PLAY_FROM_START),
            is_downloaded ? ID2P(LANG_TRIMPOD_DELETE_DOWNLOAD)
                          : ID2P(LANG_TRIMPOD_DOWNLOAD),
        };
        int choice = trimpod_context_menu(e->label, items, 3);
        if (choice == 0)
            jf_play_video_by_id(e->id, false);
        else if (choice == 1)
            jf_play_video_by_id(e->id, true);
        else if (choice == 2)
        {
            if (is_downloaded)
            {
                if (tp_jellyfin_delete_downloaded_video(row.local_path))
                    splash(HZ, ID2P(LANG_TRIMPOD_DELETED));
            }
            else
            {
                char *arr[1] = { (char *)e->id };
                jf_download_video_ids(arr, 1);
            }
        }
        return;
    }

    /* Series / Season / Video View batch download */
    const char *items[1] = { ID2P(LANG_TRIMPOD_DOWNLOAD) };
    if (trimpod_context_menu(e->label, items, 1) == 0)
    {
        struct jf_str_vec vec = {0};
        jf_collect_video_ids_for_entry(e, &vec);
        if (vec.n > 0)
            jf_download_video_ids(vec.v, vec.n);
        jf_str_vec_free(&vec);
    }
}

static void jf_browser_draw(struct trimpod_page *self)
{
    gui_synclist_draw(&((struct jf_browser_page *)self)->lists);
}

static int jf_browser_poll(struct trimpod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct jf_browser_page *)self)->lists, &action);
    return action;
}

static enum trimpod_page_result jf_browser_on_action(struct trimpod_page *self, int action)
{
    struct jf_browser_page *p = (struct jf_browser_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);
    bool have_sel = (sel >= 0 && sel < p->count);

    switch (action)
    {
        case ACTION_STD_OK:
        {
            if (!have_sel)
                return TRIMPOD_PAGE_STAY;
            p->stack[p->depth - 1].saved_sel = sel;
            struct jf_browse_entry cur = p->entries[sel];

            if (cur.kind == JF_ENT_VIDEO)
            {
                jf_play_video_by_id(cur.id, false);
                if (trimpod_home_pending)
                {
                    trimpod_home_pending = false;
                    p->base.home = true;
                    return TRIMPOD_PAGE_DONE;
                }
                return TRIMPOD_PAGE_STAY;
            }
            if (cur.kind == JF_ENT_AUDIO_TRACK)
            {
                if (trimpod_play_track(cur.aux))
                {
                    p->result = GO_TO_WPS;
                    return TRIMPOD_PAGE_DONE;
                }
                return TRIMPOD_PAGE_STAY;
            }

            if (p->depth >= JF_MAX_STACK)
                return TRIMPOD_PAGE_STAY;

            struct jf_nav_level *next = &p->stack[p->depth];
            memset(next, 0, sizeof(*next));
            strlcpy(next->title, cur.label, sizeof(next->title));

            switch (cur.kind)
            {
                case JF_ENT_VIEW:
                    if (cur.view_kind == 2)
                        next->kind = JF_LVL_AUDIO_ARTISTS;
                    else
                    {
                        next->kind = JF_LVL_VIEW_ITEMS;
                        strlcpy(next->id, cur.id, sizeof(next->id));
                    }
                    break;
                case JF_ENT_SERIES:
                    next->kind = JF_LVL_SERIES_SEASONS;
                    strlcpy(next->id, cur.id, sizeof(next->id));
                    break;
                case JF_ENT_SEASON:
                    next->kind = JF_LVL_SEASON_EPISODES;
                    strlcpy(next->id,  cur.id, sizeof(next->id));
                    strlcpy(next->aux, cur.aux, sizeof(next->aux));
                    next->num = cur.num;
                    break;
                case JF_ENT_AUDIO_ARTIST:
                    next->kind = JF_LVL_AUDIO_ALBUMS;
                    strlcpy(next->aux, cur.aux, sizeof(next->aux));
                    break;
                case JF_ENT_AUDIO_ALBUM:
                    next->kind = JF_LVL_AUDIO_TRACKS;
                    strlcpy(next->id,  cur.id, sizeof(next->id));
                    strlcpy(next->aux, cur.aux, sizeof(next->aux));
                    break;
                default:
                    return TRIMPOD_PAGE_STAY;
            }

            p->depth++;
            jf_browser_slide(p, TRIMPOD_TRANS_FORWARD);
            return TRIMPOD_PAGE_STAY;
        }

        case ACTION_STD_CONTEXT:
        {
            if (!have_sel)
                return TRIMPOD_PAGE_STAY;
            p->stack[p->depth - 1].saved_sel = sel;
            struct jf_browse_entry cur = p->entries[sel];

            if (cur.kind == JF_ENT_AUDIO_TRACK)
            {
                if (trimpod_music_context(cur.label, cur.aux, FILE_ATTR_AUDIO))
                {
                    p->result = GO_TO_WPS;
                    return TRIMPOD_PAGE_DONE;
                }
                return TRIMPOD_PAGE_STAY;
            }
            if (cur.kind == JF_ENT_AUDIO_ARTIST ||
                cur.kind == JF_ENT_AUDIO_ALBUM ||
                (cur.kind == JF_ENT_VIEW && cur.view_kind == 2))
            {
                struct jf_str_vec vec = {0};
                jf_collect_audio_paths_for_entry(&cur, &vec);
                bool started = trimpod_music_context_tracks(cur.label, vec.v, vec.n);
                jf_str_vec_free(&vec);
                if (started)
                {
                    p->result = GO_TO_WPS;
                    return TRIMPOD_PAGE_DONE;
                }
                return TRIMPOD_PAGE_STAY;
            }

            jf_handle_video_context(&cur);
            if (trimpod_home_pending)
            {
                trimpod_home_pending = false;
                p->base.home = true;
                return TRIMPOD_PAGE_DONE;
            }
            return TRIMPOD_PAGE_STAY;
        }

        case ACTION_STD_CANCEL:
            if (p->depth <= 1)
            {
                trimpod_transition_arm_back();
                return TRIMPOD_PAGE_DONE;
            }
            p->depth--;
            jf_browser_slide(p, TRIMPOD_TRANS_BACK);
            return TRIMPOD_PAGE_STAY;
    }

    return TRIMPOD_PAGE_STAY;
}

static const struct trimpod_page_vtable jf_browser_vtable = {
    .legend    = NULL,
    .draw      = jf_browser_draw,
    .poll      = jf_browser_poll,
    .on_action = jf_browser_on_action,
};

static const int jf_browser_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL, ACTION_STD_CONTEXT, -1
};

static int jf_run_browser(enum jf_browse_mode mode)
{
    if (!trimpod_jellyfin_has_media())
    {
        if (!tp_jellyfin_is_configured())
        {
            trimpod_jellyfin_settings_page(false);
            if (!trimpod_jellyfin_has_media())
                return GO_TO_ROOT;
        }
        else if (!trimpod_jellyfin_sync(true))
        {
            return GO_TO_ROOT;
        }
    }

    struct jf_browser_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt               = &jf_browser_vtable;
    p.base.context          = CONTEXT_LIST;
    p.base.allowed          = jf_browser_allowed;
    p.base.no_arm_back      = true;
    p.base.enter_honor_back = true;
    p.mode                  = mode;
    p.result                = GO_TO_ROOT;
    p.depth                 = 1;

    if (mode == JF_MODE_TV)
    {
        p.stack[0].kind = JF_LVL_ALL_SERIES;
        strlcpy(p.stack[0].title, str(LANG_TRIMPOD_TELEVISION), sizeof(p.stack[0].title));
    }
    else if (mode == JF_MODE_VIDEOS)
    {
        /* If there is exactly 1 video library on the server, open directly inside it! */
        char single_id[128] = "";
        char single_name[128] = "";
        int video_view_count = 0;
        mutex_lock(&s_jf_db_mutex);
        if (s_jf_db)
        {
            sqlite3_stmt *st = NULL;
            if (sqlite3_prepare_v2(
                    s_jf_db,
                    "SELECT id, name FROM jf_views WHERE view_kind != 2 ORDER BY ord LIMIT 2;",
                    -1, &st, NULL) == SQLITE_OK)
            {
                while (sqlite3_step(st) == SQLITE_ROW)
                {
                    if (video_view_count == 0)
                    {
                        strlcpy(single_id,   (const char *)sqlite3_column_text(st, 0), sizeof(single_id));
                        strlcpy(single_name, (const char *)sqlite3_column_text(st, 1), sizeof(single_name));
                    }
                    video_view_count++;
                }
                sqlite3_finalize(st);
            }
        }
        mutex_unlock(&s_jf_db_mutex);

        if (video_view_count == 1 && single_id[0])
        {
            p.stack[0].kind = JF_LVL_VIEW_ITEMS;
            strlcpy(p.stack[0].id,    single_id,   sizeof(p.stack[0].id));
            strlcpy(p.stack[0].title, single_name, sizeof(p.stack[0].title));
        }
        else
        {
            p.stack[0].kind = JF_LVL_VIEWS;
            strlcpy(p.stack[0].title, str(LANG_TRIMPOD_MOVIES), sizeof(p.stack[0].title));
        }
    }
    else
    {
        p.stack[0].kind = JF_LVL_VIEWS;
        strlcpy(p.stack[0].title, str(LANG_TRIMPOD_FILES), sizeof(p.stack[0].title));
    }

    gui_synclist_init(&p.lists, jf_browser_get_name, &p, false, 1, NULL);
    gui_synclist_set_svg_icon_callback(&p.lists, jf_browser_get_svg);
    gui_synclist_set_chevron_callback(&p.lists, jf_browser_get_chevron);
    jf_browser_load_level(&p);

    push_current_activity(ACTIVITY_FILEBROWSER);
    trimpod_page_run(&p.base);
    pop_current_activity();

    jf_browser_clear_entries(&p);
    return p.base.home ? GO_TO_ROOT : p.result;
}

int trimpod_jellyfin_videos_browse(bool tv_only)
{
    return jf_run_browser(tv_only ? JF_MODE_TV : JF_MODE_VIDEOS);
}

int trimpod_jellyfin_files_browse(void)
{
    return jf_run_browser(JF_MODE_FILES);
}
