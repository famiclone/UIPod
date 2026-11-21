/***************************************************************************
 * UIPod: reusable UI helpers.
 *
 * Header button-legend standard: a page sets the legend it wants shown in the
 * status-row header via uipod_set_header_legend(); the SBS skin reads it with
 * the %Tl token and overlays it.  Pages draw ONLY inside the content viewport
 * and never touch the skin-owned header.
 *
 * uipod_confirm() is a centered yes/no page (no box/border) drawn inside the
 * content viewport.  Its B/A legend lives in the header (not the body); an
 * optional `detail` line (e.g. the full path being acted on) sits above the
 * question.
 ****************************************************************************/
#include "config.h"
#include <ctype.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#include "string-extra.h"       /* strlcpy (context-menu title copy) */
#include "file.h"               /* MAX_PATH */
#include <stdio.h>
#include "kernel.h"
#include "system.h"
#include "action.h"
#include "lang.h"                /* str(): the page titles are localised */
#include "screens.h"
#include "lcd.h"
#include "font.h"
#include "rbpaths.h"             /* FONT_DIR */
#include "settings.h"            /* global_settings: theme fg/bg colours */
#include "gui/viewport.h"
#include "gui/list.h"
#include "icon.h"
#include "statusbar-skinned.h"
#include "misc.h"
#include "version.h"
#include "uipod_svg.h"
#include "uipod_plugins.h"
#include "uipod_rs_ui.h"
#include "uipod_ui.h"
#include "uipod_page.h"

/* ---- header legend standard ------------------------------------------- */

static const char *s_header_legend;   /* NULL = no legend */

void uipod_set_header_legend(const char *legend)
{
    s_header_legend = legend;
}

const char *uipod_get_header_legend(void)
{
    return s_header_legend;
}

/* The one place the on/off toggle glyph convention lives.  Returned as a list
 * indicator string (drawn right-aligned by the shared list renderer); used by
 * every toggle list -- Settings -> Menu Settings, Settings -> Visualizers, ... */
const char *uipod_toggle_str(bool on)
{
    return on ? "[x]" : "[ ]";
}

/* Re-render the status row so a just-changed legend appears/clears.  Needed
 * because a page runs its own draw loop and does not otherwise drive the skin
 * engine the way the file browser does.  Public: the run loop calls it. */
void uipod_header_refresh(void)
{
    FOR_NB_SCREENS(i)
        sb_skin_update((enum screen_type)i, true);
}

/* ---- confirm page ----------------------------------------------------- */

/* Draw `detail` (optional) above `question` and `footer` (optional) below it,
 * centered as a block in the content viewport.  The header is left to the skin. */
void uipod_centered_message(const char *question, const char *detail,
                              const char *footer)
{
    uipod_gpu_prepare_cpu_fallback();
    FOR_NB_SCREENS(i)
    {
        struct screen *s = &screens[i];
        struct viewport vp = {0};
        int dw = 0, dh = 0, qw = 0, qh = 0, fw = 0, fh = 0;

        viewport_set_defaults(&vp, s->screen_type);  /* content area, not header */
        s->set_viewport(&vp);
        s->clear_viewport();

        s->getstringsize((const unsigned char *)question, &qw, &qh);
        if (detail)
            s->getstringsize((const unsigned char *)detail, &dw, &dh);
        if (footer)
            s->getstringsize((const unsigned char *)footer, &fw, &fh);

        int gap = qh / 2 + 6;                     /* a little air between lines */
        int total = (detail ? dh + gap : 0) + qh + (footer ? gap + fh : 0);
        int y = (vp.height - total) / 2;
        if (y < 0)
            y = 0;

        if (detail)
        {
            s->putsxy((vp.width - dw) / 2, y, (const unsigned char *)detail);
            y += dh + gap;
        }
        s->putsxy((vp.width - qw) / 2, y, (const unsigned char *)question);
        y += qh;
        if (footer)
        {
            y += gap;
            s->putsxy((vp.width - fw) / 2, y, (const unsigned char *)footer);
        }

        s->update_viewport();
        s->set_viewport(NULL);
    }
}

