/***************************************************************************
 * UIPod: the Playlists screen.
 *
 * A uipod_page that lists the playlists in the catalog directory plus a
 * "+ Add Playlist" row, mirroring the Music Folders settings page:
 *   A on "+ Add Playlist" -> the on-screen keyboard; a confirmed name creates
 *     an empty .m3u8 and the list refreshes.
 *   A on a playlist row    -> its track listing (the playlist viewer); A on a
 *     track there starts playback from it.
 *   Hold A on a playlist   -> a Play/Play Shuffled/Add to Play Queue/Rename/
 *     Edit/Delete context menu; the plays start at once (track list sits
 *     behind Now Playing).
 *   B                      -> leave, back to the main menu.
 *
 * The run loop, header and key whitelist are owned by uipod_page_run; this
 * page only describes the list and what A/B/Hold-A do.
 ****************************************************************************/
#include "config.h"
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>   /* qsort */
#include <string.h>
#include "string-extra.h"
#include "strnatcmp.h"   /* natural alphabetical sort (2 before 10) */
#include "file.h"
#include "dir.h"
#include "pathfuncs.h"
#include "settings.h"
#include "lang.h"
#include "splash.h"
#include "action.h"
#include "gui/list.h"
#include "icon.h"
#include "misc.h"        /* push/pop_current_activity, ACTIVITY_PLAYLISTBROWSER */
#include "root_menu.h"
#include "playlist_catalog.h"
#include "playlist.h"          /* playlist_load/get_track_info/delete/save (Edit) */
#include "audio.h"             /* uipod_resume_if_current */
#include "appevents.h"         /* BUFFER_EVENT_BUFFER_LOW, send_event */
#include "metadata.h"
#include "filetypes.h"         /* FILE_ATTR_AUDIO (track-set inserts) */
#include "kernel.h"            /* current_tick (shuffle seed) */
#include "system.h"            /* ARRAYLEN */
#include "scratch_buf.h"       /* scratch_buffer_get -- playlist_load buffers */
#include "uipod_page.h"
#include "uipod_transition.h"  /* arm the back slide on the B-exit only */
#include "uipod_ui.h"        /* uipod_confirm */
#include "uipod_keyboard.h"  /* the on-screen keyboard */
#include "uipod_m4b.h"       /* an m4b tap resumes at its saved position */
#include "uipod_navidrome.h"
#include "uipod_jellyfin.h"
#include "uipod_downloads.h"
#include "uipod_playlists.h"
#include "uipod_svg.h"

#define PL_MAX        256
#define PL_NAMEBUF    (PL_MAX * 64)   /* packed NUL-terminated names */

#define OFFLINE_PL_DIR "/tmp/uipod_offline_pl"

enum pl_source_dir
{
    PL_SRC_NAV_FAV   = 0,
    PL_SRC_LOCAL     = 1,
    PL_SRC_NAVIDROME = 2,
    PL_SRC_JELLYFIN  = 3,
};

struct pl_item
{
    int     name_off;
    uint8_t src;
};

/* one playlists page at a time -> the listing lives in file-scope statics */
static char           pl_dir[MAX_PATH];         /* the local catalog directory */
static char           pl_namebuf[PL_NAMEBUF];   /* packed NUL-terminated playlist names */
static struct pl_item pl_items[PL_MAX];
static int            pl_count;
static bool           pl_loaded_remote;
static bool           pl_fav_checked_session;

/* Remembered selection for the standalone screen, so re-entering it (incl.
 * returning from Now Playing) re-highlights the playlist you were on.  Empty
 * string = the "+ Add Playlist" row.  Session-only. */
static char pl_last_sel[MAX_PATH];
static bool pl_have_last;

static char pl_pending_dir[MAX_PATH];
static char pl_pending_file[MAX_PATH];
static bool pl_pending_active;
static bool pl_pending_shuffle;
static bool pl_pending_view;

static const char *pl_dir_for_src(uint8_t src)
{
    if (src == PL_SRC_NAV_FAV)
        return ROCKBOX_DIR "/Playlists_Navidrome_Fav";
    if (src == PL_SRC_NAVIDROME)
        return ROCKBOX_DIR "/Playlists_Navidrome";
    if (src == PL_SRC_JELLYFIN)
        return ROCKBOX_DIR "/Playlists_Jellyfin";
    return pl_dir;
}

static const char *pl_entry_filename(int sel)
{
    if (sel < 0 || sel >= pl_count)
        return "";
    return pl_namebuf + pl_items[sel].name_off;
}

static const char *pl_entry_dir(int sel)
{
    if (sel < 0 || sel >= pl_count)
        return pl_dir;
    return pl_dir_for_src(pl_items[sel].src);
}

static void pl_entry_full_path(int sel, char *out, size_t out_len)
{
    path_append(out, pl_entry_dir(sel), pl_entry_filename(sel), out_len);
}

static bool is_playlist(const char *name)
{
    const char *dot = strrchr(name, '.');
    return dot && (!strcasecmp(dot, ".m3u") || !strcasecmp(dot, ".m3u8"));
}

/* True if the playlist file at `path` lists no tracks: empty, comments-only
 * ('#' lines), or unreadable.  Tolerates a leading UTF-8 BOM. */
static bool pl_file_is_empty(const char *path)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return true;

    static const char bom[] = "\xef\xbb\xbf";
    char buf[512];
    ssize_t n;
    int pos = 0;
    bool line_start = true, empty = true;
    while (empty && (n = read(fd, buf, sizeof(buf))) > 0)
    {
        for (ssize_t i = 0; i < n; i++, pos++)
        {
            char c = buf[i];
            if (pos < 3 && c == bom[pos])
                continue;
            if (c == '\n' || c == '\r')
                line_start = true;
            else if (line_start && c != ' ' && c != '\t')
            {
                if (c != '#')
                {
                    empty = false;
                    break;
                }
                line_start = false;         /* comment line: skip to EOL */
            }
        }
    }
    close(fd);
    return empty;
}

/* True if `path` contains at least one non-comment track path that exists on disk. */
static bool pl_file_has_offline_track(const char *path)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return false;
    off_t fsz = filesize(fd);
    if (fsz <= 0 || fsz > 2 * 1024 * 1024)
    {
        close(fd);
        return false;
    }
    char *content = malloc((size_t)fsz + 1);
    if (!content)
    {
        close(fd);
        return false;
    }
    ssize_t n = read(fd, content, (size_t)fsz);
    close(fd);
    if (n <= 0)
    {
        free(content);
        return false;
    }
    content[n] = '\0';

    bool found = false;
    char *cur = content;
    while (*cur)
    {
        char *nl = strchr(cur, '\n');
        size_t len = nl ? (size_t)(nl - cur) : strlen(cur);
        if (len > 0 && cur[len - 1] == '\r')
            len--;
        if (len > 0 && len < MAX_PATH && cur[0] != '#')
        {
            char line[MAX_PATH];
            memcpy(line, cur, len);
            line[len] = '\0';
            if (file_exists(line))
            {
                found = true;
                break;
            }
        }
        if (!nl)
            break;
        cur = nl + 1;
    }
    free(content);
    return found;
}

