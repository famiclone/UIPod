/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * Copyright (C) 2006 Dan Everton
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

#include "config.h"
#include <stdio.h>
#include <stdbool.h>
#include <stdlib.h>
#include <string.h>
#include "eq_menu.h"
#include "system.h"
#include "kernel.h"
#include "lcd.h"
#include "list.h"
#include "menu.h"
#include "action.h"
#include "button.h"
#include "audio.h"
#include "settings.h"
#include "rbpaths.h"
#include "screens.h"
#include "icons.h"
#include "font.h"
#include "lang.h"
#include "misc.h"
#include "sound.h"
#include "dsp_proc_settings.h"
#include "screen_access.h"
#include "menu_common.h"
#include "viewport.h"
#include "exported_menus.h"
#include "string-extra.h"
#include "tape_deck_fx.h"
#include "uipod_autoeq.h"
#include "uipod_page.h"
#include "uipod_rs_ui.h"
#include "uipod_ui.h"

extern struct eq_band_setting eq_defaults[EQ_NUM_BANDS];

const char *eq_q_format(char *buffer, size_t buffer_size, int value, const char *unit)
{
    snprintf(buffer, buffer_size, "%d.%d %s", value / EQ_USER_DIVISOR,
             value % EQ_USER_DIVISOR, unit);
    return buffer;
}

const char *eq_precut_format(char *buffer, size_t buffer_size, int value, const char *unit)
{
    int preamp = -value;
    int av = abs(preamp);
    const char *sign = (preamp > 0) ? "+" : ((preamp < 0) ? "-" : "");
    snprintf(buffer, buffer_size, "%s%d.%d %s", sign,
             av / EQ_USER_DIVISOR, av % EQ_USER_DIVISOR, unit);
    return buffer;
}

void eq_enabled_option_callback(bool enabled)
{
    (void)enabled;
    uipod_eq_apply();
}

static int eq_setting_callback(int action,
                               const struct menu_item_ex *this_item,
                               struct gui_synclist *this_list)
{
    (void)this_list;
    switch (action)
    {
        case ACTION_ENTER_MENUITEM:
            action = lowlatency_callback(action, this_item, NULL);
            break;
        case ACTION_EXIT_MENUITEM:
            uipod_eq_apply();
            action = lowlatency_callback(action, this_item, NULL);
            break;
    }
    return action;
}
MENUITEM_SETTING(eq_enable, &global_settings.eq_enabled, eq_setting_callback);

/* ---- Graphic Equalizer Page (`ui.figcraft` #3215) ---------------------- */

#define GEQ_COLS            11
#define GEQ_MIN_TENTHS    (-120)
#define GEQ_MAX_TENTHS      120
#define GEQ_STEP_TENTHS       5
#define GEQ_HOLD_HOME_MS   650UL

static const int k_iso_band_hz[EQ_NUM_BANDS] = {
    31, 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000
};

static int s_last_selected_col = 1; /* default to first frequency band (31Hz) */

static int clamp_tenths(int v)
{
    if (v < GEQ_MIN_TENTHS)
        return GEQ_MIN_TENTHS;
    if (v > GEQ_MAX_TENTHS)
        return GEQ_MAX_TENTHS;
    return v;
}

static int step_half_db(int cur, int dir)
{
    int next;
    if (dir > 0)
    {
        int rem = ((cur % GEQ_STEP_TENTHS) + GEQ_STEP_TENTHS) % GEQ_STEP_TENTHS;
        next = (rem == 0) ? (cur + GEQ_STEP_TENTHS) : (cur + (GEQ_STEP_TENTHS - rem));
    }
    else
    {
        int rem = ((cur % GEQ_STEP_TENTHS) + GEQ_STEP_TENTHS) % GEQ_STEP_TENTHS;
        next = (rem == 0) ? (cur - GEQ_STEP_TENTHS) : (cur - rem);
    }
    return clamp_tenths(next);
}

static bool bands_are_iso_graphic(void)
{
    for (int i = 0; i < EQ_NUM_BANDS; i++)
    {
        int fc = global_settings.eq_band_settings[i].cutoff;
        int ref = k_iso_band_hz[i];
        int lo = (ref * 72) / 100;
        int hi = (ref * 138) / 100;
        if (fc < lo || fc > hi)
            return false;
    }
    return true;
}

struct graphic_eq_page
{
    struct uipod_page base;
    int16_t           preamp_tenths;
    int16_t           band_gains[EQ_NUM_BANDS];
    int               selected_col;
    bool              adjusting;
    bool              bypassed;
    bool              was_parametric;
    bool              dirty;

