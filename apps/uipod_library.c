/* UIPod: SQLite-backed library index -- see uipod_library.h.
 *
 * The DB is a derived index; the SD-card files are the source of truth.  So on
 * schema change we migrate in place, and only rebuild from scratch when the file
 * is unreadable (never as routine recovery).
 *
 * Scanning uses libc stat() for the per-directory mtime gate.  Rockbox's app
 * file API exposes mtime only through dir_get_info() (i.e. by enumerating the
 * parent), which would defeat the gate; on this hosted target OS and Rockbox
 * paths map 1:1 (app_root_realpath()=="/"), so a direct libc stat() on the same
 * path string is valid -- the same precedent as the extern libc symlink() in
 * uipod_folders.c.  Directory *enumeration* still uses Rockbox opendir/
 * readdir/dir_get_info (whose mtime is also os_stat-derived, so the two agree),
 * and file opens for tagging use Rockbox open() so the fd matches get_metadata().
 *
 * Single-threaded: reconcile runs synchronously at init, on a source-folder
 * add/remove, and on manual rescan; there is no background writer, so the
 * journal mode is TRUNCATE (no WAL).  The parse loop yields per file so
 * playback keeps running.  The db is worked on in tmpfs and written back to
 * the card after changes (see "tmpfs working copy"). */

#include "config.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>   /* strncasecmp */
#include <ctype.h>
#include <stdio.h>
#include <sys/stat.h>  /* stat() -- directory-mtime gate (see header note) */

#include "sqlite3.h"

#include "dir.h"       /* opendir/readdir/dir_get_info, ATTR_DIRECTORY */
#include "file.h"      /* open/close/remove, O_RDONLY, MAX_PATH */
#include "pathfuncs.h"
#include "string-extra.h"
#include "rbpaths.h"   /* ROCKBOX_DIR */
#include "filetypes.h" /* filetype_get_attr, FILE_ATTR_AUDIO */
#include "metadata.h"  /* get_metadata, struct mp3entry */
#include "settings.h"  /* UIPOD_MAX_FILES_IN_PLAYLIST */
#include "lang.h"      /* str, LANG_UIPOD_SCANNING */
#include "kernel.h"    /* yield */
#include "backlight.h" /* backlight_on */
#include "uipod_folders.h"
#include "uipod_downloads.h"
#include "uipod_navidrome.h"
#include "uipod_jellyfin.h"
#include "uipod_rs_codecs.h"
#include "uipod_rs_ui.h"
#include "uipod_ui.h"

extern char *realpath(const char *path, char *resolved);
extern ssize_t readlink(const char *path, char *buf, size_t bufsiz);

#define SCHEMA_VERSION 5

static sqlite3 *db;
static uint32_t s_local_gen          = 1;
static uint32_t s_remote_gen         = 1;
static uint32_t s_merged_local_gen   = 0;
static uint32_t s_merged_remote_gen  = 0;

/* "Scanning Library NN%" held over the synchronous scan.  Hidden until the walk
 * finds work (a forced rescan shows it up front): 0% from then, the percent
 * parsed through pass 2, 100% through commit + write-back.  Centered on the
 * widest form so the number grows in place. */
#define SCAN_HIDDEN (-1)
static int      scan_shown = SCAN_HIDDEN;   /* percent on screen, or hidden */
static uint32_t scan_hud_handle = 0;

static void scan_show(int pct)
{
    if (pct == scan_shown) return;
    char det[24];
    snprintf(det, sizeof det, "%d%%", pct);
    float prog = (float)pct * 0.01f;
    if (scan_hud_handle == 0)
        scan_hud_handle = uipod_hud_progress_begin(str(LANG_UIPOD_SCANNING), det, prog);
    else
        uipod_hud_progress_update(scan_hud_handle, str(LANG_UIPOD_SCANNING), det, prog);
    backlight_on();
    scan_shown = pct;
}

static void scan_hide(void)
{
    if (scan_hud_handle != 0)
    {
        uint32_t h = scan_hud_handle;
        scan_hud_handle = 0;
        uipod_hud_progress_end(h);
    }
    scan_shown = SCAN_HIDDEN;
}

/* Files the walk found needing a parse.  Parsing is the slow part (~50 ms per
 * file on the card), so it's deferred to a second pass over this list, which
 * also gives the percent its total. */
struct pend_item { char *path; int root_id, category; time_t mtime; off_t size; };
static struct { struct pend_item *v; int n, cap; } pend;

static void pend_add(const char *path, int root_id, int category,
                     time_t mtime, off_t size)
{
    if (pend.n == pend.cap)
    {
        int ncap = pend.cap ? pend.cap * 2 : 64;
        struct pend_item *nv = realloc(pend.v, ncap * sizeof *nv);
        if (!nv) return;                 /* best effort: drop this entry, keep prior */
        pend.v = nv; pend.cap = ncap;
    }
    char *dup = strdup(path);
    if (dup)
        pend.v[pend.n++] = (struct pend_item){ dup, root_id, category, mtime, size };
}
static void pend_free(void)
{
    for (int i = 0; i < pend.n; i++) free(pend.v[i].path);
    free(pend.v); pend.v = NULL; pend.n = pend.cap = 0;
}

/* ---- schema ----------------------------------------------------------- */

static const char *SCHEMA_SQL =
    "CREATE TABLE roots("
    "  id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, category INTEGER NOT NULL);"
    "CREATE TABLE dirs("
    "  path TEXT PRIMARY KEY, parent TEXT,"
    "  root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,"
    "  mtime INTEGER NOT NULL);"
    "CREATE INDEX idx_dirs_parent ON dirs(parent);"
    "CREATE TABLE tracks("
    "  id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, dir TEXT NOT NULL,"
    "  root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,"
    "  category INTEGER NOT NULL, mtime INTEGER NOT NULL, size INTEGER NOT NULL,"
    "  title TEXT, artist TEXT, albumartist TEXT, album TEXT, genre TEXT,"
    "  composer TEXT, grouping TEXT, year INTEGER, tracknum INTEGER, discnum INTEGER,"
    "  duration_ms INTEGER, codectype INTEGER, bitrate INTEGER, frequency INTEGER,"
    "  browse_artist TEXT, sort_artist TEXT,"
    "  source INTEGER NOT NULL DEFAULT 0,"
    "  file_path TEXT NOT NULL DEFAULT '',"
    "  subtrack_index INTEGER NULL,"
    "  track_kind INTEGER NOT NULL DEFAULT 0,"
    "  chip_flags INTEGER NOT NULL DEFAULT 0,"
    "  loop_point_ms INTEGER NULL);"
    "CREATE UNIQUE INDEX idx_tracks_src_file_sub"
    "  ON tracks(source, file_path, COALESCE(subtrack_index, -1));"
    "CREATE INDEX idx_tracks_file_path ON tracks(file_path);"
    "CREATE INDEX idx_tracks_kind   ON tracks(category, track_kind);"
    "CREATE INDEX idx_tracks_dir    ON tracks(dir);"
    "CREATE INDEX idx_tracks_artist ON tracks(category, sort_artist, album, discnum, tracknum);"
    "CREATE INDEX idx_tracks_album  ON tracks(category, album, discnum, tracknum);"
    "CREATE INDEX idx_tracks_browse ON tracks(category, browse_artist, album, discnum, tracknum);"
    "CREATE INDEX idx_tracks_title  ON tracks(category, title, path);"
    "CREATE INDEX idx_tracks_genre  ON tracks(category, genre);";

/* ---- small growable path set (no fixed cap) --------------------------- */

struct pathset { char **v; int n, cap; };

static void ps_init(struct pathset *s) { s->v = NULL; s->n = s->cap = 0; }
static void ps_add(struct pathset *s, const char *p)
{
    if (s->n == s->cap)
    {
        int ncap = s->cap ? s->cap * 2 : 16;
        char **nv = realloc(s->v, ncap * sizeof *nv);
        if (!nv) return;                 /* best effort: drop this entry, keep prior */
        s->v = nv; s->cap = ncap;
    }
    char *dup = strdup(p);
    if (dup) s->v[s->n++] = dup;
}
static bool ps_has(struct pathset *s, const char *p)
{
    for (int i = 0; i < s->n; i++)
        if (strcmp(s->v[i], p) == 0) return true;
    return false;
}
static void ps_free(struct pathset *s)
{
    for (int i = 0; i < s->n; i++) free(s->v[i]);
    free(s->v); ps_init(s);
}

/* ---- helpers ---------------------------------------------------------- */

static int db_exec(const char *sql) { return sqlite3_exec(db, sql, NULL, NULL, NULL); }

static void bind_str(sqlite3_stmt *st, int i, const char *s)
{
    if (s && s[0]) sqlite3_bind_text(st, i, s, -1, SQLITE_STATIC);
    else           sqlite3_bind_null(st, i);
}

/* iPod sort key: lowercase, leading "the " dropped. */
static void make_sort_artist(const char *ba, char *out, size_t n)
{
    if (!ba) ba = "";
    if (strncasecmp(ba, "the ", 4) == 0) ba += 4;
    size_t i = 0;
    for (; ba[i] && i + 1 < n; i++) out[i] = tolower((unsigned char)ba[i]);
    out[i] = '\0';
}

static int user_version(void)
{
    sqlite3_stmt *st; int v = 0;
    if (sqlite3_prepare_v2(db, "PRAGMA user_version;", -1, &st, NULL) == SQLITE_OK)
    {
        if (sqlite3_step(st) == SQLITE_ROW) v = sqlite3_column_int(st, 0);
        sqlite3_finalize(st);
    }
    return v;
}
static void set_user_version(int v)
{
    char sql[48];
    snprintf(sql, sizeof sql, "PRAGMA user_version=%d;", v);
    db_exec(sql);
}

/* ---- track / dir row ops --------------------------------------------- */

/* The scan reuses ONE prepared statement per query for the whole reconcile
 * (prepare once, then bind/step/reset per row) instead of recompiling per
 * file -- the SQLite-idiomatic hot-loop pattern.  The statements are scoped to
 * a single reconcile: opened at its start, finalized at its end, so none
 * survives a db close/reopen. */
static const char TRACK_UPSERT_SQL[] =
    "INSERT INTO tracks(path,dir,root_id,category,mtime,size,title,artist,"
    "albumartist,album,genre,composer,grouping,year,tracknum,discnum,"
    "duration_ms,codectype,bitrate,frequency,browse_artist,sort_artist,"
    "source,file_path,subtrack_index,track_kind,chip_flags,loop_point_ms)"
    " VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28)"
    " ON CONFLICT(path) DO UPDATE SET dir=?2,root_id=?3,category=?4,mtime=?5,"
    "size=?6,title=?7,artist=?8,albumartist=?9,album=?10,genre=?11,composer=?12,"
    "grouping=?13,year=?14,tracknum=?15,discnum=?16,duration_ms=?17,codectype=?18,"
    "bitrate=?19,frequency=?20,browse_artist=?21,sort_artist=?22,"
    "source=?23,file_path=?24,subtrack_index=?25,track_kind=?26,chip_flags=?27,loop_point_ms=?28;";