/* Writes an offline-filtered copy of `src_path` into `OFFLINE_PL_DIR/<filename>`. */
static bool pl_materialize_offline(const char *src_path,
                                   const char *filename,
                                   char *out_dir, size_t dir_cap,
                                   char *out_file, size_t file_cap,
                                   char *out_full, size_t full_cap)
{
    int fd = open(src_path, O_RDONLY);
    if (fd < 0)
        return false;
    off_t fsz = filesize(fd);
    if (fsz <= 0 || fsz > 2 * 1024 * 1024)
    {
        close(fd);
        return false;
    }
    char *content = malloc((size_t)fsz + 1);
    if (!content)
    {
        close(fd);
        return false;
    }
    ssize_t n = read(fd, content, (size_t)fsz);
    close(fd);
    if (n <= 0)
    {
        free(content);
        return false;
    }
    content[n] = '\0';

    if (!dir_exists(OFFLINE_PL_DIR))
        mkdir(OFFLINE_PL_DIR);

    char dst_full[MAX_PATH];
    path_append(dst_full, OFFLINE_PL_DIR, filename, sizeof(dst_full));
    int out = open(dst_full, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (out < 0)
    {
        free(content);
        return false;
    }

    fdprintf(out, "#EXTM3U\n");
    int written = 0;
    char *cur = content;
    while (*cur)
    {
        char *nl = strchr(cur, '\n');
        size_t len = nl ? (size_t)(nl - cur) : strlen(cur);
        if (len > 0 && cur[len - 1] == '\r')
            len--;
        if (len > 0 && len < MAX_PATH && cur[0] != '#')
        {
            char line[MAX_PATH];
            memcpy(line, cur, len);
            line[len] = '\0';
            if (file_exists(line))
            {
                fdprintf(out, "%s\n", line);
                written++;
            }
        }
        if (!nl)
            break;
        cur = nl + 1;
    }
    close(out);
    free(content);

    if (written <= 0)
        return false;

    if (out_dir)  strlcpy(out_dir,  OFFLINE_PL_DIR, dir_cap);
    if (out_file) strlcpy(out_file, filename,       file_cap);
    if (out_full) strlcpy(out_full, dst_full,       full_cap);
    return true;
}

/* qsort comparator: Favourites pinned first, then natural alphabetical order */
static int pl_item_cmp(const void *a, const void *b)
{
    const struct pl_item *ia = (const struct pl_item *)a;
    const struct pl_item *ib = (const struct pl_item *)b;
    if ((ia->src == PL_SRC_NAV_FAV) != (ib->src == PL_SRC_NAV_FAV))
        return (ia->src == PL_SRC_NAV_FAV) ? -1 : 1;
    int c = strnatcasecmp(pl_namebuf + ia->name_off, pl_namebuf + ib->name_off);
    if (c != 0)
        return c;
    return (int)ia->src - (int)ib->src;
}

static bool pl_has_filename_ci(const char *fname)
{
    for (int i = 0; i < pl_count; i++)
    {
        if (pl_items[i].src == PL_SRC_NAV_FAV)
            continue;
        if (strcasecmp(pl_namebuf + pl_items[i].name_off, fname) == 0)
            return true;
    }
    return false;
}

static void pl_scan_dir_into(const char *dir_path, uint8_t src, int *used, bool offline)
{
    DIR *d = opendir(dir_path);
    if (!d)
        return;
    struct dirent *e;
    while ((e = readdir(d)))
    {
        if (e->d_name[0] == '.' || !is_playlist(e->d_name))
            continue;
        if (pl_has_filename_ci(e->d_name))
            continue;
        if (offline && src != PL_SRC_LOCAL)
        {
            char full[MAX_PATH];
            path_append(full, dir_path, e->d_name, sizeof(full));
            if (!pl_file_has_offline_track(full))
                continue;
        }
        int len = (int)strlen(e->d_name) + 1;
        if (pl_count >= PL_MAX || *used + len > (int)sizeof(pl_namebuf))
            break;
        pl_items[pl_count].name_off = *used;
        pl_items[pl_count].src      = src;
        pl_count++;
        memcpy(pl_namebuf + *used, e->d_name, (size_t)len);
        *used += len;
    }
    closedir(d);
}

/* (Re)read local + Favourites + synced Navidrome/Jellyfin playlist directories, sorted. */
static void pl_load(void)
{
    pl_count = 0;
    int used = 0;
    bool offline = uipod_offline_mode_get();

    catalog_get_directory(pl_dir, sizeof(pl_dir));   /* also creates it if absent */
    pl_scan_dir_into(pl_dir, PL_SRC_LOCAL, &used, offline);

    if (pl_loaded_remote)
    {
        const char *fav_path = uipod_navidrome_favourites_path();
        if (!offline && !pl_fav_checked_session &&
            uipod_navidrome_is_configured() && pl_file_is_empty(fav_path))
        {
            pl_fav_checked_session = true;
            uipod_navidrome_sync_favourites(false);
        }

        bool show_fav = offline ? pl_file_has_offline_track(fav_path)
                                : (uipod_navidrome_is_configured() || !pl_file_is_empty(fav_path));
        if (show_fav)
        {
            static const char fav_fname[] = "Favourites.m3u8";
            int len = (int)sizeof(fav_fname);
            if (pl_count < PL_MAX && used + len <= (int)sizeof(pl_namebuf))
            {
                pl_items[pl_count].name_off = used;
                pl_items[pl_count].src      = PL_SRC_NAV_FAV;
                pl_count++;
                memcpy(pl_namebuf + used, fav_fname, (size_t)len);
                used += len;
            }
        }

        pl_scan_dir_into(ROCKBOX_DIR "/Playlists_Navidrome", PL_SRC_NAVIDROME, &used, offline);
        pl_scan_dir_into(ROCKBOX_DIR "/Playlists_Jellyfin",  PL_SRC_JELLYFIN,  &used, offline);
    }
    qsort(pl_items, (size_t)pl_count, sizeof(pl_items[0]), pl_item_cmp);
}

struct playlists_page
{
    struct uipod_page base;
    struct gui_synclist lists;
    int result;                       /* GO_TO_* handed back to the root dispatch */

    /* "pick" mode (Hold-A = Add to Playlist): the chosen or newly created
     * playlist receives the selection via the stock Rockbox catalog_insert_into().
     * Either a single (pick_sel, pick_attr) -- a file appended, a directory
     * expanded into its tracks -- OR, when pick_paths is set, an explicit set of
     * track paths (a library album/artist has no single path to expand).
     * Normal mode (pick=false) is the standalone Playlists screen. */
    bool         pick;
    const char  *pick_sel;
    int          pick_attr;
    char       **pick_paths;          /* non-NULL -> add these tracks instead */
    int          pick_npaths;
};

/* Copy playlist row `sel`'s filename with its .m3u8/.m3u extension stripped --
 * the display name, reused for the list rows and the delete confirmation. */
static void pl_display_name(int sel, char *out, size_t out_len)
{
    if (sel >= 0 && sel < pl_count && pl_items[sel].src == PL_SRC_NAV_FAV)
    {
        strlcpy(out, str(LANG_UIPOD_FAVOURITES), out_len);
        return;
    }
    strlcpy(out, pl_entry_filename(sel), out_len);
    char *dot = strrchr(out, '.');
    if (dot && (!strcasecmp(dot, ".m3u8") || !strcasecmp(dot, ".m3u")))
        *dot = '\0';
}

/* rows: [0, pl_count) playlists, then the "+ Add Playlist" row at pl_count */
static const char *pl_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    (void)data;
    if (sel == pl_count)
        return str(LANG_UIPOD_ADD_PLAYLIST_ROW);
    if (sel < 0 || sel > pl_count)
        return "";
    pl_display_name(sel, buf, buf_len);   /* no extension shown */
    return buf;
}

