/***************************************************************************
 * UIPod: MIDI SoundFont (.sf2) persistence & flat SoundFont selector.
 *
 * Bundled default SoundFont: `Mobile_0100_base.sf2` (embedded in `uipod-codecs`).
 * Additional `.sf2` banks are discovered strictly from the user's configured
 * `Settings -> Library -> SoundFont Folders` (`soundfonts.txt`), persisted in
 * `ROCKBOX_DIR "/soundfont.cfg"`, and hot-swapped live into `MidiDecoder`.
 ****************************************************************************/
#include "config.h"
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include "string-extra.h"
#include "strnatcmp.h"
#include "kernel.h"
#include "file.h"
#include "dir.h"
#include "pathfuncs.h"
#include "rbpaths.h"
#include "settings.h"
#include "lang.h"
#include "splash.h"
#include "action.h"
#include "gui/list.h"
#include "icon.h"
#include "uipod_folders.h"
#include "uipod_page.h"
#include "uipod_transition.h"
#include "uipod_ui.h"
#include "uipod_svg.h"
#include "uipod_rs_codecs.h"
#include "uipod_midi.h"

#define SOUNDFONT_CFG_PATH ROCKBOX_DIR "/soundfont.cfg"
#define SF2_SCAN_MAX_DEPTH 3

bool uipod_midi_is_midi_path(const char *path)
{
    if (!path || !path[0])
        return false;
    const char *dot = strrchr(path, '.');
    if (!dot || !dot[1])
        return false;
    return strcasecmp(dot, ".mid") == 0 ||
           strcasecmp(dot, ".midi") == 0 ||
           strcasecmp(dot, ".kar") == 0 ||
           strcasecmp(dot, ".rmi") == 0;
}

static bool is_sf2_filename(const char *name)
{
    if (!name || !name[0])
        return false;
    const char *dot = strrchr(name, '.');
    return dot && strcasecmp(dot, ".sf2") == 0;
}

static void save_soundfont_cfg(const char *sf2_path)
{
    if (!sf2_path || !sf2_path[0])
    {
        remove(SOUNDFONT_CFG_PATH);
        return;
    }
    int fd = open(SOUNDFONT_CFG_PATH, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd >= 0)
    {
        size_t len = strlen(sf2_path);
        write(fd, sf2_path, len);
        write(fd, "\n", 1);
        close(fd);
    }
}

void uipod_midi_init(void)
{
    int fd = open(SOUNDFONT_CFG_PATH, O_RDONLY);
    if (fd < 0)
    {
        uipod_rs_midi_set_soundfont("");
        return;
    }

    char buf[MAX_PATH];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0)
    {
        uipod_rs_midi_set_soundfont("");
        return;
    }
    buf[n] = '\0';
    for (ssize_t i = 0; i < n; i++)
    {
        if (buf[i] == '\r' || buf[i] == '\n')
        {
            buf[i] = '\0';
            break;
        }
    }

    if (!buf[0] || !file_exists(buf) || !uipod_rs_midi_set_soundfont(buf))
        uipod_rs_midi_set_soundfont("");
}

bool uipod_midi_apply_soundfont(const char *sf2_path)
{
    const char *target = sf2_path ? sf2_path : "";
    if (!uipod_rs_midi_set_soundfont(target))
    {
        splash(HZ, ID2P(LANG_FAILED));
        return false;
    }
    save_soundfont_cfg(target);
    return true;
}

/* ---- Flat SoundFont Selector Page --------------------------------------- */

struct sf2_item
{
    char *label;
    char *full_path;
};

struct sf2_list_page
{
    struct uipod_page   base;
    struct gui_synclist lists;
    char                active_sf2[MAX_PATH];
    bool                has_custom_sf2;
    struct sf2_item    *items;
    int                 count;
    int                 cap;
};

static void sf2_items_free(struct sf2_list_page *p)
{
    for (int i = 0; i < p->count; i++)
    {
        free(p->items[i].label);
        free(p->items[i].full_path);
    }
    free(p->items);
    p->items = NULL;
    p->count = 0;
    p->cap   = 0;
}

static int cmp_sf2_items(const void *a, const void *b)
{
    const struct sf2_item *ia = a;
    const struct sf2_item *ib = b;
    int r = strnatcasecmp(ia->label, ib->label);
    return r != 0 ? r : strcmp(ia->full_path, ib->full_path);
}

static bool sf2_has_path(const struct sf2_list_page *p, const char *full_path)
{
    for (int i = 0; i < p->count; i++)
    {
        if (strcmp(p->items[i].full_path, full_path) == 0)
            return true;
    }
    return false;
}

static void sf2_append_item(struct sf2_list_page *p,
                            const char *filename,
                            const char *full_path)
{
    if (sf2_has_path(p, full_path))
        return;

    if (p->count == p->cap)
    {
        int ncap = p->cap ? p->cap * 2 : 32;
        struct sf2_item *ni = realloc(p->items, (size_t)ncap * sizeof(*ni));
        if (!ni)
            return;
        p->items = ni;
        p->cap   = ncap;
    }

    char disp[MAX_PATH];
    strlcpy(disp, filename, sizeof(disp));
    size_t dlen = strlen(disp);
    if (dlen > 4 && strcasecmp(disp + dlen - 4, ".sf2") == 0)
        disp[dlen - 4] = '\0';

    char *lbl  = strdup(disp);
    char *path = strdup(full_path);
    if (!lbl || !path)
    {
        free(lbl);
        free(path);
        return;
    }

    p->items[p->count].label     = lbl;
    p->items[p->count].full_path = path;
    p->count++;
}

