/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * Copyright (C) 2007 Jonathan Gordon
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

#include <stdbool.h>
#include <stddef.h>
#include <limits.h>
#include "config.h"
#include "string.h"
#include "lang.h"
#include "action.h"
#include "settings.h"
#include "power.h"
#include "powermgmt.h"
#include "menu.h"
#include "misc.h"
#include "exported_menus.h"
#include "trimpod_mainmenu.h"
#include "tree.h"
#include "storage.h"
#include "yesno.h"
#include "keyboard.h"
#include "screens.h"
#include "splash.h"
#include "file.h"
#include "dir.h"
#include "version.h"
#include "time.h"
#include "wps.h"
#include "skin_buffer.h"
#include "disk.h"
#include "root_menu.h"
#include "trimpod_folders.h"
#include "trimpod_library.h"
#include "trimpod_navidrome.h"
#include "trimpod_jellyfin.h"
#include "trimpod_page.h"
#include "trimpod_transition.h"
#include "trimpod_ui.h"

static const struct browse_folder_info config = {ROCKBOX_DIR, SHOW_CFG};
/***********************************/
/*    MANAGE SETTINGS MENU        */

static int reset_settings(void)
{
    static const char *lines[]={ID2P(LANG_ARE_YOU_SURE), ID2P(LANG_RESET)};
    static const char *yes_lines[]={
        ID2P(LANG_SETTINGS),
        ID2P(LANG_RESET_DONE_CLEAR)
    };
    static const char *no_lines[]={
        ID2P(LANG_SETTINGS),
        ID2P(LANG_CANCEL)
    };
    static const struct text_message message={lines, 2};
    static const struct text_message yes_message={yes_lines, 2};
    static const struct text_message no_message={no_lines, 2};

    switch(gui_syncyesno_run(&message, &yes_message, &no_message))
    {
        case YESNO_YES:
            settings_reset();
            settings_save();
            settings_apply(true);
            settings_apply_skins();
            break;
        case YESNO_TMO:
        case YESNO_NO:
            break;
        case YESNO_USB:
            return 1;
    }
    return 0;
}
static int write_settings_file(void* param)
{
    return settings_save_config((intptr_t)param);
}

MENUITEM_FUNCTION_W_PARAM(browse_configs, 0, ID2P(LANG_CUSTOM_CFG),
                          browse_folder, (void*)&config, NULL, Icon_NOICON);
MENUITEM_FUNCTION_W_PARAM(save_settings_item, 0, ID2P(LANG_SAVE_SETTINGS),
                          write_settings_file, (void*)SETTINGS_SAVE_ALL,
                          NULL, Icon_NOICON);
MENUITEM_FUNCTION_W_PARAM(save_theme_item, 0, ID2P(LANG_SAVE_THEME),
                          write_settings_file, (void*)SETTINGS_SAVE_THEME,
                          NULL, Icon_NOICON);
MENUITEM_FUNCTION_W_PARAM(save_sound_item, 0, ID2P(LANG_SAVE_SOUND),
                          write_settings_file, (void*)SETTINGS_SAVE_SOUND,
                          NULL, Icon_NOICON);
MENUITEM_FUNCTION(reset_settings_item, 0, ID2P(LANG_RESET),
                  reset_settings, NULL, Icon_NOICON);

MAKE_MENU(manage_settings, ID2P(LANG_MANAGE_MENU), NULL, Icon_Config,
          &browse_configs, &reset_settings_item,
          &save_settings_item, &save_sound_item, &save_theme_item);
/*    MANAGE SETTINGS MENU        */
/**********************************/


/* About: a self-scrolling credits reel (B exits). */
static int trimpod_about_page(void)
{
    trimpod_about();
    return 0;
}

/* Controls: a static list of the app's inputs (B exits). */
static int trimpod_controls_page(void)
{
    trimpod_controls();
    return 0;
}

static int trimpod_exit_app(void)
{
    sys_poweroff();
    return 0;
}

/* ---- Source type (#646): Local / Navidrome / Jellyfin ----------------- */

static bool s_source_boot_mode;

static int tp_source_select_local(void)
{
    trimpod_source_set(TRIMPOD_SOURCE_LOCAL);
    trimpod_library_mark_stale();
    if (!s_source_boot_mode)
        trimpod_home_pending = true;    /* Settings -> Source -> Local => Main */
    return 1;
}

static int tp_source_select_navidrome(void)
{
    if (trimpod_navidrome_has_tracks())
    {
        trimpod_source_set(TRIMPOD_SOURCE_NAVIDROME);
        if (!s_source_boot_mode)
            trimpod_home_pending = true;
        return 1;
    }
    if (trimpod_navidrome_settings_page(true))
    {
        if (!s_source_boot_mode)
            trimpod_home_pending = true;
        return 1;
    }
    return 0;
}

static int tp_source_select_jellyfin(void)
{
    if (trimpod_jellyfin_has_media())
    {
        trimpod_source_set(TRIMPOD_SOURCE_JELLYFIN);
        if (!s_source_boot_mode)
            trimpod_home_pending = true;
        return 1;
    }
    if (trimpod_jellyfin_settings_page(true))
    {
        if (!s_source_boot_mode)
            trimpod_home_pending = true;
        return 1;
    }
    return 0;
}

