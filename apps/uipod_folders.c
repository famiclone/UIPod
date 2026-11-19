/***************************************************************************
 * UIPod: user-managed virtual folders (Music, Podcasts, Audiobooks, Movies,
 * Television).
 *
 * Each category is a "virtual folder" that merges the contents of a
 * user-managed set of source folders.  On first run the list is SEEDED with a
 * default folder (/mnt/SDCARD/<Name>, which may or may not exist on the card --
 * a missing folder is simply skipped, never an error), but the user may remove
 * any folder, the seeded default included.  All categories share one engine,
 * parameterised by a `struct folder_category`:
 *
 *  - Storage:  ROCKBOX_DIR/<store_file>, one absolute path per line.  Seeded
 *    with default_path only when the file does not yet exist; once the file
 *    exists (even empty) the user's choices stand and the default is not forced
 *    back in.
 *  - Settings -> "<Name> Folders": a list of the chosen folders + a "+ Add
 *    Folder" entry, as uipod_page-derived pages.  A on "+ Add Folder" opens
 *    our native directory-picker page (A descends, Y adds, B backs out).  A on a
 *    folder row asks to remove it; B leaves.
 *  - Root entry: a flattened virtual view of every configured folder's
 *    contents, browsed with our native music browser.  If no folder exists or
 *    they hold no folders/audio (e.g. the user removed them all), a centered
 *    "(empty)" page is shown instead.
 *  - Root "Browse": the same browser over the whole filesystem, opening at
 *    PICKER_ROOT; B walks up to / and leaves from there.
 *
 * The flattened view is a tmpfs "farm" of symlinks
 * (<farm_path>/<name> -> <configured folder>/<name>).  The hosted filesystem
 * layer follows symlinks (filesystem-app.c dir_get_info), so rockbox_browse()
 * lists, descends into and plays the real files; one link per top-level entry of
 * each folder gives the merged *contents* of all folders rather than the folders
 * themselves.
 ****************************************************************************/
#include "config.h"
#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>   /* qsort */
#include <string.h>
#include <strings.h>  /* strcasecmp */
#include <sys/stat.h> /* stat / lstat (must precede file.h / dir.h macros) */
#include <time.h>
#include "string-extra.h"
#include "strnatcmp.h"   /* natural alphabetical sort (2 before 10) */
#include "kernel.h"      /* HZ, yield */
#include "file.h"
#include "dir.h"
#include "pathfuncs.h"
#include "settings.h"
#include "lang.h"
#include "splash.h"
#include "action.h"
#include "gui/list.h"
#include "tree.h"
#include "filetree.h"   /* ft_load: the stock browse engine */
#include "root_menu.h"
#include "misc.h"
#include "icon.h"
#include "filetypes.h"   /* filetype_get_attr / FILE_ATTR_AUDIO */
#include "metadata.h"    /* get_metadata, struct mp3entry, get_codec_string */
#include "playlist.h"    /* play a track on select */
#include "uipod_folders.h"
#include "uipod_ui.h"
#include "uipod_page.h"
#include "uipod_transition.h"   /* slide on folder descend/ascend */
#include "uipod_playlists.h"    /* Hold-A = add highlighted file/folder to a playlist */
#include "uipod_library.h"      /* Shuffle All's track set + mark_stale */
#include "uipod_navidrome.h"
#include "uipod_jellyfin.h"
#include "uipod_plugins.h"
#include "uipod_rs_ui.h"
#include "uipod_video.h"
#include "uipod_svg.h"
#include "uipod_keyboard.h"

#define PICKER_ROOT      UIPOD_SDCARD_ROOT   /* pickers & Browse root on card */
#define BROWSE_LAST_FILE ROCKBOX_DIR "/browse_last_dir.txt"
#define BROWSE_INFO_FILE UIPOD_TMP_ROOT "/uipod_info.txt"
#define MAX_FOLDERS      24
#define FPATH_LEN        MAX_PATH
#define LOAD_BUFSZ       (MAX_FOLDERS * FPATH_LEN)

/* libc symlink()/unlink(); Rockbox does not wrap them and root paths map 1:1 to
 * the OS (app_root_realpath() == "/"), so OS and Rockbox paths are interchangeable. */
extern int symlink(const char *target, const char *linkpath);
extern int unlink(const char *pathname);

/* Last browse position, so re-entering a browse view (incl. returning from
 * Now Playing) restores the folder + highlighted entry.  Session-only. */
struct browse_pos
{
    char last_dir[FPATH_LEN];
    char last_sel[FPATH_LEN];
    bool have_last;
};

/* One virtual-folder category (Music / Podcasts / Audiobooks / Movies /
 * Television). */
struct folder_category
{
    const char *store_file;     /* basename under ROCKBOX_DIR (e.g. "folders.txt") */
    const char *default_path;   /* unremovable default (e.g. "/mnt/SDCARD/Music") */
    const char *farm_path;      /* tmpfs merge dir (e.g. "/tmp/uipod_music") */
    int title_lang;             /* "<Name> Folders" settings-page title */
    int browse_lang;            /* "<Name>" browse-screen title */
    bool video;                 /* lists video; never in the music library index */

    /* runtime state */
    char folders[MAX_FOLDERS][FPATH_LEN];
    int  n_folders;

    struct browse_pos pos;
    bool active_in_dir;         /* true when WPS was entered from inside folders[active_idx] */
    int  active_idx;
};

/* Music keeps the original "folders.txt" name so existing setups are preserved.
 * The farm dirs live in tmpfs (NOT /tmp/uipod, a vfat bind-mount that cannot
 * hold symlinks). */
static struct folder_category cat_music = {
    .store_file = "folders.txt", .default_path = UIPOD_SDCARD_ROOT "/Music",
    .farm_path = UIPOD_TMP_ROOT "/uipod_music",
    .title_lang = LANG_UIPOD_FOLDERS, .browse_lang = LANG_UIPOD_FILES,
};
static struct folder_category cat_podcast = {
    .store_file = "podcasts.txt", .default_path = UIPOD_SDCARD_ROOT "/Podcasts",
    .farm_path = UIPOD_TMP_ROOT "/uipod_podcasts",
    .title_lang = LANG_UIPOD_PODCAST_FOLDERS,
    .browse_lang = LANG_UIPOD_PODCASTS,
};
static struct folder_category cat_audiobook = {
    .store_file = "audiobooks.txt", .default_path = UIPOD_SDCARD_ROOT "/Audiobooks",
    .farm_path = UIPOD_TMP_ROOT "/uipod_audiobooks",
    .title_lang = LANG_UIPOD_AUDIOBOOK_FOLDERS,
    .browse_lang = LANG_UIPOD_AUDIOBOOKS,
};
static struct folder_category cat_movies = {
    .store_file = "movies.txt", .default_path = UIPOD_SDCARD_ROOT "/Movies",
    .farm_path = UIPOD_TMP_ROOT "/uipod_movies",
    .title_lang = LANG_UIPOD_MOVIE_FOLDERS,
    .browse_lang = LANG_UIPOD_VIDEOS, .video = true,
};
static struct folder_category cat_television = {
    .store_file = "television.txt", .default_path = UIPOD_SDCARD_ROOT "/Television",
    .farm_path = UIPOD_TMP_ROOT "/uipod_television",
    .title_lang = LANG_UIPOD_TELEVISION_FOLDERS,
    .browse_lang = LANG_UIPOD_TELEVISION, .video = true,
};

/* ---- storage ---------------------------------------------------------- */

static void store_path(struct folder_category *cat, char *buf, size_t len)
{
    snprintf(buf, len, "%s/%s", ROCKBOX_DIR, cat->store_file);
}

static void folders_save(struct folder_category *cat)
{
    char path[FPATH_LEN];
    store_path(cat, path, sizeof path);
    int fd = open(path, O_WRONLY|O_CREAT|O_TRUNC, 0666);
    if (fd < 0)
        return;
    for (int i = 0; i < cat->n_folders; i++)
    {
        write(fd, cat->folders[i], strlen(cat->folders[i]));
        write(fd, "\n", 1);
    }
    close(fd);
}

static void folders_load(struct folder_category *cat)
{
    cat->n_folders = 0;
    char path[FPATH_LEN];
    store_path(cat, path, sizeof path);
    int fd = open(path, O_RDONLY);
    if (fd < 0)
    {
        /* first run (no file yet): seed with the default folder (plus NextUI's
         * /mnt/SDCARD/Media/Music for Music, where /mnt/SDCARD/Media/Music/navidrome
         * also lives).  The user may later remove it -- once the file exists we
         * honour it verbatim. */
        strlcpy(cat->folders[0], cat->default_path, FPATH_LEN);
        cat->n_folders = 1;
        if (cat == &cat_music && cat->n_folders < MAX_FOLDERS)
            strlcpy(cat->folders[cat->n_folders++], UIPOD_SDCARD_ROOT "/Media", FPATH_LEN);
        else if (cat == &cat_movies && cat->n_folders < MAX_FOLDERS)
            strlcpy(cat->folders[cat->n_folders++], UIPOD_SDCARD_ROOT "/Media", FPATH_LEN);
        else if (cat == &cat_television && cat->n_folders < MAX_FOLDERS)
            strlcpy(cat->folders[cat->n_folders++], UIPOD_SDCARD_ROOT "/Media/jellyfin/Shows", FPATH_LEN);
        return;
    }
    static char rbuf[LOAD_BUFSZ];
    int len = read(fd, rbuf, sizeof(rbuf) - 1);
    close(fd);
    if (len > 0)
    {
        rbuf[len] = '\0';
        char *line = rbuf, *nl;
        while (line && *line && cat->n_folders < MAX_FOLDERS)
        {
            nl = strchr(line, '\n');
            if (nl) *nl = '\0';
            if (*line == '/')
                strlcpy(cat->folders[cat->n_folders++], line, FPATH_LEN);
            line = nl ? nl + 1 : NULL;
        }
    }
}

/* Persist the list and reconcile the index now, under the Scanning screen. */
static void folders_changed(struct folder_category *cat)
{
    folders_save(cat);
    if (!cat->video)
        uipod_library_reconcile(false);
}

static bool folders_add(struct folder_category *cat, const char *path)
{
    if (cat->n_folders >= MAX_FOLDERS)
        return false;
    for (int i = 0; i < cat->n_folders; i++)
        if (strcmp(cat->folders[i], path) == 0)
            return false; /* already present */
    strlcpy(cat->folders[cat->n_folders++], path, FPATH_LEN);
    folders_changed(cat);
    return true;
}

static void folders_remove(struct folder_category *cat, int idx)
{
    if (idx < 0 || idx >= cat->n_folders)
        return;
    for (int i = idx; i < cat->n_folders - 1; i++)
        strlcpy(cat->folders[i], cat->folders[i + 1], FPATH_LEN);
    cat->n_folders--;
    folders_changed(cat);
}