struct scan_stmts
{
    sqlite3_stmt *track_sel;    /* SELECT mtime,size FROM tracks WHERE file_path=? LIMIT 1 */
    sqlite3_stmt *track_ins;    /* the upsert above                                        */
    sqlite3_stmt *track_del;    /* DELETE FROM tracks WHERE file_path=?                    */
    sqlite3_stmt *dir_sel;      /* SELECT mtime FROM dirs WHERE path=?                     */
    sqlite3_stmt *dir_ins;      /* upsert into dirs                                        */
    sqlite3_stmt *dir_del;      /* DELETE a dir and every dir under it                     */
    sqlite3_stmt *subtree_del;  /* DELETE the tracks in a dir and under it                 */
    sqlite3_stmt *kids_sel;     /* SELECT path FROM dirs WHERE parent=?                    */
    sqlite3_stmt *dtracks_sel;  /* SELECT DISTINCT file_path FROM tracks WHERE dir=?       */
};

static bool prep(sqlite3_stmt **st, const char *sql)
{
    return sqlite3_prepare_v2(db, sql, -1, st, NULL) == SQLITE_OK;
}
/* Returns false if any statement failed to compile (partial set is safe to
 * close: sqlite3_finalize(NULL) is a no-op). */
static bool scan_stmts_open(struct scan_stmts *s)
{
    memset(s, 0, sizeof *s);
    return prep(&s->track_sel,   "SELECT mtime,size FROM tracks WHERE file_path=? LIMIT 1;")
        && prep(&s->track_ins,   TRACK_UPSERT_SQL)
        && prep(&s->track_del,   "DELETE FROM tracks WHERE file_path=?;")
        && prep(&s->dir_sel,     "SELECT mtime FROM dirs WHERE path=?;")
        && prep(&s->dir_ins,
                "INSERT INTO dirs(path,parent,root_id,mtime) VALUES(?1,?2,?3,?4)"
                " ON CONFLICT(path) DO UPDATE SET parent=?2,root_id=?3,mtime=?4;")
        /* "X/" <= p < "X0" is everything under X ('0' follows '/'), on the index */
        && prep(&s->dir_del,     "DELETE FROM dirs WHERE path=?1"
                                 " OR (path>=?1||'/' AND path<?1||'0');")
        && prep(&s->subtree_del, "DELETE FROM tracks WHERE dir=?1"
                                 " OR (dir>=?1||'/' AND dir<?1||'0');")
        && prep(&s->kids_sel,    "SELECT path FROM dirs WHERE parent=?;")
        && prep(&s->dtracks_sel, "SELECT DISTINCT file_path FROM tracks WHERE dir=?;");
}
static void scan_stmts_close(struct scan_stmts *s)
{
    sqlite3_finalize(s->track_sel);  sqlite3_finalize(s->track_ins);
    sqlite3_finalize(s->track_del);  sqlite3_finalize(s->dir_sel);
    sqlite3_finalize(s->dir_ins);    sqlite3_finalize(s->dir_del);
    sqlite3_finalize(s->kids_sel);   sqlite3_finalize(s->dtracks_sel);
    sqlite3_finalize(s->subtree_del);
}

static bool is_nsf_container_path(const char *path)
{
    if (!path)
        return false;
    if (strchr(path, '#') != NULL)
        return false;
    const char *dot = strrchr(path, '.');
    if (!dot)
        return false;
    return strcasecmp(dot, ".nsf") == 0 || strcasecmp(dot, ".nsfe") == 0;
}

static int extract_copyright_year(const char *s)
{
    if (!s)
        return 0;
    int run = 0, val = 0;
    for (size_t i = 0; s[i]; i++)
    {
        unsigned char c = (unsigned char)s[i];
        if (c >= '0' && c <= '9')
        {
            val = val * 10 + (c - '0');
            run++;
            if (run == 4 &&
                !(s[i + 1] >= '0' && s[i + 1] <= '9') &&
                val >= 1980 && val <= 2099)
            {
                return val;
            }
        }
        else
        {
            run = 0;
            val = 0;
        }
    }
    return 0;
}

/* Each helper binds, steps, then sqlite3_reset()s so the statement is clean for
 * the next row.  We rebind every parameter each call, so no clear_bindings is
 * needed.  Each use fully drains + resets before returning, so the same
 * statement is safely reused down the recursive scan (single-threaded). */
static bool track_unchanged(sqlite3_stmt *st, const char *path,
                            time_t mtime, off_t size)
{
    bool same = false;
    sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
    if (sqlite3_step(st) == SQLITE_ROW)
        same = sqlite3_column_int64(st, 0) == (sqlite3_int64)mtime &&
               sqlite3_column_int64(st, 1) == (sqlite3_int64)size;
    sqlite3_reset(st);
    return same;
}

static int parse_and_upsert_nsf(struct scan_stmts *s, const char *path,
                                const char *dir, int root_id, int category,
                                time_t mtime, off_t size)
{
    static struct uipod_rs_nsf_container_header hdr;
    static struct uipod_rs_nsf_subtrack_info subs_static[256];

    memset(&hdr, 0, sizeof(hdr));
    size_t total = uipod_rs_nsf_scan_container(path, NULL, 0, &hdr, subs_static, 256);
    if (total == 0)
        return 0;

    struct uipod_rs_nsf_subtrack_info *subs = subs_static;
    struct uipod_rs_nsf_subtrack_info *subs_dyn = NULL;
    if (hdr.total_subtracks > 256)
    {
        subs_dyn = calloc(hdr.total_subtracks, sizeof(*subs_dyn));
        if (subs_dyn)
        {
            total = uipod_rs_nsf_scan_container(
                path, NULL, 0, &hdr, subs_dyn, hdr.total_subtracks);
            subs = subs_dyn;
        }
    }

    /* Clear any prior subtrack rows for this container before inserting fresh set */
    sqlite3_bind_text(s->track_del, 1, path, -1, SQLITE_STATIC);
    sqlite3_step(s->track_del);
    sqlite3_reset(s->track_del);

    const char *artist   = hdr.artist[0]    ? hdr.artist    : NULL;
    const char *album    = hdr.game_title[0]? hdr.game_title: NULL;
    const char *composer = hdr.copyright[0] ? hdr.copyright : NULL;
    const char *ba       = artist ? artist : "";
    int year             = extract_copyright_year(hdr.copyright);
    char sortbuf[128];
    make_sort_artist(ba, sortbuf, sizeof sortbuf);

    int inserted = 0;
    size_t limit = (hdr.total_subtracks > 256 && !subs_dyn) ? 256 : total;
    for (size_t k = 0; k < limit; k++)
    {
        char uri[MAX_PATH + 16];
        snprintf(uri, sizeof uri, "%s#%u", path, (unsigned)subs[k].subtrack_index);

        sqlite3_stmt *st = s->track_ins;
        sqlite3_bind_text (st,  1, uri, -1, SQLITE_TRANSIENT);
        sqlite3_bind_text (st,  2, dir, -1, SQLITE_STATIC);
        sqlite3_bind_int  (st,  3, root_id);
        sqlite3_bind_int  (st,  4, category);
        sqlite3_bind_int64(st,  5, (sqlite3_int64)mtime);
        sqlite3_bind_int64(st,  6, (sqlite3_int64)size);
        bind_str          (st,  7, subs[k].title);
        bind_str          (st,  8, artist);
        sqlite3_bind_null (st,  9); /* albumartist */
        bind_str          (st, 10, album);
        bind_str          (st, 11, "Chiptune");
        bind_str          (st, 12, composer);
        sqlite3_bind_null (st, 13); /* grouping */
        sqlite3_bind_int  (st, 14, year);
        sqlite3_bind_int  (st, 15, (int)subs[k].subtrack_index + 1);
        sqlite3_bind_int  (st, 16, 1);
        sqlite3_bind_int64(st, 17, (sqlite3_int64)subs[k].duration_ms);
        sqlite3_bind_int  (st, 18, (int)AFMT_NSF);
        sqlite3_bind_int  (st, 19, 1411);
        sqlite3_bind_int64(st, 20, 44100);
        sqlite3_bind_text (st, 21, ba, -1, SQLITE_STATIC);
        sqlite3_bind_text (st, 22, sortbuf, -1, SQLITE_TRANSIENT);
        sqlite3_bind_int  (st, 23, 0); /* source = 0 (Local) */
        sqlite3_bind_text (st, 24, path, -1, SQLITE_STATIC);
        sqlite3_bind_int  (st, 25, (int)subs[k].subtrack_index);
        sqlite3_bind_int  (st, 26, (int)subs[k].track_kind);
        sqlite3_bind_int  (st, 27, (int)subs[k].chip_flags);
        if (subs[k].loop_point_ms >= 0)
            sqlite3_bind_int(st, 28, (int)subs[k].loop_point_ms);
        else
            sqlite3_bind_null(st, 28);

        if (sqlite3_step(st) == SQLITE_DONE)
            inserted++;
        sqlite3_reset(st);
    }

    free(subs_dyn);
    return inserted;
}

static int parse_and_upsert(struct scan_stmts *s, const char *path, const char *dir,
                            int root_id, int category, time_t mtime, off_t size)
{
    if (is_nsf_container_path(path))
        return parse_and_upsert_nsf(s, path, dir, root_id, category, mtime, size);

    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    static struct mp3entry id3;          /* large; reused (single-threaded scan) */
    bool ok = get_metadata(&id3, fd, path);
    close(fd);
    if (!ok) return 0;

    const char *ba = (id3.albumartist && id3.albumartist[0]) ? id3.albumartist
                   : (id3.artist ? id3.artist : "");
    char sortbuf[128];
    make_sort_artist(ba, sortbuf, sizeof sortbuf);

    sqlite3_stmt *st = s->track_ins;
    sqlite3_bind_text (st,  1, path, -1, SQLITE_STATIC);
    sqlite3_bind_text (st,  2, dir,  -1, SQLITE_STATIC);
    sqlite3_bind_int  (st,  3, root_id);
    sqlite3_bind_int  (st,  4, category);
    sqlite3_bind_int64(st,  5, (sqlite3_int64)mtime);
    sqlite3_bind_int64(st,  6, (sqlite3_int64)size);
    bind_str(st,  7, id3.title);
    bind_str(st,  8, id3.artist);
    bind_str(st,  9, id3.albumartist);
    bind_str(st, 10, id3.album);
    bind_str(st, 11, id3.genre_string);
    bind_str(st, 12, id3.composer);
    bind_str(st, 13, id3.grouping);
    sqlite3_bind_int(st, 14, id3.year);
    sqlite3_bind_int(st, 15, id3.tracknum);
    sqlite3_bind_int(st, 16, id3.discnum);
    sqlite3_bind_int64(st, 17, (sqlite3_int64)id3.length);
    sqlite3_bind_int(st, 18, (int)id3.codectype);
    sqlite3_bind_int(st, 19, (int)id3.bitrate);
    sqlite3_bind_int64(st, 20, (sqlite3_int64)id3.frequency);
    sqlite3_bind_text(st, 21, ba, -1, SQLITE_STATIC);
    sqlite3_bind_text(st, 22, sortbuf, -1, SQLITE_TRANSIENT);
    sqlite3_bind_int (st, 23, 0); /* source = 0 (Local) */
    sqlite3_bind_text(st, 24, path, -1, SQLITE_STATIC);
    sqlite3_bind_null(st, 25);    /* subtrack_index = NULL */
    sqlite3_bind_int (st, 26, 0); /* track_kind = Music */
    sqlite3_bind_int (st, 27, 0); /* chip_flags = 0 */
    sqlite3_bind_null(st, 28);    /* loop_point_ms = NULL */
    bool done = sqlite3_step(st) == SQLITE_DONE;
    sqlite3_reset(st);
    return done ? 1 : 0;
}

