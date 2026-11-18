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
#include <string.h>
#include "eq_menu.h"
#include "system.h"
#include "kernel.h"
#include "lcd.h"
#include "list.h"
#include "menu.h"
#include "action.h"
#include "settings.h"
#include "rbpaths.h"
#include "screens.h"
#include "icons.h"
#include "font.h"
#include "lang.h"
#include "misc.h"
#include "sound.h"
#include "dsp_proc_settings.h"
#include "tree.h"
#include "screen_access.h"
#include "gui/scrollbar.h"
#include "menu_common.h"
#include "viewport.h"
#include "exported_menus.h"
#include "pcmbuf.h"
#include "option_select.h"
#include "string-extra.h"
#include "uipod_autoeq.h"

static void eq_apply(void);

/*
 * Utility functions
 */

const char* eq_q_format(char* buffer, size_t buffer_size, int value, const char* unit)
{
    snprintf(buffer, buffer_size, "%d.%d %s", value / EQ_USER_DIVISOR,
         value % EQ_USER_DIVISOR, unit);
    return buffer;
}

const char* eq_precut_format(char* buffer, size_t buffer_size, int value, const char* unit)
{
    snprintf(buffer, buffer_size, "%s%d.%d %s", value == 0 ? " " : "-",
        value / EQ_USER_DIVISOR, value % EQ_USER_DIVISOR, unit);
    return buffer;
}

void eq_enabled_option_callback(bool enabled)
{
    (void)enabled;
    eq_apply();
}

/*
 * Settings functions
 */
static void eq_apply(void)
{
    dsp_eq_enable(global_settings.eq_enabled);
    dsp_set_eq_precut(global_settings.eq_precut);
    /* Update all bands */
    for(int i = 0; i < EQ_NUM_BANDS; i++) {
        dsp_set_eq_coefs(i, &global_settings.eq_band_settings[i]);
    }
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
            eq_apply();
            action = lowlatency_callback(action, this_item, NULL);
            break;
    }

    return action;
}
MENUITEM_SETTING(eq_enable, &global_settings.eq_enabled, eq_setting_callback);
MENUITEM_SETTING(eq_precut, &global_settings.eq_precut, eq_setting_callback);

static char* gainitem_get_name(int selected_item, void *data, char *buffer, size_t len)
{
    (void)data;
    snprintf(buffer, len, str(LANG_EQUALIZER_GAIN_ITEM),
            global_settings.eq_band_settings[selected_item].cutoff);

    return buffer;
}

static const char* db_format(char* buffer, size_t buffer_size, int value,
                      const char* unit)
{
    int v = abs(value);

    snprintf(buffer, buffer_size, "%s%d.%d %s", value < 0 ? "-" : "",
             v / 10, v % 10, unit);
    return buffer;
}

static const struct int_setting gain_int_setting = {
    .option_callback = NULL,
    .unit = UNIT_DB,
    .step = EQ_GAIN_STEP,
    .min = EQ_GAIN_MIN,
    .max = EQ_GAIN_MAX,
    .formatter = db_format,
};

static const struct int_setting q_int_setting = {
    .option_callback = NULL,
    .unit = UNIT_INT,
    .step = EQ_Q_STEP,
    .min = EQ_Q_MIN,
    .max = EQ_Q_MAX,
    .formatter = eq_q_format,
};

static const struct int_setting cutoff_int_setting = {
    .option_callback = NULL,
    .unit = UNIT_HERTZ,
    .step = EQ_CUTOFF_STEP,
    .min = EQ_CUTOFF_MIN,
    .max = EQ_CUTOFF_MAX,
    .formatter = NULL,
};

static int simplelist_action_callback(int action, struct gui_synclist *lists)
{
    (void)lists;
    if (action == ACTION_STD_OK)
        return ACTION_STD_CANCEL;
    return action;
}

static int eq_do_simple_menu(void * param)
{
    (void)param;
    struct simplelist_info info;
    struct settings_list setting;
    char title[MAX_PATH];

    simplelist_info_init(&info, str(LANG_EQUALIZER_GAIN), EQ_NUM_BANDS, NULL);
    info.get_name = (list_get_name*)gainitem_get_name;
    info.action_callback = simplelist_action_callback;
    info.selection = -1;
    info.title_icon = Icon_Submenu;
    setting.flags = F_BANFROMQS|F_INT_SETTING|F_T_INT|F_NO_WRAP;
    setting.lang_id = LANG_GAIN;
    setting.default_val.int_ = 0;
    setting.int_setting = &gain_int_setting;

    while (true)
    {
        simplelist_show_list(&info);
        if (info.selection < 0)
            break;
        pcmbuf_set_low_latency(true);
        setting.setting = &global_settings.eq_band_settings[info.selection].gain;
        option_screen(&setting, NULL, false,
                gainitem_get_name(info.selection, NULL, title, MAX_PATH));
        eq_apply();
        pcmbuf_set_low_latency(false);
    }
    return 0;
}
MENUITEM_FUNCTION(gain_menu, 0, ID2P(LANG_EQUALIZER_GAIN),
	              eq_do_simple_menu, NULL, Icon_Submenu);

static void selection_to_banditem(int selection, int expanded_band, int *band, int *item)
{
    int diff = selection - expanded_band;

    if (expanded_band < 0 || diff < 0)
    {
        *item = 0;
        *band = selection;
    }
    else if (diff < 4)
    {
        *item = selection - expanded_band;
        *band = expanded_band;
    }
    else
    {
        *item = 0;
        *band = expanded_band + diff - 3;
    }
}

