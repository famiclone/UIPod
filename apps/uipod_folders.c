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
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>   /* qsort */
#include <string.h>
#include "string-extra.h"
#include "strnatcmp.h"   /* natural alphabetical sort (2 before 10) */
#include "kernel.h"      /* HZ */
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
#include "playlist.h"    /* play a track on select */
#include "uipod_folders.h"
#include "uipod_ui.h"
#include "uipod_page.h"
#include "uipod_transition.h"   /* slide on folder descend/ascend */
#include "uipod_playlists.h"    /* Hold-A = add highlighted file/folder to a playlist */
#include "uipod_library.h"      /* Shuffle All's track set */
#include "uipod_navidrome.h"
#include "uipod_jellyfin.h"
#include "uipod_rs_ui.h"
#include "uipod_video.h"
#include "uipod_svg.h"

#define PICKER_ROOT     "/mnt/SDCARD"   /* pickers open here, walk up to / */
#define MAX_FOLDERS     24
#define FPATH_LEN       MAX_PATH
#define LOAD_BUFSZ      (MAX_FOLDERS * FPATH_LEN)

/* libc symlink(); Rockbox does not wrap it and root paths map 1:1 to the OS
 * (app_root_realpath() == "/"), so OS and Rockbox paths are interchangeable. */
extern int symlink(const char *target, const char *linkpath);

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
    .store_file = "folders.txt", .default_path = "/mnt/SDCARD/Music",
    .farm_path = "/tmp/uipod_music",
    .title_lang = LANG_UIPOD_FOLDERS, .browse_lang = LANG_UIPOD_FILES,
};
static struct folder_category cat_podcast = {
    .store_file = "podcasts.txt", .default_path = "/mnt/SDCARD/Podcasts",
    .farm_path = "/tmp/uipod_podcasts",
    .title_lang = LANG_UIPOD_PODCAST_FOLDERS,
    .browse_lang = LANG_UIPOD_PODCASTS,
};
static struct folder_category cat_audiobook = {
    .store_file = "audiobooks.txt", .default_path = "/mnt/SDCARD/Audiobooks",
    .farm_path = "/tmp/uipod_audiobooks",
    .title_lang = LANG_UIPOD_AUDIOBOOK_FOLDERS,
    .browse_lang = LANG_UIPOD_AUDIOBOOKS,
};
static struct folder_category cat_movies = {
    .store_file = "movies.txt", .default_path = "/mnt/SDCARD/Movies",
    .farm_path = "/tmp/uipod_movies",
    .title_lang = LANG_UIPOD_MOVIE_FOLDERS,
    .browse_lang = LANG_UIPOD_VIDEOS, .video = true,
};
static struct folder_category cat_television = {
    .store_file = "television.txt", .default_path = "/mnt/SDCARD/Television",
    .farm_path = "/tmp/uipod_television",
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
            strlcpy(cat->folders[cat->n_folders++], "/mnt/SDCARD/Media", FPATH_LEN);
        else if (cat == &cat_movies && cat->n_folders < MAX_FOLDERS)
            strlcpy(cat->folders[cat->n_folders++], "/mnt/SDCARD/Media", FPATH_LEN);
        else if (cat == &cat_television && cat->n_folders < MAX_FOLDERS)
            strlcpy(cat->folders[cat->n_folders++], "/mnt/SDCARD/Media/jellyfin/Shows", FPATH_LEN);
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
    /* folder-picker mode (out!=NULL): Hold A picks a folder into out[] */
    char  *out;
    size_t out_len;
    bool   picked;
    /* music-browse mode (music=true): rows are folders+tracks, A plays a track,
     * exits with `result` for the root dispatch (GO_TO_WPS / GO_TO_ROOT) */
    bool   music;
    bool   video;                  /* music mode over video files */
    int    result;
    struct browse_pos *pos;        /* music mode: where the position persists */
    struct tree_context *mctx;     /* music mode: the stock browse engine state */
};

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
    if (!p->music && strcmp(p->curdir, start_dir) == 0)
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
    gui_synclist_set_nb_items(&p->lists, pick_count);

    /* Default to the top, unless an ascend asked us to re-highlight the folder
     * we just came out of (matched by name). */
    int sel = 0;
    if (pick_reselect[0])
    {
        for (int i = 0; i < pick_count; i++)
            if (strcmp(pick_namebuf + pick_off[i], pick_reselect) == 0)
            {
                sel = i;
                break;
            }
        pick_reselect[0] = '\0';
    }
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
    (void)buf; (void)buf_len;
    return item_name((struct picker_page *)data, sel);
}

