/***************************************************************************
 * UIPod: Navidrome / Subsonic integration bridge.
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
#include "system.h"

#include "uipod_folders.h"
#include "uipod_downloads.h"
#include "uipod_keyboard.h"
#include "uipod_library.h"
#include "uipod_navidrome.h"
#include "uipod_page.h"
#include "uipod_playlists.h"
#include "uipod_rs_navidrome.h"
#include "uipod_rs_ui.h"
#include "uipod_svg.h"
#include "uipod_transition.h"
#include "uipod_ui.h"

#ifdef RETRO_HANDHELD
#include "uipod-http-target.h"
#endif

#define UIPOD_SOURCE_FILE   ROCKBOX_DIR "/source.txt"
#define NAV_CFG_RB            ROCKBOX_DIR "/navidrome.cfg"
#define NAV_CFG_SD_1          "/mnt/SDCARD/navidrome.cfg"
#define NAV_CFG_SD_2          "/mnt/SDCARD/Tools/tg5040/UIPod.pak/navidrome.cfg"

#define NAV_DB_SD             ROCKBOX_DIR "/navidrome.db"
#define NAV_DB_RAM            "/tmp/uipod-navidrome.db"
#define NAV_PLAYLISTS_DIR     ROCKBOX_DIR "/Playlists_Navidrome"
#define NAV_FAV_DIR           ROCKBOX_DIR "/Playlists_Navidrome_Fav"
#define NAV_FAVOURITES_M3U8   NAV_FAV_DIR "/Favourites.m3u8"
#define NAV_FAV_SENTINEL      "\x01" "FAVOURITES"
#define NAV_COPY_BUF          (256 * 1024)
#define NAV_SCHEMA_VERSION    2

static enum uipod_source_kind s_source_kind;
static bool         s_db_in_ram;
static sqlite3     *s_nav_db;
static struct mutex s_nav_db_mutex;

/* ---- Source type state (`ROCKBOX_DIR "/source.txt"`) ------------------ */

static void source_load_from_disk(void)
{
    s_source_kind = UIPOD_SOURCE_LOCAL;
    int fd = open(UIPOD_SOURCE_FILE, O_RDONLY);
    if (fd < 0)
        return;
    char buf[32];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n > 0)
    {
        buf[n] = '\0';
        if (strncasecmp(buf, "navidrome", 9) == 0)
            s_source_kind = UIPOD_SOURCE_NAVIDROME;
        else if (strncasecmp(buf, "jellyfin", 8) == 0)
            s_source_kind = UIPOD_SOURCE_JELLYFIN;
    }
}

bool uipod_source_is_configured(void)
{
    return file_exists(UIPOD_SOURCE_FILE);
}

enum uipod_source_kind uipod_source_get(void)
{
    return s_source_kind;
}

void uipod_source_set(enum uipod_source_kind kind)
{
    s_source_kind = kind;
    int fd = open(UIPOD_SOURCE_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd >= 0)
    {
        const char *val = "local\n";
        if (kind == UIPOD_SOURCE_NAVIDROME)
            val = "navidrome\n";
        else if (kind == UIPOD_SOURCE_JELLYFIN)
            val = "jellyfin\n";
        write(fd, val, strlen(val));
        close(fd);
    }
}

bool uipod_source_is_navidrome(void)
{
    return s_source_kind == UIPOD_SOURCE_NAVIDROME;
}

bool uipod_source_is_jellyfin(void)
{
    return s_source_kind == UIPOD_SOURCE_JELLYFIN;
}

void uipod_source_set_navidrome(bool navidrome)
{
    uipod_source_set(navidrome ? UIPOD_SOURCE_NAVIDROME : UIPOD_SOURCE_LOCAL);
}

/* ---- Config file persistence ------------------------------------------ */

static bool read_text_file(const char *rb_path, char *buf, size_t cap)
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

static bool save_config_to_disk(void)
{
    char ini[2048];
    size_t n = tp_navidrome_format_config_text(ini, sizeof(ini));
    if (n == 0)
        return false;
    int fd = open(NAV_CFG_RB ".new", O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0)
        return false;
    bool ok = (write(fd, ini, n) == (ssize_t)n);
    close(fd);
    if (ok)
        rename(NAV_CFG_RB ".new", NAV_CFG_RB);
    return ok;
}

static void load_or_import_config(void)
{
    char ini[4096];
    if (read_text_file(NAV_CFG_RB, ini, sizeof(ini)))
    {
        tp_navidrome_load_config_text(ini);
        /* Also allow an updated user-dropped `/mnt/SDCARD/navidrome.cfg` to override
         * if `NAV_CFG_RB` was blank/unconfigured. */
        if (tp_navidrome_is_configured())
            return;
    }

    if (read_text_file(NAV_CFG_SD_1, ini, sizeof(ini)) ||
        read_text_file(NAV_CFG_SD_2, ini, sizeof(ini)))
    {
        if (tp_navidrome_load_config_text(ini) && tp_navidrome_is_configured())
            save_config_to_disk();
    }
}

/* ---- SQLite DB (`/tmp/uipod-navidrome.db` <-> `navidrome.db`) -------- */

static const char NAV_SCHEMA_SQL[] =
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
    "CREATE INDEX IF NOT EXISTS idx_nav_artist ON tracks(category, sort_artist, album, discnum, tracknum);"
    "CREATE INDEX IF NOT EXISTS idx_nav_album  ON tracks(category, album, discnum, tracknum);"
    "CREATE INDEX IF NOT EXISTS idx_nav_browse ON tracks(category, browse_artist, album, discnum, tracknum);"
    "CREATE INDEX IF NOT EXISTS idx_nav_title  ON tracks(category, title, path);"
    "CREATE INDEX IF NOT EXISTS idx_nav_genre  ON tracks(category, genre);";

static bool nav_copy_file(const char *src, const char *dst)
{
    bool ok = false;
    int in = open(src, O_RDONLY);
    if (in < 0)
        return false;
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    char *buf = malloc(NAV_COPY_BUF);
    if (out >= 0 && buf)
    {
        ssize_t n;
        while ((n = read(in, buf, NAV_COPY_BUF)) > 0)
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

static void nav_db_writeback_locked(void)
{
    if (s_db_in_ram && nav_copy_file(NAV_DB_RAM, NAV_DB_SD ".new"))
        rename(NAV_DB_SD ".new", NAV_DB_SD);
}

static int nav_db_exec(const char *sql)
{
    return sqlite3_exec(s_nav_db, sql, NULL, NULL, NULL);
}

static void make_sort_artist(const char *ba, char *out, size_t n)
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

static void bind_opt_str(sqlite3_stmt *st, int idx, const char *s)
{
    if (s && s[0])
        sqlite3_bind_text(st, idx, s, -1, SQLITE_TRANSIENT);
    else
        sqlite3_bind_null(st, idx);
}

static void rebuild_virtual_tree_locked(void)
{
    tp_navidrome_tree_clear();
    if (!s_nav_db)
    {
        tp_navidrome_tree_finalize();
        return;
    }

    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(
            s_nav_db,
            "SELECT COALESCE(server_path, ''), path, COALESCE(title, '') FROM tracks;",
            -1, &st, NULL) == SQLITE_OK)
    {
        while (sqlite3_step(st) == SQLITE_ROW)
        {
            const char *sp   = (const char *)sqlite3_column_text(st, 0);
            const char *path = (const char *)sqlite3_column_text(st, 1);
            const char *tit  = (const char *)sqlite3_column_text(st, 2);
            if (path && path[0])
                tp_navidrome_tree_insert(sp ? sp : "", path, tit ? tit : "");
        }
        sqlite3_finalize(st);
    }
    tp_navidrome_tree_finalize();
}

static bool nav_db_setup_locked(void)
{
    nav_db_exec("PRAGMA journal_mode=TRUNCATE;");
    nav_db_exec("PRAGMA synchronous=NORMAL;");

    int ver = 0;
    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(s_nav_db, "PRAGMA user_version;", -1, &st, NULL) == SQLITE_OK)
    {
        if (sqlite3_step(st) == SQLITE_ROW)
            ver = sqlite3_column_int(st, 0);
        sqlite3_finalize(st);
    }

    if (ver != NAV_SCHEMA_VERSION)
    {
        nav_db_exec("DROP TABLE IF EXISTS tracks;");
        if (nav_db_exec(NAV_SCHEMA_SQL) != SQLITE_OK)
            return false;
        char sql[48];
        snprintf(sql, sizeof(sql), "PRAGMA user_version=%d;", NAV_SCHEMA_VERSION);
        nav_db_exec(sql);
    }
    else
    {
        if (nav_db_exec(NAV_SCHEMA_SQL) != SQLITE_OK)
            return false;
    }
    return true;
}