static char *advancedmenu_item_get_name(int selected_item, void *data, char *buffer, size_t len)
{
    int band;
    int item;
    int lang = -1;

    selection_to_banditem(selected_item, *(intptr_t*)data, &band, &item);

    switch (item)
    {
    case 0: /* Band title */
        if (band == 0)
            return str(LANG_EQUALIZER_BAND_LOW_SHELF);
        else if (band == EQ_NUM_BANDS - 1)
            return str(LANG_EQUALIZER_BAND_HIGH_SHELF);
        else
        {
            snprintf(buffer, len, str(LANG_EQUALIZER_BAND_PEAK), band);
            return buffer;
        }
        break;
    case 1: /* cutoff */
        if (band == 0)
            lang = LANG_EQUALIZER_BAND_CUTOFF;
        else if (band == EQ_NUM_BANDS - 1)
            lang = LANG_EQUALIZER_BAND_CUTOFF;
        else
            lang = LANG_EQUALIZER_BAND_CENTER;
        break;
    case 2: /* Q */
        lang = LANG_EQUALIZER_BAND_Q;
        break;
    case 3: /* Gain */
        lang = LANG_GAIN;
        break;
    }

    if(lang < 0)
        buffer[0] = 0;
    else {
        buffer[0] = '\t';
        strmemccpy(&buffer[1], str(lang), len - 1);
    }

    return buffer;
}

extern struct eq_band_setting eq_defaults[EQ_NUM_BANDS];

static int eq_do_advanced_menu(void * param)
{
    (void)param;
    struct simplelist_info info;
    struct settings_list setting;
    char title[MAX_PATH];
    int band, item;
    intptr_t selected_band = -1;

    simplelist_info_init(&info, str(LANG_EQUALIZER_ADVANCED),
            EQ_NUM_BANDS, &selected_band);
    info.get_name = (list_get_name*)advancedmenu_item_get_name;
    info.action_callback = simplelist_action_callback;
    info.selection = -1;
    info.title_icon = Icon_EQ;
    setting.flags = F_BANFROMQS|F_INT_SETTING|F_T_INT|F_NO_WRAP;

    while (true)
    {
        simplelist_show_list(&info);
        if (info.selection < 0)
            break;
        selection_to_banditem(info.selection, selected_band, &band, &item);
        switch (item)
        {
            case 0: /* title, do nothing */
            {
                int extra;
                if (selected_band == band)
                {
                    extra = 0;
                    selected_band = -1;
                }
                else
                {
                    extra = 3;
                    selected_band = band;
                }
                info.selection = band;
                info.count = EQ_NUM_BANDS + extra;
                continue;
            }
            case 1: /* cutoff */
                if (band == 0 || band == EQ_NUM_BANDS - 1)
                    setting.lang_id = LANG_EQUALIZER_BAND_CUTOFF;
                else
                    setting.lang_id = LANG_EQUALIZER_BAND_CENTER;
                setting.default_val.int_ = eq_defaults[band].cutoff;
                setting.int_setting = &cutoff_int_setting;
                setting.setting = &global_settings.eq_band_settings[band].cutoff;
                break;
            case 2: /* Q */
                setting.lang_id = LANG_EQUALIZER_BAND_Q;
                setting.default_val.int_ = eq_defaults[band].q;
                setting.int_setting = &q_int_setting;
                setting.setting = &global_settings.eq_band_settings[band].q;
                break;
            case 3: /* Gain */
                setting.lang_id = LANG_GAIN;
                setting.default_val.int_ = eq_defaults[band].gain;
                setting.int_setting = &gain_int_setting;
                setting.setting = &global_settings.eq_band_settings[band].gain;
                break;
        }
        pcmbuf_set_low_latency(true);
        advancedmenu_item_get_name(info.selection, &selected_band, title, MAX_PATH);

        option_screen(&setting, NULL, false, title[0] == '\t' ? &title[1] : title);
        eq_apply();
        pcmbuf_set_low_latency(false);
    }
    return 0;
}
MENUITEM_FUNCTION(advanced_menu, 0, ID2P(LANG_EQUALIZER_ADVANCED),
                  eq_do_advanced_menu, NULL, Icon_EQ);

static int eq_save_preset(void)
{
    /* make sure that the eq is enabled for setting saving */
    bool enabled = global_settings.eq_enabled;
    global_settings.eq_enabled = true;

    bool result = settings_save_config(SETTINGS_SAVE_EQPRESET);

    global_settings.eq_enabled = enabled;

    return (result) ? 1 : 0;
}

static void eq_reset_defaults(void)
{
    for (int i = 0; i < EQ_NUM_BANDS; i++) {
        global_settings.eq_band_settings[i].cutoff = eq_defaults[i].cutoff;
        global_settings.eq_band_settings[i].q = eq_defaults[i].q;
        global_settings.eq_band_settings[i].gain = eq_defaults[i].gain;
    }
    eq_apply();
}

MENUITEM_FUNCTION(eq_autoeq, MENU_SHOW_CHEVRON, ID2P(LANG_UIPOD_AUTOEQ),
                  uipod_autoeq_page, lowlatency_callback, Icon_NOICON);
MENUITEM_FUNCTION(eq_save, 0, ID2P(LANG_EQUALIZER_SAVE),
                  eq_save_preset, NULL, Icon_NOICON);
MENUITEM_FUNCTION(eq_reset, 0, ID2P(LANG_RESET_EQUALIZER),
                  eq_reset_defaults, NULL, Icon_NOICON);
MENUITEM_FUNCTION(eq_browse, MENU_SHOW_CHEVRON, ID2P(LANG_EQUALIZER_BROWSE),
                  uipod_eq_presets_page, lowlatency_callback, Icon_NOICON);

MAKE_MENU(equalizer_menu, ID2P(LANG_EQUALIZER), NULL, Icon_EQ,
        &eq_enable, &eq_autoeq, &eq_browse, &eq_precut,
        &gain_menu, &advanced_menu, &eq_save, &eq_reset);
