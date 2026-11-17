/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * UIPod: Display & Device settings menus — 4-shade iPod Mini theme presets,
 * live Customize Theme hex editor, Language, Monochrome Album Art, Main Menu,
 * Brightness, Display Poweroff, Idle Poweroff, CPU Frequency, Charge Limit.
 *
 * This program is free software; you can redistribute it and/or
 * modify it under the terms of the GNU General Public License
 * as published by the Free Software Foundation; either version 2
 * of the License, or (at your option) any later version.
 *
 * This software is distributed on an "AS IS" basis, WITHOUT WARRANTY OF ANY
 * KIND, either express or implied.
 *
 ****************************************************************************/

#include <ctype.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "string-extra.h"
#include "config.h"
#include "action.h"
#include "gui/list.h"
#include "lang.h"
#include "language.h"
#include "settings.h"
#include "splash.h"
#include "menu.h"
#include "screen_access.h"
#include "viewport.h"
#include "exported_menus.h"
#include "uipod_keyboard.h"
#include "uipod_mainmenu.h"
#include "uipod_page.h"
#include "uipod_rs_ui.h"
#include "uipod_transition.h"
#include "uipod_ui.h"

#define UIPOD_INK_DARK  0x181C18   /* classic mono-LCD ink */
#define UIPOD_INK_LIGHT 0xFFFFFF

/* Compile-time RGB channel lerp (`pct` in 0..100 from `c0` toward `c1`) */
#define TP_CH_R(c) (((c) >> 16) & 0xFF)
#define TP_CH_G(c) (((c) >> 8) & 0xFF)
#define TP_CH_B(c) ((c) & 0xFF)
#define TP_LERP_CH(a, b, pct) (((a) * (100 - (pct)) + (b) * (pct) + 50) / 100)
#define TP_LERP_RGB(c0, c1, pct) \
    ((TP_LERP_CH(TP_CH_R(c0), TP_CH_R(c1), (pct)) << 16) | \
     (TP_LERP_CH(TP_CH_G(c0), TP_CH_G(c1), (pct)) << 8)  | \
      TP_LERP_CH(TP_CH_B(c0), TP_CH_B(c1), (pct)))

/* Derives `fg1` (~30% from bg -> fg) and `fg2` (~78% from bg -> fg), matching `ui.figcraft` */
#define TP_THEME_AUTO(nm, bg_hex, fg_hex) \
    { (nm), (bg_hex), TP_LERP_RGB((bg_hex), (fg_hex), 30), TP_LERP_RGB((bg_hex), (fg_hex), 78), (fg_hex) }

struct uipod_theme
{
    const char *name;
    int         bg;
    int         fg1;
    int         fg2;
    int         fg;
};

