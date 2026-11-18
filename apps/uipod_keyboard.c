/***************************************************************************
 * UIPod: On-screen QWERTY keyboard rendered in HiDPI Geologica via `uipod-ui`
 * (with software fallback when the GLES context is owned by the visualizer).
 *
 * Button map: D-pad moves the highlight, A engages the highlighted key,
 * Y backspaces, B cancels and restores the original string.
 * Returns 0 when enter is engaged, -1 on cancel.
 ****************************************************************************/
#include "config.h"
#include <stdbool.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include "string-extra.h"
#include "action.h"
#include "screens.h"
#include "lcd.h"
#include "font.h"
#include "gui/viewport.h"
#include "rbpaths.h"
#include "settings.h"
#include "misc.h"
#include "uipod_page.h"
#include "uipod_rs_ui.h"
#include "uipod_keyboard.h"

#define KBD_FONT_PATH   FONT_DIR "/67-Geologica.fnt"

/* QWERTY character rows (lowercase base; capslock upcases letters at draw/insert). */
static const char *const kbd_alpha_rows_plain[4] =
{
    "1234567890",
    "qwertyuiop",
    "asdfghjkl",
    "zxcvbnm-",
};

static const char *const kbd_alpha_rows_url[4] =
{
    "1234567890",
    "qwertyuiop",
    "asdfghjkl-",
    "zxcvbnm.:/",
};

static const char *const kbd_sym_rows[4] =
{
    "1234567890",
    ":/.-_@?=&+",
    "#!$%*~,;|\\",
    "()[]{}<>'\"",
};

#define KBD_NROWS       5            /* 4 char rows + 1 special row */
#define KBD_SPECIAL_ROW 4
#define KBD_MAX_KEYS    50

struct kbd_page
{
    struct uipod_page base;
    struct screen *s;
    char  *buffer;
    int    buflen;
    int    row, col;
    bool   caps;
    bool   sym_mode;
    bool   allow_symbols;
    int    fid;
    int    result;       /* 0 = accepted (enter), -1 = cancelled */
};

static int special_count(const struct kbd_page *p)
{
    return p->allow_symbols ? 5 : 4;
}

static const char *active_row_str(const struct kbd_page *p, int row)
{
    if (p->allow_symbols && p->sym_mode)
        return kbd_sym_rows[row];
    return p->allow_symbols ? kbd_alpha_rows_url[row] : kbd_alpha_rows_plain[row];
}

static int row_count(const struct kbd_page *p, int row)
{
    return (row == KBD_SPECIAL_ROW) ? special_count(p) : (int)strlen(active_row_str(p, row));
}

static char upcase(char c)
{
    return (c >= 'a' && c <= 'z') ? (char)(c - 'a' + 'A') : c;
}

static const char *key_label(const struct kbd_page *p, int row, int col, char *buf)
{
    if (row == KBD_SPECIAL_ROW)
    {
        if (p->allow_symbols)
        {
            switch (col)
            {
                case 0:  return p->caps ? "CAPS" : "caps";
                case 1:  return p->sym_mode ? "abc" : "sym";
                case 2:  return "space";
                case 3:  return "del";
                default: return "enter";
            }
        }
        switch (col)
        {
            case 0:  return p->caps ? "CAPS" : "caps";
            case 1:  return "space";
            case 2:  return "del";
            default: return "enter";
        }
    }
    char c = active_row_str(p, row)[col];
    buf[0] = (!p->sym_mode && p->caps) ? upcase(c) : c;
    buf[1] = '\0';
    return buf;
}

static int key_center_x_10x(const struct kbd_page *p, int row, int col)
{
    /* Scaled by 10 for integer precision matching `uipod-ui`'s 1024px layout */
    const int W10      = 1024 * 10;
    const int inner10  = 960 * 10;
    const int gap10    = 12 * 10;
    const int n        = row_count(p, row);
    if (n <= 0)
        return W10 / 2;

    const int kw10 = (row == KBD_SPECIAL_ROW)
                         ? (inner10 - (n - 1) * gap10) / n
                         : (inner10 - 9 * gap10) / 10;
    const int row_w10   = n * kw10 + (n - 1) * gap10;
    const int start_x10 = (W10 - row_w10) / 2;
    return start_x10 + col * (kw10 + gap10) + kw10 / 2;
}

/* Maps `(from_row, col)` to the physically nearest key center in `to_row`. */
static int remap_col_by_center(const struct kbd_page *p, int from_row, int col, int to_row)
{
    const int to_n = row_count(p, to_row);
    if (to_n <= 1)
        return 0;

    const int target_cx = key_center_x_10x(p, from_row, col);
    int best_c    = 0;
    int best_dist = 0x7FFFFFFF;
    for (int c = 0; c < to_n; c++)
    {
        int cx = key_center_x_10x(p, to_row, c);
        int d  = cx - target_cx;
        if (d < 0)
            d = -d;
        if (d < best_dist)
        {
            best_dist = d;
            best_c    = c;
        }
    }
    return best_c;
}