struct sqlite3 *uipod_navidrome_db_lock(void)
{
    mutex_lock(&s_nav_db_mutex);
    return s_nav_db;
}

void uipod_navidrome_db_unlock(void)
{
    mutex_unlock(&s_nav_db_mutex);
}

bool uipod_navidrome_is_configured(void)
{
    return tp_navidrome_is_configured();
}

const char *uipod_navidrome_favourites_path(void)
{
    return NAV_FAVOURITES_M3U8;
}

bool uipod_navidrome_has_tracks(void)
{
    bool has = false;
    mutex_lock(&s_nav_db_mutex);
    if (s_nav_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(s_nav_db, "SELECT 1 FROM tracks LIMIT 1;", -1, &st, NULL) == SQLITE_OK)
        {
            has = (sqlite3_step(st) == SQLITE_ROW);
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_nav_db_mutex);
    return has;
}

/* ---- HTTP Vtable Wiring ----------------------------------------------- */

#ifndef RETRO_HANDHELD
static int32_t stub_http_get(uint8_t is_https, const char *host, uint16_t port,
                             const char *pq, uint8_t self_signed, int32_t timeout_ms,
                             uint8_t **out_data, size_t *out_len, char *err, size_t cap)
{
    (void)is_https; (void)host; (void)port; (void)pq; (void)self_signed;
    (void)timeout_ms; (void)out_data; (void)out_len;
    snprintf(err, cap, "HTTP unsupported on this target");
    return -1;
}
static void stub_http_free(uint8_t *p) { free(p); }
static int32_t stub_http_dl(uint8_t is_https, const char *host, uint16_t port,
                            const char *pq, uint8_t self_signed, const char *dest,
                            int32_t timeout_ms, tp_nav_cancel_fn ccb, void *cctx,
                            char *err, size_t cap)
{
    (void)is_https; (void)host; (void)port; (void)pq; (void)self_signed;
    (void)dest; (void)timeout_ms; (void)ccb; (void)cctx;
    snprintf(err, cap, "HTTP unsupported on this target");
    return -1;
}
#endif

static const TpNavHttpVtable s_http_vt = {
#ifdef RETRO_HANDHELD
    .http_get           = retrohh_nav_http_get,
    .http_free          = retrohh_nav_http_free,
    .http_download_file = retrohh_nav_http_download_file,
#else
    .http_get           = stub_http_get,
    .http_free          = stub_http_free,
    .http_download_file = stub_http_dl,
#endif
};

static void repath_catalog_for_quality(uint8_t new_quality);

void uipod_navidrome_init(void)
{
    mutex_init(&s_nav_db_mutex);
    source_load_from_disk();

    const char *cache_root = "/tmp/uipod_navidrome_cache";
    struct stat st;
    if (stat("/mnt/SDCARD", &st) == 0 && S_ISDIR(st.st_mode))
        cache_root = "/mnt/SDCARD/Media/navidrome";

    tp_navidrome_init(cache_root, &s_http_vt);
    load_or_import_config();

    if (!dir_exists(NAV_PLAYLISTS_DIR))
        mkdir(NAV_PLAYLISTS_DIR);

    bool need_repath = false;
    char live_root[MAX_PATH];
    live_root[0] = '\0';
    tp_navidrome_get_cache_root(live_root, sizeof(live_root));

    mutex_lock(&s_nav_db_mutex);
    remove(NAV_DB_RAM);
    s_db_in_ram = !file_exists(NAV_DB_SD) || nav_copy_file(NAV_DB_SD, NAV_DB_RAM);
    const char *work = s_db_in_ram ? NAV_DB_RAM : NAV_DB_SD;

    if (sqlite3_open(work, &s_nav_db) != SQLITE_OK || !nav_db_setup_locked())
    {
        sqlite3_close(s_nav_db);
        s_nav_db = NULL;
        remove(NAV_DB_SD);
        remove(NAV_DB_RAM);
        s_db_in_ram = true;
        if (sqlite3_open(NAV_DB_RAM, &s_nav_db) == SQLITE_OK && nav_db_setup_locked())
            nav_db_writeback_locked();
        else
        {
            sqlite3_close(s_nav_db);
            s_nav_db = NULL;
        }
    }

    if (s_nav_db && live_root[0])
    {
        sqlite3_stmt *chk = NULL;
        if (sqlite3_prepare_v2(s_nav_db, "SELECT path FROM tracks LIMIT 1;", -1, &chk, NULL) == SQLITE_OK)
        {
            if (sqlite3_step(chk) == SQLITE_ROW)
            {
                const char *p0 = (const char *)sqlite3_column_text(chk, 0);
                size_t rlen = strlen(live_root);
                if (p0 && (strncmp(p0, live_root, rlen) != 0 ||
                           (p0[rlen] != '/' && p0[rlen] != '\0')))
                {
                    need_repath = true;
                }
            }
            sqlite3_finalize(chk);
        }
    }

    rebuild_virtual_tree_locked();
    mutex_unlock(&s_nav_db_mutex);

    if (need_repath)
    {
        TpNavConfigC cfg;
        if (tp_navidrome_get_config(&cfg))
            repath_catalog_for_quality(cfg.quality);
    }
}

void uipod_navidrome_close(void)
{
    tp_navidrome_shutdown();
    mutex_lock(&s_nav_db_mutex);
    if (s_nav_db)
    {
        sqlite3_close(s_nav_db);
        s_nav_db = NULL;
    }
    remove(NAV_DB_RAM);
    mutex_unlock(&s_nav_db_mutex);
}

/* ---- UI-thread cooperative cancel check ------------------------------- */

struct ui_cancel_state
{
    bool cancelled;
};

static uint8_t ui_cancel_check_cb(void *ctx)
{
    struct ui_cancel_state *st = (struct ui_cancel_state *)ctx;
    if (st->cancelled)
        return 1;
    yield();
    int act = get_action(CONTEXT_STD, TIMEOUT_NOBLOCK);
    if (act == ACTION_STD_CANCEL || act == ACTION_TP_HOME)
    {
        if (act == ACTION_TP_HOME)
            uipod_home_pending = true;
        st->cancelled = true;
        return 1;
    }
    return 0;
}

/* ---- Catalog Sync (`tp_navidrome_sync_catalog` -> `navidrome.db`) ----- */

struct nav_sync_ctx
{
    sqlite3_stmt          *ins_stmt;
    struct ui_cancel_state cancel;
    bool                   cleared_playlists;
};

static void sanitize_playlist_filename(const char *name, char *out, size_t cap)
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