static const struct uipod_theme uipod_themes[] = {
    /* UIPod Classic (exact `assets/icons/ui.figcraft` 4-shade iPod Mini tokens) */
    { "UIPod Classic",      0xD6D8D5, 0xA8A8A8, 0x595959, 0x3C3637 },
    { "UIPod Classic Dark", 0x1E1B1C, 0x555253, 0xAEAFAD, 0xD6D8D5 },
    /* Classic (0xD8DCD0, the mono LCD) */
    TP_THEME_AUTO("Classic Dark",     0xD8DCD0, UIPOD_INK_DARK),
    TP_THEME_AUTO("Classic Light",    0xD8DCD0, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Classic Sepia",    0xD8DCD0, 0x5D4037),
    TP_THEME_AUTO("Silver Dark",      0xC4C7CC, UIPOD_INK_DARK),
    TP_THEME_AUTO("Silver Light",     0xC4C7CC, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Silver Slate",     0xC4C7CC, 0x37474F),
    TP_THEME_AUTO("Slate Dark",       0x637D8C, UIPOD_INK_DARK),
    TP_THEME_AUTO("Slate Light",      0x637D8C, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Slate Gold",       0x637D8C, 0xFFD97D),
    TP_THEME_AUTO("Blue Dark",        0x0094E1, UIPOD_INK_DARK),
    TP_THEME_AUTO("Blue Light",       0x0094E1, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Blue Navy",        0x0094E1, 0x002B4A),
    TP_THEME_AUTO("Green Dark",       0xA0CB3B, UIPOD_INK_DARK),
    TP_THEME_AUTO("Green Light",      0xA0CB3B, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Green Forest",     0xA0CB3B, 0x2E4A1C),
    TP_THEME_AUTO("Yellow Dark",      0xFFD400, UIPOD_INK_DARK),
    TP_THEME_AUTO("Yellow Light",     0xFFD400, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Yellow Navy",      0xFFD400, 0x1D3557),
    TP_THEME_AUTO("Gold Dark",        0xFAB71F, UIPOD_INK_DARK),
    TP_THEME_AUTO("Gold Light",       0xFAB71F, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Gold Espresso",    0xFAB71F, 0x4E342E),
    TP_THEME_AUTO("Orange Dark",      0xF08A1C, UIPOD_INK_DARK),
    TP_THEME_AUTO("Orange Light",     0xF08A1C, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Orange Navy",      0xF08A1C, 0x1D3557),
    TP_THEME_AUTO("Red Dark",         0xEA323C, UIPOD_INK_DARK),
    TP_THEME_AUTO("Red Light",        0xEA323C, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Red Cream",        0xEA323C, 0xFFF3E0),
    TP_THEME_AUTO("Pink Dark",        0xEC5298, UIPOD_INK_DARK),
    TP_THEME_AUTO("Pink Light",       0xEC5298, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Pink Berry",       0xEC5298, 0x4A0728),
    TP_THEME_AUTO("Purple Dark",      0x9B72B0, UIPOD_INK_DARK),
    TP_THEME_AUTO("Purple Light",     0x9B72B0, UIPOD_INK_LIGHT),
    TP_THEME_AUTO("Purple Plum",      0x9B72B0, 0x2A1438),
    TP_THEME_AUTO("Mono Green Dark",  0xBFCBB0, UIPOD_INK_DARK),
    TP_THEME_AUTO("Mono Green Light", 0xBFCBB0, UIPOD_INK_LIGHT),
    { "Mono Green Pixel",   0xBFCBB0, 0x8BAC0F, 0x306230, 0x0F380F },
};
#define UIPOD_NTHEMES \
    ((int)(sizeof(uipod_themes)/sizeof(uipod_themes[0])))

static int tp_lerp_rgb_runtime(int c0, int c1, int pct)
{
    int r = TP_LERP_CH(TP_CH_R(c0), TP_CH_R(c1), pct);
    int g = TP_LERP_CH(TP_CH_G(c0), TP_CH_G(c1), pct);
    int b = TP_LERP_CH(TP_CH_B(c0), TP_CH_B(c1), pct);
    return (r << 16) | (g << 8) | b;
}

static void uipod_apply_4colors_live(int new_bg, int new_fg, int new_fg1, int new_fg2)
{
    unsigned old_bg = (unsigned)global_settings.bg_color;
    unsigned old_fg = (unsigned)global_settings.fg_color;

    global_settings.bg_color  = new_bg  & 0xFFFFFF;
    global_settings.fg_color  = new_fg  & 0xFFFFFF;
    global_settings.fg1_color = new_fg1 & 0xFFFFFF;
    global_settings.fg2_color = new_fg2 & 0xFFFFFF;

    screens[SCREEN_MAIN].set_background(global_settings.bg_color);
    screens[SCREEN_MAIN].set_foreground(global_settings.fg_color);
    skin_update_bg_color(old_bg, (unsigned)global_settings.bg_color);
    skin_update_fg_color(old_fg, (unsigned)global_settings.fg_color);
    viewportmanager_theme_changed(THEME_UI_VIEWPORT | THEME_STATUSBAR |
                                  THEME_LISTS);
    settings_save();
}

void uipod_theme_sync_startup(void)
{
    /* Migrate older 2-color `config.cfg` files seamlessly on first boot:
     * if `(bg_color, fg_color)` was changed from `UIPod Classic` while
     * `fg1_color`/`fg2_color` were absent (left at the `UIPod Classic` defaults),
     * populate the matching preset's auxiliary shades (or 30%/78% lerp). */
    if (global_settings.fg1_color == 0xA8A8A8 &&
        global_settings.fg2_color == 0x595959 &&
        (global_settings.bg_color != 0xD6D8D5 ||
         global_settings.fg_color != 0x3C3637))
    {
        for (int i = 0; i < UIPOD_NTHEMES; i++)
        {
            if (uipod_themes[i].bg == global_settings.bg_color &&
                uipod_themes[i].fg == global_settings.fg_color)
            {
                global_settings.fg1_color = uipod_themes[i].fg1;
                global_settings.fg2_color = uipod_themes[i].fg2;
                return;
            }
        }
        global_settings.fg1_color =
            tp_lerp_rgb_runtime(global_settings.bg_color, global_settings.fg_color, 30);
        global_settings.fg2_color =
            tp_lerp_rgb_runtime(global_settings.bg_color, global_settings.fg_color, 78);
    }
}

static void uipod_theme_apply(int idx)
{
    if (idx < 0 || idx >= UIPOD_NTHEMES)
        return;
    uipod_apply_4colors_live(uipod_themes[idx].bg,
                             uipod_themes[idx].fg,
                             uipod_themes[idx].fg1,
                             uipod_themes[idx].fg2);
}

/* ---- Customize Theme Screen (`Settings -> Display -> Customize Theme`) ---- */

enum
{
    CUST_ROW_BG  = 0,
    CUST_ROW_FG  = 1,
    CUST_ROW_FG1 = 2,
    CUST_ROW_FG2 = 3,
    CUST_ROW_COUNT = 4
};

struct customize_theme_page
{
    struct uipod_page   base;
    struct gui_synclist lists;
    char                indics[CUST_ROW_COUNT][16];
};

static int hex_nibble(char c)
{
    if (c >= '0' && c <= '9')
        return c - '0';
    if (c >= 'a' && c <= 'f')
        return 10 + (c - 'a');
    if (c >= 'A' && c <= 'F')
        return 10 + (c - 'A');
    return -1;
}

static bool parse_hex_color(const char *raw, int *out_rgb)
{
    if (!raw || !out_rgb)
        return false;

    while (*raw && isspace((unsigned char)*raw))
        raw++;
    if (*raw == '#')
        raw++;
    else if (raw[0] == '0' && (raw[1] == 'x' || raw[1] == 'X'))
        raw += 2;

    char hex[8];
    int len = 0;
    while (*raw && !isspace((unsigned char)*raw))
    {
        if (len >= 6 || hex_nibble(*raw) < 0)
            return false;
        hex[len++] = *raw++;
    }
    while (*raw)
    {
        if (!isspace((unsigned char)*raw))
            return false;
        raw++;
    }

    if (len == 3)
    {
        int r = hex_nibble(hex[0]);
        int g = hex_nibble(hex[1]);
        int b = hex_nibble(hex[2]);
        *out_rgb = ((r * 17) << 16) | ((g * 17) << 8) | (b * 17);
        return true;
    }
    if (len == 6)
    {
        int val = 0;
        for (int i = 0; i < 6; i++)
            val = (val << 4) | hex_nibble(hex[i]);
        *out_rgb = val;
        return true;
    }
    return false;
}

static int cust_get_color_by_row(int row)
{
    switch (row)
    {
        case CUST_ROW_BG:  return global_settings.bg_color  & 0xFFFFFF;
        case CUST_ROW_FG:  return global_settings.fg_color  & 0xFFFFFF;
        case CUST_ROW_FG1: return global_settings.fg1_color & 0xFFFFFF;
        case CUST_ROW_FG2: return global_settings.fg2_color & 0xFFFFFF;
        default:           return 0;
    }
}

static const char *cust_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    (void)data; (void)buf; (void)buf_len;
    switch (sel)
    {
        case CUST_ROW_BG:  return str(LANG_UIPOD_COLOR_BG);
        case CUST_ROW_FG:  return str(LANG_UIPOD_COLOR_FG);
        case CUST_ROW_FG1: return str(LANG_UIPOD_COLOR_FG1);
        case CUST_ROW_FG2: return str(LANG_UIPOD_COLOR_FG2);
        default:           return "";
    }
}

static const char *cust_get_indicator(int sel, void *data)
{
    struct customize_theme_page *p = (struct customize_theme_page *)data;
    if (sel < 0 || sel >= CUST_ROW_COUNT)
        return NULL;
    snprintf(p->indics[sel], sizeof(p->indics[sel]), "%s#%06X",
             UIPOD_INDIC_PLAIN, (unsigned)cust_get_color_by_row(sel));
    return p->indics[sel];
}

static void cust_draw(struct uipod_page *self)
{
    struct customize_theme_page *p = (struct customize_theme_page *)self;
    gui_synclist_draw(&p->lists);
}

static int cust_poll(struct uipod_page *self, int timeout)
{
    struct customize_theme_page *p = (struct customize_theme_page *)self;
    int action;
    list_do_action(self->context, timeout, &p->lists, &action);
    return action;
}

static enum uipod_page_result cust_on_action(struct uipod_page *self, int action)
{
    struct customize_theme_page *p = (struct customize_theme_page *)self;
    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;

    if (action == ACTION_STD_OK)
    {
        int sel = gui_synclist_get_sel_pos(&p->lists);
        if (sel < 0 || sel >= CUST_ROW_COUNT)
            return UIPOD_PAGE_STAY;

        char buf[16];
        snprintf(buf, sizeof(buf), "#%06X", (unsigned)cust_get_color_by_row(sel));
        if (uipod_kbd_input_ex(buf, (int)sizeof(buf), true) == 0)
        {
            int parsed = 0;
            if (!parse_hex_color(buf, &parsed))
            {
                splash(HZ, ID2P(LANG_UIPOD_INVALID_HEX));
                uipod_transition_suppress_next();
            }
            else
            {
                int bg  = global_settings.bg_color;
                int fg  = global_settings.fg_color;
                int fg1 = global_settings.fg1_color;
                int fg2 = global_settings.fg2_color;
                switch (sel)
                {
                    case CUST_ROW_BG:  bg  = parsed; break;
                    case CUST_ROW_FG:  fg  = parsed; break;
                    case CUST_ROW_FG1: fg1 = parsed; break;
                    case CUST_ROW_FG2: fg2 = parsed; break;
                }
                uipod_apply_4colors_live(bg, fg, fg1, fg2);
            }
        }
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable cust_vtable = {
    .legend    = NULL,
    .draw      = cust_draw,
    .poll      = cust_poll,
    .on_action = cust_on_action,
};

static const int cust_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL, -1
};

int uipod_customize_theme_page(void)
{
    struct customize_theme_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt      = &cust_vtable;
    p.base.context = CONTEXT_LIST;
    p.base.allowed = cust_allowed;
    p.base.title   = str(LANG_UIPOD_CUSTOMIZE_THEME);

    gui_synclist_init(&p.lists, cust_get_name, &p, false, 1, NULL);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_UIPOD_CUSTOMIZE_THEME),
                           Icon_Submenu_Entered);
    gui_synclist_set_indicator_callback(&p.lists, cust_get_indicator);
    gui_synclist_set_nb_items(&p.lists, CUST_ROW_COUNT);
    gui_synclist_select_item(&p.lists, 0);

    uipod_page_run(&p.base);
    return 0;
}

/* CPU Frequency: inline selector over the A133's cpufreq steps; applied and
 * persisted live (power-target.c owns cpu_freq.txt). */
extern int  retrohh_cpu_get_choice(void);       /* kHz; 0 = Dynamic */
extern void retrohh_cpu_set_choice(int khz);    /* persistence lives in power-target.c */

/* Charge Limit: caps charging at a % while UIPod runs (power-target.c). The
 * row is hidden entirely when the standalone Battery Care daemon owns charging. */
extern bool retrohh_charge_limit_hidden(void);
extern int  retrohh_charge_limit_get_target(void);
extern void retrohh_charge_limit_cycle(int dir);

static const int uipod_cpu_freqs[] = {
    408000, 600000, 816000, 1008000, 1200000, 1416000, 1608000, 1800000, 2000000
};
/* Display labels (MHz) paired 1:1 with uipod_cpu_freqs (kHz). */
static const char *const uipod_cpu_labels[] = {
    "408 MHz", "600 MHz", "816 MHz", "1008 MHz", "1200 MHz",
    "1416 MHz", "1608 MHz", "1800 MHz", "2000 MHz"
};
#define UIPOD_CPU_NFREQS \
    ((int)(sizeof(uipod_cpu_freqs)/sizeof(uipod_cpu_freqs[0])))

static int uipod_cpu_nearest_index(int khz)
{
    int idx = UIPOD_CPU_NFREQS - 1;
    for (int i = 0; i < UIPOD_CPU_NFREQS; i++)
        if (uipod_cpu_freqs[i] >= khz) { idx = i; break; }
    return idx;
}

/* ---- do_menu inline value-row callbacks (struct menu_value_cb) ----------- */
static const char *tp_cpu_value_get(void *ctx, char *buf, int len)
{
    (void)ctx; (void)buf; (void)len;
    int khz = retrohh_cpu_get_choice();
    if (khz <= 0)
        return "Dynamic";
    return uipod_cpu_labels[uipod_cpu_nearest_index(khz)];
}
static void tp_cpu_value_cycle(void *ctx, int dir)
{
    (void)ctx;
    int khz = retrohh_cpu_get_choice();
    int vidx = khz <= 0 ? 0 : uipod_cpu_nearest_index(khz) + 1;
    vidx += (dir < 0 ? -1 : 1);
    if (vidx < 0) vidx = 0;
    if (vidx > UIPOD_CPU_NFREQS) vidx = UIPOD_CPU_NFREQS;
    retrohh_cpu_set_choice(vidx == 0 ? 0 : uipod_cpu_freqs[vidx - 1]);
}
static const struct menu_value_cb uipod_cpu_value =
    { tp_cpu_value_get, tp_cpu_value_cycle, NULL };

/* Current theme = all 4 colors matched against the preset table;
 * -1 ("Custom") when any of the 4 colors was customized. */
static int tp_theme_cur_index(void)
{
    for (int i = 0; i < UIPOD_NTHEMES; i++)
    {
        if (uipod_themes[i].bg  == global_settings.bg_color  &&
            uipod_themes[i].fg  == global_settings.fg_color  &&
            uipod_themes[i].fg1 == global_settings.fg1_color &&
            uipod_themes[i].fg2 == global_settings.fg2_color)
        {
            return i;
        }
    }
    return -1;
}
static const char *tp_theme_value_get(void *ctx, char *buf, int len)
{
    (void)ctx; (void)buf; (void)len;
    int idx = tp_theme_cur_index();
    return idx >= 0 ? uipod_themes[idx].name : "Custom";
}
static void tp_theme_value_cycle(void *ctx, int dir)
{
    (void)ctx;
    int cur = tp_theme_cur_index();
    int idx = (cur < 0) ? 0 : cur + (dir < 0 ? -1 : 1);
    if (idx < 0) idx = 0;
    if (idx >= UIPOD_NTHEMES) idx = UIPOD_NTHEMES - 1;
    if (idx != cur)
        uipod_theme_apply(idx);
}
static const struct menu_value_cb uipod_theme_value =
    { tp_theme_value_get, tp_theme_value_cycle, NULL };

static const char *tp_charge_value_get(void *ctx, char *buf, int len)
{
    (void)ctx;
    snprintf(buf, len, "%d%%", retrohh_charge_limit_get_target());
    return buf;
}
static void tp_charge_value_cycle(void *ctx, int dir)
{
    (void)ctx;
    retrohh_charge_limit_cycle(dir);
}
static const struct menu_value_cb uipod_charge_value =
    { tp_charge_value_get, tp_charge_value_cycle, NULL };

static const char *tp_mono_art_value_get(void *ctx, char *buf, int len)
{
    (void)ctx; (void)buf; (void)len;
    return uipod_toggle_str(global_settings.mono_album_art);
}
static void tp_mono_art_value_cycle(void *ctx, int dir)
{
    (void)ctx; (void)dir;
    global_settings.mono_album_art = !global_settings.mono_album_art;
    settings_save();
}
static const struct menu_value_cb uipod_mono_art_value =
    { tp_mono_art_value_get, tp_mono_art_value_cycle, NULL };

static bool tp_lang_is_ukrainian(void)
{
    return strcmp((const char *)global_settings.lang_file, "ukrainian") == 0;
}
static const char *tp_lang_value_get(void *ctx, char *buf, int len)
{
    (void)ctx; (void)buf; (void)len;
    return tp_lang_is_ukrainian() ? "Українська" : "English";
}
static void tp_lang_value_cycle(void *ctx, int dir)
{
    (void)ctx; (void)dir;
    if (tp_lang_is_ukrainian())
    {
        global_settings.lang_file[0] = '\0';
        lang_init(core_language_builtin, language_strings,
                  LANG_LAST_INDEX_IN_ARRAY);
    }
    else
    {
        strlcpy((char *)global_settings.lang_file, "ukrainian",
                sizeof(global_settings.lang_file));
        if (lang_core_load(LANG_DIR "/ukrainian.lng") != 0)
        {
            global_settings.lang_file[0] = '\0';
            lang_init(core_language_builtin, language_strings,
                      LANG_LAST_INDEX_IN_ARRAY);
        }
    }
    viewportmanager_theme_changed(THEME_STATUSBAR | THEME_LISTS);
    settings_save();
}
static const struct menu_value_cb uipod_lang_value =
    { tp_lang_value_get, tp_lang_value_cycle, NULL };

static const char *tp_transition_value_get(void *ctx, char *buf, int len)
{
    (void)ctx; (void)buf; (void)len;
    switch (global_settings.transition_style)
    {
        case UIPOD_TRANS_STYLE_COVER:  return str(LANG_UIPOD_TRANS_COVER);
        case UIPOD_TRANS_STYLE_FADE:   return str(LANG_UIPOD_TRANS_FADE);
        case UIPOD_TRANS_STYLE_PIXELS: return str(LANG_UIPOD_TRANS_PIXELS);
        case UIPOD_TRANS_STYLE_BLINDS: return str(LANG_UIPOD_TRANS_BLINDS);
        case UIPOD_TRANS_STYLE_OFF:    return str(LANG_OFF);
        case UIPOD_TRANS_STYLE_SLIDE:
        default:                       return str(LANG_UIPOD_TRANS_SLIDE);
    }
}
static void tp_transition_value_cycle(void *ctx, int dir)
{
    (void)ctx;
    int cur = global_settings.transition_style;
    if (cur < 0 || cur >= UIPOD_TRANS_STYLE_COUNT)
        cur = 0;
    cur = (cur + (dir < 0 ? -1 : 1) + UIPOD_TRANS_STYLE_COUNT) % UIPOD_TRANS_STYLE_COUNT;
    global_settings.transition_style = cur;
    settings_save();
}
static const struct menu_value_cb uipod_transition_value =
    { tp_transition_value_get, tp_transition_value_cycle, NULL };

/* Settings -> Display (#1172): Theme, Customize Theme, Screen Transition, Language, Monochrome Album Art, Main Menu. */
MENUITEM_VALUE(tp_pw_theme, ID2P(LANG_UIPOD_THEME), &uipod_theme_value, Icon_NOICON);
MENUITEM_FUNCTION(tp_pw_customize_theme, MENU_SHOW_CHEVRON,
                  ID2P(LANG_UIPOD_CUSTOMIZE_THEME), uipod_customize_theme_page,
                  NULL, Icon_NOICON);
MENUITEM_VALUE(tp_pw_transition, ID2P(LANG_UIPOD_TRANSITION), &uipod_transition_value, Icon_NOICON);
MENUITEM_VALUE(tp_app_lang, ID2P(LANG_UIPOD_LANGUAGE), &uipod_lang_value, Icon_NOICON);
MENUITEM_VALUE(tp_pw_mono_art, ID2P(LANG_UIPOD_MONO_ALBUM_ART), &uipod_mono_art_value, Icon_NOICON);
MENUITEM_FUNCTION(tp_app_mainmenu, MENU_SHOW_CHEVRON, ID2P(LANG_UIPOD_MAINMENU),
                  uipod_mainmenu_settings, NULL, Icon_NOICON);
MAKE_MENU(uipod_appearance_menu, ID2P(LANG_UIPOD_APPEARANCE), NULL,
          Icon_Submenu_Entered,
          &tp_pw_theme, &tp_pw_customize_theme, &tp_pw_transition,
          &tp_app_lang, &tp_pw_mono_art, &tp_app_mainmenu);

int uipod_appearance_page(void)
{
    do_menu(&uipod_appearance_menu, NULL, NULL, false);
    return 0;
}

/* Settings -> Device: Brightness, Display Poweroff, Idle Poweroff, CPU, Charge Limit. */
MENUITEM_VALUE(tp_pw_cpu, ID2P(LANG_UIPOD_CPU), &uipod_cpu_value, Icon_NOICON);
MENUITEM_SETTING(tp_pw_brightness, &global_settings.brightness, NULL);
MENUITEM_SETTING(tp_pw_screenoff, &global_settings.backlight_timeout, NULL);
MENUITEM_SETTING(tp_pw_idlepoweroff, &global_settings.poweroff, NULL);
MENUITEM_VALUE(tp_pw_charge, "Charge Limit", &uipod_charge_value, Icon_NOICON);
MAKE_MENU(uipod_power_menu, ID2P(LANG_UIPOD_DEVICE), NULL, Icon_Submenu_Entered,
          &tp_pw_brightness, &tp_pw_screenoff, &tp_pw_idlepoweroff,
          &tp_pw_cpu, &tp_pw_charge);
MAKE_MENU(uipod_power_menu_nobatt, ID2P(LANG_UIPOD_DEVICE), NULL, Icon_Submenu_Entered,
          &tp_pw_brightness, &tp_pw_screenoff, &tp_pw_idlepoweroff,
          &tp_pw_cpu);

int uipod_power_page(void)
{
    const struct menu_item_ex *m =
        retrohh_charge_limit_hidden() ? &uipod_power_menu_nobatt : &uipod_power_menu;
    do_menu(m, NULL, NULL, false);
    return 0;
}