static void sf2_scan_folder_recursive(struct sf2_list_page *p,
                                      const char *dir_path,
                                      int depth)
{
    if (!dir_path || !dir_path[0] || depth > SF2_SCAN_MAX_DEPTH)
        return;

    DIR *d = opendir(dir_path);
    if (!d)
        return;

    struct dirent *e;
    while ((e = readdir(d)) != NULL)
    {
        if (e->d_name[0] == '.')
            continue;

        char full[MAX_PATH];
        path_append(full, dir_path, e->d_name, sizeof(full));

        struct dirinfo info = dir_get_info(d, e);
        if (info.attribute & ATTR_DIRECTORY)
        {
            if (depth < SF2_SCAN_MAX_DEPTH)
                sf2_scan_folder_recursive(p, full, depth + 1);
        }
        else if (is_sf2_filename(e->d_name))
        {
            sf2_append_item(p, e->d_name, full);
        }
    }
    closedir(d);
}

static void sf2_sync_active(struct sf2_list_page *p)
{
    p->has_custom_sf2 =
        uipod_rs_midi_get_soundfont(p->active_sf2, sizeof(p->active_sf2)) != 0;
}

static void sf2_collect_configured_soundfonts(struct sf2_list_page *p)
{
    sf2_items_free(p);
    sf2_sync_active(p);

    int n_roots = uipod_soundfont_folder_count();
    for (int i = 0; i < n_roots; i++)
    {
        const char *root = uipod_soundfont_folder_path(i);
        if (root && root[0] && dir_exists(root))
            sf2_scan_folder_recursive(p, root, 0);
    }

    if (p->count > 1)
        qsort(p->items, (size_t)p->count, sizeof(p->items[0]), cmp_sf2_items);
}

static const char *sf2_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    struct sf2_list_page *p = data;
    (void)buf;
    (void)buf_len;
    if (!p || sel < 0)
        return "";
    if (sel == 0)
        return str(LANG_UIPOD_SOUNDFONT_DEFAULT);
    int idx = sel - 1;
    if (idx < 0 || idx >= p->count)
        return "";
    return p->items[idx].label;
}

static const char *sf2_get_indicator(int sel, void *data)
{
    struct sf2_list_page *p = data;
    if (!p || sel < 0)
        return NULL;
    if (sel == 0)
        return !p->has_custom_sf2 ? uipod_toggle_str(true) : NULL;

    int idx = sel - 1;
    if (idx < 0 || idx >= p->count || !p->has_custom_sf2)
        return NULL;

    if (strcmp(p->items[idx].full_path, p->active_sf2) == 0)
        return uipod_toggle_str(true);
    return NULL;
}

static const struct uipod_svg_icon *sf2_get_svg_icon(int sel, void *data)
{
    struct sf2_list_page *p = data;
    if (!p || sel < 0 || sel > p->count)
        return NULL;
    return uipod_svg_tag("sf2");
}

static void sf2_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct sf2_list_page *)self)->lists);
}

static int sf2_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct sf2_list_page *)self)->lists, &action);
    return action;
}

static enum uipod_page_result sf2_on_action(struct uipod_page *self, int action)
{
    struct sf2_list_page *p = (struct sf2_list_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);

    if (action == ACTION_STD_CANCEL)
    {
        uipod_transition_arm_back();
        return UIPOD_PAGE_DONE;
    }

    if (action == ACTION_STD_OK)
    {
        if (sel == 0)
        {
            if (uipod_midi_apply_soundfont(""))
                sf2_sync_active(p);
        }
        else
        {
            int idx = sel - 1;
            if (idx >= 0 && idx < p->count)
            {
                if (uipod_midi_apply_soundfont(p->items[idx].full_path))
                    sf2_sync_active(p);
            }
        }
        return UIPOD_PAGE_STAY;
    }

    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable sf2_list_vtable =
{
    .legend    = NULL,
    .draw      = sf2_draw,
    .poll      = sf2_poll,
    .on_action = sf2_on_action,
};

static const int sf2_list_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL, -1
};

void uipod_midi_pick_soundfont(void)
{
    struct sf2_list_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt      = &sf2_list_vtable;
    p.base.context = CONTEXT_LIST;
    p.base.allowed = sf2_list_allowed;
    p.base.title   = str(LANG_UIPOD_SOUNDFONT);

    sf2_collect_configured_soundfonts(&p);

    int total_rows = 1 + p.count;
    int start_sel  = 0;
    if (p.has_custom_sf2)
    {
        for (int i = 0; i < p.count; i++)
        {
            if (strcmp(p.items[i].full_path, p.active_sf2) == 0)
            {
                start_sel = i + 1;
                break;
            }
        }
    }

    gui_synclist_init(&p.lists, sf2_get_name, &p, false, 1, NULL);
    gui_synclist_set_indicator_callback(&p.lists, sf2_get_indicator);
    gui_synclist_set_svg_icon_callback(&p.lists, sf2_get_svg_icon);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_UIPOD_SOUNDFONT), Icon_Audio);
    gui_synclist_set_nb_items(&p.lists, total_rows);
    gui_synclist_select_item(&p.lists, start_sel);

    uipod_page_run(&p.base);
    sf2_items_free(&p);
}