static void sync_on_song(void *vctx, const TpNavSongC *s)
{
    struct nav_sync_ctx *ctx = (struct nav_sync_ctx *)vctx;
    if (!ctx->ins_stmt || !s || !s->id || !s->local_track_path)
        return;

    const char *ba = (s->album_artist && s->album_artist[0]) ? s->album_artist
                   : (s->artist ? s->artist : "");
    char sortbuf[128];
    make_sort_artist(ba, sortbuf, sizeof(sortbuf));

    sqlite3_stmt *st = ctx->ins_stmt;
    sqlite3_bind_text (st,  1, s->id, -1, SQLITE_TRANSIENT);
    sqlite3_bind_text (st,  2, s->local_track_path, -1, SQLITE_TRANSIENT);
    bind_opt_str      (st,  3, s->cover_id);
    bind_opt_str      (st,  4, s->server_path);
    bind_opt_str      (st,  5, s->suffix);
    sqlite3_bind_int  (st,  6, (int)s->bit_depth);
    sqlite3_bind_int  (st,  7, TP_CAT_MUSIC);
    sqlite3_bind_int64(st,  8, (sqlite3_int64)s->size);
    bind_opt_str      (st,  9, s->title);
    bind_opt_str      (st, 10, s->artist);
    bind_opt_str      (st, 11, s->album_artist);
    bind_opt_str      (st, 12, s->album);
    bind_opt_str      (st, 13, s->genre);
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

static void sync_on_playlist(void *vctx, const char *playlist_name,
                             const char *m3u8_content)
{
    struct nav_sync_ctx *ctx = (struct nav_sync_ctx *)vctx;
    (void)ctx;
    if (!playlist_name || !playlist_name[0] || !m3u8_content)
        return;

    if (strcmp(playlist_name, NAV_FAV_SENTINEL) == 0)
    {
        if (!dir_exists(NAV_FAV_DIR))
            mkdir(NAV_FAV_DIR);
        int fd = open(NAV_FAVOURITES_M3U8, O_WRONLY | O_CREAT | O_TRUNC, 0666);
        if (fd >= 0)
        {
            size_t len = strlen(m3u8_content);
            if (len > 0)
                write(fd, m3u8_content, len);
            close(fd);
        }
        return;
    }

    if (!dir_exists(NAV_PLAYLISTS_DIR))
        mkdir(NAV_PLAYLISTS_DIR);

    char safe_name[128];
    sanitize_playlist_filename(playlist_name, safe_name, sizeof(safe_name));

    char path[MAX_PATH];
    snprintf(path, sizeof(path), "%s/%s.m3u8", NAV_PLAYLISTS_DIR, safe_name);

    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd >= 0)
    {
        size_t len = strlen(m3u8_content);
        if (len > 0)
            write(fd, m3u8_content, len);
        close(fd);
    }
}

static void sync_on_progress(void *vctx, uint32_t synced_tracks)
{
    (void)vctx;
    char widest[64], msg[64];
    snprintf(widest, sizeof(widest), "%s 99999", str(LANG_UIPOD_NAV_SYNCING));
    snprintf(msg, sizeof(msg), "%s %u", str(LANG_UIPOD_NAV_SYNCING),
             (unsigned)synced_tracks);
    uipod_fullscreen_message(msg, widest);
    backlight_on();
    yield();
}

static uint8_t sync_cancel_bridge(void *vctx)
{
    struct nav_sync_ctx *ctx = (struct nav_sync_ctx *)vctx;
    return ui_cancel_check_cb(&ctx->cancel);
}

bool uipod_navidrome_sync(bool show_summary_splash)
{
    if (!tp_navidrome_is_configured())
    {
        splash(HZ * 3 / 2, ID2P(LANG_UIPOD_NAV_CONFIG_NEEDED));
        return false;
    }

    uipod_fullscreen_message(str(LANG_UIPOD_NAV_CONNECTING), NULL);
    backlight_on();

    mutex_lock(&s_nav_db_mutex);
    if (!s_nav_db)
    {
        mutex_unlock(&s_nav_db_mutex);
        splash(HZ * 3 / 2, ID2P(LANG_FAILED));
        return false;
    }

    nav_db_exec("BEGIN IMMEDIATE;");
    nav_db_exec("DELETE FROM tracks;");

    static const char INS_SQL[] =
        "INSERT OR REPLACE INTO tracks("
        "song_id,path,cover_id,server_path,suffix,bit_depth,category,size,"
        "title,artist,albumartist,album,genre,year,tracknum,discnum,"
        "duration_ms,bitrate,frequency,browse_artist,sort_artist)"
        " VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21);";

    struct nav_sync_ctx ctx;
    memset(&ctx, 0, sizeof(ctx));
    if (sqlite3_prepare_v2(s_nav_db, INS_SQL, -1, &ctx.ins_stmt, NULL) != SQLITE_OK)
    {
        nav_db_exec("ROLLBACK;");
        mutex_unlock(&s_nav_db_mutex);
        splash(HZ * 3 / 2, ID2P(LANG_FAILED));
        return false;
    }

    char err[160] = "";
    int32_t total = tp_navidrome_sync_catalog(
        sync_on_song,
        sync_on_playlist,
        sync_on_progress,
        sync_cancel_bridge,
        &ctx,
        err,
        sizeof(err)
    );

    sqlite3_finalize(ctx.ins_stmt);

    if (total < 0)
    {
        nav_db_exec("ROLLBACK;");
        mutex_unlock(&s_nav_db_mutex);
        if (!ctx.cancel.cancelled)
            splashf(HZ * 2, "%s", err[0] ? err : (const char *)str(LANG_FAILED));
        return false;
    }

    nav_db_exec("COMMIT;");
    rebuild_virtual_tree_locked();
    nav_db_writeback_locked();
    mutex_unlock(&s_nav_db_mutex);

    uipod_library_bump_remote_gen();

    if (show_summary_splash)
        splashf(HZ * 3 / 2, str(LANG_UIPOD_NAV_SYNCED_FMT), (int)total);
    return true;
}

bool uipod_navidrome_sync_favourites(bool show_summary_splash)
{
    if (!tp_navidrome_is_configured() || uipod_offline_mode_get())
        return false;

    if (show_summary_splash)
    {
        uipod_fullscreen_message(str(LANG_UIPOD_NAV_CONNECTING), NULL);
        backlight_on();
    }

    mutex_lock(&s_nav_db_mutex);
    if (!s_nav_db)
    {
        mutex_unlock(&s_nav_db_mutex);
        return false;
    }

    int before_changes = sqlite3_total_changes(s_nav_db);
    nav_db_exec("BEGIN IMMEDIATE;");

    static const char INS_IGNORE_SQL[] =
        "INSERT OR IGNORE INTO tracks("
        "song_id,path,cover_id,server_path,suffix,bit_depth,category,size,"
        "title,artist,albumartist,album,genre,year,tracknum,discnum,"
        "duration_ms,bitrate,frequency,browse_artist,sort_artist)"
        " VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21);";

    struct nav_sync_ctx ctx;
    memset(&ctx, 0, sizeof(ctx));
    sqlite3_prepare_v2(s_nav_db, INS_IGNORE_SQL, -1, &ctx.ins_stmt, NULL);

    char err[160] = "";
    int32_t fav_cnt = tp_navidrome_sync_starred(
        sync_on_song,
        sync_on_playlist,
        show_summary_splash ? sync_cancel_bridge : NULL,
        &ctx,
        err,
        sizeof(err));

    sqlite3_finalize(ctx.ins_stmt);

    if (fav_cnt < 0)
    {
        nav_db_exec("ROLLBACK;");
        mutex_unlock(&s_nav_db_mutex);
        if (show_summary_splash && !ctx.cancel.cancelled)
            splashf(HZ * 3 / 2, "%s", err[0] ? err : (const char *)str(LANG_FAILED));
        return false;
    }

    nav_db_exec("COMMIT;");
    bool db_changed = (sqlite3_total_changes(s_nav_db) != before_changes);
    if (db_changed)
    {
        rebuild_virtual_tree_locked();
        nav_db_writeback_locked();
    }
    mutex_unlock(&s_nav_db_mutex);

    if (db_changed)
        uipod_library_bump_remote_gen();

    if (show_summary_splash)
        splashf(HZ, str(LANG_UIPOD_NAV_SYNCED_FMT), (int)fav_cnt);
    return true;
}

/* ---- Fast Offline Re-Pathing When Switching Audio Quality ------------- */

