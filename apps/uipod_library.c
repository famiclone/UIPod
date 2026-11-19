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
#include "uipod_ui.h" /* uipod_fullscreen_message */

#define SCHEMA_VERSION 4

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
static int scan_shown = SCAN_HIDDEN;   /* percent on screen, or hidden */

static void scan_show(int pct)
{
    if (pct == scan_shown) return;
    char widest[64], msg[64];
    snprintf(widest, sizeof widest, "%s 100%%", str(LANG_UIPOD_SCANNING));
    snprintf(msg, sizeof msg, "%s %d%%", str(LANG_UIPOD_SCANNING), pct);
    uipod_fullscreen_message(msg, widest);
    backlight_on();
    scan_shown = pct;
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
    "  browse_artist TEXT, sort_artist TEXT);"
    "CREATE INDEX idx_tracks_dir ON tracks(dir);"
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
    "duration_ms,codectype,bitrate,frequency,browse_artist,sort_artist)"
    " VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)"
    " ON CONFLICT(path) DO UPDATE SET dir=?2,root_id=?3,category=?4,mtime=?5,"
    "size=?6,title=?7,artist=?8,albumartist=?9,album=?10,genre=?11,composer=?12,"
    "grouping=?13,year=?14,tracknum=?15,discnum=?16,duration_ms=?17,codectype=?18,"
    "bitrate=?19,frequency=?20,browse_artist=?21,sort_artist=?22;";

struct scan_stmts
{
    sqlite3_stmt *track_sel;    /* SELECT mtime,size FROM tracks WHERE path=? */
    sqlite3_stmt *track_ins;    /* the upsert above                          */
    sqlite3_stmt *track_del;    /* DELETE FROM tracks WHERE path=?            */
    sqlite3_stmt *dir_sel;      /* SELECT mtime FROM dirs WHERE path=?        */
    sqlite3_stmt *dir_ins;      /* upsert into dirs                          */
    sqlite3_stmt *dir_del;      /* DELETE a dir and every dir under it        */
    sqlite3_stmt *subtree_del;  /* DELETE the tracks in a dir and under it    */
    sqlite3_stmt *kids_sel;     /* SELECT path FROM dirs WHERE parent=?       */
    sqlite3_stmt *dtracks_sel;  /* SELECT path FROM tracks WHERE dir=?        */
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
    return prep(&s->track_sel,   "SELECT mtime,size FROM tracks WHERE path=?;")
        && prep(&s->track_ins,   TRACK_UPSERT_SQL)
        && prep(&s->track_del,   "DELETE FROM tracks WHERE path=?;")
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
        && prep(&s->dtracks_sel, "SELECT path FROM tracks WHERE dir=?;");
}
static void scan_stmts_close(struct scan_stmts *s)
{
    sqlite3_finalize(s->track_sel);  sqlite3_finalize(s->track_ins);
    sqlite3_finalize(s->track_del);  sqlite3_finalize(s->dir_sel);
    sqlite3_finalize(s->dir_ins);    sqlite3_finalize(s->dir_del);
    sqlite3_finalize(s->kids_sel);   sqlite3_finalize(s->dtracks_sel);
    sqlite3_finalize(s->subtree_del);
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

static bool parse_and_upsert(sqlite3_stmt *st, const char *path, const char *dir,
                             int root_id, int category, time_t mtime, off_t size)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0) return false;
    static struct mp3entry id3;          /* large; reused (single-threaded scan) */
    bool ok = get_metadata(&id3, fd, path);
    close(fd);
    if (!ok) return false;

    const char *ba = (id3.albumartist && id3.albumartist[0]) ? id3.albumartist
                   : (id3.artist ? id3.artist : "");
    char sortbuf[128];
    make_sort_artist(ba, sortbuf, sizeof sortbuf);

    sqlite3_bind_text (st, 1, path, -1, SQLITE_STATIC);
    sqlite3_bind_text (st, 2, dir,  -1, SQLITE_STATIC);
    sqlite3_bind_int  (st, 3, root_id);
    sqlite3_bind_int  (st, 4, category);
    sqlite3_bind_int64(st, 5, (sqlite3_int64)mtime);
    sqlite3_bind_int64(st, 6, (sqlite3_int64)size);
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
    bool done = sqlite3_step(st) == SQLITE_DONE;
    sqlite3_reset(st);
    return done;
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
#define DB_RAM   "/tmp/uipod-library.db"
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
            if (parse_and_upsert(s.track_ins, it->path, dir, it->root_id,
                                 it->category, it->mtime, it->size))
                touched++;
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

static void build_track_dedup_key(int category,
                                  const char *path,
                                  const char *browse_artist,
                                  const char *album,
                                  int discnum,
                                  int tracknum,
                                  const char *title,
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
    snprintf(out, cap, "m:%s\x1f%s\x1f%d\x1f%d\x1f%s",
             n_artist, n_album, ndisc, ntrk, n_title);
}

static void merge_copy_from_db(sqlite3 *src_db,
                               sqlite3_stmt *ins_stmt,
                               bool require_file_exists)
{
    if (!src_db || !ins_stmt)
        return;

    sqlite3_stmt *sel = NULL;
    if (sqlite3_prepare_v2(
            src_db,
            "SELECT path, category, title, artist, browse_artist, sort_artist,"
            " album, genre, year, tracknum, discnum, COALESCE(duration_ms, 0) FROM tracks;",
            -1, &sel, NULL) != SQLITE_OK)
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

        if (!path || !path[0])
            continue;
        if (filetype_is_video(path))
            continue;
        if (require_file_exists)
        {
            struct stat st;
            if (stat(path, &st) != 0 || !S_ISREG(st.st_mode) || st.st_size <= 0)
                continue;
        }

        char dkey[512];
        build_track_dedup_key(category, path, ba, album, discnum, tracknum,
                              title, dkey, sizeof(dkey));

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
        merge_copy_from_db(db, ins, false);

        /* 2) Navidrome catalog (in Offline Mode, only cached/downloaded files) */
        sqlite3 *nav_db = uipod_navidrome_db_lock();
        if (nav_db)
            merge_copy_from_db(nav_db, ins, offline);
        uipod_navidrome_db_unlock();

        /* 3) Jellyfin music catalog (in Offline Mode, only cached/downloaded files) */
        sqlite3 *jf_db = uipod_jellyfin_db_lock();
        if (jf_db)
            merge_copy_from_db(jf_db, ins, offline);
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
            " WHERE category=? ORDER BY id;",
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

/* ---- open / migrate / init ------------------------------------------- */

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