/* ---- library access (uipod_library.c) ------------------------------- */

static struct folder_category *cat_by_index(int cat)
{
    switch (cat)
    {
        case TP_CAT_MUSIC:     return &cat_music;
        case TP_CAT_PODCAST:   return &cat_podcast;
        case TP_CAT_AUDIOBOOK: return &cat_audiobook;
        default:               return NULL;
    }
}

void uipod_folders_load_all(void)
{
    folders_load(&cat_music);
    folders_load(&cat_podcast);
    folders_load(&cat_audiobook);
}

int uipod_folders_root_count(int cat)
{
    struct folder_category *c = cat_by_index(cat);
    return c ? c->n_folders : 0;
}

const char *uipod_folders_root_path(int cat, int idx)
{
    struct folder_category *c = cat_by_index(cat);
    if (!c || idx < 0 || idx >= c->n_folders)
        return NULL;
    return c->folders[idx];
}

/* ---- "+ Add Folder" picker: a native directory-picker page --------------
 * A uipod_page that lists the sub-directories of the current directory:
 *   A = descend into the highlighted folder
 *   Y = add the highlighted folder (or the current folder if it has none)
 *   B = up one level, or leave the picker at the start root
 * No Rockbox file browser involved -- it's a first-class page like the others. */

#define PICK_MAX_DIRS   512
#define PICK_NAMEBUF    (24 * 1024)

/* one picker at a time -> the listing lives in file-scope statics, off-stack */
static char pick_namebuf[PICK_NAMEBUF];   /* packed NUL-terminated names */
static int  pick_off[PICK_MAX_DIRS];      /* offset of each name in pick_namebuf */
static bool pick_isdir[PICK_MAX_DIRS];    /* is entry i a folder (vs a track)? */
static int  pick_count;
static char pick_reselect[FPATH_LEN];     /* if set, pick_load highlights this
                                           * entry by name (used on ascend to
                                           * restore the folder we came from) */

struct picker_page
{
    struct uipod_page base;
    struct gui_synclist lists;
    char   curdir[FPATH_LEN];
    char   root[FPATH_LEN];
    char   title_buf[FPATH_LEN];
    /* folder-picker mode (out!=NULL): Hold A (or row 0 in dest_mode) picks a folder */
    char  *out;
    size_t out_len;
    bool   picked;
    bool   dest_mode;              /* Copy/Move destination picker with row 0 "[ Here ]" */
    bool   dest_is_move;           /* true = Move, false = Copy */
    /* music-browse mode (music=true): rows are folders+tracks, A plays a track,
     * exits with `result` for the root dispatch (GO_TO_WPS / GO_TO_ROOT) */
    bool   music;
    bool   video;                  /* music mode over video files */
    bool   full_browse;            /* true in root Browse (SHOW_ALL + file manager ops) */
    int    result;
    struct browse_pos *pos;        /* music mode: where the position persists */
    struct tree_context *mctx;     /* music mode: the stock browse engine state */
};

static struct browse_pos s_files_browse_pos;
static bool s_files_browse_loaded;

static void strip_trailing_slash_inplace(char *s);

static void files_browse_save_pos(const struct browse_pos *pos)
{
    if (!pos || !pos->have_last || !pos->last_dir[0])
        return;
    int fd = open(BROWSE_LAST_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0)
        return;
    write(fd, pos->last_dir, strlen(pos->last_dir));
    write(fd, "\n", 1);
    if (pos->last_sel[0])
        write(fd, pos->last_sel, strlen(pos->last_sel));
    write(fd, "\n", 1);
    close(fd);
}

static void files_browse_load_pos(const char *fallback_root)
{
    if (!s_files_browse_loaded)
    {
        s_files_browse_loaded = true;
        int fd = open(BROWSE_LAST_FILE, O_RDONLY);
        if (fd >= 0)
        {
            char rbuf[FPATH_LEN * 2 + 8];
            int len = read(fd, rbuf, sizeof(rbuf) - 1);
            close(fd);
            if (len > 0)
            {
                rbuf[len] = '\0';
                char *line1 = rbuf;
                char *nl1 = strchr(line1, '\n');
                char *line2 = NULL;
                if (nl1)
                {
                    *nl1 = '\0';
                    line2 = nl1 + 1;
                    char *nl2 = strchr(line2, '\n');
                    if (nl2)
                        *nl2 = '\0';
                }
                size_t l1 = strlen(line1);
                if (l1 > 0 && line1[l1 - 1] == '\r')
                    line1[--l1] = '\0';
                if (line1[0] == '/')
                {
                    strlcpy(s_files_browse_pos.last_dir, line1,
                            sizeof(s_files_browse_pos.last_dir));
                    strip_trailing_slash_inplace(s_files_browse_pos.last_dir);
                    s_files_browse_pos.have_last = true;
                }
                if (line2 && line2[0])
                {
                    size_t l2 = strlen(line2);
                    if (l2 > 0 && line2[l2 - 1] == '\r')
                        line2[--l2] = '\0';
                    strlcpy(s_files_browse_pos.last_sel, line2,
                            sizeof(s_files_browse_pos.last_sel));
                }
            }
        }
    }

    if (s_files_browse_pos.have_last && !dir_exists(s_files_browse_pos.last_dir))
    {
        /* Walk up to the nearest still-existing parent directory under fallback_root */
        char cur[FPATH_LEN];
        strlcpy(cur, s_files_browse_pos.last_dir, sizeof(cur));
        size_t rlen = strlen(fallback_root);
        while (!dir_exists(cur))
        {
            const char *par = NULL;
            size_t n = path_dirname(cur, &par);
            if (!par || n == 0 || n >= sizeof(cur))
            {
                strlcpy(cur, fallback_root, sizeof(cur));
                break;
            }
            char next[FPATH_LEN];
            memcpy(next, par, n);
            next[n] = '\0';
            strip_trailing_slash_inplace(next);
            if (strcmp(next, cur) == 0 ||
                (rlen > 1 && (strncmp(next, fallback_root, rlen) != 0 ||
                              (next[rlen] != '\0' && next[rlen] != '/'))))
            {
                strlcpy(cur, fallback_root, sizeof(cur));
                break;
            }
            strlcpy(cur, next, sizeof(cur));
        }
        strlcpy(s_files_browse_pos.last_dir, cur, sizeof(s_files_browse_pos.last_dir));
        s_files_browse_pos.last_sel[0] = '\0';
        files_browse_save_pos(&s_files_browse_pos);
    }
}

static const char *dir_basename_str(const char *path, char *out, size_t out_len)
{
    char tmp[FPATH_LEN];
    strlcpy(tmp, path, sizeof(tmp));
    size_t n = strlen(tmp);
    while (n > 1 && tmp[n - 1] == '/')
        tmp[--n] = '\0';
    if (strcmp(tmp, "/") == 0)
    {
        strlcpy(out, "/", out_len);
        return out;
    }
    const char *leaf = NULL;
    path_basename(tmp, &leaf);
    strlcpy(out, (leaf && *leaf) ? leaf : tmp, out_len);
    return out;
}

static void picker_update_title(struct picker_page *p)
{
    const char *start_dir = dir_exists(PICKER_ROOT) ? PICKER_ROOT : "/";
    if (p->dest_mode)
        dir_basename_str(p->curdir, p->title_buf, sizeof(p->title_buf));
    else if (!p->music && strcmp(p->curdir, start_dir) == 0)
        strlcpy(p->title_buf, str(LANG_UIPOD_ADD_DIRECTORY), sizeof(p->title_buf));
    else
        dir_basename_str(p->curdir, p->title_buf, sizeof(p->title_buf));
    gui_synclist_set_title(&p->lists, p->title_buf,
                           p->music ? Icon_Audio : Icon_file_view_menu);
}

static void strip_trailing_slash_inplace(char *s)
{
    if (!s)
        return;
    size_t n = strlen(s);
    while (n > 1 && s[n - 1] == '/')
        s[--n] = '\0';
}

/* ---- mode-aware list accessors --------------------------------------------
 * Music mode is backed by the stock tree_context (one browse + playlist engine,
 * shared with the file browser); the "+ Add Folder" picker keeps its own simple
 * pick_*[] directory listing.  These hide which source a row comes from. */
static int item_count(struct picker_page *p)
{
    if (!p)
        return 0;
    return p->music ? (p->mctx ? p->mctx->filesindir : 0) : pick_count;
}
static const char *item_name(struct picker_page *p, int sel)
{
    if (!p)
        return "";
    if (p->music)
    {
        if (!p->mctx || sel < 0 || sel >= p->mctx->filesindir)
            return "";
        struct entry *e = tree_get_entry_at(p->mctx, sel);
        return (e && e->name) ? e->name : "";
    }
    if (sel < 0 || sel >= pick_count)
        return "";
    return pick_namebuf + pick_off[sel];
}
static bool item_isdir(struct picker_page *p, int sel)
{
    if (!p)
        return false;
    if (p->music)
    {
        if (!p->mctx || sel < 0 || sel >= p->mctx->filesindir)
            return false;
        struct entry *e = tree_get_entry_at(p->mctx, sel);
        return e && (e->attr & ATTR_DIRECTORY);
    }
    return (sel >= 0 && sel < pick_count) && pick_isdir[sel];
}

/* Music mode: load p->curdir into the tree_context via the stock engine
 * (folders-first natural-alpha, same order the file browser uses).  dirfilter /
 * sort are configured once in folder_browse. */
static void music_load(struct picker_page *p)
{
    struct tree_context *c = p->mctx;
    strlcpy(c->currdir, p->curdir, sizeof(c->currdir));
    c->selected_item = 0;
    if (ft_load(c, NULL) < 0)
        c->filesindir = 0;
    picker_update_title(p);
    gui_synclist_set_nb_items(&p->lists, c->filesindir);

    int sel = 0;
    if (pick_reselect[0])
    {
        for (int i = 0; i < c->filesindir; i++)
        {
            struct entry *e = tree_get_entry_at(c, i);
            if (e && e->name && strcmp(e->name, pick_reselect) == 0)
            {
                sel = i;
                break;
            }
        }
        pick_reselect[0] = '\0';
    }
    if (c->filesindir > 0)
        gui_synclist_select_item(&p->lists, sel);
}

/* Remember the current folder + highlighted entry so the next entry into this
 * view (incl. returning from Now Playing) lands back here. */