static void upsert_dir(sqlite3_stmt *st, const char *path, const char *parent,
                       int root_id, time_t mtime)
{
    sqlite3_bind_text (st, 1, path, -1, SQLITE_STATIC);
    bind_str          (st, 2, parent);
    sqlite3_bind_int  (st, 3, root_id);
    sqlite3_bind_int64(st, 4, (sqlite3_int64)mtime);
    sqlite3_step(st);
    sqlite3_reset(st);
}

static bool dir_stored_mtime(sqlite3_stmt *st, const char *path, time_t *out)
{
    bool found = false;
    sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
    if (sqlite3_step(st) == SQLITE_ROW)
    { *out = (time_t)sqlite3_column_int64(st, 0); found = true; }
    sqlite3_reset(st);
    return found;
}

/* Collect child paths (col 0) from a reused single-text-param query. */
static void collect(sqlite3_stmt *st, const char *param, struct pathset *out)
{
    sqlite3_bind_text(st, 1, param, -1, SQLITE_STATIC);
    while (sqlite3_step(st) == SQLITE_ROW)
        ps_add(out, (const char *)sqlite3_column_text(st, 0));
    sqlite3_reset(st);
}

static void delete_where(sqlite3_stmt *st, const char *path)
{
    sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
    sqlite3_step(st);
    sqlite3_reset(st);
}

/* ---- recursive stat-gated scan --------------------------------------- */

static int scan_dir(struct scan_stmts *s, const char *path, const char *parent,
                    int root_id, int category, bool force)
{
    struct stat st;
    if (stat(path, &st) != 0)
        return 0;                        /* vanished; the parent pass prunes its rows */
    time_t cur = st.st_mtime;

    /* Gate: unchanged dir -> its direct entries are unchanged; skip the readdir
     * and just recurse known subdirs (a deep change bumps only that subdir). */
    if (!force)
    {
        time_t stored;
        if (dir_stored_mtime(s->dir_sel, path, &stored) && stored == cur)
        {
            struct pathset kids; ps_init(&kids);
            collect(s->kids_sel, path, &kids);
            int touched = 0;
            for (int i = 0; i < kids.n; i++)
                touched += scan_dir(s, kids.v[i], path, root_id, category, force);
            ps_free(&kids);
            return touched;
        }
    }

    /* New or changed: enumerate and reconcile this directory's direct children. */
    DIR *d = opendir(path);
    if (!d) return 0;
    int touched = 0;
    struct pathset seen_files, seen_dirs;
    ps_init(&seen_files); ps_init(&seen_dirs);
    struct dirent *e;
    while ((e = readdir(d)))
    {
        if (e->d_name[0] == '.') continue;
        char child[MAX_PATH];
        if ((size_t)snprintf(child, sizeof child, "%s/%s", path, e->d_name) >= sizeof child)
            continue;
        struct dirinfo info = dir_get_info(d, e);
        if (info.attribute & ATTR_DIRECTORY)
        {
            ps_add(&seen_dirs, child);
            touched += scan_dir(s, child, path, root_id, category, force);
        }
        else if ((filetype_get_attr(e->d_name) & FILE_ATTR_MASK) == FILE_ATTR_AUDIO &&
                 !filetype_is_video(e->d_name))
        {
            ps_add(&seen_files, child);
            if (!track_unchanged(s->track_sel, child, info.mtime, info.size))
            {
                scan_show(0);                /* first real work: show the screen */
                pend_add(child, root_id, category, info.mtime, info.size);
            }
        }
    }
    closedir(d);

    /* Prune direct children that disappeared. */
    struct pathset rows; ps_init(&rows);
    collect(s->dtracks_sel, path, &rows);
    for (int i = 0; i < rows.n; i++)
        if (!ps_has(&seen_files, rows.v[i]))
        { delete_where(s->track_del, rows.v[i]); touched++; }
    ps_free(&rows);

    ps_init(&rows);
    collect(s->kids_sel, path, &rows);
    for (int i = 0; i < rows.n; i++)
        if (!ps_has(&seen_dirs, rows.v[i]))
        {
            /* tracks reference roots, not dirs: nothing cascades */
            delete_where(s->subtree_del, rows.v[i]);
            delete_where(s->dir_del, rows.v[i]);
            touched++;
        }
    ps_free(&rows);

    upsert_dir(s->dir_ins, path, parent, root_id, cur);
    ps_free(&seen_files); ps_free(&seen_dirs);
    return touched;
}

/* ---- roots sync ------------------------------------------------------- */

static bool root_is_configured(const char *path)
{
    for (int c = 0; c < TP_CAT_COUNT; c++)
        for (int i = 0, n = uipod_folders_root_count(c); i < n; i++)
            if (strcmp(uipod_folders_root_path(c, i), path) == 0) return true;
    return false;
}

static int root_id_for(const char *path)
{
    sqlite3_stmt *st; int id = 0;
    if (sqlite3_prepare_v2(db, "SELECT id FROM roots WHERE path=?;",
                           -1, &st, NULL) == SQLITE_OK)
    {
        sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
        if (sqlite3_step(st) == SQLITE_ROW) id = sqlite3_column_int(st, 0);
        sqlite3_finalize(st);
    }
    return id;
}

static void sync_roots(void)
{
    /* upsert every configured root */
    for (int c = 0; c < TP_CAT_COUNT; c++)
        for (int i = 0, n = uipod_folders_root_count(c); i < n; i++)
        {
            const char *p = uipod_folders_root_path(c, i);
            sqlite3_stmt *st;
            if (sqlite3_prepare_v2(db,
                    "INSERT INTO roots(path,category) VALUES(?1,?2)"
                    " ON CONFLICT(path) DO UPDATE SET category=?2 WHERE category!=?2;",
                    -1, &st, NULL) == SQLITE_OK)
            {
                sqlite3_bind_text(st, 1, p, -1, SQLITE_STATIC);
                sqlite3_bind_int (st, 2, c);
                sqlite3_step(st);
                sqlite3_finalize(st);
            }
        }
    /* drop roots no longer configured (cascades their dirs + tracks) */
    struct pathset gone; ps_init(&gone);
    sqlite3_stmt *st;
    if (sqlite3_prepare_v2(db, "SELECT path FROM roots;", -1, &st, NULL) == SQLITE_OK)
    {
        while (sqlite3_step(st) == SQLITE_ROW)
        {
            const char *p = (const char *)sqlite3_column_text(st, 0);
            if (!root_is_configured(p)) ps_add(&gone, p);
        }
        sqlite3_finalize(st);
    }
    if (gone.n)
    {
        sqlite3_stmt *del;
        if (sqlite3_prepare_v2(db, "DELETE FROM roots WHERE path=?;",
                               -1, &del, NULL) == SQLITE_OK)
        {
            for (int i = 0; i < gone.n; i++)
                delete_where(del, gone.v[i]);
            sqlite3_finalize(del);
        }
    }
    ps_free(&gone);
}

/* ---- tmpfs working copy ------------------------------------------------ *
 * The card is a `sync` vfat mount: every 4 KB page SQLite writes costs ~18 ms,
 * and a library change dirties most pages twice (journal + db), so committing
 * there took ~15 s.  The index is worked on in tmpfs instead and copied back
 * to the card in one sequential pass after a reconcile that changed it. */
#define DB_SD    ROCKBOX_DIR "/library.db"
#define DB_RAM   UIPOD_TMP_ROOT "/uipod-library.db"
#define COPY_BUF (256 * 1024)

static bool db_in_ram;

static bool copy_file(const char *src, const char *dst)
{
    bool ok = false;
    int in = open(src, O_RDONLY);
    if (in < 0) return false;
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    char *buf = malloc(COPY_BUF);
    if (out >= 0 && buf)
    {
        ssize_t n;
        while ((n = read(in, buf, COPY_BUF)) > 0)
            if (write(out, buf, n) != n) break;
        ok = n == 0;
    }
    free(buf);
    if (out >= 0) close(out);
    close(in);
    return ok;
}

/* tmp + rename: a kill mid-write leaves the card's old copy intact. */
static void db_writeback(void)
{
    if (db_in_ram && copy_file(DB_RAM, DB_SD ".new"))
        rename(DB_SD ".new", DB_SD);
}

/* ---- public API ------------------------------------------------------- */

int uipod_library_reconcile(bool force_full)
{
    if (!db) return 0;
    uipod_folders_load_all();
    scan_shown = SCAN_HIDDEN;
    if (force_full) scan_show(0);            /* a full walk is always worth showing */
    db_exec("BEGIN;");
    int changes_before = sqlite3_total_changes(db);
    sync_roots();
    int touched = 0;
    struct scan_stmts s;
    if (scan_stmts_open(&s))
    {
        /* Pass 1: stat-gated walk -- prune, upsert dirs, collect files to parse. */
        for (int c = 0; c < TP_CAT_COUNT; c++)
            for (int i = 0, n = uipod_folders_root_count(c); i < n; i++)
            {
                const char *r = uipod_folders_root_path(c, i);
                int id = root_id_for(r);
                if (id > 0) touched += scan_dir(&s, r, NULL, id, c, force_full);
            }
        /* Pass 2: parse them, with the percent; yield so the codec keeps up. */
        for (int i = 0; i < pend.n; i++)
        {
            scan_show(i * 100 / pend.n);
            yield();
            struct pend_item *it = &pend.v[i];
            char dir[MAX_PATH];
            const char *sl = strrchr(it->path, '/');
            snprintf(dir, sizeof dir, "%.*s", (int)(sl ? sl - it->path : 0), it->path);
            touched += parse_and_upsert(&s, it->path, dir, it->root_id,
                                        it->category, it->mtime, it->size);
        }
    }
    /* Hold the screen through commit + write-back (seconds on the card); a pure
     * removal has shown nothing yet. */
    bool changed = sqlite3_total_changes(db) != changes_before;
    if (changed) scan_show(100);
    pend_free();
    scan_stmts_close(&s);
    db_exec("COMMIT;");
    s_local_gen++;
    if (changed) db_writeback();
    scan_hide();
    return touched;
}

