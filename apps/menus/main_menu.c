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
#include "uipod_mainmenu.h"
#include "tree.h"
#include "storage.h"
#include "yesno.h"
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
#include "uipod_folders.h"
#include "uipod_library.h"
#include "uipod_navidrome.h"
#include "uipod_jellyfin.h"
#include "uipod_lastfm.h"
#include "uipod_downloads.h"
#include "uipod_plugins.h"
#include "uipod_page.h"
#include "uipod_transition.h"
#include "uipod_ui.h"

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


/* About: opens `about.txt` manual & credits in Text Viewer (B exits). */
static int uipod_about_page(void)
{
    uipod_about();
    return 0;
}

static int uipod_exit_app(void)
{
    uipod_lastfm_shutdown();
    uipod_downloads_shutdown();
    sys_poweroff();
    return 0;
}

/* ---- Accounts (`Settings -> Accounts -> Last.fm`) --------------------- */

MENUITEM_FUNCTION(tp_acc_lastfm, MENU_SHOW_CHEVRON,
                  ID2P(LANG_UIPOD_LASTFM), uipod_lastfm_menu_page,
                  NULL, Icon_NOICON);
MAKE_MENU(uipod_accounts_menu, ID2P(LANG_UIPOD_ACCOUNTS), NULL,
          Icon_Submenu_Entered,
          &tp_acc_lastfm);

static int uipod_accounts_page(void)
{
    do_menu(&uipod_accounts_menu, NULL, NULL, false);
    return 0;
}

/* ---- Sources (#646): Offline Mode + Navidrome / Jellyfin ------------ */

MENUITEM_VALUE(tp_set_offline, ID2P(LANG_UIPOD_OFFLINE_MODE),
               &uipod_offline_mode_value_cb, Icon_NOICON);

MENUITEM_FUNCTION(tp_src_navidrome, MENU_SHOW_CHEVRON,
                  ID2P(LANG_UIPOD_SOURCE_NAVIDROME), uipod_navidrome_menu_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_src_jellyfin, MENU_SHOW_CHEVRON,
                  ID2P(LANG_UIPOD_SOURCE_JELLYFIN), uipod_jellyfin_menu_page,
                  NULL, Icon_NOICON);
MAKE_MENU(uipod_sources_menu, ID2P(LANG_UIPOD_SOURCES), NULL,
          Icon_Submenu_Entered,
          &tp_set_offline, &tp_src_navidrome, &tp_src_jellyfin);

int uipod_sources_page(void)
{
    do_menu(&uipod_sources_menu, NULL, NULL, false);
    return 0;
}

/* ---- Extras (opened from Main -> Extras) ------------------------------ */

MENUITEM_FUNCTION(tp_ext_plugins, MENU_SHOW_CHEVRON,
                  ID2P(LANG_UIPOD_PLUGINS), uipod_plugins_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_ext_downloads, MENU_SHOW_CHEVRON,
                  ID2P(LANG_UIPOD_DOWNLOADS), uipod_downloads_page,
                  NULL, Icon_NOICON);
MAKE_MENU(uipod_extras_menu, ID2P(LANG_UIPOD_EXTRAS), NULL,
          Icon_Submenu_Entered,
          &tp_ext_plugins, &tp_ext_downloads);

int uipod_extras_page(void)
{
    return do_menu(&uipod_extras_menu, NULL, NULL, false);
}

/* ---- Settings (#864) -------------------------------------------------- */

MENUITEM_FUNCTION(tp_set_appearance, MENU_SHOW_CHEVRON,
                  ID2P(LANG_UIPOD_APPEARANCE), uipod_appearance_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_sources, MENU_SHOW_CHEVRON,
                  ID2P(LANG_UIPOD_SOURCES), uipod_sources_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_accounts, MENU_SHOW_CHEVRON,
                  ID2P(LANG_UIPOD_ACCOUNTS), uipod_accounts_page,
                  NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_audio, MENU_SHOW_CHEVRON, ID2P(LANG_UIPOD_LIBRARY),
                  uipod_audio_folders_page, NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_viz, MENU_SHOW_CHEVRON, ID2P(LANG_UIPOD_AUDIO_VISUALIZER),
                  uipod_av_page, NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_power, MENU_SHOW_CHEVRON, ID2P(LANG_UIPOD_DEVICE),
                  uipod_power_page, NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_about, MENU_SHOW_CHEVRON, ID2P(LANG_UIPOD_ABOUT),
                  uipod_about_page, NULL, Icon_NOICON);
MENUITEM_FUNCTION(tp_set_exit, 0, ID2P(LANG_UIPOD_EXIT),
                  uipod_exit_app, NULL, Icon_NOICON);
MAKE_MENU(uipod_settings_menu, ID2P(LANG_SETTINGS), NULL, Icon_Submenu_Entered,
          &tp_set_appearance, &sound_settings, &tp_set_offline,
          &tp_set_sources, &tp_set_accounts,
          &tp_set_audio, &tp_set_viz,
          &tp_set_power, &tp_set_about, &tp_set_exit);
int uipod_settings_page(void)
{
    do_menu(&uipod_settings_menu, NULL, NULL, false);
    return 0;
}