void uipod_fullscreen_message(const char *msg, const char *widest)
{
    uipod_gpu_prepare_cpu_fallback();
    if (!widest)
        widest = msg;
    FOR_NB_SCREENS(i)
    {
        struct screen *s = &screens[i];
        struct viewport vp = {0};   /* buffer must be NULL: init_viewport derefs it */
        int w = 0, h = 0;

        viewport_set_fullscreen(&vp, s->screen_type);
        s->set_viewport(&vp);
        s->clear_viewport();
        s->getstringsize((const unsigned char *)widest, &w, &h);
        s->putsxy((vp.width - w) / 2, (vp.height - h) / 2,
                  (const unsigned char *)msg);
        s->update_viewport();
        s->set_viewport(NULL);
    }
}

/* The confirm page, derived from uipod_page.  The run loop owns the legend,
 * header refresh, key whitelist and input loop; this page only describes its
 * legend, how to draw, and how to react to A/B. */
struct confirm_page
{
    struct uipod_page base;
    const char *question;
    const char *detail;
    bool result;
};

static void confirm_page_draw(struct uipod_page *self)
{
    struct confirm_page *p = (struct confirm_page *)self;
    /* the choices live in the page body, not a header legend */
    uipod_centered_message(p->question, p->detail,
                             str(LANG_UIPOD_CONFIRM_LEGEND));
}

static enum uipod_page_result confirm_on_action(struct uipod_page *self,
                                                  int action)
{
    struct confirm_page *p = (struct confirm_page *)self;
    if (action == ACTION_STD_OK)     { p->result = true;  return UIPOD_PAGE_DONE; }
    if (action == ACTION_STD_CANCEL) { p->result = false; return UIPOD_PAGE_DONE; }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable confirm_vtable =
{
    .legend    = NULL,                 /* choices are drawn in the page body */
    .draw      = confirm_page_draw,
    .poll      = NULL,                 /* default: get_action(CONTEXT_STD) */
    .on_action = confirm_on_action,
};

/* only A (OK) and B (Cancel) act on this page */
static const int confirm_allowed[] = { ACTION_STD_OK, ACTION_STD_CANCEL, -1 };

bool uipod_confirm(const char *question, const char *detail)
{
    struct confirm_page p =
    {
        .base = { .vt = &confirm_vtable, .context = CONTEXT_STD,
                  .allowed = confirm_allowed },
        .question = question,
        .detail   = detail,
        .result   = false,
    };
    uipod_page_run(&p.base);
    return p.result;
}

/* ---- context submenu (Hold-A) ----------------------------------------- */

/* A small sliding list Page (not a popup): holding A on a context-aware entry
 * opens it, A picks a row (index returned to the caller), B backs out (-1). */
struct ctxmenu_page
{
    struct uipod_page base;
    struct gui_synclist lists;
    const char *const  *items;
    int                 result;        /* chosen row, or -1 if cancelled */
    char                title[MAX_PATH];  /* owned copy: callers pass volatile
                                           * pointers (e.g. a tree_context entry
                                           * name) that the run can invalidate */
};

static const char *ctxmenu_get_name(int sel, void *data, char *buf, size_t len)
{
    (void)buf; (void)len;
    return P2STR((const unsigned char *)((struct ctxmenu_page *)data)->items[sel]);
}

static void ctxmenu_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct ctxmenu_page *)self)->lists);
}

static int ctxmenu_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct ctxmenu_page *)self)->lists, &action);
    return action;
}