static void playlists_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct playlists_page *)self)->lists);
}

static int playlists_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct playlists_page *)self)->lists, &action);
    return action;
}

/* Name a new playlist with the on-screen keyboard and create an empty .m3u8.
 * The keyboard yields a bare name (its glyph set can't produce path separators),
 * so the file is just <catalog>/<name>.m3u8.  Cancel/empty leaves the list as-is. */
static void add_playlist(struct playlists_page *p)
{
    char name[MAX_PATH];
    name[0] = '\0';
    if (uipod_kbd_input(name, sizeof(name)) != 0 || name[0] == '\0')
        return;

    char path[MAX_PATH];
    snprintf(path, sizeof(path), "%s/%s.m3u8", pl_dir, name);

    if (file_exists(path) && !uipod_confirm(str(LANG_UIPOD_OVERWRITE_PLAYLIST_Q), name))
        return;

    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd >= 0)
        close(fd);
    else
        splash(HZ, ID2P(LANG_UIPOD_PLAYLIST_CREATE_FAILED));
    pl_load();
    gui_synclist_set_nb_items(&p->lists, pl_count + 1);
}

/* ---- Edit Playlist: track list, Y removes the highlighted track --------- *
 * Taps the stock playlist API (playlist_load / playlist_delete / playlist_save)
 * on the separate on-disk playlist_info, so it never disturbs playback -- the
 * same path the playlist viewer uses.  Deletions apply in memory and are written
 * back once on exit. */

struct editor_page
{
    struct uipod_page   base;
    struct gui_synclist   lists;
    struct playlist_info *pl;
    char  path[MAX_PATH];        /* full .m3u8 path, for the save-on-exit */
    bool  dirty;
};

/* track display name = the file's basename */
static void editor_track_name(struct editor_page *p, int sel, char *out, size_t len)
{
    struct playlist_track_info info;
    if (p->pl && playlist_get_track_info(p->pl, sel, &info) == 0)
    {
        const char *base = strrchr(info.filename, PATH_SEPCH);
        strlcpy(out, base ? base + 1 : info.filename, len);
    }
    else
        out[0] = '\0';
}

static const char *editor_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    editor_track_name((struct editor_page *)data, sel, buf, buf_len);
    return buf;
}

static const struct uipod_svg_icon *editor_get_svg_icon(int sel, void *data)
{
    struct editor_page *p = (struct editor_page *)data;
    struct playlist_track_info info;
    if (p->pl && playlist_get_track_info(p->pl, sel, &info) == 0)
        return uipod_svg_for_path(info.filename, false);
    return NULL;
}

/* No legend: B=back is the universal convention and Remove lives in the Hold-A
 * context submenu. */

static void editor_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct editor_page *)self)->lists);
}

static int editor_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct editor_page *)self)->lists, &action);
    return action;
}

static enum uipod_page_result editor_on_action(struct uipod_page *self, int action)
{
    struct editor_page *p = (struct editor_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);
    int n   = p->pl ? playlist_amount_ex(p->pl) : 0;

    if (action == ACTION_STD_CANCEL)             /* B: leave (saved on exit) */
        return UIPOD_PAGE_DONE;