static const struct uipod_svg_icon *pick_get_svg_icon(int sel, void *data)
{
    struct picker_page *p = (struct picker_page *)data;
    if (sel < 0 || sel >= item_count(p))
        return NULL;
    if (!p->music)
        return uipod_svg_tag("folder");
    return uipod_svg_for_path(item_name(p, sel), item_isdir(p, sel));
}

static bool pick_get_chevron(int sel, void *data)
{
    struct picker_page *p = (struct picker_page *)data;
    return sel >= 0 && sel < item_count(p);
}

static const char *picker_legend(struct uipod_page *self)
{
    /* music: no legend (A plays / opens, Hold-A adds to a playlist, B backs out).
     * folder-picker (Settings): A opens and B backs (universal), but Hold-A to add
     * is the one non-obvious gesture, so publicize just that. */
    return ((struct picker_page *)self)->music ? NULL
                                               : str(LANG_UIPOD_HOLD_A_TO_ADD);
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
        if (!nm || !nm[0] || filetype_is_video(nm))
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
        if (!nm || !nm[0] || filetype_is_video(nm))
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

static enum uipod_page_result picker_on_action(struct uipod_page *self,
                                                 int action)
{
    struct picker_page *p = (struct picker_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);
    bool have_sel = (item_count(p) > 0 && sel >= 0 && sel < item_count(p));

    switch (action)
    {
        case ACTION_STD_OK:          /* A: open a folder, or (music) play a track */
            if (!have_sel)
                return UIPOD_PAGE_STAY;
            if (p->music && !item_isdir(p, sel))   /* a track: queue folder tracks */
            {
                const char *iname = item_name(p, sel);
                char path[FPATH_LEN];
                path_append(path, p->curdir, iname, sizeof(path));
                browse_save_pos(p);   /* return from Now Playing lands back here */
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
            }
            return UIPOD_PAGE_STAY;

        case ACTION_STD_CONTEXT:     /* Hold A: music context menu / pick folder */
            if (p->music)
            {
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
                       int *result);

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
                           cat->video, &r);
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
                       int *result)
{
    struct tree_context *c = tree_get_context();
    static int browse_filter;
    int  *saved_filter = c->dirfilter;
    int   saved_sort   = c->sort_dir;
    bool  saved_brows  = c->is_browsing;
    struct browse_context *saved_browse = c->browse;
    char  saved_last[sizeof(global_status.browse_last_folder)];
    memcpy(saved_last, global_status.browse_last_folder, sizeof(saved_last));

    browse_filter  = video ? SHOW_VIDEO : SHOW_MUSIC;
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
        .music = true, .video = video,
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

    /* If a restored folder turned up empty (its files were since removed),
     * fall back to `start` rather than showing the "(empty)" page. */
    if (item_count(&p) == 0 && strcmp(p.curdir, clean_start) != 0)
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
                       &cat->pos, true, cat->video, &r);
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

/* root "Browse": the whole filesystem in the same browser as the categories.
 * Every entry opens at the card -- no remembered position, which could be the
 * Linux root after backing out.  An empty folder is just an empty list. */
int uipod_files_browse(void *param)
{
    (void)param;
    uint8_t prev_src = uipod_gpu_get_header_source();
    uipod_gpu_set_header_source(UIPOD_SRC_ICON_MICROSD);
    struct browse_pos pos = {0};   /* fresh each time: open at the card */
    int r = GO_TO_ROOT;
    browse_run("/", dir_exists(PICKER_ROOT) ? PICKER_ROOT : "/",
               LANG_UIPOD_BROWSE, &pos, true, false, &r);
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