static void browse_save_pos(struct picker_page *p)
{
    if (!p->music || !p->pos)
        return;
    strlcpy(p->pos->last_dir, p->curdir, sizeof(p->pos->last_dir));
    int sel = gui_synclist_get_sel_pos(&p->lists);
    if (sel >= 0 && sel < item_count(p))
        strlcpy(p->pos->last_sel, item_name(p, sel), sizeof(p->pos->last_sel));
    else
        p->pos->last_sel[0] = '\0';
    p->pos->have_last = true;
    if (p->full_browse)
        files_browse_save_pos(p->pos);
}

/* qsort comparator: natural alphabetical order of the names at two offsets */
static int pick_off_cmp(const void *a, const void *b)
{
    return strnatcasecmp(pick_namebuf + *(const int *)a,
                         pick_namebuf + *(const int *)b);
}

/* Read curdir into the packed name table.  Picker mode: sub-directories only.
 * Music mode: folders first (pass 0) then audio tracks (pass 1).  Each group is
 * sorted alphabetically. */
static void pick_load(struct picker_page *p)
{
    pick_count = 0;
    int used = 0, nfolders = 0;
    int passes = p->music ? 2 : 1;
    for (int pass = 0; pass < passes; pass++)
    {
        DIR *d = opendir(p->curdir);
        if (!d)
            break;
        struct dirent *e;
        while ((e = readdir(d)))
        {
            if (e->d_name[0] == '.')          /* skip ., .., hidden */
                continue;
            struct dirinfo info = dir_get_info(d, e);
            bool is_dir = (info.attribute & ATTR_DIRECTORY);
            if (!p->music)
            {
                if (!is_dir)                  /* picker: directories only */
                    continue;
            }
            else if (pass == 0)               /* music pass 0: folders */
            {
                if (!is_dir)
                    continue;
            }
            else                              /* music pass 1: audio tracks */
            {
                if (is_dir ||
                    (filetype_get_attr(e->d_name) & FILE_ATTR_MASK) != FILE_ATTR_AUDIO)
                    continue;
            }
            int len = strlen(e->d_name) + 1;
            if (pick_count >= PICK_MAX_DIRS ||
                used + len > (int)sizeof(pick_namebuf))
                break;                        /* listing full (rare) */
            pick_off[pick_count] = used;
            pick_isdir[pick_count] = is_dir;
            pick_count++;
            memcpy(pick_namebuf + used, e->d_name, len);
            used += len;
        }
        closedir(d);
        if (pass == 0)
            nfolders = pick_count;            /* folders occupy [0, nfolders) */
    }
    /* sort folders, then tracks, alphabetically (each group on its own) */
    qsort(pick_off, nfolders, sizeof pick_off[0], pick_off_cmp);
    qsort(pick_off + nfolders, pick_count - nfolders, sizeof pick_off[0],
          pick_off_cmp);
    picker_update_title(p);
    int total_rows = p->dest_mode ? (pick_count + 1) : pick_count;
    gui_synclist_set_nb_items(&p->lists, total_rows);

    /* Default to the top, unless an ascend asked us to re-highlight the folder
     * we just came out of (matched by name). */
    int sel = 0;
    if (pick_reselect[0])
    {
        for (int i = 0; i < pick_count; i++)
            if (strcmp(pick_namebuf + pick_off[i], pick_reselect) == 0)
            {
                sel = p->dest_mode ? (i + 1) : i;
                break;
            }
        pick_reselect[0] = '\0';
    }
    if (total_rows > 0)
        gui_synclist_select_item(&p->lists, sel);
}

/* render the (reloaded) current dir off-screen for a slide transition */
static struct picker_page *s_reload_p;
static void pick_reload_render(void *ctx)
{
    (void)ctx;
    if (s_reload_p->music)
        music_load(s_reload_p);
    else
        pick_load(s_reload_p);
    gui_synclist_draw(&s_reload_p->lists);
}
static void pick_slide(struct picker_page *p, enum uipod_transition_dir dir)
{
    s_reload_p = p;
    uipod_transition_animate(dir, pick_reload_render, NULL);
}

static const char *pick_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    struct picker_page *p = (struct picker_page *)data;
    if (p && p->dest_mode)
    {
        if (sel == 0)
        {
            char cur_base[FPATH_LEN];
            dir_basename_str(p->curdir, cur_base, sizeof(cur_base));
            snprintf(buf, buf_len,
                     str(p->dest_is_move ? LANG_UIPOD_MOVE_HERE_FMT
                                         : LANG_UIPOD_COPY_HERE_FMT),
                     cur_base);
            return buf;
        }
        int idx = sel - 1;
        return (idx >= 0 && idx < pick_count) ? (pick_namebuf + pick_off[idx]) : "";
    }
    (void)buf; (void)buf_len;
    return item_name(p, sel);
}

static const struct uipod_svg_icon *pick_get_svg_icon(int sel, void *data)
{
    struct picker_page *p = (struct picker_page *)data;
    if (p && p->dest_mode)
    {
        if (sel == 0)
            return uipod_svg_tag("add_dir");
        return (sel >= 1 && sel <= pick_count) ? uipod_svg_tag("folder") : NULL;
    }
    if (sel < 0 || sel >= item_count(p))
        return NULL;
    if (!p->music)
        return uipod_svg_tag("folder");
    return uipod_svg_for_path(item_name(p, sel), item_isdir(p, sel));
}

static bool pick_get_chevron(int sel, void *data)
{
    struct picker_page *p = (struct picker_page *)data;
    if (p && p->dest_mode)
        return sel >= 1 && sel <= pick_count;
    return sel >= 0 && sel < item_count(p);
}

static const char *picker_legend(struct uipod_page *self)
{
    struct picker_page *p = (struct picker_page *)self;
    if (p->music || p->dest_mode)
        return NULL;
    return str(LANG_UIPOD_HOLD_A_TO_ADD);
}

static void picker_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct picker_page *)self)->lists);
}

static int picker_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct picker_page *)self)->lists, &action);
    return action;
}

static bool is_playable_audio_name(const char *nm)
{
    if (!nm || !nm[0] || filetype_is_video(nm) || uipod_plugins_can_open_file(nm))
        return false;
    return filetype_get_attr(nm) == FILE_ATTR_AUDIO;
}

static bool folder_play_or_ctx_track(struct picker_page *p, int sel,
                                     const char *title, bool is_ctx)
{
    int total = item_count(p);
    int track_cnt = 0;
    int track_sel = -1;
    for (int i = 0; i < total; i++)
    {
        if (item_isdir(p, i))
            continue;
        const char *nm = item_name(p, i);
        if (!is_playable_audio_name(nm))
            continue;
        if (i == sel)
            track_sel = track_cnt;
        track_cnt++;
    }
    if (track_cnt <= 0 || track_sel < 0)
        return false;

    char **paths = calloc((size_t)track_cnt, sizeof(*paths));
    if (!paths)
        return false;

    int k = 0;
    bool alloc_ok = true;
    for (int i = 0; i < total && k < track_cnt; i++)
    {
        if (item_isdir(p, i))
            continue;
        const char *nm = item_name(p, i);
        if (!is_playable_audio_name(nm))
            continue;
        char full[FPATH_LEN];
        path_append(full, p->curdir, nm, sizeof(full));
        paths[k] = strdup(full);
        if (!paths[k])
        {
            alloc_ok = false;
            break;
        }
        k++;
    }

    bool started = false;
    if (alloc_ok && k == track_cnt)
    {
        tree_unlock_cache(p->mctx);
        started = is_ctx
            ? uipod_music_context_track_in_list(title, paths, k, track_sel)
            : uipod_play_tracks_at(paths, k, track_sel);
        tree_lock_cache(p->mctx);
    }

    for (int i = 0; i < k; i++)
        free(paths[i]);
    free(paths);
    return started;
}

static const struct uipod_page_vtable picker_vtable;
static const int picker_allowed[];

static bool is_lang_ukrainian(void)
{
    return strcmp((const char *)global_settings.lang_file, "ukrainian") == 0;
}

static bool is_protected_sys_path(const char *path)
{
    if (!path || !path[0])
        return true;
    return strcmp(path, "/") == 0 ||
           strcmp(path, "/mnt") == 0 ||
           strcmp(path, PICKER_ROOT) == 0 ||
           strcmp(path, ROCKBOX_DIR) == 0;
}

/* True if `candidate` equals `dir_path` or sits inside `dir_path/...` */
static bool is_path_equal_or_child(const char *dir_path, const char *candidate)
{
    size_t n = strlen(dir_path);
    while (n > 1 && dir_path[n - 1] == '/')
        n--;
    if (strncmp(candidate, dir_path, n) != 0)
        return false;
    return candidate[n] == '\0' || candidate[n] == '/';
}

static bool is_valid_entry_name(const char *nm)
{
    if (!nm || !nm[0])
        return false;
    if (strcmp(nm, ".") == 0 || strcmp(nm, "..") == 0)
        return false;
    if (strchr(nm, '/') || strchr(nm, '\\'))
        return false;
    return true;
}

static bool delete_path_recursive(const char *path, int depth)
{
    if (depth > 32 || is_protected_sys_path(path))
        return false;

    struct stat st;
    if (lstat(path, &st) != 0)
        return false;

    if (!S_ISDIR(st.st_mode))
        return (unlink(path) == 0 || remove(path) == 0);

    DIR *d = opendir(path);
    if (!d)
        return false;

    bool ok = true;
    struct dirent *e;
    while ((e = readdir(d)))
    {
        if (e->d_name[0] == '.' &&
            (e->d_name[1] == '\0' ||
             (e->d_name[1] == '.' && e->d_name[2] == '\0')))
            continue;
        char child[FPATH_LEN];
        path_append(child, path, e->d_name, sizeof(child));
        if (!delete_path_recursive(child, depth + 1))
            ok = false;
    }
    closedir(d);
    yield();
    if (!ok)
        return false;
    return rmdir(path) == 0;
}

static bool copy_single_file(const char *src, const char *dst)
{
    int in_fd = open(src, O_RDONLY);
    if (in_fd < 0)
        return false;
    int out_fd = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (out_fd < 0)
    {
        close(in_fd);
        return false;
    }

    const size_t bufsz = 65536;
    char *buf = malloc(bufsz);
    if (!buf)
    {
        close(out_fd);
        close(in_fd);
        unlink(dst);
        return false;
    }

    bool ok = true;
    for (;;)
    {
        ssize_t r = read(in_fd, buf, bufsz);
        if (r < 0)
        {
            ok = false;
            break;
        }
        if (r == 0)
            break;
        ssize_t off = 0;
        while (off < r)
        {
            ssize_t w = write(out_fd, buf + off, (size_t)(r - off));
            if (w <= 0)
            {
                ok = false;
                break;
            }
            off += w;
        }
        if (!ok)
            break;
        yield();
    }

    free(buf);
    close(out_fd);
    close(in_fd);
    if (!ok)
        unlink(dst);
    return ok;
}