MENUITEM_FUNCTION(tp_src_local, MENU_SHOW_CHEVRON | MENU_FUNC_CHECK_RETVAL,
                  ID2P(LANG_TRIMPOD_SOURCE_LOCAL), tp_source_select_local,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_src_navidrome, MENU_SHOW_CHEVRON | MENU_FUNC_CHECK_RETVAL,
                  ID2P(LANG_TRIMPOD_SOURCE_NAVIDROME), tp_source_select_navidrome,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_src_jellyfin, MENU_SHOW_CHEVRON | MENU_FUNC_CHECK_RETVAL,
                  ID2P(LANG_TRIMPOD_SOURCE_JELLYFIN), tp_source_select_jellyfin,
                  NULL, Icon_NOICON);
MAKE_MENU(trimpod_source_menu, ID2P(LANG_TRIMPOD_SOURCE_TYPE), NULL,
          Icon_Submenu_Entered, &tp_src_local, &tp_src_navidrome, &tp_src_jellyfin);

int trimpod_source_page(void)
{
    s_source_boot_mode = false;
    do_menu(&trimpod_source_menu, NULL, NULL, false);
    return 0;
}

int trimpod_source_boot_page(void)
{
    s_source_boot_mode = true;
    int r = do_menu(&trimpod_source_menu, NULL, NULL, false);
    if (!trimpod_source_is_configured())
        trimpod_source_set(TRIMPOD_SOURCE_LOCAL);
    if (r == GO_TO_PREVIOUS)
        trimpod_transition_take_back(); /* boot: slide forward into Main */
    s_source_boot_mode = false;
    return 0;
}

/* ---- Extras (opened from Main -> Extras) ------------------------------ */

static int tp_ext_shuffle_fn(void)
{
    int r = trimpod_shuffle_all(NULL);
    return (r == GO_TO_WPS) ? GO_TO_WPS : 0;
}

static int tp_ext_playlists_fn(void)
{
    return GO_TO_PLAYLISTS_SCREEN;
}

MENUITEM_FUNCTION(tp_ext_shuffle, MENU_FUNC_CHECK_RETVAL,
                  ID2P(LANG_TRIMPOD_SHUFFLE_ALL), tp_ext_shuffle_fn,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_ext_playlists, MENU_SHOW_CHEVRON | MENU_FUNC_CHECK_RETVAL,
                  ID2P(LANG_PLAYLISTS), tp_ext_playlists_fn,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_ext_viz, MENU_SHOW_CHEVRON,
                  ID2P(LANG_TRIMPOD_AUDIO_VISUALIZER), trimpod_av_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_ext_controls, MENU_SHOW_CHEVRON,
                  ID2P(LANG_TRIMPOD_CONTROLS), trimpod_controls_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_ext_about, MENU_SHOW_CHEVRON,
                  ID2P(LANG_TRIMPOD_ABOUT), trimpod_about_page,
                  NULL, Icon_NOICON);
MAKE_MENU(trimpod_extras_menu, ID2P(LANG_TRIMPOD_EXTRAS), NULL,
          Icon_Submenu_Entered,
          &tp_ext_shuffle, &tp_ext_playlists, &tp_ext_viz,
          &tp_ext_controls, &tp_ext_about);

int trimpod_extras_page(void)
{
    return do_menu(&trimpod_extras_menu, NULL, NULL, false);
}

/* ---- Settings (#864) -------------------------------------------------- */

MENUITEM_FUNCTION(tp_set_appearance, MENU_SHOW_CHEVRON,
                  ID2P(LANG_TRIMPOD_APPEARANCE), trimpod_appearance_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_source, MENU_SHOW_CHEVRON,
                  ID2P(LANG_TRIMPOD_SOURCE_TYPE), trimpod_source_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_navidrome, MENU_SHOW_CHEVRON,
                  ID2P(LANG_TRIMPOD_SOURCE_NAVIDROME), trimpod_navidrome_menu_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_jellyfin, MENU_SHOW_CHEVRON,
                  ID2P(LANG_TRIMPOD_SOURCE_JELLYFIN), trimpod_jellyfin_menu_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_audio, MENU_SHOW_CHEVRON, ID2P(LANG_TRIMPOD_LIBRARY),
                  trimpod_audio_folders_page, NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_viz, MENU_SHOW_CHEVRON, ID2P(LANG_TRIMPOD_AUDIO_VISUALIZER),
                  trimpod_av_page, NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_power, MENU_SHOW_CHEVRON, ID2P(LANG_TRIMPOD_DEVICE),
                  trimpod_power_page, NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_controls, MENU_SHOW_CHEVRON, ID2P(LANG_TRIMPOD_CONTROLS),
                  trimpod_controls_page, NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_about, MENU_SHOW_CHEVRON, ID2P(LANG_TRIMPOD_ABOUT),
                  trimpod_about_page, NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_exit, 0, ID2P(LANG_TRIMPOD_EXIT),
                  trimpod_exit_app, NULL, Icon_NOICON);
MAKE_MENU(trimpod_settings_menu, ID2P(LANG_SETTINGS), NULL, Icon_Submenu_Entered,
          &tp_set_appearance, &sound_settings, &tp_set_source,
          &tp_set_navidrome, &tp_set_jellyfin, &tp_set_audio, &tp_set_viz,
          &tp_set_power, &tp_set_controls, &tp_set_about, &tp_set_exit);
int trimpod_settings_page(void)
{
    do_menu(&trimpod_settings_menu, NULL, NULL, false);
    return 0;
}