static void rewrite_single_m3u8_locked(const char *pl_path,
                                       sqlite3_stmt *lookup_stmt)
{
    char tmp_path[MAX_PATH];
    snprintf(tmp_path, sizeof(tmp_path), "%s.tmp", pl_path);

    int in = open(pl_path, O_RDONLY);
    if (in < 0)
        return;
    off_t fsz = filesize(in);
    if (fsz <= 0 || fsz > 2 * 1024 * 1024)
    {
        close(in);
        return;
    }
    char *content = malloc((size_t)fsz + 1);
    if (!content)
    {
        close(in);
        return;
    }
    ssize_t n = read(in, content, (size_t)fsz);
    close(in);
    if (n <= 0)
    {
        free(content);
        return;
    }
    content[n] = '\0';

    int out = open(tmp_path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (out < 0)
    {
        free(content);
        return;
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

static void rewrite_nav_playlists_paths_locked(sqlite3_stmt *lookup_stmt)
{
    if (file_exists(NAV_FAVOURITES_M3U8))
        rewrite_single_m3u8_locked(NAV_FAVOURITES_M3U8, lookup_stmt);

    DIR *d = opendir(NAV_PLAYLISTS_DIR);
    if (!d)
        return;

    struct dirent *e;
    while ((e = readdir(d)) != NULL)
    {
        const char *dot = strrchr(e->d_name, '.');
        if (!dot || (strcasecmp(dot, ".m3u8") != 0 && strcasecmp(dot, ".m3u") != 0))
            continue;

        char pl_path[MAX_PATH];
        snprintf(pl_path, sizeof(pl_path), "%s/%s", NAV_PLAYLISTS_DIR, e->d_name);
        rewrite_single_m3u8_locked(pl_path, lookup_stmt);
    }
    closedir(d);
}

static void repath_catalog_for_quality(uint8_t new_quality)
{
    mutex_lock(&s_nav_db_mutex);
    if (!s_nav_db)
    {
        mutex_unlock(&s_nav_db_mutex);
        return;
    }

    nav_db_exec("BEGIN IMMEDIATE;");
    nav_db_exec("DROP TABLE IF EXISTS temp.nav_repath;");
    nav_db_exec("CREATE TEMP TABLE nav_repath("
                "id INTEGER PRIMARY KEY, old_path TEXT UNIQUE, new_path TEXT NOT NULL UNIQUE);");

    sqlite3_stmt *sel = NULL, *ins = NULL, *chk = NULL;
    if (sqlite3_prepare_v2(
            s_nav_db,
            "SELECT id, song_id, COALESCE(server_path, ''), COALESCE(browse_artist, ''),"
            " COALESCE(album, ''), tracknum, COALESCE(title, ''), COALESCE(suffix, ''), path"
            " FROM tracks;",
            -1, &sel, NULL) == SQLITE_OK &&
        sqlite3_prepare_v2(
            s_nav_db,
            "INSERT INTO temp.nav_repath(id, old_path, new_path) VALUES(?1, ?2, ?3);",
            -1, &ins, NULL) == SQLITE_OK &&
        sqlite3_prepare_v2(
            s_nav_db,
            "SELECT 1 FROM temp.nav_repath WHERE new_path = ?1;",
            -1, &chk, NULL) == SQLITE_OK)
    {
        while (sqlite3_step(sel) == SQLITE_ROW)
        {
            int id           = sqlite3_column_int(sel, 0);
            const char *sid  = (const char *)sqlite3_column_text(sel, 1);
            const char *sp   = (const char *)sqlite3_column_text(sel, 2);
            const char *bart = (const char *)sqlite3_column_text(sel, 3);
            const char *alb  = (const char *)sqlite3_column_text(sel, 4);
            int trk          = sqlite3_column_int(sel, 5);
            const char *tit  = (const char *)sqlite3_column_text(sel, 6);
            const char *sfx  = (const char *)sqlite3_column_text(sel, 7);
            const char *oldp = (const char *)sqlite3_column_text(sel, 8);
            if (!sid || !oldp)
                continue;
            const char *use_sfx = (sfx && sfx[0]) ? sfx : "mp3";
            char newp[MAX_PATH];
            if (tp_navidrome_track_path_for(sp ? sp : "",
                                            bart ? bart : "",
                                            alb ? alb : "",
                                            trk,
                                            tit ? tit : "",
                                            sid,
                                            use_sfx,
                                            new_quality,
                                            newp, sizeof(newp)) > 0)
            {
                sqlite3_bind_text(chk, 1, newp, -1, SQLITE_STATIC);
                bool collision = (sqlite3_step(chk) == SQLITE_ROW);
                sqlite3_reset(chk);
                if (collision)
                {
                    char disamb[MAX_PATH];
                    if (tp_navidrome_disambiguate_track_path(newp, sid, use_sfx,
                                                             new_quality,
                                                             disamb, sizeof(disamb)) > 0)
                    {
                        strlcpy(newp, disamb, sizeof(newp));
                    }
                }

                sqlite3_bind_int (ins, 1, id);
                sqlite3_bind_text(ins, 2, oldp, -1, SQLITE_TRANSIENT);
                sqlite3_bind_text(ins, 3, newp, -1, SQLITE_TRANSIENT);
                sqlite3_step(ins);
                sqlite3_reset(ins);
            }
        }
    }
    sqlite3_finalize(sel);
    sqlite3_finalize(ins);
    sqlite3_finalize(chk);

    nav_db_exec("UPDATE tracks SET path = ("
                "SELECT new_path FROM temp.nav_repath WHERE temp.nav_repath.id = tracks.id)"
                " WHERE id IN (SELECT id FROM temp.nav_repath);");

    sqlite3_stmt *lookup = NULL;
    if (sqlite3_prepare_v2(s_nav_db,
                           "SELECT new_path FROM temp.nav_repath WHERE old_path = ?1;",
                           -1, &lookup, NULL) == SQLITE_OK)
    {
        rewrite_nav_playlists_paths_locked(lookup);
        sqlite3_finalize(lookup);
    }

    nav_db_exec("DROP TABLE IF EXISTS temp.nav_repath;");
    nav_db_exec("COMMIT;");

    rebuild_virtual_tree_locked();
    nav_db_writeback_locked();
    mutex_unlock(&s_nav_db_mutex);
    uipod_library_bump_remote_gen();
}

/* ---- Playback, Metadata, Cover Art & Scrobble Hooks ------------------- */

bool uipod_navidrome_is_track_path(const char *path)
{
    return path && path[0] && tp_navidrome_is_track_path(path);
}

static bool lookup_song_ids_by_path(const char *path,
                                    char *song_id_out, size_t song_cap,
                                    char *cover_id_out, size_t cover_cap,
                                    int *bit_depth_out)
{
    bool found = false;
    if (song_id_out && song_cap > 0)
        song_id_out[0] = '\0';
    if (cover_id_out && cover_cap > 0)
        cover_id_out[0] = '\0';
    if (bit_depth_out)
        *bit_depth_out = 0;

    mutex_lock(&s_nav_db_mutex);
    if (s_nav_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(
                s_nav_db,
                "SELECT song_id, COALESCE(cover_id, ''), bit_depth FROM tracks WHERE path=?1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
            if (sqlite3_step(st) == SQLITE_ROW)
            {
                const char *sid = (const char *)sqlite3_column_text(st, 0);
                const char *cid = (const char *)sqlite3_column_text(st, 1);
                if (sid && song_id_out)
                    strlcpy(song_id_out, sid, song_cap);
                if (cid && cover_id_out)
                    strlcpy(cover_id_out, cid, cover_cap);
                if (bit_depth_out)
                    *bit_depth_out = sqlite3_column_int(st, 2);
                found = (sid && sid[0]);
            }
            sqlite3_finalize(st);
        }
    }
    mutex_unlock(&s_nav_db_mutex);
    return found;
}

bool uipod_navidrome_prepare_track(const char *path, bool ui_blocking)
{
    if (!uipod_navidrome_is_track_path(path))
        return true;

    /* Fast path: already in the persistent SD cache! */
    if (file_exists(path))
        return true;

    if (uipod_offline_mode_get())
    {
        if (ui_blocking)
            splash(HZ, ID2P(LANG_UIPOD_OFFLINE_MODE));
        return false;
    }

    char song_id[128], cover_id[128];
    if (!lookup_song_ids_by_path(path, song_id, sizeof(song_id),
                                 cover_id, sizeof(cover_id), NULL))
        return false;

    if (ui_blocking)
    {
        uipod_fullscreen_message(str(LANG_UIPOD_NAV_BUFFERING), NULL);
        backlight_on();
    }

    struct ui_cancel_state cancel = { .cancelled = false };
    char err[160] = "";
    int32_t rc = tp_navidrome_ensure_track_cached(
        path,
        song_id,
        cover_id,
        ui_blocking ? ui_cancel_check_cb : NULL,
        ui_blocking ? &cancel : NULL,
        err,
        sizeof(err)
    );

    if (rc < 0)
    {
        if (ui_blocking && !cancel.cancelled)
            splashf(HZ * 3 / 2, "%s", err[0] ? err : (const char *)str(LANG_FAILED));
        return false;
    }
    return file_exists(path);
}

bool uipod_navidrome_download_tracks(char *const *paths, int count)
{
    if (!paths || count <= 0)
        return false;

    if (!tp_navidrome_is_configured())
    {
        splash(HZ * 3 / 2, ID2P(LANG_UIPOD_NAV_CONFIG_NEEDED));
        return false;
    }

    int enqueued = 0;
    mutex_lock(&s_nav_db_mutex);
    sqlite3_stmt *st = NULL;
    if (s_nav_db)
    {
        sqlite3_prepare_v2(
            s_nav_db,
            "SELECT song_id, COALESCE(cover_id, ''),"
            " COALESCE(artist, ''), COALESCE(title, '')"
            " FROM tracks WHERE path=?1;",
            -1, &st, NULL);
    }

    for (int i = 0; i < count; i++)
    {
        const char *p = paths[i];
        if (!st || !uipod_navidrome_is_track_path(p))
            continue;

        sqlite3_bind_text(st, 1, p, -1, SQLITE_STATIC);
        if (sqlite3_step(st) == SQLITE_ROW)
        {
            const char *sid = (const char *)sqlite3_column_text(st, 0);
            const char *cid = (const char *)sqlite3_column_text(st, 1);
            const char *art = (const char *)sqlite3_column_text(st, 2);
            const char *tit = (const char *)sqlite3_column_text(st, 3);
            if (sid && sid[0])
            {
                char disp[192];
                if (art && art[0] && tit && tit[0])
                    snprintf(disp, sizeof(disp), "%s - %s", art, tit);
                else if (tit && tit[0])
                    strlcpy(disp, tit, sizeof(disp));
                else
                    disp[0] = '\0';

                if (uipod_downloads_enqueue_nav_track(p, sid, cid ? cid : "", disp))
                    enqueued++;
            }
        }
        sqlite3_reset(st);
    }

    if (st)
        sqlite3_finalize(st);
    mutex_unlock(&s_nav_db_mutex);

    return enqueued > 0;
}

bool uipod_navidrome_download_track(const char *path)
{
    if (!path || !path[0])
        return false;
    char *arr[1] = { (char *)path };
    return uipod_navidrome_download_tracks(arr, 1);
}

static char *append_id3_buf(char *base, size_t cap, size_t *used, const char *src)
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

static size_t current_id3v2buf_used(const struct mp3entry *id3)
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

bool uipod_navidrome_fill_id3(const char *path, struct mp3entry *id3)
{
    if (!path || !id3 || !uipod_navidrome_is_track_path(path))
        return false;

    wipe_mp3entry(id3);
    strlcpy(id3->path, path, sizeof(id3->path));
    id3->codectype = probe_file_format(path);

    bool ok = false;
    mutex_lock(&s_nav_db_mutex);
    if (s_nav_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(
                s_nav_db,
                "SELECT title, artist, albumartist, album, genre,"
                " year, tracknum, discnum, duration_ms, bitrate, frequency, size"
                " FROM tracks WHERE path=?1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
            if (sqlite3_step(st) == SQLITE_ROW)
            {
                size_t used = 0;
                id3->title        = append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                   (const char *)sqlite3_column_text(st, 0));
                id3->artist       = append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                   (const char *)sqlite3_column_text(st, 1));
                id3->albumartist  = append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                   (const char *)sqlite3_column_text(st, 2));
                id3->album        = append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                   (const char *)sqlite3_column_text(st, 3));
                id3->genre_string = append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
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
    mutex_unlock(&s_nav_db_mutex);
    return ok;
}

void uipod_navidrome_enrich_id3(struct mp3entry *id3)
{
    if (!id3 || !uipod_navidrome_is_track_path(id3->path))
        return;

    mutex_lock(&s_nav_db_mutex);
    if (s_nav_db)
    {
        sqlite3_stmt *st = NULL;
        if (sqlite3_prepare_v2(
                s_nav_db,
                "SELECT title, artist, albumartist, album, genre,"
                " year, tracknum, discnum, duration_ms, bitrate, frequency"
                " FROM tracks WHERE path=?1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, id3->path, -1, SQLITE_STATIC);
            if (sqlite3_step(st) == SQLITE_ROW)
            {
                size_t used = current_id3v2buf_used(id3);
                if (!id3->title || !id3->title[0])
                    id3->title = append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                (const char *)sqlite3_column_text(st, 0));
                if (!id3->artist || !id3->artist[0])
                    id3->artist = append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                 (const char *)sqlite3_column_text(st, 1));
                if (!id3->albumartist || !id3->albumartist[0])
                    id3->albumartist = append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                      (const char *)sqlite3_column_text(st, 2));
                if (!id3->album || !id3->album[0])
                    id3->album = append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
                                                (const char *)sqlite3_column_text(st, 3));
                if (!id3->genre_string || !id3->genre_string[0])
                    id3->genre_string = append_id3_buf(id3->id3v2buf, ID3V2_BUF_SIZE, &used,
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
    mutex_unlock(&s_nav_db_mutex);
}

bool uipod_navidrome_get_cover_info(const char *path,
                                      char *cover_path_out,
                                      size_t cover_cap,
                                      int *out_bit_depth)
{
    if (cover_path_out && cover_cap > 0)
        cover_path_out[0] = '\0';
    if (out_bit_depth)
        *out_bit_depth = 0;

    if (!uipod_navidrome_is_track_path(path))
        return false;

    char song_id[128], cover_id[128];
    int bit_depth = 0;
    if (!lookup_song_ids_by_path(path, song_id, sizeof(song_id),
                                 cover_id, sizeof(cover_id), &bit_depth))
        return false;

    TpNavConfigC cfg;
    if (out_bit_depth && tp_navidrome_get_config(&cfg) &&
        cfg.quality == TP_NAV_QUALITY_ORIGINAL)
    {
        *out_bit_depth = bit_depth;
    }

    if (!cover_id[0] || !cover_path_out || cover_cap == 0)
        return false;

    if (tp_navidrome_cover_path_for(path, cover_id, cover_path_out, cover_cap) > 0 &&
        file_exists(cover_path_out))
    {
        return true;
    }

    if (uipod_offline_mode_get())
        return false;

    return tp_navidrome_ensure_cover_cached(path, cover_id, cover_path_out, cover_cap);
}

void uipod_navidrome_prefetch_steps(int max_steps)
{
    if (uipod_offline_mode_get() || !uipod_navidrome_has_tracks())
        return;

    if (max_steps <= 0)
    {
        tp_navidrome_enqueue_prefetch3("", "", "", "", "", "", "", "", "");
        return;
    }
    if (max_steps > 3)
        max_steps = 3;

    char p_buf[3][MAX_PATH];
    char sid[3][128];
    char cid[3][128];
    const char *use_p[3] = { "", "", "" };
    int n = 0;

    for (int i = 0; i < 3; i++)
    {
        sid[i][0] = '\0';
        cid[i][0] = '\0';
    }

    for (int step = 1; step <= max_steps && n < 3; step++)
    {
        const char *pn = playlist_peek(step, p_buf[n], sizeof(p_buf[n]));
        if (pn && uipod_navidrome_is_track_path(pn) &&
            lookup_song_ids_by_path(pn, sid[n], sizeof(sid[n]),
                                    cid[n], sizeof(cid[n]), NULL))
        {
            use_p[n] = p_buf[n];
            n++;
        }
    }

    tp_navidrome_enqueue_prefetch3(use_p[0], sid[0], cid[0],
                                   use_p[1], sid[1], cid[1],
                                   use_p[2], sid[2], cid[2]);
}

void uipod_navidrome_on_track_change(const char *cur_path)
{
    if (uipod_offline_mode_get() || !uipod_navidrome_has_tracks())
        return;

    if (uipod_navidrome_is_track_path(cur_path))
    {
        char cur_sid[128], cur_cid[128];
        if (lookup_song_ids_by_path(cur_path, cur_sid, sizeof(cur_sid),
                                    cur_cid, sizeof(cur_cid), NULL))
        {
            tp_navidrome_notify_track_start(cur_sid);
        }
    }
}

void uipod_navidrome_on_track_finish(const struct mp3entry *id3)
{
    if (uipod_offline_mode_get() || !id3 || !uipod_navidrome_is_track_path(id3->path))
        return;

    char sid[128];
    if (lookup_song_ids_by_path(id3->path, sid, sizeof(sid), NULL, 0, NULL))
    {
        uint32_t played_secs   = (uint32_t)(id3->elapsed / 1000UL);
        uint32_t duration_secs = (uint32_t)(id3->length / 1000UL);
        tp_navidrome_notify_track_played(sid, played_secs, duration_secs);
    }
}

/* ---- Navidrome Settings / Setup Page (`Sources Navidrome` [1884]) ----- */

enum
{
    NAV_ROW_URL = 0,
    NAV_ROW_USER,
    NAV_ROW_PASS,
    NAV_ROW_SYNC,
    NAV_ROW_CACHE_LOC,
    NAV_ROW_QUALITY,
    NAV_ROW_SELF_SIGNED,
    NAV_ROW_CACHE_LIMIT,
    NAV_ROW_CLEAR_CACHE,
    NAV_ROW_COUNT
};

static const uint32_t s_cache_presets_mb[] = { 512, 1024, 2048, 4096, 0 };

struct nav_settings_page
{
    struct uipod_page base;
    struct gui_synclist lists;
    TpNavConfigC        cfg;
    bool                from_source_picker;
    bool                synced_ok;
    char                indic_buf[NAV_ROW_COUNT][128];
};

static const char *quality_label(uint8_t q)
{
    switch (q)
    {
        case TP_NAV_QUALITY_MP3_320:  return str(LANG_UIPOD_NAV_QUAL_320);
        case TP_NAV_QUALITY_MP3_192:  return str(LANG_UIPOD_NAV_QUAL_192);
        case TP_NAV_QUALITY_ORIGINAL:
        default:                      return str(LANG_UIPOD_NAV_QUAL_ORIGINAL);
    }
}

static void format_cache_limit(uint32_t mb, char *out, size_t cap)
{
    if (mb == 0)
        strlcpy(out, str(LANG_UIPOD_NAV_UNLIMITED), cap);
    else if (mb >= 1024 && (mb % 1024) == 0)
        snprintf(out, cap, "%u GB", (unsigned)(mb / 1024));
    else
        snprintf(out, cap, "%u MB", (unsigned)mb);
}

static void nav_short_cache_loc(const char *path, char *out, size_t cap)
{
    if (!path || !path[0])
    {
        snprintf(out, cap, "%s/Media", UIPOD_INDIC_PLAIN);
        return;
    }
    const char *disp = path;
    if (strncmp(disp, "/mnt/SDCARD/", 12) == 0)
        disp += 11;
    snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN, disp);
}

static const char *nav_set_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    (void)data; (void)buf; (void)buf_len;
    switch (sel)
    {
        case NAV_ROW_URL:         return str(LANG_UIPOD_NAV_SERVER_URL);
        case NAV_ROW_USER:        return str(LANG_UIPOD_NAV_USERNAME);
        case NAV_ROW_PASS:        return str(LANG_UIPOD_NAV_PASSWORD);
        case NAV_ROW_SYNC:        return str(LANG_UIPOD_NAV_SYNC);
        case NAV_ROW_CACHE_LOC:   return str(LANG_UIPOD_CACHE_LOCATION);
        case NAV_ROW_QUALITY:     return str(LANG_UIPOD_NAV_QUALITY);
        case NAV_ROW_SELF_SIGNED: return str(LANG_UIPOD_NAV_SELF_SIGNED);
        case NAV_ROW_CACHE_LIMIT: return str(LANG_UIPOD_NAV_CACHE_LIMIT);
        case NAV_ROW_CLEAR_CACHE: return str(LANG_UIPOD_NAV_CLEAR_CACHE);
        default:                  return "";
    }
}

static const char *nav_set_get_indicator(int sel, void *data)
{
    struct nav_settings_page *p = (struct nav_settings_page *)data;
    if (sel < 0 || sel >= NAV_ROW_COUNT)
        return NULL;
    char *out = p->indic_buf[sel];
    size_t cap = sizeof(p->indic_buf[sel]);

    switch (sel)
    {
        case NAV_ROW_URL:
            if (!p->cfg.url[0])
            {
                snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN, str(LANG_UIPOD_NAV_NOT_SET));
                return out;
            }
            {
                const char *u = p->cfg.url;
                if (strncasecmp(u, "http://", 7) == 0)
                    u += 7;
                else if (strncasecmp(u, "https://", 8) == 0)
                    u += 8;
                snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN, u);
                return out;
            }
        case NAV_ROW_USER:
            snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN,
                     p->cfg.username[0] ? p->cfg.username
                                        : (const char *)str(LANG_UIPOD_NAV_NOT_SET));
            return out;
        case NAV_ROW_PASS:
            snprintf(out, cap, "%s%s", UIPOD_INDIC_PLAIN,
                     p->cfg.password[0] ? "••••••••"
                                        : (const char *)str(LANG_UIPOD_NAV_NOT_SET));
            return out;
        case NAV_ROW_SYNC:
            return ">";
        case NAV_ROW_CACHE_LOC:
            nav_short_cache_loc(p->cfg.cache_location, out, cap);
            return out;
        case NAV_ROW_QUALITY:
            return quality_label(p->cfg.quality);
        case NAV_ROW_SELF_SIGNED:
            return uipod_toggle_str(p->cfg.allow_self_signed != 0);
        case NAV_ROW_CACHE_LIMIT:
            format_cache_limit(p->cfg.cache_limit_mb, out, cap);
            return out;
        case NAV_ROW_CLEAR_CACHE:
        {
            uint64_t bytes = tp_navidrome_cache_usage_bytes();
            unsigned mb = (unsigned)((bytes + (512ULL * 1024ULL)) / (1024ULL * 1024ULL));
            snprintf(out, cap, "%s%u MB", UIPOD_INDIC_PLAIN, mb);
            return out;
        }
        default:
            return NULL;
    }
}