static const char *visible_tail(struct screen *s, const char *text, int maxw)
{
    const char *p = text;
    int w, h;
    s->getstringsize((const unsigned char *)p, &w, &h);
    while (*p && w > maxw)
    {
        p++;
        s->getstringsize((const unsigned char *)p, &w, &h);
    }
    return p;
}

static void draw_sw_fallback(struct kbd_page *p, struct viewport *vp)
{
    if (p->fid < 0)
    {
        p->fid = font_load(KBD_FONT_PATH);
        if (p->fid < 0)
            p->fid = FONT_UI;
    }

    struct screen *s = p->s;
    s->setfont(p->fid);

    const int W = vp->width;
    const int margin = 32, gap = 12;
    const unsigned fg  = (unsigned)global_settings.fg_color;
    const unsigned fg1 = (unsigned)global_settings.fg1_color;
    const unsigned fg2 = (unsigned)global_settings.fg2_color;
    const unsigned bg  = (unsigned)global_settings.bg_color;

    s->set_viewport(vp);
    s->set_background(bg);
    s->clear_viewport();

    /* ---- input field ------------------------------------------------- */
    const int fx = margin, fy = 32, fw = W - 2 * margin, fh = 96;
    s->set_foreground(fg1);
    for (int b = 0; b < 4; b++)
        s->drawrect(fx + b, fy + b, fw - 2 * b, fh - 2 * b);

    int tw, th;
    const char *vis = visible_tail(s, p->buffer, fw - 44);
    s->getstringsize((const unsigned char *)vis, &tw, &th);
    s->set_foreground(fg);
    lcd_set_drawmode(DRMODE_FG);
    s->putsxy(fx + 20, fy + (fh - th) / 2, (const unsigned char *)vis);

    lcd_set_drawmode(DRMODE_SOLID);
    s->set_foreground(fg2);
    s->fillrect(fx + 20 + tw + 4, fy + 20, 6, fh - 40);

    /* ---- key grid ---------------------------------------------------- */
    const int n_sp = special_count(p);
    const int grid_top = 156;
    const int key_h = 106, row_step = key_h + gap;
    const int key_w = (W - 2 * margin - 9 * gap) / 10;
    const int sp_w  = (W - 2 * margin - (n_sp - 1) * gap) / n_sp;

    for (int r = 0; r < KBD_NROWS; r++)
    {
        const int n = row_count(p, r);
        const int kw = (r == KBD_SPECIAL_ROW) ? sp_w : key_w;
        const int row_w = n * kw + (n - 1) * gap;
        const int start_x = (W - row_w) / 2;
        const int y = grid_top + r * row_step;

        for (int c = 0; c < n; c++)
        {
            const int x = start_x + c * (kw + gap);
            const bool selected = (r == p->row && c == p->col);
            char cbuf[2];
            const char *label = key_label(p, r, c, cbuf);

            lcd_set_drawmode(DRMODE_SOLID);
            if (selected)
            {
                s->set_foreground(fg);
                s->fillrect(x, y, kw, key_h);
            }
            else
            {
                s->set_foreground(fg1);
                for (int b = 0; b < 4; b++)
                    s->drawrect(x + b, y + b, kw - 2 * b, key_h - 2 * b);
            }

            int lw, lh;
            s->getstringsize((const unsigned char *)label, &lw, &lh);
            s->set_foreground(selected ? bg : ((r == KBD_SPECIAL_ROW) ? fg2 : fg));
            lcd_set_drawmode(DRMODE_FG);
            s->putsxy(x + (kw - lw) / 2, y + (key_h - lh) / 2,
                      (const unsigned char *)label);
        }
    }

    lcd_set_drawmode(DRMODE_SOLID);
    s->update_viewport();
}

static bool draw_gpu(const struct kbd_page *p)
{
    struct UIPodRsKeyboardKey keys[KBD_MAX_KEYS];
    char char_labels[KBD_MAX_KEYS][4];
    size_t row_counts[KBD_NROWS];
    size_t total = 0;

    for (int r = 0; r < KBD_NROWS; r++)
    {
        int n = row_count(p, r);
        row_counts[r] = (size_t)n;
        for (int c = 0; c < n && total < KBD_MAX_KEYS; c++)
        {
            const char *lbl = key_label(p, r, c, char_labels[total]);
            bool active_toggle = false;
            if (r == KBD_SPECIAL_ROW)
            {
                if (c == 0 && p->caps)
                    active_toggle = true;
                else if (p->allow_symbols && c == 1 && p->sym_mode)
                    active_toggle = true;
            }
            keys[total].label            = lbl;
            keys[total].selected         = (r == p->row && c == p->col);
            keys[total].is_modifier      = (r == KBD_SPECIAL_ROW);
            keys[total].is_active_toggle = active_toggle;
            total++;
        }
    }

    return uipod_gpu_render_keyboard(p->buffer, row_counts, keys);
}

static void append_char(char *buffer, int buflen, char c)
{
    int len = (int)strlen(buffer);
    if (len < buflen - 1)
    {
        buffer[len] = c;
        buffer[len + 1] = '\0';
    }
}