extern bool retrohh_usb_host_connected(void);   /* power-target.c */

static bool     library_stale;
static uint32_t s_seen_dl_seq;

void uipod_library_mark_stale(void)
{
    library_stale = true;
}

void uipod_library_bump_remote_gen(void)
{
    s_remote_gen++;
}

void uipod_library_refresh_if_stale(void)
{
    uint32_t dl_seq = uipod_downloads_completed_seq();
    if (dl_seq != s_seen_dl_seq)
    {
        s_seen_dl_seq = dl_seq;
        s_remote_gen++;
        library_stale = true;
    }
    /* A computer still attached may be mid-copy: wait for the unplug. */
    if (!library_stale || retrohh_usb_host_connected()) return;
    library_stale = false;
    uipod_library_reconcile(false);
}

/* ---- Unified Local + Navidrome + Jellyfin Merged Music Table ---------- */

static const char MERGED_SCHEMA_SQL[] =
    "CREATE TEMP TABLE IF NOT EXISTS merged_tracks("
    "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
    "  path TEXT NOT NULL UNIQUE,"
    "  dedup_key TEXT NOT NULL,"
    "  category INTEGER NOT NULL,"
    "  title TEXT,"
    "  artist TEXT,"
    "  browse_artist TEXT,"
    "  sort_artist TEXT,"
    "  album TEXT,"
    "  genre TEXT,"
    "  year INTEGER,"
    "  tracknum INTEGER,"
    "  discnum INTEGER,"
    "  duration_ms INTEGER,"
    "  UNIQUE(category, dedup_key));"
    "CREATE INDEX IF NOT EXISTS temp.idx_mt_artist ON merged_tracks(category, sort_artist, album, discnum, tracknum);"
    "CREATE INDEX IF NOT EXISTS temp.idx_mt_album  ON merged_tracks(category, album, discnum, tracknum);"
    "CREATE INDEX IF NOT EXISTS temp.idx_mt_browse ON merged_tracks(category, browse_artist, album, discnum, tracknum);"
    "CREATE INDEX IF NOT EXISTS temp.idx_mt_title  ON merged_tracks(category, title, path);"
    "CREATE INDEX IF NOT EXISTS temp.idx_mt_genre  ON merged_tracks(category, genre);";

static void norm_dedup_token(const char *src, char *dst, size_t cap)
{
    if (cap == 0)
        return;
    if (!src)
    {
        dst[0] = '\0';
        return;
    }

    while (*src && (unsigned char)*src <= ' ')
        src++;

    size_t w = 0;
    bool in_ws = false;
    for (size_t i = 0; src[i] && w + 3 < cap; )
    {
        unsigned char c = (unsigned char)src[i];
        if (c <= ' ')
        {
            in_ws = true;
            i++;
            continue;
        }
        if (in_ws && w > 0)
        {
            dst[w++] = ' ';
            in_ws = false;
        }
        if (c >= 'A' && c <= 'Z')
        {
            dst[w++] = (char)(c + 32);
            i++;
        }
        else if (c == 0xD0 && src[i + 1])
        {
            unsigned char c2 = (unsigned char)src[i + 1];
            /* Cyrillic uppercase А..П (D0 90..9F) -> а..п (D0 B0..BF) */
            if (c2 >= 0x90 && c2 <= 0x9F)
            {
                dst[w++] = (char)0xD0;
                dst[w++] = (char)(c2 + 0x20);
                i += 2;
            }
            /* Cyrillic uppercase Р..Я (D0 A0..AF) -> р..я (D1 80..8F) */
            else if (c2 >= 0xA0 && c2 <= 0xAF)
            {
                dst[w++] = (char)0xD1;
                dst[w++] = (char)(c2 - 0x20);
                i += 2;
            }
            /* Ukrainian І, Ї, Є (D0 86, D0 87, D0 84) -> і, ї, є (D1 96, D1 97, D1 94) */
            else if (c2 == 0x81 || c2 == 0x84 || c2 == 0x86 || c2 == 0x87)
            {
                dst[w++] = (char)0xD1;
                dst[w++] = (char)(c2 + 0x10);
                i += 2;
            }
            else
            {
                dst[w++] = (char)c;
                i++;
            }
        }
        else if (c == 0xD2 && (unsigned char)src[i + 1] == 0x90)
        {
            /* Ukrainian Ґ (D2 90) -> ґ (D2 91) */
            dst[w++] = (char)0xD2;
            dst[w++] = (char)0x91;
            i += 2;
        }
        else
        {
            dst[w++] = (char)c;
            i++;
        }
    }
    dst[w] = '\0';
}

static void strip_subtrack_fragment(const char *uri, char *out, size_t cap)
{
    if (!uri || cap == 0)
    {
        if (cap > 0) out[0] = '\0';
        return;
    }
    const char *hash = strrchr(uri, '#');
    if (hash && hash[1] >= '0' && hash[1] <= '9')
    {
        bool all_digits = true;
        for (const char *p = hash + 1; *p; p++)
            if (*p < '0' || *p > '9') { all_digits = false; break; }
        if (all_digits)
        {
            size_t len = (size_t)(hash - uri);
            if (len >= cap) len = cap - 1;
            memcpy(out, uri, len);
            out[len] = '\0';
            return;
        }
    }
    strlcpy(out, uri, cap);
}

static void build_track_dedup_key(int category,
                                  const char *path,
                                  const char *browse_artist,
                                  const char *album,
                                  int discnum,
                                  int tracknum,
                                  const char *title,
                                  int subtrack_index,
                                  char *out,
                                  size_t cap)
{
    char n_title[192];
    norm_dedup_token(title, n_title, sizeof(n_title));
    if (category != TP_CAT_MUSIC || !n_title[0])
    {
        snprintf(out, cap, "p:%s", path ? path : "");
        return;
    }

    char n_artist[160], n_album[160];
    norm_dedup_token(browse_artist, n_artist, sizeof(n_artist));
    norm_dedup_token(album, n_album, sizeof(n_album));
    int ndisc = (discnum <= 1) ? 1 : discnum;
    int ntrk  = (tracknum > 0) ? tracknum : 0;
    if (subtrack_index >= 0)
    {
        snprintf(out, cap, "m:%s\x1f%s\x1f%d\x1f%d\x1f%s\x1fsub:%d",
                 n_artist, n_album, ndisc, ntrk, n_title, subtrack_index);
    }
    else
    {
        snprintf(out, cap, "m:%s\x1f%s\x1f%d\x1f%d\x1f%s",
                 n_artist, n_album, ndisc, ntrk, n_title);
    }
}

static void merge_copy_from_db(sqlite3 *src_db,
                               sqlite3_stmt *ins_stmt,
                               bool require_file_exists,
                               bool is_local_db)
{
    if (!src_db || !ins_stmt)
        return;

    const char *sql = is_local_db
        ? "SELECT path, category, title, artist, browse_artist, sort_artist,"
          " album, genre, year, tracknum, discnum, COALESCE(duration_ms, 0),"
          " COALESCE(subtrack_index, -1)"
          " FROM tracks WHERE COALESCE(track_kind, 0) = 0;"
        : "SELECT path, category, title, artist, browse_artist, sort_artist,"
          " album, genre, year, tracknum, discnum, COALESCE(duration_ms, 0),"
          " -1 FROM tracks;";

    sqlite3_stmt *sel = NULL;
    if (sqlite3_prepare_v2(src_db, sql, -1, &sel, NULL) != SQLITE_OK)
    {
        return;
    }

    while (sqlite3_step(sel) == SQLITE_ROW)
    {
        const char *path   = (const char *)sqlite3_column_text(sel, 0);
        int category       = sqlite3_column_int(sel, 1);
        const char *title  = (const char *)sqlite3_column_text(sel, 2);
        const char *artist = (const char *)sqlite3_column_text(sel, 3);
        const char *ba     = (const char *)sqlite3_column_text(sel, 4);
        const char *sa     = (const char *)sqlite3_column_text(sel, 5);
        const char *album  = (const char *)sqlite3_column_text(sel, 6);
        const char *genre  = (const char *)sqlite3_column_text(sel, 7);
        int year           = sqlite3_column_int(sel, 8);
        int tracknum       = sqlite3_column_int(sel, 9);
        int discnum        = sqlite3_column_int(sel, 10);
        int dur_ms         = sqlite3_column_int(sel, 11);
        int sub_idx        = sqlite3_column_int(sel, 12);

        if (!path || !path[0])
            continue;
        if (filetype_is_video(path))
            continue;
        if (require_file_exists)
        {
            char base_path[MAX_PATH];
            strip_subtrack_fragment(path, base_path, sizeof(base_path));
            struct stat st;
            if (stat(base_path, &st) != 0 || !S_ISREG(st.st_mode) || st.st_size <= 0)
                continue;
        }

        char dkey[512];
        build_track_dedup_key(category, path, ba, album, discnum, tracknum,
                              title, sub_idx, dkey, sizeof(dkey));

        sqlite3_bind_text(ins_stmt,  1, path, -1, SQLITE_TRANSIENT);
        sqlite3_bind_text(ins_stmt,  2, dkey, -1, SQLITE_TRANSIENT);
        sqlite3_bind_int (ins_stmt,  3, category);
        if (title && title[0])   sqlite3_bind_text(ins_stmt, 4, title,  -1, SQLITE_TRANSIENT);
        else                     sqlite3_bind_null(ins_stmt, 4);
        if (artist && artist[0]) sqlite3_bind_text(ins_stmt, 5, artist, -1, SQLITE_TRANSIENT);
        else                     sqlite3_bind_null(ins_stmt, 5);
        sqlite3_bind_text(ins_stmt,  6, ba ? ba : "", -1, SQLITE_TRANSIENT);
        sqlite3_bind_text(ins_stmt,  7, sa ? sa : "", -1, SQLITE_TRANSIENT);
        if (album && album[0])   sqlite3_bind_text(ins_stmt, 8, album,  -1, SQLITE_TRANSIENT);
        else                     sqlite3_bind_null(ins_stmt, 8);
        if (genre && genre[0])   sqlite3_bind_text(ins_stmt, 9, genre,  -1, SQLITE_TRANSIENT);
        else                     sqlite3_bind_null(ins_stmt, 9);
        sqlite3_bind_int (ins_stmt, 10, year);
        sqlite3_bind_int (ins_stmt, 11, tracknum);
        sqlite3_bind_int (ins_stmt, 12, discnum);
        sqlite3_bind_int (ins_stmt, 13, dur_ms);

        sqlite3_step(ins_stmt);
        sqlite3_reset(ins_stmt);
    }

    sqlite3_finalize(sel);
}

