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
#include "config.h"
#include "sound.h"
#include "lang.h"
#include "action.h"
#include "settings.h"
#include "menu.h"
#include "eq_menu.h"
#include "exported_menus.h"
#include "menu_common.h"
#include "misc.h"

/***********************************/
/*    SOUND MENU                   */

MENUITEM_SETTING(balance, &global_settings.balance, NULL);
MENUITEM_SETTING(channel_config, &global_settings.channel_config,
                 lowlatency_callback);
MENUITEM_SETTING(crossfeed, &global_settings.crossfeed, lowlatency_callback);
MENUITEM_SETTING(menu_click, &global_settings.menu_click, NULL);

/* ReplayGain submenu */
static int replaygain_callback(int action,
                               const struct menu_item_ex *this_item,
                               struct gui_synclist *this_list)
{
    (void)this_item;
    (void)this_list;
    if (action == ACTION_EXIT_MENUITEM)
        replaygain_update();
    return action;
}

MENUITEM_SETTING(replaygain_noclip,
                 &global_settings.replaygain_settings.noclip,
                 replaygain_callback);
MENUITEM_SETTING(replaygain_type,
                 &global_settings.replaygain_settings.type,
                 replaygain_callback);
MENUITEM_SETTING(replaygain_preamp,
                 &global_settings.replaygain_settings.preamp,
                 replaygain_callback);
MAKE_MENU(replaygain_settings_menu, ID2P(LANG_REPLAYGAIN), 0, Icon_NOICON,
          &replaygain_type, &replaygain_noclip, &replaygain_preamp);

MAKE_MENU(sound_settings, ID2P(LANG_UIPOD_SOUND), NULL, Icon_Audio,
          &equalizer_menu,
          &replaygain_settings_menu,
          &balance,
          &channel_config,
          &crossfeed,
          &menu_click);