    /* Chord-aware repeat state for LEFT/RIGHT and A+UP/DOWN */
    long              prev_held;
    unsigned long     lr_next_ms;
    unsigned long     ud_next_ms;
    unsigned long     b_down_ms;
    bool              b_was_down;
    bool              b_home_fired;
};

static void geq_load_from_settings(struct graphic_eq_page *p)
{
    uipod_eq_apply();
    p->preamp_tenths = (int16_t)clamp_tenths(-global_settings.eq_precut);
    if (bands_are_iso_graphic())
    {
        p->was_parametric = false;
        for (int i = 0; i < EQ_NUM_BANDS; i++)
            p->band_gains[i] = (int16_t)clamp_tenths(global_settings.eq_band_settings[i].gain);
    }
    else
    {
        p->was_parametric = true;
        for (int i = 0; i < EQ_NUM_BANDS; i++)
        {
            int sampled = tape_deck_fx_eval_eq_curve_tenths_db((double)k_iso_band_hz[i]);
            p->band_gains[i] = (int16_t)clamp_tenths(sampled);
        }
    }
}

static void geq_commit_change(struct graphic_eq_page *p)
{
    if (p->was_parametric)
    {
        for (int i = 0; i < EQ_NUM_BANDS; i++)
        {
            global_settings.eq_band_settings[i].cutoff = eq_defaults[i].cutoff;
            global_settings.eq_band_settings[i].q      = eq_defaults[i].q;
            global_settings.eq_band_settings[i].gain   = p->band_gains[i];
        }
        p->was_parametric = false;
    }
    else
    {
        for (int i = 0; i < EQ_NUM_BANDS; i++)
            global_settings.eq_band_settings[i].gain = p->band_gains[i];
    }
    global_settings.eq_precut  = -(int)p->preamp_tenths;
    global_settings.eq_enabled = true;
    p->dirty = true;
    uipod_eq_apply();
}

static void geq_draw_sw_fallback(const struct graphic_eq_page *p)
{
    static const char *const col_names[GEQ_COLS] = {
        "Pre", "31", "63", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"
    };
    struct screen *s = &screens[SCREEN_MAIN];
    struct viewport vp = {0};
    viewport_set_defaults(&vp, SCREEN_MAIN);
    s->set_viewport(&vp);
    s->set_background(global_settings.bg_color);
    s->clear_viewport();

    const int margin = 32;
    const int col_w = (vp.width - 2 * margin) / GEQ_COLS;
    const int top_y = 24;
    const int track_h = vp.height - 96;

    for (int c = 0; c < GEQ_COLS; c++)
    {
        int cx = margin + c * col_w + col_w / 2;
        int tx = cx - 12;
        bool sel = (c == p->selected_col);
        int tenths = (c == 0) ? p->preamp_tenths : p->band_gains[c - 1];
        int fill_h = ((tenths + 120) * (track_h - 8)) / 240;
        if (fill_h < 0) fill_h = 0;
        if (fill_h > track_h - 8) fill_h = track_h - 8;

        s->set_foreground(sel ? global_settings.fg2_color : global_settings.fg1_color);
        s->drawrect(tx, top_y, 24, track_h);
        if (fill_h > 0)
        {
            s->set_foreground(p->bypassed ? global_settings.fg1_color
                                          : (sel ? global_settings.fg_color : global_settings.fg2_color));
            s->fillrect(tx + 4, top_y + track_h - 4 - fill_h, 16, fill_h);
        }
        int lw = 0, lh = 0;
        s->getstringsize((const unsigned char *)col_names[c], &lw, &lh);
        s->set_foreground(sel ? global_settings.fg_color : global_settings.fg2_color);
        s->putsxy(cx - lw / 2, top_y + track_h + 12, (const unsigned char *)col_names[c]);
    }
    s->update_viewport();
    s->set_viewport(NULL);
}

static void geq_page_draw(struct uipod_page *self)
{
    struct graphic_eq_page *p = (struct graphic_eq_page *)self;
    const char *title = p->bypassed ? str(LANG_UIPOD_BYPASS)
                                    : str(LANG_UIPOD_GRAPHIC_EQ);
    if (uipod_gpu_render_graphic_eq(title,
                                    p->preamp_tenths,
                                    p->band_gains,
                                    (size_t)p->selected_col,
                                    p->adjusting,
                                    p->bypassed))
    {
        return;
    }
    geq_draw_sw_fallback(p);
}