static void nav_set_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct nav_settings_page *)self)->lists);
}

static int nav_set_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct nav_settings_page *)self)->lists, &action);
    return action;
}

static void nav_apply_and_persist_cfg(struct nav_settings_page *p,
                                      uint8_t prev_quality,
                                      const char *prev_cache_loc)
{
    tp_navidrome_set_config(&p->cfg);
    tp_navidrome_get_config(&p->cfg);
    save_config_to_disk();
    bool loc_changed = prev_cache_loc &&
                       strcmp(p->cfg.cache_location, prev_cache_loc) != 0;
    if (p->cfg.quality != prev_quality || loc_changed)
        repath_catalog_for_quality(p->cfg.quality);
}

static void nav_cycle_cache_limit(struct nav_settings_page *p,
                                  int dir,
                                  uint8_t prev_quality,
                                  const char *prev_loc)
{
    int cur = 0;
    for (size_t i = 0; i < ARRAYLEN(s_cache_presets_mb); i++)
    {
        if (s_cache_presets_mb[i] == p->cfg.cache_limit_mb)
        {
            cur = (int)i;
            break;
        }
    }
    int n = (int)ARRAYLEN(s_cache_presets_mb);
    cur = (cur + (dir < 0 ? -1 : 1) + n) % n;
    p->cfg.cache_limit_mb = s_cache_presets_mb[cur];
    nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
}