static bool copy_path_recursive(const char *src, const char *dst, int depth)
{
    if (depth > 32)
        return false;

    struct stat st;
    if (stat(src, &st) != 0)
        return false;

    if (!S_ISDIR(st.st_mode))
        return copy_single_file(src, dst);

    if (!dir_exists(dst) && mkdir(dst) != 0 && !dir_exists(dst))
        return false;

    DIR *d = opendir(src);
    if (!d)
        return false;

    bool ok = true;
    struct dirent *e;
    while ((e = readdir(d)))
    {
        if (e->d_name[0] == '.' &&
            (e->d_name[1] == '\0' ||
             (e->d_name[1] == '.' && e->d_name[2] == '\0')))
            continue;
        char csrc[FPATH_LEN], cdst[FPATH_LEN];
        path_append(csrc, src, e->d_name, sizeof(csrc));
        path_append(cdst, dst, e->d_name, sizeof(cdst));
        if (!copy_path_recursive(csrc, cdst, depth + 1))
        {
            ok = false;
            break;
        }
    }
    closedir(d);
    return ok;
}

static void make_duplicate_path(const char *dir, const char *name, bool isdir,
                                char *out_name, size_t name_cap,
                                char *out_path, size_t path_cap)
{
    char stem[FPATH_LEN];
    const char *ext = "";
    strlcpy(stem, name, sizeof(stem));
    if (!isdir)
    {
        char *dot = strrchr(stem, '.');
        if (dot && dot != stem)
        {
            ext = name + (dot - stem);
            *dot = '\0';
        }
    }
    for (int n = 1; n <= 999; n++)
    {
        if (n == 1)
            snprintf(out_name, name_cap, "%s_copy%s", stem, ext);
        else
            snprintf(out_name, name_cap, "%s_copy%d%s", stem, n, ext);
        path_append(out_path, dir, out_name, path_cap);
        struct stat st;
        if (lstat(out_path, &st) != 0)
            return;
    }
}

static bool browse_pick_dest_dir(const char *start_dir, bool is_move,
                                 char *out, size_t out_len)
{
    const char *card_root = dir_exists(PICKER_ROOT) ? PICKER_ROOT : "/";
    struct picker_page dp =
    {
        .base = { .vt = &picker_vtable, .context = CONTEXT_LIST,
                  .allowed = picker_allowed },
        .out = out, .out_len = out_len, .picked = false,
        .dest_mode = true, .dest_is_move = is_move,
    };
    strlcpy(dp.root, card_root, sizeof(dp.root));
    strlcpy(dp.curdir, dir_exists(start_dir) ? start_dir : card_root,
            sizeof(dp.curdir));
    strip_trailing_slash_inplace(dp.curdir);
    pick_reselect[0] = '\0';

    gui_synclist_init(&dp.lists, pick_get_name, &dp, false, 1, NULL);
    gui_synclist_set_svg_icon_callback(&dp.lists, pick_get_svg_icon);
    gui_synclist_set_chevron_callback(&dp.lists, pick_get_chevron);
    pick_load(&dp);

    push_current_activity(ACTIVITY_FILEBROWSER);
    uipod_page_run(&dp.base);
    pop_current_activity();

    return dp.picked && !uipod_home_pending;
}

static void u64_to_grouped_str(uint64_t v, char *out, size_t cap)
{
    char rev[32];
    int n = 0, digits = 0;
    if (v == 0)
    {
        strlcpy(out, "0", cap);
        return;
    }
    while (v > 0 && n < (int)sizeof(rev) - 2)
    {
        if (digits > 0 && (digits % 3) == 0)
            rev[n++] = ',';
        rev[n++] = (char)('0' + (v % 10));
        v /= 10;
        digits++;
    }
    size_t k = 0;
    while (n > 0 && k + 1 < cap)
        out[k++] = rev[--n];
    out[k] = '\0';
}

static void format_human_bytes(uint64_t bytes, bool uk, char *out, size_t cap)
{
    char exact[32];
    u64_to_grouped_str(bytes, exact, sizeof(exact));
    const char *b_unit = uk ? "Б" : "B";
    if (bytes < 1024ULL)
    {
        snprintf(out, cap, "%s %s", exact, b_unit);
        return;
    }
    const char *unit = uk ? "КБ" : "KB";
    uint64_t div = 1024ULL;
    if (bytes >= 1024ULL * 1024ULL * 1024ULL)
    {
        unit = uk ? "ГБ" : "GB";
        div = 1024ULL * 1024ULL * 1024ULL;
    }
    else if (bytes >= 1024ULL * 1024ULL)
    {
        unit = uk ? "МБ" : "MB";
        div = 1024ULL * 1024ULL;
    }
    unsigned long whole = (unsigned long)(bytes / div);
    unsigned long frac  = (unsigned long)(((bytes % div) * 100ULL + div / 2ULL) / div);
    if (frac >= 100UL)
    {
        whole++;
        frac = 0;
    }
    snprintf(out, cap, "%lu.%02lu %s (%s %s)", whole, frac, unit, exact, b_unit);
}

struct dir_stats_acc
{
    uint64_t total_bytes;
    unsigned long files;
    unsigned long dirs;
    unsigned long visited;
    bool capped;
};

static void walk_dir_stats(const char *path, int depth, struct dir_stats_acc *acc)
{
    if (depth > 16 || acc->capped)
        return;
    DIR *d = opendir(path);
    if (!d)
        return;
    struct dirent *e;
    while ((e = readdir(d)))
    {
        if (e->d_name[0] == '.' &&
            (e->d_name[1] == '\0' ||
             (e->d_name[1] == '.' && e->d_name[2] == '\0')))
            continue;
        if (++acc->visited > 10000UL)
        {
            acc->capped = true;
            break;
        }
        if ((acc->visited & 255UL) == 0)
            yield();
        char child[FPATH_LEN];
        path_append(child, path, e->d_name, sizeof(child));
        struct stat st;
        if (lstat(child, &st) != 0)
            continue;
        if (S_ISDIR(st.st_mode))
        {
            acc->dirs++;
            walk_dir_stats(child, depth + 1, acc);
        }
        else
        {
            acc->files++;
            if (st.st_size > 0)
                acc->total_bytes += (uint64_t)st.st_size;
        }
    }
    closedir(d);
}

static bool write_info_report(const char *name, const char *path, bool isdir)
{
    bool uk = is_lang_ukrainian();
    int fd = open(BROWSE_INFO_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0)
        return false;

    fdprintf(fd, "# %s\n\n", name);
    fdprintf(fd, "## %s\n\n", uk ? "Загальне" : "General");

    const char *type_str;
    if (isdir)
        type_str = uk ? "Папка" : "Folder";
    else if (filetype_is_video(name))
        type_str = uk ? "Відеофайл" : "Video File";
    else if ((filetype_get_attr(name) & FILE_ATTR_MASK) == FILE_ATTR_AUDIO)
        type_str = uk ? "Аудіофайл" : "Audio File";
    else if (uipod_plugins_can_open_file(path))
        type_str = uk ? "Документ / Плагін" : "Document / Plugin";
    else
        type_str = uk ? "Файл" : "File";

    fdprintf(fd, "• %s: %s\n", uk ? "Тип" : "Type", type_str);
    fdprintf(fd, "• %s: %s\n", uk ? "Шлях" : "Path", path);

    struct stat st;
    bool have_st = (stat(path, &st) == 0);
    if (isdir)
    {
        struct dir_stats_acc acc = {0};
        walk_dir_stats(path, 0, &acc);
        char sz_buf[80];
        format_human_bytes(acc.total_bytes, uk, sz_buf, sizeof(sz_buf));
        const char *plus = acc.capped ? "+" : "";
        if (uk)
        {
            fdprintf(fd, "• Вміст: %lu%s файлів, %lu%s папок\n",
                     acc.files, plus, acc.dirs, plus);
            fdprintf(fd, "• Загальний розмір: %s%s\n", sz_buf, plus);
        }
        else
        {
            fdprintf(fd, "• Contents: %lu%s files, %lu%s folders\n",
                     acc.files, plus, acc.dirs, plus);
            fdprintf(fd, "• Total Size: %s%s\n", sz_buf, plus);
        }
    }
    else if (have_st)
    {
        char sz_buf[80];
        uint64_t fbytes = st.st_size > 0 ? (uint64_t)st.st_size : 0;
        format_human_bytes(fbytes, uk, sz_buf, sizeof(sz_buf));
        fdprintf(fd, "• %s: %s\n", uk ? "Розмір" : "Size", sz_buf);
    }

    if (have_st)
    {
        struct tm tm_val;
        if (localtime_r(&st.st_mtime, &tm_val))
        {
            fdprintf(fd, "• %s: %04d-%02d-%02d %02d:%02d:%02d\n",
                     uk ? "Змінено" : "Modified",
                     tm_val.tm_year + 1900, tm_val.tm_mon + 1, tm_val.tm_mday,
                     tm_val.tm_hour, tm_val.tm_min, tm_val.tm_sec);
        }
    }

    if (!isdir && (filetype_get_attr(name) & FILE_ATTR_MASK) == FILE_ATTR_AUDIO)
    {
        struct mp3entry *id3 = calloc(1, sizeof(*id3));
        if (id3)
        {
            int afd = open(path, O_RDONLY);
            if (afd >= 0)
            {
                bool got = get_metadata(id3, afd, path);
                close(afd);
                if (got)
                {
                    fdprintf(fd, "\n## %s\n\n",
                             uk ? "Аудіо метадані" : "Audio Metadata");
                    const char *codec = get_codec_string(id3->codectype);
                    if (codec && codec[0])
                        fdprintf(fd, "• %s: %s\n", uk ? "Формат" : "Codec", codec);
                    if (id3->length > 0)
                    {
                        unsigned long sec = id3->length / 1000UL;
                        unsigned long hrs = sec / 3600UL;
                        unsigned long min = (sec % 3600UL) / 60UL;
                        unsigned long rem = sec % 60UL;
                        if (hrs > 0)
                            fdprintf(fd, "• %s: %lu:%02lu:%02lu\n",
                                     uk ? "Тривалість" : "Duration", hrs, min, rem);
                        else
                            fdprintf(fd, "• %s: %lu:%02lu\n",
                                     uk ? "Тривалість" : "Duration", min, rem);
                    }
                    if (id3->bitrate > 0)
                        fdprintf(fd, "• %s: %u kbps%s\n",
                                 uk ? "Бітрейт" : "Bitrate",
                                 id3->bitrate, id3->vbr ? " (VBR)" : "");
                    if (id3->frequency > 0)
                        fdprintf(fd, "• %s: %lu Hz\n",
                                 uk ? "Частота" : "Sample Rate", id3->frequency);
                    if (id3->title && id3->title[0])
                        fdprintf(fd, "• %s: %s\n", uk ? "Назва" : "Title", id3->title);
                    if (id3->artist && id3->artist[0])
                        fdprintf(fd, "• %s: %s\n", uk ? "Виконавець" : "Artist", id3->artist);
                    if (id3->album && id3->album[0])
                        fdprintf(fd, "• %s: %s\n", uk ? "Альбом" : "Album", id3->album);
                    if (id3->albumartist && id3->albumartist[0] &&
                        (!id3->artist || strcmp(id3->albumartist, id3->artist) != 0))
                        fdprintf(fd, "• %s: %s\n",
                                 uk ? "Виконавець альбому" : "Album Artist",
                                 id3->albumartist);
                    if (id3->genre_string && id3->genre_string[0])
                        fdprintf(fd, "• %s: %s\n", uk ? "Жанр" : "Genre", id3->genre_string);
                    if (id3->year > 0)
                        fdprintf(fd, "• %s: %d\n", uk ? "Рік" : "Year", id3->year);
                    else if (id3->year_string && id3->year_string[0])
                        fdprintf(fd, "• %s: %s\n", uk ? "Рік" : "Year", id3->year_string);
                    if (id3->tracknum > 0)
                    {
                        if (id3->discnum > 0)
                            fdprintf(fd, "• %s: %d (%s %d)\n",
                                     uk ? "Трек" : "Track", id3->tracknum,
                                     uk ? "Диск" : "Disc", id3->discnum);
                        else
                            fdprintf(fd, "• %s: %d\n",
                                     uk ? "Трек" : "Track", id3->tracknum);
                    }
#ifdef HAVE_ALBUMART
                    if (id3->has_embedded_albumart)
                        fdprintf(fd, "• %s: %s (%d KB)\n",
                                 uk ? "Вбудована обкладинка" : "Embedded Art",
                                 uk ? "Так" : "Yes",
                                 (id3->albumart.size + 1023) / 1024);
#endif
                }
            }
            free(id3);
        }
    }

    close(fd);
    return true;
}