static int geq_page_poll(struct uipod_page *self, int timeout)
{
    (void)timeout;
    struct graphic_eq_page *p = (struct graphic_eq_page *)self;

    int wait_ticks = (p->adjusting || p->bypassed || uipod_gpu_is_volume_popup_active())
                         ? 1
                         : (HZ / 25);
    long ev = button_get_w_tmo(wait_ticks);
    (void)uipod_gpu_tick_volume_popup();

    if (ev != BUTTON_NONE && (ev & SYS_EVENT) != 0)
        return (int)ev;

    long held = button_status();
    unsigned long now_ms = sdl_get_ms();
    bool need_redraw = uipod_gpu_check_window_resized();

#if defined(BUTTON_VOL_UP) && defined(BUTTON_VOL_DOWN)
    if (ev != BUTTON_NONE && !(ev & BUTTON_REL))
    {
        long base_ev = ev & ~(BUTTON_REPEAT | BUTTON_REL);
        if ((base_ev & (BUTTON_VOL_UP | BUTTON_VOL_DOWN)) != 0)
        {
#if defined(BUTTON_SELECT) && defined(HAVE_BACKLIGHT_BRIGHTNESS)
            if ((held & BUTTON_SELECT) != 0)
                adjust_brightness((base_ev & BUTTON_VOL_UP) ? 1 : -1);
            else
#endif
                adjust_volume((base_ev & BUTTON_VOL_UP) ? 1 : -1);
        }
    }
#endif

#ifdef BUTTON_START
    if (ev == BUTTON_START)
    {
        int st = audio_status();
        if (st & AUDIO_STATUS_PLAY)
        {
            if (st & AUDIO_STATUS_PAUSE)
                audio_resume();
            else
                audio_pause();
            need_redraw = true;
        }
    }
#endif

#ifdef BUTTON_MENU
    if (ev != BUTTON_NONE && !(ev & (BUTTON_REPEAT | BUTTON_REL)) &&
        (ev & ~(BUTTON_REPEAT | BUTTON_REL)) == BUTTON_MENU)
    {
        uipod_menu_click();
        return ACTION_STD_CANCEL;
    }
#endif

    /* B tap -> back; Hold-B >= 650 ms -> Home */
    bool b_down = (held & BUTTON_B) != 0;
    if (b_down && !p->b_was_down)
    {
        p->b_was_down   = true;
        p->b_down_ms    = now_ms;
        p->b_home_fired = false;
    }
    else if (b_down && p->b_was_down && !p->b_home_fired)
    {
        if (now_ms - p->b_down_ms >= GEQ_HOLD_HOME_MS)
        {
            p->b_home_fired = true;
            uipod_menu_click();
            return ACTION_TP_HOME;
        }
    }
    else if (!b_down && p->b_was_down)
    {
        bool was_home = p->b_home_fired;
        p->b_was_down   = false;
        p->b_home_fired = false;
        if (!was_home)
        {
            uipod_menu_click();
            return ACTION_STD_CANCEL;
        }
    }

    /* Hold-X: instantaneous momentary EQ bypass */
    bool want_bypass = (held & BUTTON_X) != 0;
    if (want_bypass != p->bypassed)
    {
        p->bypassed = want_bypass;
        tape_deck_fx_set_eq_bypass(want_bypass);
        need_redraw = true;
    }

    /* Hold-A: live gain adjustment mode ("63Hz: +4.5" in Header) */
    bool want_adj = (held & BUTTON_A) != 0;
    if (want_adj != p->adjusting)
    {
        p->adjusting = want_adj;
        p->ud_next_ms = 0;
        need_redraw = true;
    }

    /* LEFT / RIGHT: select slider column 0..10 */
    int lr_dir = ((held & BUTTON_RIGHT) != 0) ? 1 : (((held & BUTTON_LEFT) != 0) ? -1 : 0);
    int prev_lr = ((p->prev_held & BUTTON_RIGHT) != 0) ? 1 : (((p->prev_held & BUTTON_LEFT) != 0) ? -1 : 0);
    if (lr_dir != 0)
    {
        if (lr_dir != prev_lr)
        {
            p->selected_col = (p->selected_col + lr_dir + GEQ_COLS) % GEQ_COLS;
            s_last_selected_col = p->selected_col;
            p->lr_next_ms = now_ms + 280UL;
            uipod_menu_click();
            need_redraw = true;
        }
        else if (now_ms >= p->lr_next_ms)
        {
            p->selected_col = (p->selected_col + lr_dir + GEQ_COLS) % GEQ_COLS;
            s_last_selected_col = p->selected_col;
            p->lr_next_ms = now_ms + 105UL;
            uipod_menu_click();
            need_redraw = true;
        }
    }

    /* While A is held, UP / DOWN adjust the selected slider in 0.5 dB steps */
    if (p->adjusting)
    {
        int ud_dir = ((held & BUTTON_UP) != 0) ? 1 : (((held & BUTTON_DOWN) != 0) ? -1 : 0);
        int prev_ud = ((p->prev_held & BUTTON_UP) != 0) ? 1 : (((p->prev_held & BUTTON_DOWN) != 0) ? -1 : 0);
        bool step_now = false;
        if (ud_dir != 0)
        {
            if (ud_dir != prev_ud)
            {
                step_now = true;
                p->ud_next_ms = now_ms + 260UL;
            }
            else if (now_ms >= p->ud_next_ms)
            {
                step_now = true;
                p->ud_next_ms = now_ms + 75UL;
            }
        }
        if (step_now)
        {
            if (p->selected_col == 0)
            {
                int cur = p->preamp_tenths;
                int next = step_half_db(cur, ud_dir);
                if (next != cur)
                {
                    p->preamp_tenths = (int16_t)next;
                    geq_commit_change(p);
                }
            }
            else
            {
                int b = p->selected_col - 1;
                int cur = p->band_gains[b];
                int next = step_half_db(cur, ud_dir);
                if (next != cur)
                {
                    p->band_gains[b] = (int16_t)next;
                    geq_commit_change(p);
                }
            }
            uipod_menu_click();
            need_redraw = true;
        }
    }

    p->prev_held = held;

    if (need_redraw)
        geq_page_draw(self);

    return ACTION_NONE;
}