static enum uipod_page_result nav_set_on_action(struct uipod_page *self, int action)
{
    struct nav_settings_page *p = (struct nav_settings_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);

    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;

    uint8_t prev_quality = p->cfg.quality;
    char prev_loc[sizeof(p->cfg.cache_location)];
    strlcpy(prev_loc, p->cfg.cache_location, sizeof(prev_loc));

    if (action == ACTION_SETTINGS_DEC || action == ACTION_SETTINGS_INC)
    {
        int dir = (action == ACTION_SETTINGS_DEC) ? -1 : 1;
        if (sel == NAV_ROW_QUALITY)
        {
            p->cfg.quality = (uint8_t)((p->cfg.quality + (dir < 0 ? 2 : 1)) % 3);
            nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
        }
        else if (sel == NAV_ROW_SELF_SIGNED)
        {
            p->cfg.allow_self_signed = p->cfg.allow_self_signed ? 0 : 1;
            nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
        }
        else if (sel == NAV_ROW_CACHE_LIMIT)
        {
            nav_cycle_cache_limit(p, dir, prev_quality, prev_loc);
        }
        return UIPOD_PAGE_STAY;
    }

    if (action != ACTION_STD_OK)
        return UIPOD_PAGE_STAY;

    switch (sel)
    {
        case NAV_ROW_URL:
        {
            char buf[sizeof(p->cfg.url)];
            if (p->cfg.url[0])
                strlcpy(buf, p->cfg.url, sizeof(buf));
            else
                strlcpy(buf, "http://", sizeof(buf));
            if (uipod_kbd_input_ex(buf, sizeof(buf), true) == 0)
            {
                strlcpy(p->cfg.url, buf, sizeof(p->cfg.url));
                nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
            }
            break;
        }
        case NAV_ROW_USER:
            if (uipod_kbd_input_ex(p->cfg.username, sizeof(p->cfg.username), true) == 0)
                nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
            break;
        case NAV_ROW_PASS:
            if (uipod_kbd_input_ex(p->cfg.password, sizeof(p->cfg.password), true) == 0)
                nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
            break;
        case NAV_ROW_SYNC:
            nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
            if (uipod_navidrome_sync(true))
            {
                p->synced_ok = true;
                if (p->from_source_picker)
                {
                    p->base.no_arm_back = true;
                    return UIPOD_PAGE_DONE;
                }
            }
            break;
        case NAV_ROW_CACHE_LOC:
        {
            char picked[MAX_PATH];
            if (uipod_folder_pick(picked, sizeof(picked)))
            {
                strlcpy(p->cfg.cache_location, picked, sizeof(p->cfg.cache_location));
                nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
            }
            break;
        }
        case NAV_ROW_QUALITY:
        {
            const char *opts[3] = {
                str(LANG_UIPOD_NAV_QUAL_ORIGINAL),
                str(LANG_UIPOD_NAV_QUAL_320),
                str(LANG_UIPOD_NAV_QUAL_192),
            };
            int pick = uipod_context_menu(str(LANG_UIPOD_NAV_QUALITY), opts, 3);
            if (pick >= 0 && pick <= 2)
            {
                p->cfg.quality = (uint8_t)pick;
                nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
            }
            break;
        }
        case NAV_ROW_CACHE_LIMIT:
        {
            char labels[ARRAYLEN(s_cache_presets_mb)][32];
            const char *opts[ARRAYLEN(s_cache_presets_mb)];
            for (size_t i = 0; i < ARRAYLEN(s_cache_presets_mb); i++)
            {
                format_cache_limit(s_cache_presets_mb[i], labels[i], sizeof(labels[i]));
                opts[i] = labels[i];
            }
            int pick = uipod_context_menu(str(LANG_UIPOD_NAV_CACHE_LIMIT),
                                            opts, (int)ARRAYLEN(s_cache_presets_mb));
            if (pick >= 0 && (size_t)pick < ARRAYLEN(s_cache_presets_mb))
            {
                p->cfg.cache_limit_mb = s_cache_presets_mb[pick];
                nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
            }
            break;
        }
        case NAV_ROW_SELF_SIGNED:
            p->cfg.allow_self_signed = p->cfg.allow_self_signed ? 0 : 1;
            nav_apply_and_persist_cfg(p, prev_quality, prev_loc);
            break;
        case NAV_ROW_CLEAR_CACHE:
            if (uipod_confirm(str(LANG_UIPOD_NAV_CLEAR_CACHE_Q), NULL))
            {
                tp_navidrome_clear_cache();
                splash(HZ, ID2P(LANG_UIPOD_NAV_CACHE_CLEARED));
            }
            break;
    }

    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable nav_settings_vtable = {
    .legend    = NULL,
    .draw      = nav_set_draw,
    .poll      = nav_set_poll,
    .on_action = nav_set_on_action,
};

static const int nav_settings_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL,
    ACTION_SETTINGS_DEC, ACTION_SETTINGS_INC, -1
};