enum browse_ctx_act
{
    BCTX_PLAY = 0,
    BCTX_PLAY_SHUFFLED,
    BCTX_PLAY_NEXT,
    BCTX_ADD_QUEUE,
    BCTX_ADD_PLAYLIST,
    BCTX_DELETE,
    BCTX_RENAME,
    BCTX_COPY,
    BCTX_MOVE,
    BCTX_INFO,
};

static enum uipod_page_result browse_file_context(struct picker_page *p,
                                                  int sel, bool have_sel)
{
    if (!have_sel)
    {
        /* Empty folder: allow inspecting the current folder's Info */
        char cur_base[FPATH_LEN];
        dir_basename_str(p->curdir, cur_base, sizeof(cur_base));
        static const char *const empty_opts[] = { ID2P(LANG_UIPOD_FILE_INFO) };
        if (uipod_context_menu(cur_base, empty_opts, 1) == 0 &&
            write_info_report(cur_base, p->curdir, true))
        {
            tree_unlock_cache(p->mctx);
            uipod_plugins_open_file(BROWSE_INFO_FILE);
            tree_lock_cache(p->mctx);
            if (uipod_home_pending)
            {
                p->base.home = true;
                return UIPOD_PAGE_DONE;
            }
            music_load(p);
        }
        return UIPOD_PAGE_STAY;
    }

    char chosen[FPATH_LEN];
    strlcpy(chosen, item_name(p, sel), sizeof(chosen));
    char path[FPATH_LEN];
    path_append(path, p->curdir, chosen, sizeof(path));
    bool isdir = item_isdir(p, sel);
    bool playable_audio = !isdir && is_playable_audio_name(chosen);
    browse_save_pos(p);

    const char *opts[10];
    enum browse_ctx_act acts[10];
    int n = 0;

    if (isdir)
    {
        opts[n] = ID2P(LANG_PLAY);                  acts[n++] = BCTX_PLAY;
        opts[n] = ID2P(LANG_UIPOD_PLAY_SHUFFLED);   acts[n++] = BCTX_PLAY_SHUFFLED;
        opts[n] = ID2P(LANG_UIPOD_ADD_TO_PLAYLIST); acts[n++] = BCTX_ADD_PLAYLIST;
    }
    else if (playable_audio)
    {
        opts[n] = ID2P(LANG_PLAY);                    acts[n++] = BCTX_PLAY;
        opts[n] = ID2P(LANG_PLAY_NEXT);               acts[n++] = BCTX_PLAY_NEXT;
        opts[n] = ID2P(LANG_UIPOD_ADD_TO_PLAY_QUEUE); acts[n++] = BCTX_ADD_QUEUE;
        opts[n] = ID2P(LANG_UIPOD_ADD_TO_PLAYLIST);   acts[n++] = BCTX_ADD_PLAYLIST;
    }

    opts[n] = ID2P(LANG_UIPOD_FILE_DELETE); acts[n++] = BCTX_DELETE;
    opts[n] = ID2P(LANG_UIPOD_FILE_RENAME); acts[n++] = BCTX_RENAME;
    opts[n] = ID2P(LANG_UIPOD_FILE_COPY);   acts[n++] = BCTX_COPY;
    opts[n] = ID2P(LANG_UIPOD_FILE_MOVE);   acts[n++] = BCTX_MOVE;
    opts[n] = ID2P(LANG_UIPOD_FILE_INFO);   acts[n++] = BCTX_INFO;

    int pick = uipod_context_menu(chosen, opts, n);
    if (pick < 0 || pick >= n)
        return UIPOD_PAGE_STAY;

    switch (acts[pick])
    {
        case BCTX_PLAY:
        case BCTX_PLAY_SHUFFLED:
        {
            bool shuf = (acts[pick] == BCTX_PLAY_SHUFFLED);
            bool started = false;
            if (isdir)
            {
                tree_unlock_cache(p->mctx);
                started = uipod_queue_add(path, ATTR_DIRECTORY,
                                          PLAYLIST_REPLACE, shuf);
                tree_lock_cache(p->mctx);
            }
            else
            {
                started = folder_play_or_ctx_track(p, sel, chosen, false);
            }
            if (started)
            {
                p->result = GO_TO_WPS;
                return UIPOD_PAGE_DONE;
            }
            strlcpy(pick_reselect, chosen, sizeof(pick_reselect));
            music_load(p);
            break;
        }

        case BCTX_PLAY_NEXT:
            uipod_queue_add(path, FILE_ATTR_AUDIO, PLAYLIST_INSERT, false);
            break;

        case BCTX_ADD_QUEUE:
            uipod_queue_add(path, FILE_ATTR_AUDIO, PLAYLIST_INSERT_LAST, false);
            break;

        case BCTX_ADD_PLAYLIST:
            tree_unlock_cache(p->mctx);
            uipod_playlists_pick(path, isdir ? ATTR_DIRECTORY : FILE_ATTR_AUDIO);
            tree_lock_cache(p->mctx);
            strlcpy(pick_reselect, chosen, sizeof(pick_reselect));
            music_load(p);
            break;

        case BCTX_DELETE:
        {
            if (is_protected_sys_path(path))
            {
                splash(HZ, ID2P(LANG_UIPOD_FILE_OP_FAILED));
                break;
            }
            const char *q = str(isdir ? LANG_UIPOD_DELETE_DIR_Q
                                      : LANG_UIPOD_DELETE_FILE_Q);
            if (uipod_confirm(q, chosen))
            {
                if (delete_path_recursive(path, 0))
                {
                    uipod_library_mark_stale();
                    pick_reselect[0] = '\0';
                    music_load(p);
                    int cnt = item_count(p);
                    if (cnt > 0)
                    {
                        int next_sel = (sel < cnt) ? sel : (cnt - 1);
                        gui_synclist_select_item(&p->lists, next_sel);
                    }
                    browse_save_pos(p);
                    splash(HZ / 2, ID2P(LANG_UIPOD_DELETED));
                }
                else
                {
                    splash(HZ, ID2P(LANG_UIPOD_FILE_OP_FAILED));
                }
            }
            break;
        }

        case BCTX_RENAME:
        {
            char newname[FPATH_LEN];
            strlcpy(newname, chosen, sizeof(newname));
            if (uipod_kbd_input_ex(newname, sizeof(newname), true) == 0)
            {
                if (!is_valid_entry_name(newname))
                {
                    splash(HZ, ID2P(LANG_UIPOD_INVALID_NAME));
                    break;
                }
                if (strcmp(newname, chosen) == 0)
                    break;

                char newpath[FPATH_LEN];
                path_append(newpath, p->curdir, newname, sizeof(newpath));
                struct stat dst_st;
                if (strcasecmp(newname, chosen) != 0 &&
                    lstat(newpath, &dst_st) == 0)
                {
                    splash(HZ, ID2P(LANG_UIPOD_NAME_EXISTS));
                    break;
                }
                if (rename(path, newpath) == 0)
                {
                    uipod_library_mark_stale();
                    strlcpy(pick_reselect, newname, sizeof(pick_reselect));
                    music_load(p);
                    browse_save_pos(p);
                }
                else
                {
                    splash(HZ, ID2P(LANG_UIPOD_RENAME_FAILED));
                }
            }
            break;
        }

        case BCTX_COPY:
        case BCTX_MOVE:
        {
            bool is_move = (acts[pick] == BCTX_MOVE);
            char dest_dir[FPATH_LEN];
            tree_unlock_cache(p->mctx);
            bool picked = browse_pick_dest_dir(p->curdir, is_move,
                                               dest_dir, sizeof(dest_dir));
            tree_lock_cache(p->mctx);
            if (uipod_home_pending)
            {
                p->base.home = true;
                return UIPOD_PAGE_DONE;
            }
            if (!picked)
            {
                strlcpy(pick_reselect, chosen, sizeof(pick_reselect));
                music_load(p);
                break;
            }

            char dst_name[FPATH_LEN];
            char dst_path[FPATH_LEN];
            strlcpy(dst_name, chosen, sizeof(dst_name));
            path_append(dst_path, dest_dir, dst_name, sizeof(dst_path));

            if (isdir && is_path_equal_or_child(path, dest_dir))
            {
                strlcpy(pick_reselect, chosen, sizeof(pick_reselect));
                music_load(p);
                splash(HZ, ID2P(LANG_UIPOD_INVALID_DEST));
                break;
            }

            if (strcmp(path, dst_path) == 0)
            {
                if (is_move)
                {
                    strlcpy(pick_reselect, chosen, sizeof(pick_reselect));
                    music_load(p);
                    break;
                }
                make_duplicate_path(dest_dir, chosen, isdir,
                                    dst_name, sizeof(dst_name),
                                    dst_path, sizeof(dst_path));
            }
            else
            {
                struct stat dst_st;
                if (lstat(dst_path, &dst_st) == 0)
                {
                    if (isdir || S_ISDIR(dst_st.st_mode))
                    {
                        strlcpy(pick_reselect, chosen, sizeof(pick_reselect));
                        music_load(p);
                        splash(HZ, ID2P(LANG_UIPOD_NAME_EXISTS));
                        break;
                    }
                    if (!uipod_confirm(str(LANG_UIPOD_OVERWRITE_Q), chosen))
                    {
                        strlcpy(pick_reselect, chosen, sizeof(pick_reselect));
                        music_load(p);
                        break;
                    }
                    unlink(dst_path);
                }
            }

            bool ok = false;
            if (is_move)
            {
                if (rename(path, dst_path) == 0)
                {
                    ok = true;
                }
                else
                {
                    uipod_fullscreen_message(str(LANG_UIPOD_MOVING), NULL);
                    if (copy_path_recursive(path, dst_path, 0))
                        ok = delete_path_recursive(path, 0);
                }
            }
            else
            {
                uipod_fullscreen_message(str(LANG_UIPOD_COPYING), NULL);
                ok = copy_path_recursive(path, dst_path, 0);
            }

            if (ok)
            {
                uipod_library_mark_stale();
                if (!is_move && strcmp(dest_dir, p->curdir) == 0)
                    strlcpy(pick_reselect, dst_name, sizeof(pick_reselect));
                else if (!is_move)
                    strlcpy(pick_reselect, chosen, sizeof(pick_reselect));
                else
                    pick_reselect[0] = '\0';
                music_load(p);
                if (is_move && item_count(p) > 0)
                {
                    int cnt = item_count(p);
                    int next_sel = (sel < cnt) ? sel : (cnt - 1);
                    gui_synclist_select_item(&p->lists, next_sel);
                }
                browse_save_pos(p);
                splash(HZ / 2, is_move ? ID2P(LANG_UIPOD_MOVED)
                                       : ID2P(LANG_UIPOD_COPIED));
            }
            else
            {
                strlcpy(pick_reselect, chosen, sizeof(pick_reselect));
                music_load(p);
                splash(HZ, ID2P(LANG_UIPOD_FILE_OP_FAILED));
            }
            break;
        }

        case BCTX_INFO:
        {
            if (write_info_report(chosen, path, isdir))
            {
                tree_unlock_cache(p->mctx);
                uipod_plugins_open_file(BROWSE_INFO_FILE);
                tree_lock_cache(p->mctx);
                if (uipod_home_pending)
                {
                    p->base.home = true;
                    return UIPOD_PAGE_DONE;
                }
                strlcpy(pick_reselect, chosen, sizeof(pick_reselect));
                music_load(p);
            }
            break;
        }
    }

    return UIPOD_PAGE_STAY;
}