static enum uipod_page_result geq_page_on_action(struct uipod_page *self, int action)
{
    (void)self;
    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable geq_vtable = {
    .legend    = NULL,
    .draw      = geq_page_draw,
    .poll      = geq_page_poll,
    .on_action = geq_page_on_action,
};

static const int geq_allowed[] = { ACTION_STD_CANCEL, -1 };

static int uipod_graphic_eq_page(void)
{
    struct graphic_eq_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt                = &geq_vtable;
    p.base.context           = CONTEXT_KEYBOARD;
    p.base.allowed           = geq_allowed;
    p.base.title             = str(LANG_UIPOD_GRAPHIC_EQ);
    p.base.no_theme          = true;
    p.base.no_header_refresh = true;
    p.selected_col           = (s_last_selected_col >= 0 && s_last_selected_col < GEQ_COLS)
                                   ? s_last_selected_col
                                   : 1;

    geq_load_from_settings(&p);
    tape_deck_fx_set_eq_bypass(false);

    uipod_page_run(&p.base);

    tape_deck_fx_set_eq_bypass(false);
    if (p.dirty)
        settings_save();

    return p.base.home ? GO_TO_ROOT : 0;
}

static int eq_save_preset(void)
{
    bool enabled = global_settings.eq_enabled;
    global_settings.eq_enabled = true;

    bool result = settings_save_config(SETTINGS_SAVE_EQPRESET);

    global_settings.eq_enabled = enabled;
    return result ? 1 : 0;
}

static void eq_reset_defaults(void)
{
    global_settings.eq_precut = 0;
    for (int i = 0; i < EQ_NUM_BANDS; i++)
    {
        global_settings.eq_band_settings[i].cutoff = eq_defaults[i].cutoff;
        global_settings.eq_band_settings[i].q      = eq_defaults[i].q;
        global_settings.eq_band_settings[i].gain   = eq_defaults[i].gain;
    }
    uipod_eq_apply();
    settings_save();
}

MENUITEM_FUNCTION(eq_graphic, MENU_SHOW_CHEVRON, ID2P(LANG_UIPOD_GRAPHIC_EQ),
                  uipod_graphic_eq_page, NULL, Icon_NOICON);
MENUITEM_FUNCTION(eq_autoeq, MENU_SHOW_CHEVRON, ID2P(LANG_UIPOD_AUTOEQ),
                  uipod_autoeq_page, lowlatency_callback, Icon_NOICON);
MENUITEM_FUNCTION(eq_browse, MENU_SHOW_CHEVRON, ID2P(LANG_EQUALIZER_BROWSE),
                  uipod_eq_presets_page, lowlatency_callback, Icon_NOICON);
MENUITEM_FUNCTION(eq_save, 0, ID2P(LANG_EQUALIZER_SAVE),
                  eq_save_preset, NULL, Icon_NOICON);
MENUITEM_FUNCTION(eq_reset, 0, ID2P(LANG_RESET_EQUALIZER),
                  eq_reset_defaults, NULL, Icon_NOICON);

MAKE_MENU(equalizer_menu, ID2P(LANG_EQUALIZER), NULL, Icon_EQ,
          &eq_enable,
          &eq_graphic,
          &eq_autoeq,
          &eq_browse,
          &eq_save,
          &eq_reset);