int uipod_navidrome_settings_page(bool from_source_picker)
{
    uint8_t prev_src = uipod_gpu_get_header_source();
    uipod_gpu_set_header_source(UIPOD_SRC_ICON_NAVIDROME);

    struct nav_settings_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt      = &nav_settings_vtable;
    p.base.context = CONTEXT_SETTINGS_RECTRIGGER;
    p.base.allowed = nav_settings_allowed;
    p.base.title   = str(LANG_UIPOD_SOURCE_NAVIDROME);
    p.from_source_picker = from_source_picker;

    tp_navidrome_get_config(&p.cfg);

    gui_synclist_init(&p.lists, nav_set_get_name, &p, false, 1, NULL);
    gui_synclist_set_indicator_callback(&p.lists, nav_set_get_indicator);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_UIPOD_SOURCE_NAVIDROME),
                           Icon_Submenu_Entered);
    gui_synclist_set_nb_items(&p.lists, NAV_ROW_COUNT);
    gui_synclist_select_item(&p.lists, tp_navidrome_is_configured() ? NAV_ROW_SYNC
                                                                    : NAV_ROW_URL);

    uipod_page_run(&p.base);
    uipod_gpu_set_header_source(prev_src);
    return p.synced_ok ? 1 : 0;
}

int uipod_navidrome_menu_page(void)
{
    uipod_navidrome_settings_page(false);
    return 0;
}

/* ---- Virtual `Files` Browser (Navidrome Server Hierarchy) ------------- */

struct nav_files_page
{
    struct uipod_page base;
    struct gui_synclist lists;
    char                curdir[MAX_PATH];
    char                title_buf[128];
    TpNavTreeEntryC    *entries;
    int                 count;
    int                 result;
};

static struct
{
    bool valid;
    char dir[MAX_PATH];
    char sel_name[256];
} s_nav_files_pos;

static char s_nav_reselect[256];

static void nav_files_free_entries(struct nav_files_page *p)
{
    free(p->entries);
    p->entries = NULL;
    p->count = 0;
}

static void nav_files_load(struct nav_files_page *p)
{
    nav_files_free_entries(p);
    size_t n = tp_navidrome_tree_list_count(p->curdir);
    if (n > 0)
    {
        p->entries = calloc(n, sizeof(TpNavTreeEntryC));
        if (p->entries)
        {
            size_t got = 0;
            for (size_t i = 0; i < n; i++)
                if (tp_navidrome_tree_list_get(p->curdir, i, &p->entries[got]))
                    got++;
            p->count = (int)got;
        }
    }

    if (!p->curdir[0])
        strlcpy(p->title_buf, str(LANG_UIPOD_FILES), sizeof(p->title_buf));
    else
    {
        const char *slash = strrchr(p->curdir, '/');
        strlcpy(p->title_buf, slash ? slash + 1 : p->curdir, sizeof(p->title_buf));
    }

    gui_synclist_set_title(&p->lists, p->title_buf, Icon_Audio);
    gui_synclist_set_nb_items(&p->lists, p->count);

    int sel = 0;
    if (s_nav_reselect[0])
    {
        for (int i = 0; i < p->count; i++)
        {
            if (strcmp(p->entries[i].name, s_nav_reselect) == 0 ||
                strcmp(p->entries[i].target_path, s_nav_reselect) == 0)
            {
                sel = i;
                break;
            }
        }
        s_nav_reselect[0] = '\0';
    }
    gui_synclist_select_item(&p->lists, sel);
}

static struct nav_files_page *s_slide_files_p;
static void nav_files_slide_render(void *ctx)
{
    (void)ctx;
    nav_files_load(s_slide_files_p);
    gui_synclist_draw(&s_slide_files_p->lists);
}

static void nav_files_slide(struct nav_files_page *p, enum uipod_transition_dir dir)
{
    s_slide_files_p = p;
    uipod_transition_animate(dir, nav_files_slide_render, NULL);
}