    if (action == ACTION_STD_CONTEXT && sel >= 0 && sel < n)   /* Hold A: context */
    {
        char name[MAX_PATH];
        editor_track_name(p, sel, name, sizeof(name));
        static const char *const opts[] = { ID2P(LANG_UIPOD_REMOVE_FROM_PLAYLIST) };
        if (uipod_context_menu(name, opts, 1) == 0 &&
            uipod_confirm(str(LANG_UIPOD_REMOVE_FROM_PLAYLIST_Q), name) &&
            playlist_delete(p->pl, sel) == 0)
        {
            p->dirty = true;
            int left = playlist_amount_ex(p->pl);
            gui_synclist_set_nb_items(&p->lists, left);
            if (sel >= left && left > 0)
                gui_synclist_select_item(&p->lists, left - 1);
        }
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable editor_vtable =
{
    .legend    = NULL,
    .draw      = editor_draw,
    .poll      = editor_poll,
    .on_action = editor_on_action,
};

/* only Hold-A (context: remove) and B (back) act; scrolling is consumed in poll */
static const int editor_allowed[] = { ACTION_STD_CONTEXT, ACTION_STD_CANCEL, -1 };

/* Edit playlist `sel`: load it, let the user remove tracks, save if changed. */
static void edit_playlist(int sel)
{
    size_t bufsz;
    char *buf = scratch_buffer_get(&bufsz);
    if (!buf || bufsz < 8192)
    {
        splash(HZ, ID2P(LANG_UIPOD_PLAYLIST_EDIT_FAILED));
        return;
    }

    struct editor_page p =
    {
        .base = { .vt = &editor_vtable, .context = CONTEXT_LIST,
                  .allowed = editor_allowed },
    };
    pl_entry_full_path(sel, p.path, sizeof(p.path));

    int index_sz = (int)playlist_get_index_bufsz(bufsz - 4096);
    p.pl = playlist_load(pl_entry_dir(sel), pl_entry_filename(sel),
                         buf, index_sz, buf + index_sz, (int)(bufsz - index_sz));
    if (!p.pl)
    {
        splash(HZ, ID2P(LANG_UIPOD_PLAYLIST_LOAD_FAILED));
        return;
    }

    char title[MAX_PATH];
    pl_display_name(sel, title, sizeof(title));
    gui_synclist_init(&p.lists, editor_get_name, &p, false, 1, NULL);
    gui_synclist_set_svg_icon_callback(&p.lists, editor_get_svg_icon);
    gui_synclist_set_title(&p.lists, title, Icon_Playlist);
    gui_synclist_set_nb_items(&p.lists, playlist_amount_ex(p.pl));
    gui_synclist_select_item(&p.lists, 0);

    push_current_activity(ACTIVITY_PLAYLISTBROWSER);
    uipod_page_run(&p.base);
    pop_current_activity();

    if (p.dirty)
    {
        if (playlist_amount_ex(p.pl) > 0)
            playlist_save(p.pl, p.path);
        else
        {   /* emptied: playlist_save refuses 0 tracks -- truncate instead */
            int fd = open(p.path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
            if (fd >= 0)
                close(fd);
        }
    }
    playlist_close(p.pl);
}

/* ---- the live play queue -------------------------------------------------
 * Tracks go into the current playlist at `position`: PLAYLIST_INSERT (Play
 * Next: after the current track, successive picks in order while that track
 * stays current), PLAYLIST_INSERT_LAST (Add to Play Queue: the end) or
 * PLAYLIST_REPLACE (a new queue).  With no queue the selection becomes one
 * and starts playing.  Adds are tmpfs journal lines (rbpaths.h), so any size
 * costs no card I/O.  Inserts are silent (progress=false): the user stays on
 * the current screen, so the "Inserted N" modal would just flash over it. */

/* The saved queue is only loaded into RAM by Resume Playback, so until then
 * playlist_amount() is 0 although a queue exists; judge by the resume point. */
static bool queue_exists(void)
{
    return playlist_amount() > 0 || global_status.resume_index != -1;
}

bool uipod_resume_if_current(const char *path)
{
    int status = audio_status();
    struct mp3entry *id3 = audio_current_track();
    if (!(status & AUDIO_STATUS_PLAY) || !id3 || strcmp(id3->path, path) != 0)
        return false;
    if (status & AUDIO_STATUS_PAUSE)
        audio_resume();
    return true;
}

/* False when declined or the control file is unavailable: nothing to close. */
bool uipod_queue_begin(struct uipod_queue *q, int position)
{
    memset(q, 0, sizeof *q);
    bool replace = position == PLAYLIST_REPLACE;
    if (replace && !warn_on_pl_erase())
        return false;
    q->fresh = replace || !queue_exists();
    if (!q->fresh && playlist_amount() <= 0 && playlist_resume() == -1)
        q->fresh = true;                           /* unloadable: start over */
    if (q->fresh && playlist_create(NULL, NULL) == -1)
        return false;
    /* Play Next: the engine chains PLAYLIST_INSERT after its last insert, a
     * cursor that survives skips back and repeat wrap-around.  Chain only
     * while the same track is current; otherwise start right after it. */
    static int chain_track = -1;
    if (position == PLAYLIST_INSERT)
    {
        int cur = q->fresh ? -1 : playlist_get_display_index();
        if (cur != chain_track)
            position = PLAYLIST_INSERT_FIRST;
        chain_track = cur;
    }
    q->pos = replace ? PLAYLIST_INSERT_LAST : position;   /* new queue: appends */
    if (playlist_insert_context_create(playlist_get_current(), &q->ctx,
                                       q->pos, false, false) < 0)
    {
        playlist_insert_context_release(&q->ctx);  /* a failed create still locks */
        return false;
    }
    return true;
}

bool uipod_queue_add_path(const char *path, void *q)
{
    struct uipod_queue *b = q;
    yield();                                   /* keep the codec fed */
    return playlist_insert_context_add(&b->ctx, path) >= 0;
}

/* (Re)start the current playlist at its first track, shuffled first if
 * `shuffle`: built queues, saved-playlist plays and Now Playing's Shuffle. */
static void queue_start(bool shuffle, unsigned long elapsed,
                        unsigned long offset)
{
    global_settings.playlist_shuffle = shuffle;    /* the queue's order */
    replaygain_update();                           /* track gain if shuffling */
    if (shuffle)
        playlist_shuffle(current_tick);
    playlist_start(playlist_get_first_index(NULL), elapsed, offset);
}

/* ---- 3-by-3 Windowed Background Prefetch Coordinator ------------------ */

static struct mutex s_pf_mutex;
static bool         s_pf_mutex_ready;
static bool         s_pf_active;
static int          s_pf_win_start;
static int          s_pf_win_end;

static void pf_lock(void)
{
    if (!s_pf_mutex_ready)
    {
        mutex_init(&s_pf_mutex);
        s_pf_mutex_ready = true;
    }
    mutex_lock(&s_pf_mutex);
}

static void pf_unlock(void)
{
    mutex_unlock(&s_pf_mutex);
}

static void pf_dispatch_steps(int steps)
{
    if (steps <= 0 || uipod_offline_mode_get())
        return;
    uipod_navidrome_prefetch_steps(steps);
    uipod_jellyfin_prefetch_steps(steps);
}

void uipod_prefetch_on_queue_start(void)
{
    int pos = playlist_get_display_index() - 1;
    if (pos < 0)
        pos = 0;

    pf_lock();
    s_pf_active    = true;
    s_pf_win_start = pos;
    s_pf_win_end   = pos + 2;   /* Track `pos` playing + 2 prefetched = 3 tracks */
    pf_unlock();

    pf_dispatch_steps(2);
}

bool uipod_prefetch_on_track_change(void)
{
    int pos = playlist_get_display_index() - 1;
    if (pos < 0)
        return false;

    int dispatch = 0;
    bool window_changed = false;

    pf_lock();
    if (!s_pf_active || pos < s_pf_win_start || pos > s_pf_win_end)
    {
        /* Manual skip outside the active window (or uninitialized):
         * start a fresh [pos .. pos + 2] 3-track window. */
        s_pf_active    = true;
        s_pf_win_start = pos;
        s_pf_win_end   = pos + 2;
        dispatch       = 2;
        window_changed = true;
    }
    else if (pos == s_pf_win_end)
    {
        /* The 3rd track of the current window just started playing:
         * prefetch the next 3 tracks [pos + 1 .. pos + 3]! */
        s_pf_win_start = pos;
        s_pf_win_end   = pos + 3;
        dispatch       = 3;
        window_changed = true;
    }
    pf_unlock();

    if (dispatch > 0)
        pf_dispatch_steps(dispatch);

    return window_changed;
}

bool uipod_prefetch_allow_peek_offset(int peek_offset)
{
    if (peek_offset <= 0)
        return true;

    int pos = playlist_get_display_index() - 1;
    if (pos < 0)
        pos = 0;

    pf_lock();
    bool allow = s_pf_active ? ((pos + peek_offset) <= s_pf_win_end)
                             : (peek_offset <= 2);
    pf_unlock();
    return allow;
}

static void pf_refresh_live_queue_insert(void)
{
    if (!(audio_status() & AUDIO_STATUS_PLAY))
        return;

    int pos = playlist_get_display_index() - 1;
    if (pos < 0)
        return;

    int rem_steps = 0;
    pf_lock();
    if (!s_pf_active || pos < s_pf_win_start || pos > s_pf_win_end)
    {
        s_pf_active    = true;
        s_pf_win_start = pos;
        s_pf_win_end   = pos + 2;
    }
    rem_steps = s_pf_win_end - pos;
    pf_unlock();

    if (rem_steps > 3)
        rem_steps = 3;
    if (rem_steps > 0)
        pf_dispatch_steps(rem_steps);
    send_event(BUFFER_EVENT_BUFFER_LOW, NULL);
}

/* A fresh queue starts playing, shuffled first if `shuffle`; returns whether
 * it did.  The one start for built queues. */
bool uipod_queue_end(struct uipod_queue *q, bool shuffle)
{
    playlist_insert_context_release(&q->ctx);
    if (!q->fresh)
    {
        if (playlist_amount() > 0)
            pf_refresh_live_queue_insert();
        return false;
    }
    if (playlist_amount() <= 0)
        return false;
    queue_start(shuffle, q->elapsed, q->offset);
    return true;
}

bool uipod_play_track(const char *path)
{
    if (!path || !path[0])
        return false;
    char *arr[1] = { (char *)path };
    return uipod_play_tracks_at(arr, 1, 0);
}

bool uipod_play_tracks_at(char *const *paths, int count, int start_idx)
{
    if (!paths || count <= 0)
        return false;
    if (start_idx < 0 || start_idx >= count)
        start_idx = 0;

    const char *start_path = paths[start_idx];
    if (!start_path || !start_path[0])
        return false;

    /* Only shortcut to Now Playing without rebuilding if the live queue
     * already matches this track list size and is playing `start_path`! */
    if (playlist_amount() == count && uipod_resume_if_current(start_path))
        return true;

    struct uipod_queue q;
    if (!uipod_queue_begin(&q, PLAYLIST_REPLACE))
        return false;

    uipod_m4b_resume_get(start_path, &q.elapsed, &q.offset);

    int slice_begin = 0;
    int slice_end   = count;
    if (count > UIPOD_MAX_FILES_IN_PLAYLIST)
    {
        slice_begin = start_idx - (UIPOD_MAX_FILES_IN_PLAYLIST / 2);
        if (slice_begin < 0)
            slice_begin = 0;
        if (slice_begin + UIPOD_MAX_FILES_IN_PLAYLIST > count)
            slice_begin = count - UIPOD_MAX_FILES_IN_PLAYLIST;
        slice_end = slice_begin + UIPOD_MAX_FILES_IN_PLAYLIST;
    }

    int inserted = 0;
    int target_pl_idx = 0;
    for (int i = slice_begin; i < slice_end; i++)
    {
        if (!paths[i] || !paths[i][0])
            continue;
        if (!uipod_queue_add_path(paths[i], &q))
            break;
        if (i == start_idx)
            target_pl_idx = inserted;
        inserted++;
    }

    playlist_insert_context_release(&q.ctx);
    if (!q.fresh || playlist_amount() <= 0 || inserted <= 0)
        return false;

    global_settings.playlist_shuffle = false;
    replaygain_update();
    playlist_start(target_pl_idx, q.elapsed, q.offset);
    return true;
}

void uipod_queue_reshuffle(void)
{
    queue_start(true, 0, 0);
}

/* tracksearch callback: the first audio file ends the walk */
static int first_audio_cb(char *name, void *found)
{
    (void)name;
    *(bool *)found = true;
    return -1;
}

bool uipod_queue_add(const char *path, int attr, int position, bool shuffle)
{
    struct uipod_queue q;
    bool replace = position == PLAYLIST_REPLACE;
    if ((attr & ATTR_DIRECTORY) && (replace || !queue_exists()))
    {
        /* About to build a queue from it: a folder with no audio anywhere
         * under it is a no-op, not a new empty queue. */
        bool found = false;
        playlist_directory_tracksearch(path, true, first_audio_cb, &found);
        if (!found)
        {
            if (replace)
                splash(HZ, ID2P(LANG_UIPOD_NO_MUSIC));
            return false;
        }
    }
    if (!uipod_queue_begin(&q, position))
        return false;
    if (attr & ATTR_DIRECTORY)
        playlist_insert_directory(NULL, path, q.pos, false, true, &q.ctx);
    else
        playlist_insert_context_add(&q.ctx, path);
    return uipod_queue_end(&q, shuffle);
}

/* At most the cap; stops at the first refusal (the engine splashes). */
static bool queue_add_tracks(char **paths, int count, int position,
                             bool shuffle)
{
    struct uipod_queue q;
    if (!uipod_queue_begin(&q, position))
        return false;
    for (int i = 0; i < count && i < UIPOD_MAX_FILES_IN_PLAYLIST; i++)
        if (!uipod_queue_add_path(paths[i], &q))
            break;
    return uipod_queue_end(&q, shuffle);
}

static void queue_add_playlist(const char *path)
{
    struct uipod_queue q;
    if (!uipod_queue_begin(&q, PLAYLIST_INSERT_LAST))
        return;
    playlist_entries_iterate(path, &q.ctx, NULL);
    uipod_queue_end(&q, false);
}

static bool has_downloadable_tracks(char *const *paths, int count)
{
    if (uipod_offline_mode_get())
        return false;
    for (int i = 0; i < count; i++)
    {
        const char *p = paths[i];
        if (p && (uipod_navidrome_is_track_path(p) ||
                  uipod_jellyfin_is_track_path(p)))
        {
            return true;
        }
    }
    return false;
}

static void download_mixed_tracks(char *const *paths, int count)
{
    if (!paths || count <= 0)
        return;

    char **nav_list = malloc((size_t)count * sizeof(*nav_list));
    char **jf_list  = malloc((size_t)count * sizeof(*jf_list));
    int nav_n = 0, jf_n = 0;

    if (nav_list && jf_list)
    {
        for (int i = 0; i < count; i++)
        {
            char *p = paths[i];
            if (!p)
                continue;
            if (uipod_navidrome_is_track_path(p))
                nav_list[nav_n++] = p;
            else if (uipod_jellyfin_is_track_path(p))
                jf_list[jf_n++] = p;
        }

        if (nav_n > 0)
            uipod_navidrome_download_tracks(nav_list, nav_n);
        if (jf_n > 0 && !uipod_home_pending)
            uipod_jellyfin_download_tracks(jf_list, jf_n);
    }

    free(nav_list);
    free(jf_list);
}

/* The per-playlist Hold-A context menu, titled with the playlist's name.
 * uipod_context_menu returns the row index or -1 on cancel. */
static void download_playlist_file(const char *m3u_path)
{
    int fd = open(m3u_path, O_RDONLY);
    if (fd < 0)
        return;
    off_t fsz = filesize(fd);
    if (fsz <= 0 || fsz > 2 * 1024 * 1024)
    {
        close(fd);
        return;
    }
    char *content = malloc((size_t)fsz + 1);
    if (!content)
    {
        close(fd);
        return;
    }
    ssize_t n = read(fd, content, (size_t)fsz);
    close(fd);
    if (n <= 0)
    {
        free(content);
        return;
    }
    content[n] = '\0';

    char **paths = NULL;
    int count = 0, cap = 0;
    char *cursor = content;
    while (*cursor && count < UIPOD_MAX_FILES_IN_PLAYLIST)
    {
        char *nl = strchr(cursor, '\n');
        size_t len = nl ? (size_t)(nl - cursor) : strlen(cursor);
        if (len > 0 && cursor[len - 1] == '\r')
            len--;
        if (len > 0 && cursor[0] != '#')
        {
            if (count == cap)
            {
                int ncap = cap ? cap * 2 : 32;
                char **nv = realloc(paths, (size_t)ncap * sizeof(*nv));
                if (!nv)
                    break;
                paths = nv;
                cap = ncap;
            }
            char *line = malloc(len + 1);
            if (line)
            {
                memcpy(line, cursor, len);
                line[len] = '\0';
                paths[count++] = line;
            }
        }
        if (!nl)
            break;
        cursor = nl + 1;
    }
    free(content);

    if (count > 0)
        download_mixed_tracks(paths, count);

    for (int i = 0; i < count; i++)
        free(paths[i]);
    free(paths);
}

enum { PL_ACT_PLAY = 0, PL_ACT_SHUFFLED, PL_ACT_QUEUE,
       PL_ACT_RENAME, PL_ACT_EDIT, PL_ACT_DELETE, PL_ACT_DOWNLOAD };
static const char *const playlist_action_opts[] =
    { ID2P(LANG_PLAY), ID2P(LANG_UIPOD_PLAY_SHUFFLED),
      ID2P(LANG_UIPOD_ADD_TO_PLAY_QUEUE),
      ID2P(LANG_RENAME), ID2P(LANG_EDIT), ID2P(LANG_DELETE),
      ID2P(LANG_UIPOD_DOWNLOAD) };

/* Open the context menu for playlist `sel`; returns UIPOD_PAGE_DONE when a
 * chosen action leaves the page (play -> Now Playing), else STAY. */
static enum uipod_page_result playlist_action(struct playlists_page *p, int sel)
{
    char path[MAX_PATH], name[MAX_PATH];
    const char *entry_dir  = pl_entry_dir(sel);
    const char *entry_file = pl_entry_filename(sel);
    pl_entry_full_path(sel, path, sizeof(path));
    pl_display_name(sel, name, sizeof(name));

    bool offline = uipod_offline_mode_get();

    /* Synthetic Navidrome Favourites playlist menu */
    if (pl_items[sel].src == PL_SRC_NAV_FAV)
    {
        const char *fav_opts[5];
        int fav_codes[5];
        int fn = 0;
        fav_opts[fn] = ID2P(LANG_PLAY);                    fav_codes[fn++] = PL_ACT_PLAY;
        fav_opts[fn] = ID2P(LANG_UIPOD_PLAY_SHUFFLED);     fav_codes[fn++] = PL_ACT_SHUFFLED;
        fav_opts[fn] = ID2P(LANG_UIPOD_ADD_TO_PLAY_QUEUE); fav_codes[fn++] = PL_ACT_QUEUE;
        if (!offline)
        {
            fav_opts[fn] = ID2P(LANG_UIPOD_REFRESH);       fav_codes[fn++] = 100;
            fav_opts[fn] = ID2P(LANG_UIPOD_DOWNLOAD);      fav_codes[fn++] = PL_ACT_DOWNLOAD;
        }
        int pick = uipod_context_menu(name, fav_opts, fn);
        if (pick < 0 || pick >= fn)
            return UIPOD_PAGE_STAY;
        int act_code = fav_codes[pick];
        if (act_code == 100)
        {
            uipod_navidrome_sync_favourites(true);
            pl_load();
            gui_synclist_set_nb_items(&p->lists, pl_count + 1);
            return UIPOD_PAGE_STAY;
        }
        if (act_code == PL_ACT_PLAY || act_code == PL_ACT_SHUFFLED)
        {
            if (!warn_on_pl_erase())
                return UIPOD_PAGE_STAY;
            if (offline)
            {
                if (!pl_materialize_offline(path, entry_file,
                                            pl_pending_dir, sizeof(pl_pending_dir),
                                            pl_pending_file, sizeof(pl_pending_file),
                                            NULL, 0))
                {
                    splash(HZ, ID2P(LANG_UIPOD_PLAYLIST_EMPTY));
                    return UIPOD_PAGE_STAY;
                }
            }
            else
            {
                strlcpy(pl_pending_dir,  entry_dir,  sizeof pl_pending_dir);
                strlcpy(pl_pending_file, entry_file, sizeof pl_pending_file);
            }
            pl_pending_shuffle = (act_code == PL_ACT_SHUFFLED);
            pl_pending_active  = true;
            p->result = GO_TO_PLAYLIST_VIEWER;
            return UIPOD_PAGE_DONE;
        }
        if (act_code == PL_ACT_QUEUE)
        {
            char use_path[MAX_PATH];
            if (offline)
            {
                if (pl_materialize_offline(path, entry_file, NULL, 0, NULL, 0,
                                           use_path, sizeof(use_path)))
                    queue_add_playlist(use_path);
            }
            else
            {
                queue_add_playlist(path);
            }
            return UIPOD_PAGE_STAY;
        }
        if (act_code == PL_ACT_DOWNLOAD)
        {
            download_playlist_file(path);
            return UIPOD_PAGE_STAY;
        }
        return UIPOD_PAGE_STAY;
    }

    bool remote_src = !offline &&
                      ((pl_items[sel].src != PL_SRC_LOCAL) ||
                       uipod_navidrome_has_tracks() ||
                       uipod_jellyfin_has_tracks());
    int n_opts = ARRAYLEN(playlist_action_opts) - (remote_src ? 0 : 1);
    int act = uipod_context_menu(name, playlist_action_opts, n_opts);
    switch (act)
    {
        case PL_ACT_PLAY:
        case PL_ACT_SHUFFLED:
        {
            if (!warn_on_pl_erase())            /* about to replace the playlist */
                break;                          /* declined: stay on the list */
            if (offline)
            {
                if (!pl_materialize_offline(path, entry_file,
                                            pl_pending_dir, sizeof(pl_pending_dir),
                                            pl_pending_file, sizeof(pl_pending_file),
                                            NULL, 0))
                {
                    splash(HZ, ID2P(LANG_UIPOD_PLAYLIST_EMPTY));
                    break;
                }
            }
            else
            {
                strlcpy(pl_pending_dir,  entry_dir,  sizeof pl_pending_dir);
                strlcpy(pl_pending_file, entry_file, sizeof pl_pending_file);
            }
            pl_pending_shuffle = (act == PL_ACT_SHUFFLED);
            pl_pending_active = true;
            p->result = GO_TO_PLAYLIST_VIEWER;
            return UIPOD_PAGE_DONE;
        }
        case PL_ACT_QUEUE:
        {
            char use_path[MAX_PATH];
            if (offline)
            {
                if (pl_materialize_offline(path, entry_file, NULL, 0, NULL, 0,
                                           use_path, sizeof(use_path)))
                    queue_add_playlist(use_path);
            }
            else
            {
                queue_add_playlist(path);       /* the saved file is never edited */
            }
            break;
        }
        case PL_ACT_RENAME:
        {
            if (uipod_kbd_input(name, sizeof(name)) == 0 && name[0])
            {
                char newpath[MAX_PATH];
                snprintf(newpath, sizeof(newpath), "%s/%s.m3u8", entry_dir, name);
                if (strcmp(newpath, path) == 0)
                    break;                              /* unchanged */
                if (file_exists(newpath))
                    splash(HZ, ID2P(LANG_UIPOD_NAME_EXISTS));
                else if (rename(path, newpath) == 0)
                {
                    pl_load();
                    gui_synclist_set_nb_items(&p->lists, pl_count + 1);
                }
                else
                    splash(HZ, ID2P(LANG_UIPOD_RENAME_FAILED));
            }
            break;
        }
        case PL_ACT_EDIT:
            edit_playlist(sel);
            break;
        case PL_ACT_DELETE:
        {
            if (uipod_confirm(str(LANG_UIPOD_DELETE_PLAYLIST_Q), name))
            {
                remove(path);
                pl_load();
                gui_synclist_set_nb_items(&p->lists, pl_count + 1);
                if (sel > pl_count)
                    gui_synclist_select_item(&p->lists, pl_count);
            }
            break;
        }
        case PL_ACT_DOWNLOAD:
            download_playlist_file(path);
            break;
        default:                                /* -1: cancelled */
            break;
    }
    return UIPOD_PAGE_STAY;
}

/* ---- pick mode: add the pending file/dir to a chosen / new playlist ----- */

/* Insert the pending selection into an existing playlist `sel`: a single file/dir
 * (Rockbox expands a directory into its tracks), or an explicit set of track
 * paths (a library album/artist).  The screen closes afterwards. */
static enum uipod_page_result pick_into_existing(struct playlists_page *p, int sel)
{
    char path[MAX_PATH];
    pl_entry_full_path(sel, path, sizeof(path));
    if (p->pick_paths)
        for (int i = 0; i < p->pick_npaths; i++)
            catalog_insert_into(path, false, p->pick_paths[i], FILE_ATTR_AUDIO);
    else
        catalog_insert_into(path, false, p->pick_sel, p->pick_attr);
    splash(HZ, ID2P(LANG_UIPOD_ADDED_TO_PLAYLIST));
    return UIPOD_PAGE_DONE;
}

/* Standalone screen only: remember row `sel` so the next entry re-highlights it
 * (empty string = the "+ Add Playlist" row). */
static void pl_save_pos(struct playlists_page *p, int sel)
{
    if (p->pick)
        return;
    if (sel >= 0 && sel < pl_count)
        strlcpy(pl_last_sel, pl_entry_filename(sel), sizeof(pl_last_sel));
    else
        pl_last_sel[0] = '\0';
    pl_have_last = true;
}

static enum uipod_page_result playlists_on_action(struct uipod_page *self,
                                                    int action)
{
    struct playlists_page *p = (struct playlists_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);

    if (action == ACTION_STD_CANCEL)            /* B: leave */
    {
        pl_save_pos(p, sel);
        uipod_transition_arm_back();          /* returning to the parent */
        return UIPOD_PAGE_DONE;
    }

    if (action == ACTION_STD_OK)                /* A */
    {
        if (sel == pl_count)                    /* "+ Add Playlist": create empty */
            add_playlist(p);                    /* (both modes) then stay, so a
                                                 * pick-mode user can now select it */
        else if (sel >= 0 && sel < pl_count)    /* a playlist row */
        {
            if (p->pick)
            {
                enum uipod_page_result r = pick_into_existing(p, sel);
                if (r == UIPOD_PAGE_DONE)
                    pl_save_pos(p, sel);
                return r;
            }
            /* tap A: open this playlist's track listing (playlist_view()
             * consumes the pending view and runs the viewer on it).  The
             * viewer refuses an empty playlist, so teach the add gesture
             * instead of bouncing. */
            char path[MAX_PATH];
            pl_entry_full_path(sel, path, sizeof(path));
            bool offline = uipod_offline_mode_get();
            bool is_empty = offline ? !pl_file_has_offline_track(path)
                                    : pl_file_is_empty(path);
            if (is_empty)
            {
                char name[MAX_PATH];
                pl_display_name(sel, name, sizeof(name));
                if (pl_items[sel].src == PL_SRC_NAV_FAV)
                {
                    static const char *const fav_rows[] =
                        { ID2P(LANG_UIPOD_PLAYLIST_EMPTY),
                          ID2P(LANG_UIPOD_FAV_EMPTY_HINT) };
                    uipod_message_page(name, fav_rows, 2);
                }
                else
                {
                    static const char *const rows[] =
                        { ID2P(LANG_UIPOD_PLAYLIST_EMPTY),
                          ID2P(LANG_UIPOD_PLAYLIST_EMPTY_HINT1),
                          ID2P(LANG_UIPOD_PLAYLIST_EMPTY_HINT2) };
                    uipod_message_page(name, rows, 3);
                }
                return UIPOD_PAGE_STAY;
            }
            if (offline)
            {
                pl_materialize_offline(path, pl_entry_filename(sel),
                                       pl_pending_dir, sizeof(pl_pending_dir),
                                       pl_pending_file, sizeof(pl_pending_file),
                                       NULL, 0);
            }
            else
            {
                strlcpy(pl_pending_dir,  pl_entry_dir(sel),      sizeof pl_pending_dir);
                strlcpy(pl_pending_file, pl_entry_filename(sel), sizeof pl_pending_file);
            }
            pl_pending_view = true;
            p->result = GO_TO_PLAYLIST_VIEWER;
            pl_save_pos(p, sel);
            return UIPOD_PAGE_DONE;
        }
    }
    else if (action == ACTION_STD_CONTEXT       /* Hold A: playlist actions */
             && !p->pick && sel >= 0 && sel < pl_count)
    {
        enum uipod_page_result r = playlist_action(p, sel);
        if (r == UIPOD_PAGE_DONE)              /* leaving (play -> WPS) */
            pl_save_pos(p, sel);
        return r;
    }
    return UIPOD_PAGE_STAY;                    /* the runner redraws each frame */
}

static const struct uipod_page_vtable playlists_vtable =
{
    .legend    = NULL,                  /* normal header (no legend) */
    .draw      = playlists_draw,
    .poll      = playlists_poll,
    .on_action = playlists_on_action,
};

/* A (open/add), Hold-A (playlist actions) and B (back) act; list scrolling is
 * consumed in poll */
static const int playlists_allowed[] = { ACTION_STD_OK, ACTION_STD_CONTEXT,
                                         ACTION_STD_CANCEL, -1 };

/* Shared driver: (re)read the catalog, run the page with the given title. */
static int run_playlists(struct playlists_page *p, const char *title)
{
    pl_loaded_remote = !p->pick;
    pl_load();
    gui_synclist_init(&p->lists, pl_get_name, NULL, false, 1, NULL);
    gui_synclist_set_title(&p->lists, (char *)title, Icon_Playlist);
    gui_synclist_set_nb_items(&p->lists, pl_count + 1);

    /* Standalone screen: restore the remembered selection (incl. on return from
     * Now Playing); pick mode always starts at the top. */
    int sel = 0;
    if (!p->pick && pl_have_last)
    {
        if (pl_last_sel[0])
        {
            for (int i = 0; i < pl_count; i++)
                if (strcmp(pl_entry_filename(i), pl_last_sel) == 0)
                {
                    sel = i;
                    break;
                }
        }
        else
            sel = pl_count;          /* the "+ Add Playlist" row */
    }
    gui_synclist_select_item(&p->lists, sel);

    push_current_activity(ACTIVITY_PLAYLISTBROWSER);
    uipod_page_run(&p->base);
    pop_current_activity();
    return p->base.home ? GO_TO_ROOT : p->result;
}

bool uipod_playlists_take_pending_view(char *path, size_t len)
{
    if (!pl_pending_view)
        return false;
    pl_pending_view = false;
    path_append(path, pl_pending_dir[0] ? pl_pending_dir : pl_dir,
                pl_pending_file, len);
    return true;
}

int uipod_playlists_start_pending(void)
{
    if (!pl_pending_active)
        return 0;
    pl_pending_active = false;

    const char *dir = pl_pending_dir[0] ? pl_pending_dir : pl_dir;
    if (playlist_create(dir, pl_pending_file) == -1 || playlist_amount() <= 0)
    {
        splash(HZ, ID2P(LANG_UIPOD_PLAYLIST_EMPTY));
        return -1;
    }
    queue_start(pl_pending_shuffle, 0, 0);
    return 1;
}

int uipod_playlists_screen(void *param)
{
    (void)param;
    struct playlists_page p =
    {
        /* enter_honor_back: returning from the viewer / Now Playing (which
         * armed a back) slides this screen in L->R; a fresh open from the root
         * enters forward.  no_arm_back: the exit slide is armed explicitly
         * (only on B), so tap-A slides forward into the track listing. */
        .base = { .vt = &playlists_vtable, .context = CONTEXT_LIST,
                  .allowed = playlists_allowed, .enter_honor_back = true,
                  .no_arm_back = true },
        /* Back to the root menu via GO_TO_ROOT (NOT GO_TO_PREVIOUS): the root
         * dispatch keeps last_screen == our entry, so the menu re-highlights the
         * "Playlists" row on return -- the same contract Settings uses. */
        .result = GO_TO_ROOT,
    };
    return run_playlists(&p, str(LANG_PLAYLISTS));
}

void uipod_playlists_pick(const char *sel, int sel_attr)
{
    struct playlists_page p =
    {
        .base = { .vt = &playlists_vtable, .context = CONTEXT_LIST,
                  .allowed = playlists_allowed },
        .result = GO_TO_ROOT,
        .pick = true, .pick_sel = sel, .pick_attr = sel_attr,
    };
    run_playlists(&p, str(LANG_UIPOD_ADD_TO_PLAYLIST));
}

/* Add an explicit set of track paths (a library album/artist, which has no
 * single directory to expand) to a chosen / new playlist. */
static void uipod_playlists_pick_tracks(char **paths, int count)
{
    struct playlists_page p =
    {
        .base = { .vt = &playlists_vtable, .context = CONTEXT_LIST,
                  .allowed = playlists_allowed },
        .result = GO_TO_ROOT,
        .pick = true, .pick_paths = paths, .pick_npaths = count,
    };
    run_playlists(&p, str(LANG_UIPOD_ADD_TO_PLAYLIST));
}

/* ---- shared Hold-A music context menu ------------------------------------
 * Play / Play Shuffled / Play Next / Add to Play Queue / Add to Playlist for
 * the highlighted row; a track has no Play Shuffled.  Everything is handled
 * here; true means a Play started a new queue (the caller -> Now Playing). */
enum { MUSIC_CTX_PLAY = 0, MUSIC_CTX_SHUFFLED, MUSIC_CTX_PLAY_NEXT,
       MUSIC_CTX_ADD_QUEUE, MUSIC_CTX_ADD_PL, MUSIC_CTX_DOWNLOAD };

/* The picked MUSIC_CTX_* for a set (folder, artist, album) or a track; -1 on
 * cancel. */
static int music_ctx_menu(const char *title, bool set, bool can_download)
{
    static const char *const set_opts[] =
        { ID2P(LANG_PLAY), ID2P(LANG_UIPOD_PLAY_SHUFFLED),
          ID2P(LANG_PLAY_NEXT), ID2P(LANG_UIPOD_ADD_TO_PLAY_QUEUE),
          ID2P(LANG_UIPOD_ADD_TO_PLAYLIST), ID2P(LANG_UIPOD_DOWNLOAD) };
    static const char *const track_opts[] =
        { ID2P(LANG_PLAY), ID2P(LANG_PLAY_NEXT),
          ID2P(LANG_UIPOD_ADD_TO_PLAY_QUEUE),
          ID2P(LANG_UIPOD_ADD_TO_PLAYLIST), ID2P(LANG_UIPOD_DOWNLOAD) };
    if (set)
    {
        int n = (int)ARRAYLEN(set_opts) - (can_download ? 0 : 1);
        return uipod_context_menu(title, set_opts, n);
    }
    int n = (int)ARRAYLEN(track_opts) - (can_download ? 0 : 1);
    int row = uipod_context_menu(title, track_opts, n);
    return row > MUSIC_CTX_PLAY ? row + 1 : row;   /* past the missing row */
}

bool uipod_music_context(const char *title, const char *path, int attr)
{
    bool dir = attr & ATTR_DIRECTORY;
    bool is_nav = !dir && uipod_navidrome_is_track_path(path);
    bool is_jf  = !dir && uipod_jellyfin_is_track_path(path);
    bool can_dl = !uipod_offline_mode_get() && (is_nav || is_jf);
    switch (music_ctx_menu(title, dir, can_dl))
    {
        case MUSIC_CTX_PLAY:
            return dir ? uipod_queue_add(path, attr, PLAYLIST_REPLACE, false)
                       : uipod_play_track(path);
        case MUSIC_CTX_SHUFFLED:
            return uipod_queue_add(path, attr, PLAYLIST_REPLACE, true);
        case MUSIC_CTX_PLAY_NEXT:
            uipod_queue_add(path, attr, PLAYLIST_INSERT, false);
            break;
        case MUSIC_CTX_ADD_QUEUE:
            uipod_queue_add(path, attr, PLAYLIST_INSERT_LAST, false);
            break;
        case MUSIC_CTX_ADD_PL:
            uipod_playlists_pick(path, attr);
            break;
        case MUSIC_CTX_DOWNLOAD:
            if (is_jf)
                uipod_jellyfin_download_track(path);
            else
                uipod_navidrome_download_track(path);
            break;
    }
    return false;
}

bool uipod_music_context_track_in_list(const char *title, char **paths,
                                       int count, int sel)
{
    if (!paths || sel < 0 || sel >= count || !paths[sel] || !paths[sel][0])
        return false;
    const char *path = paths[sel];
    bool is_nav = uipod_navidrome_is_track_path(path);
    bool is_jf  = uipod_jellyfin_is_track_path(path);
    bool can_dl = !uipod_offline_mode_get() && (is_nav || is_jf);
    switch (music_ctx_menu(title, false, can_dl))
    {
        case MUSIC_CTX_PLAY:
            return uipod_play_tracks_at(paths, count, sel);
        case MUSIC_CTX_PLAY_NEXT:
            uipod_queue_add(path, FILE_ATTR_AUDIO, PLAYLIST_INSERT, false);
            break;
        case MUSIC_CTX_ADD_QUEUE:
            uipod_queue_add(path, FILE_ATTR_AUDIO, PLAYLIST_INSERT_LAST, false);
            break;
        case MUSIC_CTX_ADD_PL:
            uipod_playlists_pick(path, FILE_ATTR_AUDIO);
            break;
        case MUSIC_CTX_DOWNLOAD:
            if (is_jf)
                uipod_jellyfin_download_track(path);
            else
                uipod_navidrome_download_track(path);
            break;
    }
    return false;
}

bool uipod_music_context_tracks(const char *title, char **paths, int count)
{
    if (count <= 0)
        return false;
    bool can_dl = has_downloadable_tracks(paths, count);
    switch (music_ctx_menu(title, true, can_dl))
    {
        case MUSIC_CTX_PLAY:
            return queue_add_tracks(paths, count, PLAYLIST_REPLACE, false);
        case MUSIC_CTX_SHUFFLED:
            return queue_add_tracks(paths, count, PLAYLIST_REPLACE, true);
        case MUSIC_CTX_PLAY_NEXT:
            queue_add_tracks(paths, count, PLAYLIST_INSERT, false);
            break;
        case MUSIC_CTX_ADD_QUEUE:
            queue_add_tracks(paths, count, PLAYLIST_INSERT_LAST, false);
            break;
        case MUSIC_CTX_ADD_PL:
            uipod_playlists_pick_tracks(paths, count);
            break;
        case MUSIC_CTX_DOWNLOAD:
            download_mixed_tracks(paths, count);
            break;
    }
    return false;
}