static void backspace(char *buffer)
{
    int len = (int)strlen(buffer);
    while (len > 0 && ((unsigned char)buffer[len - 1] & 0xC0) == 0x80)
        len--;
    if (len > 0)
        buffer[len - 1] = '\0';
}

/* ---- the keyboard as a uipod_page ----------------------------------- */

static void kbd_page_draw(struct uipod_page *self)
{
    struct kbd_page *p = (struct kbd_page *)self;
    if (draw_gpu(p))
        return;

    struct viewport vp = {0};
    viewport_set_fullscreen(&vp, SCREEN_MAIN);
    draw_sw_fallback(p, &vp);
    p->s->set_viewport(NULL);
}

static int kbd_page_poll(struct uipod_page *self, int timeout)
{
    (void)self;
    return get_action(CONTEXT_KEYBOARD, timeout);
}

static enum uipod_page_result kbd_page_on_action(struct uipod_page *self,
                                                 int action)
{
    struct kbd_page *p = (struct kbd_page *)self;

    switch (action)
    {
        case ACTION_KBD_LEFT:
            p->col = (p->col - 1 + row_count(p, p->row)) % row_count(p, p->row);
            uipod_menu_click();
            break;
        case ACTION_KBD_RIGHT:
            p->col = (p->col + 1) % row_count(p, p->row);
            uipod_menu_click();
            break;
        case ACTION_KBD_UP:
        {
            int from_row = p->row;
            int to_row   = (p->row - 1 + KBD_NROWS) % KBD_NROWS;
            p->col = remap_col_by_center(p, from_row, p->col, to_row);
            p->row = to_row;
            uipod_menu_click();
            break;
        }
        case ACTION_KBD_DOWN:
        {
            int from_row = p->row;
            int to_row   = (p->row + 1) % KBD_NROWS;
            p->col = remap_col_by_center(p, from_row, p->col, to_row);
            p->row = to_row;
            uipod_menu_click();
            break;
        }

        case ACTION_KBD_SELECT:            /* A: engage the highlighted key */
            if (p->row == KBD_SPECIAL_ROW)
            {
                if (p->allow_symbols)
                {
                    switch (p->col)
                    {
                        case 0: p->caps = !p->caps; break;
                        case 1: p->sym_mode = !p->sym_mode; break;
                        case 2: append_char(p->buffer, p->buflen, ' '); break;
                        case 3: backspace(p->buffer); break;
                        case 4: p->result = 0; return UIPOD_PAGE_DONE;
                    }
                }
                else
                {
                    switch (p->col)
                    {
                        case 0: p->caps = !p->caps; break;
                        case 1: append_char(p->buffer, p->buflen, ' '); break;
                        case 2: backspace(p->buffer); break;
                        case 3: p->result = 0; return UIPOD_PAGE_DONE;
                    }
                }
            }
            else
            {
                char c = active_row_str(p, p->row)[p->col];
                append_char(p->buffer, p->buflen,
                            (!p->sym_mode && p->caps) ? upcase(c) : c);
            }
            break;

        case ACTION_KBD_BACKSPACE:         /* Y: backspace */
            backspace(p->buffer);
            break;
        case ACTION_KBD_ABORT:             /* B: always cancel / back */
            p->result = -1;
            return UIPOD_PAGE_DONE;
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable kbd_vtable =
{
    .legend    = NULL,
    .draw      = kbd_page_draw,
    .poll      = kbd_page_poll,
    .on_action = kbd_page_on_action,
};

static const int kbd_allowed[] =
{
    ACTION_KBD_LEFT, ACTION_KBD_RIGHT, ACTION_KBD_UP, ACTION_KBD_DOWN,
    ACTION_KBD_SELECT, ACTION_KBD_BACKSPACE, ACTION_KBD_ABORT, -1
};

int uipod_kbd_input_ex(char *buffer, int buflen, bool allow_symbols)
{
    if (!buffer || buflen <= 0)
        return -1;

    char *backup = malloc((size_t)buflen);
    if (backup)
        strlcpy(backup, buffer, (size_t)buflen);

    struct screen *s = &screens[SCREEN_MAIN];

    struct kbd_page p =
    {
        .base = { .vt = &kbd_vtable, .context = CONTEXT_KEYBOARD,
                  .allowed = kbd_allowed,
                  .no_theme = true,
                  .no_header_refresh = true,
                  .animated = true },
        .s = s, .buffer = buffer, .buflen = buflen,
        .row = 1, .col = 0, .caps = false, .sym_mode = false,
        .allow_symbols = allow_symbols, .fid = -1, .result = -1,
    };

    uipod_page_run(&p.base);

    if (p.result != 0 && backup)
        strlcpy(buffer, backup, (size_t)buflen);
    free(backup);

    s->setfont(FONT_UI);
    if (p.fid >= 0 && p.fid != FONT_UI)
        font_unload(p.fid);
    return p.result;
}

int uipod_kbd_input(char *buffer, int buflen)
{
    return uipod_kbd_input_ex(buffer, buflen, false);
}