static const char *nav_files_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    (void)buf; (void)buf_len;
    struct nav_files_page *p = (struct nav_files_page *)data;
    if (sel < 0 || sel >= p->count)
        return "";
    return p->entries[sel].name;
}

static const struct uipod_svg_icon *nav_files_get_svg(int sel, void *data)
{
    struct nav_files_page *p = (struct nav_files_page *)data;
    if (sel < 0 || sel >= p->count)
        return NULL;
    const TpNavTreeEntryC *e = &p->entries[sel];
    return uipod_svg_for_path(e->is_dir ? e->name : e->target_path, e->is_dir != 0);
}

static bool nav_files_get_chevron(int sel, void *data)
{
    struct nav_files_page *p = (struct nav_files_page *)data;
    return sel >= 0 && sel < p->count;
}

static void nav_files_save_pos(struct nav_files_page *p, int sel)
{
    s_nav_files_pos.valid = true;
    strlcpy(s_nav_files_pos.dir, p->curdir, sizeof(s_nav_files_pos.dir));
    if (sel >= 0 && sel < p->count)
        strlcpy(s_nav_files_pos.sel_name, p->entries[sel].name,
                sizeof(s_nav_files_pos.sel_name));
    else
        s_nav_files_pos.sel_name[0] = '\0';
}

struct nav_collect_vec
{
    char **v;
    int    n;
    int    cap;
};

static uint8_t nav_collect_cb(void *vctx, const char *path)
{
    struct nav_collect_vec *vec = (struct nav_collect_vec *)vctx;
    if (vec->n >= UIPOD_MAX_FILES_IN_PLAYLIST)
        return 0;
    if (vec->n == vec->cap)
    {
        int ncap = vec->cap ? vec->cap * 2 : 32;
        char **nv = realloc(vec->v, (size_t)ncap * sizeof(*nv));
        if (!nv)
            return 0;
        vec->v = nv;
        vec->cap = ncap;
    }
    char *dup = strdup(path);
    if (dup)
        vec->v[vec->n++] = dup;
    return 1;
}

static void nav_collect_free(struct nav_collect_vec *vec)
{
    for (int i = 0; i < vec->n; i++)
        free(vec->v[i]);
    free(vec->v);
    vec->v = NULL;
    vec->n = vec->cap = 0;
}

static void nav_files_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct nav_files_page *)self)->lists);
}

static int nav_files_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct nav_files_page *)self)->lists, &action);
    return action;
}

static bool nav_files_play_or_ctx(struct nav_files_page *p, int sel, bool is_ctx)
{
    int track_cnt = 0;
    int track_sel = -1;
    for (int i = 0; i < p->count; i++)
    {
        if (!p->entries[i].is_dir && p->entries[i].target_path[0])
        {
            if (i == sel)
                track_sel = track_cnt;
            track_cnt++;
        }
    }
    if (track_cnt <= 0 || track_sel < 0)
        return false;

    char **paths = malloc((size_t)track_cnt * sizeof(*paths));
    if (!paths)
    {
        return is_ctx
            ? uipod_music_context(p->entries[sel].name,
                                  p->entries[sel].target_path,
                                  FILE_ATTR_AUDIO)
            : uipod_play_track(p->entries[sel].target_path);
    }

    int k = 0;
    for (int i = 0; i < p->count; i++)
    {
        if (!p->entries[i].is_dir && p->entries[i].target_path[0])
            paths[k++] = p->entries[i].target_path;
    }

    bool started = is_ctx
        ? uipod_music_context_track_in_list(p->entries[sel].name, paths, k, track_sel)
        : uipod_play_tracks_at(paths, k, track_sel);
    free(paths);
    return started;
}

static enum uipod_page_result nav_files_on_action(struct uipod_page *self, int action)
{
    struct nav_files_page *p = (struct nav_files_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);
    bool have_sel = (sel >= 0 && sel < p->count);

    switch (action)
    {
        case ACTION_STD_OK:
            if (!have_sel)
                return UIPOD_PAGE_STAY;
            if (p->entries[sel].is_dir)
            {
                strlcpy(p->curdir, p->entries[sel].target_path, sizeof(p->curdir));
                nav_files_slide(p, UIPOD_TRANS_FORWARD);
                return UIPOD_PAGE_STAY;
            }
            else
            {
                nav_files_save_pos(p, sel);
                if (nav_files_play_or_ctx(p, sel, false))
                {
                    p->result = GO_TO_WPS;
                    return UIPOD_PAGE_DONE;
                }
                return UIPOD_PAGE_STAY;
            }

        case ACTION_STD_CONTEXT:
            if (!have_sel)
                return UIPOD_PAGE_STAY;
            nav_files_save_pos(p, sel);
            if (p->entries[sel].is_dir)
            {
                struct nav_collect_vec vec = {0};
                tp_navidrome_tree_collect_tracks(p->entries[sel].target_path,
                                                 nav_collect_cb, &vec);
                bool started = uipod_music_context_tracks(p->entries[sel].name,
                                                            vec.v, vec.n);
                nav_collect_free(&vec);
                if (started)
                {
                    p->result = GO_TO_WPS;
                    return UIPOD_PAGE_DONE;
                }
            }
            else
            {
                if (nav_files_play_or_ctx(p, sel, true))
                {
                    p->result = GO_TO_WPS;
                    return UIPOD_PAGE_DONE;
                }
            }
            return UIPOD_PAGE_STAY;

        case ACTION_STD_CANCEL:
            if (!p->curdir[0])
            {
                nav_files_save_pos(p, sel);
                uipod_transition_arm_back();
                return UIPOD_PAGE_DONE;
            }
            {
                const char *slash = strrchr(p->curdir, '/');
                if (slash)
                {
                    strlcpy(s_nav_reselect, slash + 1, sizeof(s_nav_reselect));
                    size_t plen = (size_t)(slash - p->curdir);
                    if (plen >= sizeof(p->curdir))
                        plen = sizeof(p->curdir) - 1;
                    p->curdir[plen] = '\0';
                }
                else
                {
                    strlcpy(s_nav_reselect, p->curdir, sizeof(s_nav_reselect));
                    p->curdir[0] = '\0';
                }
                nav_files_slide(p, UIPOD_TRANS_BACK);
                return UIPOD_PAGE_STAY;
            }
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable nav_files_vtable = {
    .legend    = NULL,
    .draw      = nav_files_draw,
    .poll      = nav_files_poll,
    .on_action = nav_files_on_action,
};

static const int nav_files_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL, ACTION_STD_CONTEXT, -1
};

int uipod_navidrome_files_browse(void)
{
    if (!uipod_navidrome_has_tracks())
    {
        uipod_navidrome_settings_page(false);
        return GO_TO_ROOT;
    }

    struct nav_files_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt               = &nav_files_vtable;
    p.base.context          = CONTEXT_LIST;
    p.base.allowed          = nav_files_allowed;
    p.base.no_arm_back      = true;
    p.base.enter_honor_back = true;
    p.result                = GO_TO_ROOT;

    if (s_nav_files_pos.valid &&
        tp_navidrome_tree_list_count(s_nav_files_pos.dir) > 0)
    {
        strlcpy(p.curdir, s_nav_files_pos.dir, sizeof(p.curdir));
        strlcpy(s_nav_reselect, s_nav_files_pos.sel_name, sizeof(s_nav_reselect));
    }

    struct mp3entry *cur_id3 = audio_current_track();
    if ((audio_status() & AUDIO_STATUS_PLAY) && cur_id3 &&
        uipod_navidrome_is_track_path(cur_id3->path))
    {
        strlcpy(s_nav_reselect, cur_id3->path, sizeof(s_nav_reselect));
    }

    gui_synclist_init(&p.lists, nav_files_get_name, &p, false, 1, NULL);
    gui_synclist_set_svg_icon_callback(&p.lists, nav_files_get_svg);
    gui_synclist_set_chevron_callback(&p.lists, nav_files_get_chevron);
    nav_files_load(&p);

    push_current_activity(ACTIVITY_FILEBROWSER);
    uipod_page_run(&p.base);
    pop_current_activity();

    nav_files_free_entries(&p);
    return p.base.home ? GO_TO_ROOT : p.result;
}