static bool ensure_merged_tracks(void)
{
    if (!db)
        return false;

    uint32_t dl_seq = uipod_downloads_completed_seq();
    if (dl_seq != s_seen_dl_seq)
    {
        s_seen_dl_seq = dl_seq;
        s_remote_gen++;
        library_stale = true;
    }

    if (s_merged_local_gen == s_local_gen &&
        s_merged_remote_gen == s_remote_gen)
    {
        return true;
    }

    if (db_exec(MERGED_SCHEMA_SQL) != SQLITE_OK)
        return false;

    bool offline = uipod_offline_mode_get();

    db_exec("BEGIN;");
    db_exec("DELETE FROM temp.merged_tracks;");

    sqlite3_stmt *ins = NULL;
    if (sqlite3_prepare_v2(
            db,
            "INSERT OR IGNORE INTO temp.merged_tracks("
            "path, dedup_key, category, title, artist, browse_artist, sort_artist,"
            "album, genre, year, tracknum, discnum, duration_ms)"
            " VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13);",
            -1, &ins, NULL) == SQLITE_OK)
    {
        /* 1) Local SD tracks first (already on disk -> instant local playback) */
        merge_copy_from_db(db, ins, false, true);

        /* 2) Navidrome catalog (in Offline Mode, only cached/downloaded files) */
        sqlite3 *nav_db = uipod_navidrome_db_lock();
        if (nav_db)
            merge_copy_from_db(nav_db, ins, offline, false);
        uipod_navidrome_db_unlock();

        /* 3) Jellyfin music catalog (in Offline Mode, only cached/downloaded files) */
        sqlite3 *jf_db = uipod_jellyfin_db_lock();
        if (jf_db)
            merge_copy_from_db(jf_db, ins, offline, false);
        uipod_jellyfin_db_unlock();

        sqlite3_finalize(ins);
    }

    db_exec("COMMIT;");
    s_merged_local_gen  = s_local_gen;
    s_merged_remote_gen = s_remote_gen;
    return true;
}

/* ---- Shuffle All's track set ------------------------------------------ */

void uipod_library_music_paths(bool (*cb)(const char *path, void *ctx),
                                 void *ctx)
{
    if (!cb || !ensure_merged_tracks()) return;
    /* Title order is what Shuffle Off restores.  The LIMIT is the queue cap, so
     * an over-cap library yields a random sample, not its alphabetical head. */
    sqlite3_stmt *st;
    if (sqlite3_prepare_v2(db,
            "SELECT path FROM (SELECT path, title FROM temp.merged_tracks WHERE category=?"
            " ORDER BY RANDOM() LIMIT ?) ORDER BY title, path;", -1, &st, NULL)
            != SQLITE_OK)
    {
        return;
    }
    sqlite3_bind_int(st, 1, TP_CAT_MUSIC);
    sqlite3_bind_int(st, 2, UIPOD_MAX_FILES_IN_PLAYLIST);
    while (sqlite3_step(st) == SQLITE_ROW)
    {
        const char *p = (const char *)sqlite3_column_text(st, 0);
        if (p && !cb(p, ctx))
            break;
    }
    sqlite3_finalize(st);
}

void uipod_library_artists(int category,
        void (*cb)(const char *, int, void *), void *ctx)
{
    if (!cb || !ensure_merged_tracks()) return;
    sqlite3_stmt *st;
    if (sqlite3_prepare_v2(db,
            "SELECT browse_artist, COUNT(*) FROM temp.merged_tracks WHERE category=?"
            " GROUP BY sort_artist ORDER BY sort_artist;", -1, &st, NULL) != SQLITE_OK)
    {
        return;
    }
    sqlite3_bind_int(st, 1, category);
    while (sqlite3_step(st) == SQLITE_ROW)
        cb((const char *)sqlite3_column_text(st, 0), sqlite3_column_int(st, 1), ctx);
    sqlite3_finalize(st);
}

void uipod_library_albums(int category, const char *ba,
        void (*cb)(const char *, int, void *), void *ctx)
{
    if (!cb || !ensure_merged_tracks()) return;
    /* One artist -> chronological (year, then title); ba==NULL -> every album in
     * the library, alphabetical (the flat "Albums" facet). */
    char sql[192];
    snprintf(sql, sizeof sql,
        "SELECT album, MAX(year) FROM temp.merged_tracks WHERE category=?%s"
        " GROUP BY album ORDER BY %s;",
        ba ? " AND browse_artist=?" : "",
        ba ? "MAX(year), album" : "album");
    sqlite3_stmt *st;
    if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK)
    {
        return;
    }
    sqlite3_bind_int(st, 1, category);
    if (ba) sqlite3_bind_text(st, 2, ba, -1, SQLITE_STATIC);
    while (sqlite3_step(st) == SQLITE_ROW)
        cb((const char *)sqlite3_column_text(st, 0), sqlite3_column_int(st, 1), ctx);
    sqlite3_finalize(st);
}

void uipod_library_tracks(int category, const char *ba, const char *album,
        void (*cb)(const char *title, const char *path, void *ctx), void *ctx)
{
    if (!cb || !ensure_merged_tracks()) return;
    /* Deterministic order: the browse queues rows as listed, so a row index is
     * its playlist index.  album==NULL -> the artist's whole library; ba==NULL
     * too -> the "Songs" facet. */
    const char *order = album ? " ORDER BY discnum,tracknum"
                      : ba    ? " ORDER BY path"
                      :         " ORDER BY title, path";
    char sql[256];
    snprintf(sql, sizeof sql,
        "SELECT title, path FROM temp.merged_tracks WHERE category=?%s%s%s;",
        ba ? " AND browse_artist=?" : "",
        album ? " AND album=?" : "",
        order);
    sqlite3_stmt *st;
    if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK)
    {
        return;
    }
    int bi = 1;
    sqlite3_bind_int (st, bi++, category);
    if (ba)    sqlite3_bind_text(st, bi++, ba,    -1, SQLITE_STATIC);
    if (album) sqlite3_bind_text(st, bi++, album, -1, SQLITE_STATIC);
    while (sqlite3_step(st) == SQLITE_ROW)
        cb((const char *)sqlite3_column_text(st, 0),
           (const char *)sqlite3_column_text(st, 1), ctx);
    sqlite3_finalize(st);
}

void uipod_library_genres(int category,
        void (*cb)(const char *genre, int track_count, void *ctx), void *ctx)
{
    if (!cb || !ensure_merged_tracks()) return;
    sqlite3_stmt *st;
    if (sqlite3_prepare_v2(db,
            "SELECT COALESCE(genre, ''), COUNT(*) FROM temp.merged_tracks WHERE category=?"
            " GROUP BY COALESCE(genre, '')"
            " ORDER BY CASE WHEN genre IS NULL OR genre='' THEN 1 ELSE 0 END,"
            " genre COLLATE NOCASE;", -1, &st, NULL) != SQLITE_OK)
    {
        return;
    }
    sqlite3_bind_int(st, 1, category);
    while (sqlite3_step(st) == SQLITE_ROW)
        cb((const char *)sqlite3_column_text(st, 0), sqlite3_column_int(st, 1), ctx);
    sqlite3_finalize(st);
}

void uipod_library_genre_tracks(int category, const char *genre,
        void (*cb)(const char *title, const char *path, void *ctx), void *ctx)
{
    if (!cb || !ensure_merged_tracks()) return;
    sqlite3_stmt *st;
    if (sqlite3_prepare_v2(db,
            "SELECT title, path FROM temp.merged_tracks"
            " WHERE category=? AND COALESCE(genre, '')=?"
            " ORDER BY album COLLATE NOCASE, discnum, tracknum, title COLLATE NOCASE, path;",
            -1, &st, NULL) != SQLITE_OK)
    {
        return;
    }
    sqlite3_bind_int (st, 1, category);
    sqlite3_bind_text(st, 2, genre ? genre : "", -1, SQLITE_STATIC);
    while (sqlite3_step(st) == SQLITE_ROW)
        cb((const char *)sqlite3_column_text(st, 0),
           (const char *)sqlite3_column_text(st, 1), ctx);
    sqlite3_finalize(st);
}

struct cached_local_track
{
    char    *path;
    char    *artist;
    char    *title;
    uint32_t duration_ms;
};

static struct cached_local_track *s_local_tracks;
static int                        s_local_track_cnt;
static int                        s_local_track_cap;
static uint32_t                   s_local_tracks_gen;

static void free_local_track_cache(void)
{
    for (int i = 0; i < s_local_track_cnt; i++)
    {
        free(s_local_tracks[i].path);
        free(s_local_tracks[i].artist);
        free(s_local_tracks[i].title);
    }
    free(s_local_tracks);
    s_local_tracks     = NULL;
    s_local_track_cnt  = 0;
    s_local_track_cap  = 0;
    s_local_tracks_gen = 0;
}

static void ensure_local_track_cache(void)
{
    uipod_library_refresh_if_stale();
    if (!db)
    {
        free_local_track_cache();
        return;
    }
    if (s_local_tracks_gen == s_local_gen && s_local_tracks != NULL)
        return;

    free_local_track_cache();

    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(
            db,
            "SELECT path,"
            " COALESCE(NULLIF(artist, ''), browse_artist, ''),"
            " COALESCE(title, ''),"
            " COALESCE(duration_ms, 0)"
            " FROM tracks"
            " WHERE category=? AND COALESCE(track_kind, 0)=0 ORDER BY id;",
            -1, &st, NULL) != SQLITE_OK)
    {
        return;
    }
    sqlite3_bind_int(st, 1, TP_CAT_MUSIC);

    while (sqlite3_step(st) == SQLITE_ROW)
    {
        const char *p = (const char *)sqlite3_column_text(st, 0);
        if (!p || p[0] != '/')
            continue;
        const char *a = (const char *)sqlite3_column_text(st, 1);
        const char *t = (const char *)sqlite3_column_text(st, 2);
        int dur       = sqlite3_column_int(st, 3);

        if (s_local_track_cnt == s_local_track_cap)
        {
            int ncap = s_local_track_cap ? s_local_track_cap * 2 : 256;
            struct cached_local_track *nv =
                realloc(s_local_tracks, (size_t)ncap * sizeof(*nv));
            if (!nv)
                break;
            s_local_tracks    = nv;
            s_local_track_cap = ncap;
        }

        char *dp = strdup(p);
        char *da = strdup(a ? a : "");
        char *dt = strdup(t ? t : "");
        if (!dp || !da || !dt)
        {
            free(dp);
            free(da);
            free(dt);
            break;
        }

        s_local_tracks[s_local_track_cnt++] = (struct cached_local_track){
            .path        = dp,
            .artist      = da,
            .title       = dt,
            .duration_ms = (dur > 0) ? (uint32_t)dur : 0u,
        };
    }

    sqlite3_finalize(st);
    s_local_tracks_gen = s_local_gen;
}