static enum uipod_page_result ctxmenu_on_action(struct uipod_page *self,
                                                  int action)
{
    struct ctxmenu_page *p = (struct ctxmenu_page *)self;
    if (action == ACTION_STD_OK)
    {
        p->result = gui_synclist_get_sel_pos(&p->lists);
        return UIPOD_PAGE_DONE;
    }
    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;           /* result stays -1 */
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable ctxmenu_vtable =
{
    .legend    = NULL,
    .draw      = ctxmenu_draw,
    .poll      = ctxmenu_poll,
    .on_action = ctxmenu_on_action,
};

static const int ctxmenu_allowed[] = { ACTION_STD_OK, ACTION_STD_CANCEL, -1 };

int uipod_context_menu(const char *title, const char *const *items, int count)
{
    struct ctxmenu_page p =
    {
        .base = { .vt = &ctxmenu_vtable, .context = CONTEXT_LIST,
                  .allowed = ctxmenu_allowed },
        .items  = items,
        .result = -1,
    };
    strlcpy(p.title, title ? title : "", sizeof p.title);
    gui_synclist_init(&p.lists, ctxmenu_get_name, &p, false, 1, NULL);
    gui_synclist_set_title(&p.lists, p.title, Icon_NOICON);
    gui_synclist_set_nb_items(&p.lists, count);
    gui_synclist_select_item(&p.lists, 0);
    uipod_page_run(&p.base);
    return p.result;
}


/* ---- About: opens `ROCKBOX_DIR "/about.txt"` in the native Text Viewer - */

void uipod_about(void)
{
    uipod_plugins_open_file(ROCKBOX_DIR "/about.txt");
}

/* ---- Message page: a static block of text rows.  Same chrome as every other
 * page (title header + slide); the body is a left-aligned list, the block
 * itself centred on the widest row.  B leaves.  The Controls reference card
 * and the empty-playlist hint are both this page with different rows. ------- */

struct message_page
{
    struct uipod_page base;
    const char *const *rows;
    int nrows;
};

static void message_draw(struct uipod_page *self)
{
    uipod_gpu_prepare_cpu_fallback();
    struct message_page *p = (struct message_page *)self;
    struct screen *s = &screens[SCREEN_MAIN];
    struct viewport vp = {0};

    viewport_set_defaults(&vp, s->screen_type);   /* content area, not header */
    s->set_viewport(&vp);
    s->clear_viewport();

    int w, fh = 0, block = 0;
    for (int i = 0; i < p->nrows; i++)
    {
        s->getstringsize(P2STR((const unsigned char *)p->rows[i]), &w, &fh);
        if (w > block) block = w;                 /* the widest row sets the left edge */
    }

    /* 26px leading, shrunk as needed so tall row lists still fit the page */
    int pad = 26;
    if (p->nrows * (fh + pad) > vp.height)
    {
        pad = vp.height / p->nrows - fh;
        if (pad < 6) pad = 6;
    }
    const int line = fh + pad;
    int x = (vp.width - block) / 2;
    int y = (vp.height - p->nrows * line) / 2;
    if (x < 0) x = 0;
    if (y < 0) y = 0;

    for (int i = 0; i < p->nrows; i++, y += line)
        s->putsxy(x, y, P2STR((const unsigned char *)p->rows[i]));

    s->update_viewport();
    s->set_viewport(NULL);
}

static enum uipod_page_result message_on_action(struct uipod_page *self,
                                                  int action)
{
    (void)self;
    return (action == ACTION_STD_CANCEL) ? UIPOD_PAGE_DONE : UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable message_vtable =
{
    .legend    = NULL,            /* just the title, no status-bar legend */
    .draw      = message_draw,
    .poll      = NULL,            /* default: get_action(CONTEXT_STD) */
    .on_action = message_on_action,
};

/* only B leaves */
static const int message_allowed[] = { ACTION_STD_CANCEL, -1 };

void uipod_message_page(const char *title, const char *const *rows, int nrows)
{
    struct message_page p =
    {
        .base = { .vt = &message_vtable, .context = CONTEXT_STD,
                  .allowed = message_allowed, .title = title },
        .rows = rows, .nrows = nrows,
    };
    uipod_page_run(&p.base);
}

/* ---- Lightweight GPU UI list icon tag descriptors ---------------------- */

#define ICON_TAG_SLOTS 64

const struct uipod_svg_icon *uipod_svg_tag(const char *tag)
{
    static struct uipod_svg_icon s_tags[ICON_TAG_SLOTS];
    static int s_tag_count = 0;

    if (!tag || !*tag)
        return NULL;

    for (int i = 0; i < s_tag_count; i++)
    {
        if (strcmp(s_tags[i].name, tag) == 0)
            return &s_tags[i];
    }
    if (s_tag_count >= ICON_TAG_SLOTS)
        return NULL;

    struct uipod_svg_icon *slot = &s_tags[s_tag_count++];
    strlcpy(slot->name, tag, sizeof(slot->name));
    return slot;
}

const struct uipod_svg_icon *uipod_svg_for_path(const char *path, bool is_dir)
{
    if (is_dir)
        return uipod_svg_tag("folder");
    if (!path || !*path)
        return NULL;

    const char *slash = strrchr(path, '/');
    const char *base  = slash ? slash + 1 : path;
    const char *dot   = strrchr(base, '.');
    if (!dot || dot == base || !dot[1])
        return NULL;

    char ext[24];
    size_t i = 0;
    for (const char *s = dot + 1; *s && *s != '#' && i < sizeof(ext) - 1; s++, i++)
        ext[i] = (char)tolower((unsigned char)*s);
    ext[i] = '\0';

    return uipod_svg_tag(ext);
}