static enum uipod_page_result picker_on_action(struct uipod_page *self,
                                                 int action)
{
    struct picker_page *p = (struct picker_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);
    bool have_sel = (item_count(p) > 0 && sel >= 0 && sel < item_count(p));

    switch (action)
    {
        case ACTION_STD_OK:          /* A: open a folder, or (music) play a track */
            if (p->dest_mode)
            {
                if (sel == 0)
                {
                    strlcpy(p->out, p->curdir, p->out_len);
                    strip_trailing_slash_inplace(p->out);
                    p->picked = true;
                    return UIPOD_PAGE_DONE;
                }
                int idx = sel - 1;
                if (idx >= 0 && idx < pick_count)
                {
                    char next[FPATH_LEN];
                    path_append(next, p->curdir, pick_namebuf + pick_off[idx],
                                sizeof(next));
                    strip_trailing_slash_inplace(next);
                    strlcpy(p->curdir, next, sizeof(p->curdir));
                    pick_reselect[0] = '\0';
                    pick_slide(p, UIPOD_TRANS_FORWARD);
                }
                return UIPOD_PAGE_STAY;
            }
            if (!have_sel)
                return UIPOD_PAGE_STAY;
            if (p->music && !item_isdir(p, sel))   /* a track: queue folder tracks */
            {
                const char *iname = item_name(p, sel);
                char path[FPATH_LEN];
                path_append(path, p->curdir, iname, sizeof(path));
                browse_save_pos(p);   /* return from Now Playing lands back here */
                if (uipod_plugins_can_open_file(path))
                {
                    tree_unlock_cache(p->mctx);
                    uipod_plugins_open_file(path);
                    tree_lock_cache(p->mctx);
                    if (uipod_home_pending)
                    {
                        p->base.home = true;
                        return UIPOD_PAGE_DONE;
                    }
                    return UIPOD_PAGE_STAY;
                }
                if (p->video || filetype_is_video(iname))
                {
                    /* A cut, not a slide: the video leaves on black. */
                    uipod_video_play(path);
                    uipod_transition_suppress_next();
                    uipod_transition_arm_back();
                    return UIPOD_PAGE_STAY;
                }
                if (folder_play_or_ctx_track(p, sel, iname, false))
                {
                    p->result = GO_TO_WPS;
                    return UIPOD_PAGE_DONE;
                }
                return UIPOD_PAGE_STAY;   /* play cancelled at a prompt */
            }
            {                                      /* a folder -> descend (slide) */
                char chosen[FPATH_LEN];
                strlcpy(chosen, item_name(p, sel), sizeof(chosen));
                if (!chosen[0])
                    return UIPOD_PAGE_STAY;
                char next[FPATH_LEN];
                path_append(next, p->curdir, chosen, sizeof(next));
                strip_trailing_slash_inplace(next);
                strlcpy(p->curdir, next, sizeof(p->curdir));
                pick_reselect[0] = '\0';
                pick_slide(p, UIPOD_TRANS_FORWARD);
                browse_save_pos(p);
            }
            return UIPOD_PAGE_STAY;

        case ACTION_STD_CONTEXT:     /* Hold A / MENU: context menu / pick folder */
            if (p->dest_mode)
            {
                if (sel == 0)
                {
                    strlcpy(p->out, p->curdir, p->out_len);
                }
                else if (sel >= 1 && sel <= pick_count)
                {
                    path_append(p->out, p->curdir,
                                pick_namebuf + pick_off[sel - 1], p->out_len);
                }
                else
                {
                    return UIPOD_PAGE_STAY;
                }
                strip_trailing_slash_inplace(p->out);
                p->picked = true;
                return UIPOD_PAGE_DONE;
            }
            if (p->music)
            {
                if (p->full_browse)
                    return browse_file_context(p, sel, have_sel);

                if (have_sel && !p->video)
                {
                    char chosen[FPATH_LEN];
                    strlcpy(chosen, item_name(p, sel), sizeof(chosen));
                    char path[FPATH_LEN];
                    path_append(path, p->curdir, chosen, sizeof(path));
                    bool isdir = item_isdir(p, sel);
                    browse_save_pos(p);   /* before an insert reuses mctx */
                    bool started = false;
                    if (isdir)
                    {
                        /* Unpin mctx while uipod_music_context may walk subfolders */
                        tree_unlock_cache(p->mctx);
                        started = uipod_music_context(chosen, path, ATTR_DIRECTORY);
                        tree_lock_cache(p->mctx);
                    }
                    else
                    {
                        started = folder_play_or_ctx_track(p, sel, chosen, true);
                    }
                    if (started)
                    {
                        p->result = GO_TO_WPS;
                        return UIPOD_PAGE_DONE;
                    }
                    if (isdir)   /* the insert walked the folder through mctx */
                    {
                        strlcpy(pick_reselect, p->pos->last_sel,
                                sizeof(pick_reselect));
                        music_load(p);
                    }
                }
                return UIPOD_PAGE_STAY;
            }
            /* folder-picker: pick the highlighted folder (or curdir if empty) */
            if (have_sel)
            {
                path_append(p->out, p->curdir, item_name(p, sel), p->out_len);
                strip_trailing_slash_inplace(p->out);
            }
            else
            {
                strlcpy(p->out, p->curdir, p->out_len);
                strip_trailing_slash_inplace(p->out);
            }
            p->picked = true;
            return UIPOD_PAGE_DONE;

        case ACTION_STD_CANCEL:      /* B: up one level, or leave at the root */
        {
            size_t rlen = strlen(p->root);
            bool outside_root = (rlen > 1) &&
                (strncmp(p->curdir, p->root, rlen) != 0 ||
                 (p->curdir[rlen] != '\0' && p->curdir[rlen] != '/'));
            if (strcmp(p->curdir, p->root) == 0 ||
                strcmp(p->curdir, "/") == 0 ||
                outside_root)
            {
                if (p->music)                      /* slide the main menu back in */
                {
                    browse_save_pos(p);   /* remember the root highlight too */
                    uipod_transition_arm_back();
                }
                return UIPOD_PAGE_DONE;
            }
            {
                /* Remember the folder we're leaving so the parent re-highlights
                 * it (not row 0) once it reloads. */
                const char *leaf = NULL;
                size_t leaf_len = path_basename(p->curdir, &leaf);
                if (leaf && leaf_len > 0)
                {
                    if (leaf_len >= sizeof(pick_reselect))
                        leaf_len = sizeof(pick_reselect) - 1;
                    memcpy(pick_reselect, leaf, leaf_len);
                    pick_reselect[leaf_len] = '\0';
                }
                else
                {
                    pick_reselect[0] = '\0';
                }

                const char *par = NULL;
                size_t n = path_dirname(p->curdir, &par);
                char parent[FPATH_LEN];
                if (!par || n == 0)
                {
                    strlcpy(parent, p->root, sizeof(parent));
                }
                else
                {
                    if (n >= sizeof(parent))
                        n = sizeof(parent) - 1;
                    memcpy(parent, par, n);
                    parent[n] = '\0';
                    strip_trailing_slash_inplace(parent);
                }
                if (strcmp(parent, p->curdir) == 0)
                {
                    if (p->music)
                    {
                        browse_save_pos(p);
                        uipod_transition_arm_back();
                    }
                    return UIPOD_PAGE_DONE;
                }
                strlcpy(p->curdir, parent, sizeof(p->curdir));
                pick_slide(p, UIPOD_TRANS_BACK);
                browse_save_pos(p);
            }
            return UIPOD_PAGE_STAY;
        }
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable picker_vtable =
{
    .legend    = picker_legend,
    .draw      = picker_draw,
    .poll      = picker_poll,
    .on_action = picker_on_action,
};

/* A (descend/play), B (up/leave), Hold-A (music: context menu / picker: add folder) */
static const int picker_allowed[] =
    { ACTION_STD_OK, ACTION_STD_CANCEL, ACTION_STD_CONTEXT, -1 };

/* Returns true and fills out[] with the chosen folder if the user picked one. */
static bool folder_pick(char *out, size_t out_len)
{
    struct picker_page p =
    {
        .base = { .vt = &picker_vtable, .context = CONTEXT_LIST,
                  .allowed = picker_allowed },
        .out = out, .out_len = out_len, .picked = false,
    };
    strlcpy(p.root, "/", sizeof(p.root));
    strlcpy(p.curdir, dir_exists(PICKER_ROOT) ? PICKER_ROOT : "/",
            sizeof(p.curdir));

    gui_synclist_init(&p.lists, pick_get_name, &p, false, 1, NULL);
    gui_synclist_set_svg_icon_callback(&p.lists, pick_get_svg_icon);
    gui_synclist_set_chevron_callback(&p.lists, pick_get_chevron);
    pick_load(&p);

    push_current_activity(ACTIVITY_FILEBROWSER);
    uipod_page_run(&p.base);
    pop_current_activity();

    return p.picked;
}

bool uipod_folder_pick(char *out, size_t out_len)
{
    uint8_t prev_src = uipod_gpu_get_header_source();
    uipod_gpu_set_header_source(UIPOD_SRC_ICON_MICROSD);
    bool ok = folder_pick(out, out_len);
    uipod_gpu_set_header_source(prev_src);
    return ok;
}

/* ---- Files hub & Settings -> "<Name> Folders" ------------------------- */

static bool browse_run(const char *root, const char *start, int title_lang,
                       struct browse_pos *pos, bool show_empty, bool video,
                       bool show_all, int *result);

struct folders_page
{
    struct uipod_page base;
    struct gui_synclist lists;
    struct folder_category *cat;
    bool browse_mode;           /* true = Files/Videos hub (#694); false = Settings */
    int  title_lang;
    int  result;
};

static const char *mgmt_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    struct folders_page *p = (struct folders_page *)data;
    struct folder_category *cat = p->cat;
    if (sel == cat->n_folders)
        return str(LANG_UIPOD_ADD_DIRECTORY);
    if (p->browse_mode)
        return dir_basename_str(cat->folders[sel], buf, buf_len);
    snprintf(buf, buf_len, "%s", cat->folders[sel]);
    return buf;
}

static const struct uipod_svg_icon *mgmt_get_svg_icon(int sel, void *data)
{
    struct folders_page *p = (struct folders_page *)data;
    struct folder_category *cat = p->cat;
    if (sel < 0 || sel > cat->n_folders)
        return NULL;
    if (sel == cat->n_folders)
        return uipod_svg_tag("add_dir");
    return uipod_svg_tag("folder");
}

static bool mgmt_get_chevron(int sel, void *data)
{
    struct folders_page *p = (struct folders_page *)data;
    struct folder_category *cat = p->cat;
    return sel >= 0 && sel <= cat->n_folders;
}

static void folders_page_draw(struct uipod_page *self)
{
    struct folders_page *p = (struct folders_page *)self;
    gui_synclist_set_title(&p->lists, (char *)str(p->title_lang),
                           p->browse_mode ? Icon_Audio : Icon_file_view_menu);
    gui_synclist_draw(&p->lists);
}

static int folders_page_poll(struct uipod_page *self, int timeout)
{
    struct folders_page *p = (struct folders_page *)self;
    int action;
    list_do_action(self->context, timeout, &p->lists, &action);
    return action;
}

static enum uipod_page_result folders_page_on_action(struct uipod_page *self,
                                                       int action)
{
    struct folders_page *p = (struct folders_page *)self;
    struct folder_category *cat = p->cat;
    int sel = gui_synclist_get_sel_pos(&p->lists);

    if (action == ACTION_STD_CANCEL)            /* B: leave */
    {
        if (p->browse_mode)
            uipod_transition_arm_back();
        return UIPOD_PAGE_DONE;
    }

    if (action == ACTION_STD_CONTEXT && p->browse_mode &&
        sel >= 0 && sel < cat->n_folders)
    {
        const char *dir = cat->folders[sel];
        if (!cat->video && dir_exists(dir))
        {
            char name[FPATH_LEN];
            dir_basename_str(dir, name, sizeof(name));
            static const char *const hub_opts[] = {
                ID2P(LANG_PLAY),
                ID2P(LANG_UIPOD_PLAY_SHUFFLED),
                ID2P(LANG_UIPOD_ADD_TO_PLAYLIST),
                ID2P(LANG_UIPOD_REMOVE_DIRECTORY),
            };
            int act = uipod_context_menu(name, hub_opts, 4);
            if (act == 0 || act == 1)
            {
                if (uipod_queue_add(dir, ATTR_DIRECTORY, PLAYLIST_REPLACE,
                                      act == 1))
                {
                    p->result = GO_TO_WPS;
                    return UIPOD_PAGE_DONE;
                }
            }
            else if (act == 2)
                uipod_playlists_pick(dir, ATTR_DIRECTORY);
            else if (act == 3)
            {
                if (uipod_confirm(str(LANG_UIPOD_REMOVE_FOLDER_Q), dir))
                {
                    folders_remove(cat, sel);
                    gui_synclist_set_nb_items(&p->lists, cat->n_folders + 1);
                    if (sel > cat->n_folders)
                        gui_synclist_select_item(&p->lists, cat->n_folders);
                }
            }
        }
        else if (uipod_confirm(str(LANG_UIPOD_REMOVE_FOLDER_Q), dir))
        {
            folders_remove(cat, sel);
            gui_synclist_set_nb_items(&p->lists, cat->n_folders + 1);
            if (sel > cat->n_folders)
                gui_synclist_select_item(&p->lists, cat->n_folders);
        }
        return UIPOD_PAGE_STAY;
    }

    if (action == ACTION_STD_OK)                /* A */
    {
        if (sel == cat->n_folders)              /* "Add directory" */
        {
            char picked[FPATH_LEN];
            if (folder_pick(picked, sizeof(picked)))
            {
                if (!folders_add(cat, picked))
                    splash(HZ, ID2P(LANG_UIPOD_FOLDER_ADD_FAILED));
                gui_synclist_set_nb_items(&p->lists, cat->n_folders + 1);
                if (cat->n_folders > 0)
                    gui_synclist_select_item(&p->lists, cat->n_folders - 1);
            }
        }
        else if (sel >= 0 && sel < cat->n_folders)   /* a folder row */
        {
            if (p->browse_mode)
            {
                const char *dir = cat->folders[sel];
                if (strcmp(dir, cat->default_path) == 0 && !dir_exists(dir))
                    mkdir(dir);
                if (!dir_exists(dir))
                {
                    splash(HZ, ID2P(LANG_FILE_NOT_FOUND));
                    folders_page_draw(&p->base);
                    return UIPOD_PAGE_STAY;
                }
                int r = GO_TO_ROOT;
                browse_run(dir, dir, cat->browse_lang, &cat->pos, true,
                           cat->video, false, &r);
                if (r == GO_TO_WPS)
                {
                    cat->active_in_dir = true;
                    cat->active_idx = sel;
                    p->result = GO_TO_WPS;
                    return UIPOD_PAGE_DONE;
                }
                return UIPOD_PAGE_STAY;
            }
            if (uipod_confirm(str(LANG_UIPOD_REMOVE_FOLDER_Q), cat->folders[sel]))
            {
                folders_remove(cat, sel);
                gui_synclist_set_nb_items(&p->lists, cat->n_folders + 1);
                if (sel > cat->n_folders)
                    gui_synclist_select_item(&p->lists, cat->n_folders);
            }
        }
    }
    return UIPOD_PAGE_STAY;                    /* the runner redraws each frame */
}

static const struct uipod_page_vtable folders_vtable =
{
    .legend    = NULL,                  /* normal header (no legend) */
    .draw      = folders_page_draw,
    .poll      = folders_page_poll,
    .on_action = folders_page_on_action,
};

/* A (open/add/remove), B (back), Hold-A (context menu on Files hub) */
static const int folders_allowed[] =
    { ACTION_STD_OK, ACTION_STD_CANCEL, ACTION_STD_CONTEXT, -1 };

static int folders_settings(struct folder_category *cat)
{
    folders_load(cat);

    struct folders_page p =
    {
        .base = { .vt = &folders_vtable, .context = CONTEXT_LIST,
                  .allowed = folders_allowed },
        .cat = cat,
        .browse_mode = false,
        .title_lang = cat->title_lang,
        .result = 0,
    };
    gui_synclist_init(&p.lists, mgmt_get_name, &p, false, 1, NULL);
    gui_synclist_set_svg_icon_callback(&p.lists, mgmt_get_svg_icon);
    gui_synclist_set_chevron_callback(&p.lists, mgmt_get_chevron);
    gui_synclist_set_title(&p.lists, (char *)str(p.title_lang),
                           Icon_file_view_menu);
    gui_synclist_set_nb_items(&p.lists, cat->n_folders + 1);
    gui_synclist_select_item(&p.lists, 0);

    uipod_page_run(&p.base);
    return 0;
}

/* ---- root virtual-folder entry (flattened view) ----------------------- */

/* Remove every entry currently in the farm dir (each is a symlink). */
static void farm_clear(struct folder_category *cat)
{
    DIR *d = opendir(cat->farm_path);
    if (!d)
        return;
    struct dirent *e;
    while ((e = readdir(d)))
    {
        if (e->d_name[0] == '.' &&
            (e->d_name[1] == '\0' ||
             (e->d_name[1] == '.' && e->d_name[2] == '\0')))
            continue;
        char path[FPATH_LEN];
        path_append(path, cat->farm_path, e->d_name, sizeof(path));
        remove(path);
    }
    closedir(d);
}

/* (Re)build the symlink farm. Returns the number of links created.
 * First folder wins on name collisions (symlink() fails with EEXIST).
 * Folders that don't exist are silently skipped. */
static int farm_build(struct folder_category *cat)
{
    mkdir(cat->farm_path);
    farm_clear(cat);

    int created = 0;
    for (int i = 0; i < cat->n_folders; i++)
    {
        DIR *d = opendir(cat->folders[i]);
        if (!d)
            continue;                  /* missing/unreadable folder: skip */
        struct dirent *e;
        while ((e = readdir(d)))
        {
            if (e->d_name[0] == '.')   /* skip ., .., hidden */
                continue;
            char target[FPATH_LEN], link[FPATH_LEN];
            path_append(target, cat->folders[i], e->d_name, sizeof(target));
            path_append(link, cat->farm_path, e->d_name, sizeof(link));
            if (symlink(target, link) == 0)
                created++;
        }
        closedir(d);
    }
    return created;
}

/* Resolve a category's flattened root the same way for browsing and shuffle:
 * a single existing source folder is used directly; several are merged into the
 * symlink farm.  Returns NULL if no source folder currently exists.  The folder
 * list must already be loaded; the returned pointer is owned by `cat`. */
static const char *folder_resolve_root(struct folder_category *cat)
{
    /* Only existing source folders matter; a missing folder (the seeded default
     * included) is fine -- it's simply not part of the view. */
    int n_exist = 0, only = -1;
    for (int i = 0; i < cat->n_folders; i++)
        if (dir_exists(cat->folders[i])) { n_exist++; only = i; }

    if (n_exist == 1)
        return cat->folders[only];          /* single source: use it directly */
    if (n_exist > 1 && farm_build(cat) > 0)
        return cat->farm_path;              /* several: a merged symlink farm */
    return NULL;
}

/* Browse with the stock browse engine, rendered by our picker: folders descend,
 * a track tap plays that track alone, B walks up to `root` and leaves from
 * there; a fresh open lands in `start` (under root).  The global tree_context
 * is borrowed and restored.  A start with no folders/audio is shown only if
 * `show_empty`; otherwise nothing is shown and false comes back.  The page
 * result lands in *result. */
static bool browse_run(const char *root, const char *start, int title_lang,
                       struct browse_pos *pos, bool show_empty, bool video,
                       bool show_all, int *result)
{
    struct tree_context *c = tree_get_context();
    static int browse_filter;
    int  *saved_filter = c->dirfilter;
    int   saved_sort   = c->sort_dir;
    bool  saved_brows  = c->is_browsing;
    struct browse_context *saved_browse = c->browse;
    char  saved_last[sizeof(global_status.browse_last_folder)];
    memcpy(saved_last, global_status.browse_last_folder, sizeof(saved_last));

    browse_filter  = show_all ? SHOW_ALL : (video ? SHOW_VIDEO : SHOW_MUSIC);
    c->dirfilter   = &browse_filter;    /* dirs + files, folders-first */
    c->sort_dir    = SORT_ALPHA;
    c->is_browsing = false;
    c->browse      = NULL;
    c->out_of_tree = 0;                 /* ensure ft_load actually loads */

    struct picker_page p =
    {
        /* enter_honor_back: returning from Now Playing (which armed a back)
         * slides the browser in L->R; a fresh open from the root still
         * enters forward.  no_arm_back: the exit slide is armed explicitly
         * (only on B at the root), not automatically. */
        .base = { .vt = &picker_vtable, .context = CONTEXT_LIST,
                  .allowed = picker_allowed, .no_arm_back = true,
                  .enter_honor_back = true },
        .music = true, .video = video, .full_browse = show_all,
        .result = GO_TO_ROOT,                  /* root re-highlights our entry */
        .pos = pos, .mctx = c,
    };
    strlcpy(p.root, root, sizeof(p.root));
    strip_trailing_slash_inplace(p.root);

    char clean_start[FPATH_LEN];
    strlcpy(clean_start, start, sizeof(clean_start));
    strip_trailing_slash_inplace(clean_start);

    /* Restore the last browse position (incl. on return from Now Playing):
     * the saved folder if it still exists and sits under this root, else
     * `start`.  pick_reselect re-highlights the saved entry on load. */
    size_t root_len = strlen(p.root);
    bool valid_last = pos->have_last && dir_exists(pos->last_dir) &&
        (strcmp(p.root, "/") == 0 ||
         (strncmp(pos->last_dir, p.root, root_len) == 0 &&
          (pos->last_dir[root_len] == '\0' || pos->last_dir[root_len] == '/')));
    if (valid_last)
    {
        strlcpy(p.curdir, pos->last_dir, sizeof(p.curdir));
        strip_trailing_slash_inplace(p.curdir);
        strlcpy(pick_reselect, pos->last_sel, sizeof(pick_reselect));
    }
    else
    {
        strlcpy(p.curdir, clean_start, sizeof(p.curdir));
        pick_reselect[0] = '\0';
    }

    /* Pin tree_context entry/name buffers against background buflib compaction */
    tree_lock_cache(c);

    gui_synclist_init(&p.lists, pick_get_name, &p, false, 1, NULL);
    gui_synclist_set_svg_icon_callback(&p.lists, pick_get_svg_icon);
    gui_synclist_set_chevron_callback(&p.lists, pick_get_chevron);
    (void)title_lang;
    music_load(&p);

    /* If a restored category folder turned up empty (its audio files were
     * removed), fall back to `start` rather than showing the "(empty)" page.
     * In full Browse (`show_all`), an existing empty folder is intentionally kept. */
    if (!show_all && item_count(&p) == 0 && strcmp(p.curdir, clean_start) != 0)
    {
        strlcpy(p.curdir, clean_start, sizeof(p.curdir));
        pick_reselect[0] = '\0';
        music_load(&p);
    }

    bool browsed = show_empty || item_count(&p) > 0;
    if (browsed)
    {
        push_current_activity(ACTIVITY_FILEBROWSER);
        uipod_page_run(&p.base);
        pop_current_activity();
        browse_save_pos(&p);
    }

    tree_unlock_cache(c);

    /* Restore the tree context untouched. */
    c->dirfilter   = saved_filter;
    c->sort_dir    = saved_sort;
    c->is_browsing = saved_brows;
    c->browse      = saved_browse;
    memcpy(global_status.browse_last_folder, saved_last, sizeof(saved_last));

    if (!browsed)
        return false;
    *result = p.base.home ? GO_TO_ROOT : p.result;
    return true;
}

static int folder_browse(struct folder_category *cat)
{
    folders_load(cat);

    int start_sel = 0;
    if (cat->active_in_dir)
    {
        int idx = cat->active_idx;
        cat->active_in_dir = false;
        if (idx >= 0 && idx < cat->n_folders && dir_exists(cat->folders[idx]))
        {
            start_sel = idx;
            int r = GO_TO_ROOT;
            browse_run(cat->folders[idx], cat->folders[idx], cat->browse_lang,
                       &cat->pos, true, cat->video, false, &r);
            if (r == GO_TO_WPS)
            {
                cat->active_in_dir = true;
                cat->active_idx = idx;
                return GO_TO_WPS;
            }
            if (uipod_home_pending)
                return GO_TO_ROOT;
        }
    }

    struct folders_page p =
    {
        .base = { .vt = &folders_vtable, .context = CONTEXT_LIST,
                  .allowed = folders_allowed, .no_arm_back = true,
                  .enter_honor_back = true },
        .cat = cat,
        .browse_mode = true,
        .title_lang = cat->browse_lang,
        .result = GO_TO_ROOT,
    };
    gui_synclist_init(&p.lists, mgmt_get_name, &p, false, 1, NULL);
    gui_synclist_set_svg_icon_callback(&p.lists, mgmt_get_svg_icon);
    gui_synclist_set_chevron_callback(&p.lists, mgmt_get_chevron);
    gui_synclist_set_title(&p.lists, (char *)str(p.title_lang), Icon_Audio);
    gui_synclist_set_nb_items(&p.lists, cat->n_folders + 1);
    if (start_sel < 0 || start_sel > cat->n_folders)
        start_sel = 0;
    gui_synclist_select_item(&p.lists, start_sel);

    push_current_activity(ACTIVITY_FILEBROWSER);
    uipod_page_run(&p.base);
    pop_current_activity();

    return p.base.home ? GO_TO_ROOT : p.result;
}

/* ---- public entry points (one thin wrapper per category) -------------- */

int uipod_music_settings(void)     { return folders_settings(&cat_music); }
int uipod_podcast_settings(void)   { return folders_settings(&cat_podcast); }
int uipod_audiobook_settings(void) { return folders_settings(&cat_audiobook); }
int uipod_movies_settings(void)    { return folders_settings(&cat_movies); }
int uipod_television_settings(void) { return folders_settings(&cat_television); }

int uipod_music_browse(void *param)
{
    (void)param;
    uint8_t prev_src = uipod_gpu_get_header_source();
    uipod_gpu_set_header_source(UIPOD_SRC_ICON_MICROSD);
    int r = folder_browse(&cat_music);
    uipod_gpu_set_header_source(prev_src);
    return r;
}
int uipod_podcast_browse(void *param)   { (void)param; return folder_browse(&cat_podcast); }
int uipod_audiobook_browse(void *param) { (void)param; return folder_browse(&cat_audiobook); }
int uipod_movies_browse(void *param)
{
    (void)param;
    if (uipod_jellyfin_has_media())
        return uipod_jellyfin_videos_browse(false);
    return folder_browse(&cat_movies);
}
int uipod_television_browse(void *param)
{
    (void)param;
    if (uipod_jellyfin_has_media())
        return uipod_jellyfin_videos_browse(true);
    return folder_browse(&cat_television);
}

/* root "Browse": full SD-card file browser & manager, opening at the last
 * visited directory (persisted across restarts in BROWSE_LAST_FILE). */
int uipod_files_browse(void *param)
{
    (void)param;
    uint8_t prev_src = uipod_gpu_get_header_source();
    uipod_gpu_set_header_source(UIPOD_SRC_ICON_MICROSD);
    const char *card_root = dir_exists(PICKER_ROOT) ? PICKER_ROOT : "/";
    files_browse_load_pos(card_root);
    int r = GO_TO_ROOT;
    browse_run(card_root, card_root,
               LANG_UIPOD_BROWSE, &s_files_browse_pos, true, false, true, &r);
    uipod_gpu_set_header_source(prev_src);
    return r;
}

/* ---- root "Shuffle All": shuffle + play the whole Music library -----------
 * Every Music track from the index (no filesystem walk), shuffled, straight to
 * Now Playing.  Music only -- podcasts/audiobooks are never shuffled. */
int uipod_shuffle_all(void *param)
{
    (void)param;
    uipod_library_refresh_if_stale();
    folders_load(&cat_music);

    bool has_any_music = (folder_resolve_root(&cat_music) != NULL) ||
                         uipod_navidrome_has_tracks() ||
                         uipod_jellyfin_has_tracks();
    if (!has_any_music)
    {
        folders_settings(&cat_music);
        return GO_TO_ROOT;
    }

    struct uipod_queue q;
    if (!uipod_queue_begin(&q, PLAYLIST_REPLACE))   /* declined the erase */
        return GO_TO_ROOT;
    uipod_library_music_paths(uipod_queue_add_path, &q);
    if (!uipod_queue_end(&q, true))                 /* nothing indexed */
    {
        splash(HZ, ID2P(LANG_UIPOD_NO_MUSIC));
        uipod_transition_suppress_next();   /* only a splash: don't re-slide the menu */
        return GO_TO_ROOT;
    }
    return GO_TO_WPS;
}