int uipod_library_music_count(void)
{
    ensure_local_track_cache();
    return s_local_track_cnt;
}

bool uipod_library_music_at(int index,
                            char *path_out, size_t path_cap,
                            char *artist_out, size_t artist_cap,
                            char *title_out, size_t title_cap,
                            uint32_t *duration_ms_out)
{
    ensure_local_track_cache();
    if (index < 0 || index >= s_local_track_cnt || !s_local_tracks)
        return false;

    const struct cached_local_track *it = &s_local_tracks[index];
    if (path_out && path_cap > 0)
        strlcpy(path_out, it->path, path_cap);
    if (artist_out && artist_cap > 0)
        strlcpy(artist_out, it->artist, artist_cap);
    if (title_out && title_cap > 0)
        strlcpy(title_out, it->title, title_cap);
    if (duration_ms_out)
        *duration_ms_out = it->duration_ms;
    return true;
}

/* Resolve `/tmp/uipod_<cat>/<link>[/<rest>]` symlink-farm paths in tmpfs RAM
 * without touching the VFAT SD card (`realpath()` stats every path component!).
 * Returns `out` when `path` lived under the symlink farm and resolved, else `path`. */
static const char *resolve_farm_symlink_fast(const char *path, char *out, size_t cap)
{
    static const char prefix[] = UIPOD_TMP_ROOT "/uipod_";
    const size_t plen = sizeof(prefix) - 1;
    if (!path || strncmp(path, prefix, plen) != 0)
        return path;

    const char *cat_slash = strchr(path + plen, '/');
    if (!cat_slash || !cat_slash[1])
        return path;

    const char *entry_end = strchr(cat_slash + 1, '/');
    size_t link_len = entry_end ? (size_t)(entry_end - path) : strlen(path);
    if (link_len >= MAX_PATH)
        return path;

    char link_buf[MAX_PATH];
    memcpy(link_buf, path, link_len);
    link_buf[link_len] = '\0';

    char target[MAX_PATH];
    ssize_t tlen = readlink(link_buf, target, sizeof(target) - 1);
    if (tlen <= 0)
        return path;
    target[tlen] = '\0';

    if (entry_end && *entry_end)
        snprintf(out, cap, "%s%s", target, entry_end);
    else
        strlcpy(out, target, cap);
    return out;
}

#define NSF_COUNT_CACHE_CAP   2048u
#define NSF_COUNT_CACHE_MASK  (NSF_COUNT_CACHE_CAP - 1u)
#define NSF_COUNT_PROBE_STEPS 4u

struct nsf_count_cache_entry
{
    uint32_t hash;
    uint32_t gen;
    int      music_count;
    char     path[MAX_PATH];
};
static struct nsf_count_cache_entry s_nsf_count_cache[NSF_COUNT_CACHE_CAP];

static uint32_t fnv1a_path_hash(const char *s)
{
    uint32_t h = 2166136261u;
    for (const unsigned char *p = (const unsigned char *)s; *p; p++)
    {
        h ^= (uint32_t)(*p);
        h *= 16777619u;
    }
    return h ? h : 1u;
}

static bool nsf_count_cache_get(const char *phys_path, int *out_count)
{
    uint32_t h = fnv1a_path_hash(phys_path);
    uint32_t base = h & NSF_COUNT_CACHE_MASK;
    for (uint32_t step = 0; step < NSF_COUNT_PROBE_STEPS; step++)
    {
        const struct nsf_count_cache_entry *e =
            &s_nsf_count_cache[(base + step) & NSF_COUNT_CACHE_MASK];
        if (e->gen == s_local_gen && e->hash == h &&
            strcmp(e->path, phys_path) == 0)
        {
            *out_count = e->music_count;
            return true;
        }
    }
    return false;
}

static void nsf_count_cache_put(const char *phys_path, int count)
{
    uint32_t h = fnv1a_path_hash(phys_path);
    uint32_t base = h & NSF_COUNT_CACHE_MASK;
    uint32_t victim = base;
    for (uint32_t step = 0; step < NSF_COUNT_PROBE_STEPS; step++)
    {
        uint32_t idx = (base + step) & NSF_COUNT_CACHE_MASK;
        struct nsf_count_cache_entry *e = &s_nsf_count_cache[idx];
        if (e->gen != s_local_gen ||
            (e->hash == h && strcmp(e->path, phys_path) == 0))
        {
            victim = idx;
            break;
        }
        victim = idx;
    }
    struct nsf_count_cache_entry *dst = &s_nsf_count_cache[victim];
    dst->hash        = h;
    dst->gen         = s_local_gen;
    dst->music_count = count;
    strlcpy(dst->path, phys_path, sizeof(dst->path));
}

static int expand_nsf_internal(const char *nsf_path,
                               bool fallback_if_empty,
                               bool header_only_if_unindexed,
                               bool (*cb)(const char *uri, void *ctx),
                               void *ctx)
{
    if (!nsf_path || !nsf_path[0] || !cb)
        return 0;

    char base_path[MAX_PATH];
    strip_subtrack_fragment(nsf_path, base_path, sizeof(base_path));
    if (strcmp(base_path, nsf_path) != 0)
    {
        /* Already a canonical `<file>#<idx>` URI */
        return cb(nsf_path, ctx) ? 1 : 0;
    }
    if (!is_nsf_container_path(base_path))
    {
        return cb(nsf_path, ctx) ? 1 : 0;
    }

    char resolved_buf[MAX_PATH];
    const char *phys_path =
        resolve_farm_symlink_fast(base_path, resolved_buf, sizeof(resolved_buf));

    uipod_library_refresh_if_stale();
    if (db)
    {
        const char *qpaths[2] = {
            phys_path,
            (strcmp(phys_path, base_path) != 0) ? base_path : NULL,
        };
        for (int p = 0; p < 2; p++)
        {
            if (!qpaths[p])
                continue;

            int total_rows = 0;
            sqlite3_stmt *cnt_st = NULL;
            if (sqlite3_prepare_v2(
                    db,
                    "SELECT COUNT(*) FROM tracks WHERE source=0 AND file_path=?;",
                    -1, &cnt_st, NULL) == SQLITE_OK)
            {
                sqlite3_bind_text(cnt_st, 1, qpaths[p], -1, SQLITE_STATIC);
                if (sqlite3_step(cnt_st) == SQLITE_ROW)
                    total_rows = sqlite3_column_int(cnt_st, 0);
                sqlite3_finalize(cnt_st);
            }

            if (total_rows > 0)
            {
                int emitted = 0;
                sqlite3_stmt *st = NULL;
                if (sqlite3_prepare_v2(
                        db,
                        "SELECT COALESCE(subtrack_index, 0) FROM tracks"
                        " WHERE source=0 AND file_path=? AND COALESCE(track_kind, 0)=0"
                        " ORDER BY COALESCE(subtrack_index, 0), id;",
                        -1, &st, NULL) == SQLITE_OK)
                {
                    sqlite3_bind_text(st, 1, qpaths[p], -1, SQLITE_STATIC);
                    while (sqlite3_step(st) == SQLITE_ROW)
                    {
                        int sub_idx = sqlite3_column_int(st, 0);
                        if (sub_idx >= 0)
                        {
                            char uri[MAX_PATH + 16];
                            snprintf(uri, sizeof(uri), "%s#%d", phys_path, sub_idx);
                            emitted++;
                            if (!cb(uri, ctx))
                                break;
                        }
                    }
                    sqlite3_finalize(st);
                }

                if (emitted == 0 && fallback_if_empty)
                {
                    char fallback_uri[MAX_PATH + 16];
                    snprintf(fallback_uri, sizeof(fallback_uri), "%s#0", phys_path);
                    if (cb(fallback_uri, ctx))
                        emitted = 1;
                }
                return emitted;
            }
        }
    }

    struct uipod_rs_nsf_container_header hdr;
    struct uipod_rs_nsf_subtrack_info *subs = calloc(256, sizeof(*subs));
    if (!subs)
        return 0;

    memset(&hdr, 0, sizeof(hdr));
    size_t n = header_only_if_unindexed
        ? uipod_rs_nsf_inspect_header_fast(phys_path, NULL, 0, &hdr, subs, 256)
        : uipod_rs_nsf_scan_container(phys_path, NULL, 0, &hdr, subs, 256);

    int emitted = 0;
    int music_total = 0;
    for (size_t k = 0; k < n; k++)
    {
        if (subs[k].track_kind == 0)
            music_total++;
    }
    if (n > 0)
        nsf_count_cache_put(phys_path, music_total);

    for (size_t k = 0; k < n; k++)
    {
        if (subs[k].track_kind == 0)
        {
            char uri[MAX_PATH + 16];
            snprintf(uri, sizeof(uri), "%s#%u",
                     phys_path, (unsigned)subs[k].subtrack_index);
            emitted++;
            if (!cb(uri, ctx))
                break;
        }
    }
    if (emitted == 0 && n > 0 && fallback_if_empty)
    {
        char uri[MAX_PATH + 16];
        unsigned start = (hdr.starting_subtrack < n) ? hdr.starting_subtrack : 0;
        snprintf(uri, sizeof(uri), "%s#%u", phys_path, start);
        if (cb(uri, ctx))
            emitted = 1;
    }
    free(subs);
    return emitted;
}

int uipod_library_expand_nsf_music(const char *nsf_path,
                                   bool fallback_if_empty,
                                   bool (*cb)(const char *uri, void *ctx),
                                   void *ctx)
{
    return expand_nsf_internal(nsf_path, fallback_if_empty, false, cb, ctx);
}

int uipod_library_expand_nsf_fast(const char *nsf_path,
                                  bool fallback_if_empty,
                                  bool (*cb)(const char *uri, void *ctx),
                                  void *ctx)
{
    return expand_nsf_internal(nsf_path, fallback_if_empty, true, cb, ctx);
}

int uipod_library_nsf_music_count(const char *nsf_path)
{
    if (!nsf_path || !nsf_path[0])
        return 0;

    char base_path[MAX_PATH];
    strip_subtrack_fragment(nsf_path, base_path, sizeof(base_path));

    char resolved_buf[MAX_PATH];
    const char *phys_path =
        resolve_farm_symlink_fast(base_path, resolved_buf, sizeof(resolved_buf));

    int cached = 0;
    if (nsf_count_cache_get(phys_path, &cached))
        return cached;

    int count = -1;
    if (db)
    {
        const char *qpaths[2] = {
            phys_path,
            (strcmp(phys_path, base_path) != 0) ? base_path : NULL,
        };

        for (int p = 0; p < 2 && count < 0; p++)
        {
            if (!qpaths[p])
                continue;
            sqlite3_stmt *st = NULL;
            if (sqlite3_prepare_v2(
                    db,
                    "SELECT COUNT(*),"
                    " SUM(CASE WHEN COALESCE(track_kind, 0) = 0 THEN 1 ELSE 0 END)"
                    " FROM tracks WHERE source = 0 AND file_path = ?;",
                    -1, &st, NULL) == SQLITE_OK)
            {
                sqlite3_bind_text(st, 1, qpaths[p], -1, SQLITE_STATIC);
                if (sqlite3_step(st) == SQLITE_ROW)
                {
                    int total = sqlite3_column_int(st, 0);
                    if (total > 0)
                        count = sqlite3_column_int(st, 1);
                }
                sqlite3_finalize(st);
            }
        }
    }

    if (count < 0)
    {
        /* Unindexed directory in File Browser: NEVER run headless 6502 CPU emulation
         * inside a 60 FPS list row callback! Read only the 128-byte `.nsf` header or
         * `NSFE` `plst` chunk via `uipod_rs_nsf_inspect_header_fast` (< 0.05 ms). */
        struct uipod_rs_nsf_container_header hdr;
        struct uipod_rs_nsf_subtrack_info *subs = calloc(256, sizeof(*subs));
        count = 0;
        if (subs)
        {
            memset(&hdr, 0, sizeof(hdr));
            size_t n = uipod_rs_nsf_inspect_header_fast(phys_path, NULL, 0, &hdr, subs, 256);
            for (size_t k = 0; k < n; k++)
            {
                if (subs[k].track_kind == 0)
                    count++;
            }
            free(subs);
        }
    }

    nsf_count_cache_put(phys_path, count);
    return count;
}

int uipod_library_get_loop_point_ms(const char *track_uri)
{
    if (!db || !track_uri || !track_uri[0])
        return -1;

    sqlite3_stmt *st = NULL;
    int lp = -1;
    if (sqlite3_prepare_v2(
            db,
            "SELECT COALESCE(loop_point_ms, -1) FROM tracks WHERE path = ? LIMIT 1;",
            -1, &st, NULL) == SQLITE_OK)
    {
        sqlite3_bind_text(st, 1, track_uri, -1, SQLITE_STATIC);
        if (sqlite3_step(st) == SQLITE_ROW)
            lp = sqlite3_column_int(st, 0);
        sqlite3_finalize(st);
    }
    if (lp >= 0)
        return lp;

    /* Handle symlink-farm URIs such as `/tmp/uipod_music/Game.nsf#3` */
    char base_path[MAX_PATH];
    int sub_idx = uipod_rs_nsf_parse_uri(track_uri, base_path, sizeof(base_path));
    if (sub_idx >= 0)
    {
        char resolved[MAX_PATH];
        const char *real =
            resolve_farm_symlink_fast(base_path, resolved, sizeof(resolved));
        if (real && strcmp(real, base_path) != 0)
        {
            char real_uri[MAX_PATH + 16];
            snprintf(real_uri, sizeof(real_uri), "%s#%d", real, sub_idx);
            if (sqlite3_prepare_v2(
                    db,
                    "SELECT COALESCE(loop_point_ms, -1) FROM tracks WHERE path = ? LIMIT 1;",
                    -1, &st, NULL) == SQLITE_OK)
            {
                sqlite3_bind_text(st, 1, real_uri, -1, SQLITE_STATIC);
                if (sqlite3_step(st) == SQLITE_ROW)
                    lp = sqlite3_column_int(st, 0);
                sqlite3_finalize(st);
            }
        }
    }
    return lp;
}

size_t uipod_library_load_nsf_container(const char *nsf_path,
                                        struct uipod_rs_nsf_container_header *out_header,
                                        struct uipod_rs_nsf_subtrack_info *out_subtracks,
                                        size_t max_subtracks)
{
    if (!nsf_path || !nsf_path[0] || !out_header || !out_subtracks || max_subtracks == 0)
        return 0;

    memset(out_header, 0, sizeof(*out_header));
    char base_path[MAX_PATH];
    strip_subtrack_fragment(nsf_path, base_path, sizeof(base_path));

    char resolved[MAX_PATH];
    const char *phys_path =
        resolve_farm_symlink_fast(base_path, resolved, sizeof(resolved));

    if (db)
    {
        const char *qpaths[2] = {
            phys_path,
            (strcmp(phys_path, base_path) != 0) ? base_path : NULL,
        };

        for (int p = 0; p < 2; p++)
        {
            if (!qpaths[p])
                continue;
            sqlite3_stmt *st = NULL;
            if (sqlite3_prepare_v2(
                    db,
                    "SELECT COALESCE(subtrack_index, 0),"
                    " COALESCE(track_kind, 0),"
                    " COALESCE(chip_flags, 0),"
                    " COALESCE(duration_ms, 0),"
                    " COALESCE(loop_point_ms, -1),"
                    " COALESCE(title, ''),"
                    " COALESCE(album, ''),"
                    " COALESCE(artist, '')"
                    " FROM tracks WHERE source = 0 AND file_path = ?"
                    " ORDER BY COALESCE(subtrack_index, 0);",
                    -1, &st, NULL) == SQLITE_OK)
            {
                sqlite3_bind_text(st, 1, qpaths[p], -1, SQLITE_STATIC);
                size_t n = 0;
                int music_cnt = 0;
                while (n < max_subtracks && sqlite3_step(st) == SQLITE_ROW)
                {
                    int sub_idx        = sqlite3_column_int(st, 0);
                    int kind           = sqlite3_column_int(st, 1);
                    int cflags         = sqlite3_column_int(st, 2);
                    int dur_ms         = sqlite3_column_int(st, 3);
                    int lp_ms          = sqlite3_column_int(st, 4);
                    const char *title  = (const char *)sqlite3_column_text(st, 5);
                    const char *album  = (const char *)sqlite3_column_text(st, 6);
                    const char *artist = (const char *)sqlite3_column_text(st, 7);

                    out_subtracks[n].subtrack_index = (uint16_t)(sub_idx >= 0 ? sub_idx : 0);
                    out_subtracks[n].track_kind     = (uint8_t)(kind != 0 ? 1 : 0);
                    out_subtracks[n].chip_flags     = (uint8_t)(cflags & 0xFF);
                    out_subtracks[n].duration_ms    = (uint32_t)(dur_ms > 0 ? dur_ms : 0);
                    out_subtracks[n].fade_ms        = 0;
                    out_subtracks[n].loop_point_ms  = lp_ms;
                    strlcpy(out_subtracks[n].title, title ? title : "",
                            sizeof(out_subtracks[n].title));
                    if (kind == 0)
                        music_cnt++;

                    if (n == 0)
                    {
                        strlcpy(out_header->game_title, album ? album : "",
                                sizeof(out_header->game_title));
                        strlcpy(out_header->artist, artist ? artist : "",
                                sizeof(out_header->artist));
                        out_header->chip_flags = (uint8_t)(cflags & 0xFF);
                    }
                    n++;
                }
                sqlite3_finalize(st);
                if (n > 0)
                {
                    out_header->total_subtracks = (uint16_t)n;
                    nsf_count_cache_put(phys_path, music_cnt);
                    return n;
                }
            }
        }
    }

    size_t scanned = uipod_rs_nsf_scan_container(phys_path, NULL, 0,
                                                 out_header, out_subtracks, max_subtracks);
    if (scanned > 0)
    {
        int music_cnt = 0;
        size_t lim = scanned < max_subtracks ? scanned : max_subtracks;
        for (size_t k = 0; k < lim; k++)
        {
            if (out_subtracks[k].track_kind == 0)
                music_cnt++;
        }
        nsf_count_cache_put(phys_path, music_cnt);
    }
    return scanned;
}

bool uipod_library_folder_music_paths(const char *dir_path,
                                      bool shuffle,
                                      bool (*cb)(const char *uri, void *ctx),
                                      void *ctx)
{
    if (!dir_path || !dir_path[0] || !cb)
        return false;

    char resolved_buf[MAX_PATH];
    const char *phys_dir =
        resolve_farm_symlink_fast(dir_path, resolved_buf, sizeof(resolved_buf));

    uipod_library_refresh_if_stale();
    if (!db)
        return false;

    /* Check whether `phys_dir` is an indexed directory in `dirs` */
    bool indexed = false;
    sqlite3_stmt *chk = NULL;
    if (sqlite3_prepare_v2(db, "SELECT 1 FROM dirs WHERE path=? LIMIT 1;",
                           -1, &chk, NULL) == SQLITE_OK)
    {
        sqlite3_bind_text(chk, 1, phys_dir, -1, SQLITE_STATIC);
        indexed = (sqlite3_step(chk) == SQLITE_ROW);
        sqlite3_finalize(chk);
    }
    if (!indexed)
        return false;

    const char *sql = shuffle
        ? "SELECT path FROM tracks"
          " WHERE (dir=?1 OR (dir>=?1||'/' AND dir<?1||'0'))"
          " AND COALESCE(track_kind, 0)=0"
          " ORDER BY RANDOM() LIMIT ?2;"
        : "SELECT path FROM tracks"
          " WHERE (dir=?1 OR (dir>=?1||'/' AND dir<?1||'0'))"
          " AND COALESCE(track_kind, 0)=0"
          " ORDER BY dir, file_path, COALESCE(subtrack_index, -1), path"
          " LIMIT ?2;";

    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(db, sql, -1, &st, NULL) != SQLITE_OK)
        return false;

    sqlite3_bind_text(st, 1, phys_dir, -1, SQLITE_STATIC);
    sqlite3_bind_int (st, 2, UIPOD_MAX_FILES_IN_PLAYLIST);
    while (sqlite3_step(st) == SQLITE_ROW)
    {
        const char *u = (const char *)sqlite3_column_text(st, 0);
        if (u && !cb(u, ctx))
            break;
    }
    sqlite3_finalize(st);
    return true;
}

void uipod_library_on_path_deleted(const char *path, bool is_dir)
{
    if (!db || !path || !path[0])
        return;

    sqlite3_stmt *st = NULL;
    if (is_dir)
    {
        if (sqlite3_prepare_v2(
                db,
                "DELETE FROM tracks WHERE dir=?1 OR (dir>=?1||'/' AND dir<?1||'0');",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
            sqlite3_step(st);
            sqlite3_finalize(st);
        }
        if (sqlite3_prepare_v2(
                db,
                "DELETE FROM dirs WHERE path=?1 OR (path>=?1||'/' AND path<?1||'0');",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
            sqlite3_step(st);
            sqlite3_finalize(st);
        }
    }
    else
    {
        if (sqlite3_prepare_v2(
                db,
                "DELETE FROM tracks WHERE file_path=?1 OR path=?1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, path, -1, SQLITE_STATIC);
            sqlite3_step(st);
            sqlite3_finalize(st);
        }
    }

    s_local_gen++;
    db_writeback();
    uipod_library_mark_stale();
}

void uipod_library_on_path_renamed(const char *old_path,
                                   const char *new_path,
                                   bool is_dir)
{
    if (!db || !old_path || !old_path[0] || !new_path || !new_path[0])
        return;
    if (strcmp(old_path, new_path) == 0)
        return;

    db_exec("BEGIN;");
    sqlite3_stmt *st = NULL;

    if (!is_dir)
    {
        char new_dir[MAX_PATH];
        const char *sl = strrchr(new_path, '/');
        snprintf(new_dir, sizeof(new_dir), "%.*s",
                 (int)(sl ? sl - new_path : 0), new_path);

        if (sqlite3_prepare_v2(
                db,
                "DELETE FROM tracks WHERE file_path=?1 OR path=?1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, new_path, -1, SQLITE_STATIC);
            sqlite3_step(st);
            sqlite3_finalize(st);
        }

        if (sqlite3_prepare_v2(
                db,
                "UPDATE tracks SET"
                " path = CASE WHEN subtrack_index IS NOT NULL"
                "        THEN (?2 || '#' || subtrack_index) ELSE ?2 END,"
                " file_path = ?2,"
                " dir = ?3"
                " WHERE file_path = ?1 OR path = ?1;",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, old_path, -1, SQLITE_STATIC);
            sqlite3_bind_text(st, 2, new_path, -1, SQLITE_STATIC);
            sqlite3_bind_text(st, 3, new_dir,  -1, SQLITE_TRANSIENT);
            sqlite3_step(st);
            sqlite3_finalize(st);
        }
    }
    else
    {
        int cut_pos = (int)strlen(old_path) + 1;
        char new_parent[MAX_PATH];
        const char *sl = strrchr(new_path, '/');
        snprintf(new_parent, sizeof(new_parent), "%.*s",
                 (int)(sl ? sl - new_path : 0), new_path);

        if (sqlite3_prepare_v2(
                db,
                "UPDATE tracks SET"
                " path = ?2 || substr(path, ?3),"
                " file_path = ?2 || substr(file_path, ?3),"
                " dir = ?2 || substr(dir, ?3)"
                " WHERE dir = ?1 OR (dir >= ?1||'/' AND dir < ?1||'0');",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, old_path, -1, SQLITE_STATIC);
            sqlite3_bind_text(st, 2, new_path, -1, SQLITE_STATIC);
            sqlite3_bind_int (st, 3, cut_pos);
            sqlite3_step(st);
            sqlite3_finalize(st);
        }

        if (sqlite3_prepare_v2(
                db,
                "UPDATE dirs SET"
                " path = ?2 || substr(path, ?3),"
                " parent = CASE WHEN path = ?1 THEN ?4"
                "          ELSE ?2 || substr(parent, ?3) END"
                " WHERE path = ?1 OR (path >= ?1||'/' AND path < ?1||'0');",
                -1, &st, NULL) == SQLITE_OK)
        {
            sqlite3_bind_text(st, 1, old_path, -1, SQLITE_STATIC);
            sqlite3_bind_text(st, 2, new_path, -1, SQLITE_STATIC);
            sqlite3_bind_int (st, 3, cut_pos);
            bind_str         (st, 4, new_parent);
            sqlite3_step(st);
            sqlite3_finalize(st);
        }
    }

    db_exec("COMMIT;");
    s_local_gen++;
    db_writeback();
    uipod_library_mark_stale();
}

/* ---- open / migrate / init ------------------------------------------- */

static bool tracks_has_column(const char *col_name)
{
    sqlite3_stmt *st = NULL;
    bool found = false;
    if (sqlite3_prepare_v2(db, "PRAGMA table_info(tracks);", -1, &st, NULL) == SQLITE_OK)
    {
        while (sqlite3_step(st) == SQLITE_ROW)
        {
            const char *name = (const char *)sqlite3_column_text(st, 1);
            if (name && strcmp(name, col_name) == 0)
            {
                found = true;
                break;
            }
        }
        sqlite3_finalize(st);
    }
    return found;
}

static bool add_tracks_column_if_missing(const char *col_name, const char *col_ddl)
{
    if (tracks_has_column(col_name))
        return true;
    char sql[256];
    snprintf(sql, sizeof(sql), "ALTER TABLE tracks ADD COLUMN %s;", col_ddl);
    return db_exec(sql) == SQLITE_OK;
}

/* Ordered in-place migrations from an older on-disk schema up to SCHEMA_VERSION.
 * `from` is the stored version; each guard brings it one step forward.  Steps
 * are idempotent (CREATE ... IF NOT EXISTS) so a re-run is harmless. */
static bool db_migrate(int from)
{
    if (from < 2)                                /* v2: index the artist-filtered queries */
        if (db_exec("CREATE INDEX IF NOT EXISTS idx_tracks_browse"
                    " ON tracks(category, browse_artist, album, discnum, tracknum);")
                != SQLITE_OK)
            return false;
    if (from < 3)                                /* v3: index the Songs facet (title order) */
        if (db_exec("CREATE INDEX IF NOT EXISTS idx_tracks_title"
                    " ON tracks(category, title, path);")
                != SQLITE_OK)
            return false;
    if (from < 4)                                /* v4: rows left under removed folders */
    {
        do
            if (db_exec("DELETE FROM dirs WHERE parent IS NOT NULL"
                        " AND parent NOT IN (SELECT path FROM dirs);") != SQLITE_OK)
                return false;
        while (sqlite3_changes(db) > 0);
        if (db_exec("DELETE FROM tracks WHERE dir NOT IN (SELECT path FROM dirs);")
                != SQLITE_OK)
            return false;
    }
    if (from < 5)                                /* v5: native NSF/NSFe multi-subtrack schema */
    {
        if (!add_tracks_column_if_missing("source", "source INTEGER NOT NULL DEFAULT 0"))
            return false;
        if (!add_tracks_column_if_missing("file_path", "file_path TEXT NOT NULL DEFAULT ''"))
            return false;
        if (!add_tracks_column_if_missing("subtrack_index", "subtrack_index INTEGER NULL"))
            return false;
        if (!add_tracks_column_if_missing("track_kind", "track_kind INTEGER NOT NULL DEFAULT 0"))
            return false;
        if (!add_tracks_column_if_missing("chip_flags", "chip_flags INTEGER NOT NULL DEFAULT 0"))
            return false;
        if (!add_tracks_column_if_missing("loop_point_ms", "loop_point_ms INTEGER NULL"))
            return false;

        if (db_exec("UPDATE tracks SET file_path = path"
                    " WHERE file_path = '' OR file_path IS NULL;") != SQLITE_OK)
            return false;

        /* Force parent dirs of legacy single-row .nsf/.nsfe files to rescan & expand */
        if (db_exec("UPDATE dirs SET mtime = 0 WHERE path IN ("
                    "  SELECT DISTINCT dir FROM tracks"
                    "  WHERE subtrack_index IS NULL"
                    "    AND (lower(path) LIKE '%.nsf' OR lower(path) LIKE '%.nsfe'));")
                != SQLITE_OK)
            return false;
        if (db_exec("DELETE FROM tracks"
                    " WHERE subtrack_index IS NULL"
                    "   AND (lower(path) LIKE '%.nsf' OR lower(path) LIKE '%.nsfe');")
                != SQLITE_OK)
            return false;

        if (db_exec("CREATE UNIQUE INDEX IF NOT EXISTS idx_tracks_src_file_sub"
                    " ON tracks(source, file_path, COALESCE(subtrack_index, -1));")
                != SQLITE_OK)
            return false;
        if (db_exec("CREATE INDEX IF NOT EXISTS idx_tracks_file_path"
                    " ON tracks(file_path);") != SQLITE_OK)
            return false;
        if (db_exec("CREATE INDEX IF NOT EXISTS idx_tracks_kind"
                    " ON tracks(category, track_kind);") != SQLITE_OK)
            return false;
    }
    return true;
}

static bool db_setup(void)
{
    db_exec("PRAGMA foreign_keys=ON;");
    db_exec("PRAGMA journal_mode=TRUNCATE;");   /* single writer; exFAT-safe, no WAL */
    db_exec("PRAGMA synchronous=NORMAL;");
    int ver = user_version();
    if (ver == 0)                                /* fresh */
    {
        if (db_exec(SCHEMA_SQL) != SQLITE_OK) return false;
        set_user_version(SCHEMA_VERSION);
    }
    else if (ver < SCHEMA_VERSION)               /* migrate in place */
    {
        if (!db_migrate(ver)) return false;
        set_user_version(SCHEMA_VERSION);
        db_writeback();                          /* or it reruns every boot */
    }
    else if (ver > SCHEMA_VERSION)               /* newer/foreign db -> rebuild */
        return false;
    return true;
}

void uipod_library_init(void)
{
    /* Work on a tmpfs copy of the card's index (see db_writeback); fall back to
     * the card itself if the copy can't be made. */
    remove(DB_RAM);                          /* never inherit a stale copy */
    db_in_ram = copy_file(DB_SD, DB_RAM) || !file_exists(DB_SD);
    const char *dbpath = db_in_ram ? DB_RAM : DB_SD;

    if (sqlite3_open(dbpath, &db) != SQLITE_OK || !db_setup())
    {
        /* Unreadable / incompatible: the SD files are truth, so rebuild once. */
        if (db) sqlite3_close(db);
        db = NULL;
        remove(dbpath);
        if (db_in_ram) remove(DB_SD);
        if (sqlite3_open(dbpath, &db) != SQLITE_OK || !db_setup())
        {
            if (db) { sqlite3_close(db); db = NULL; }
            return;                              /* app still runs without the index */
        }
    }
    s_merged_local_gen  = 0;
    s_merged_remote_gen = 0;
    /* Reconciled on first use, not at boot: a big library's walk is seconds.
     * Every reader refreshes first (uipod_library_refresh_if_stale). */
    uipod_library_mark_stale();
}

void uipod_library_close(void)
{
    free_local_track_cache();
    if (db) { sqlite3_close(db); db = NULL; }
    if (db_in_ram) remove(DB_RAM);
}
