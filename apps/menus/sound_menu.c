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

/* Effects submenu:
 *   1. Enable Effects (master switch synced with Now Playing context menu & FX badge)
 *   2. Speed (Ableton Texture Warp)
 *   3. Tape Mode submenu
 *   4. Reverb submenu
 *   5. Delay submenu
 *   6. Chorus submenu
 *   7. Flanger submenu */
MENUITEM_SETTING(fx_enabled, &global_settings.fx_enabled, NULL);
MENUITEM_SETTING(fx_speed,   &global_settings.fx_speed,   NULL);

MENUITEM_SETTING(tape_mode,     &global_settings.tape_mode,     NULL);
MENUITEM_SETTING(tape_wow,      &global_settings.tape_wow,      NULL);
MENUITEM_SETTING(tape_hiss,     &global_settings.tape_hiss,     NULL);
MENUITEM_SETTING(tape_crinkle,  &global_settings.tape_crinkle,  NULL);
MENUITEM_SETTING(tape_sat,      &global_settings.tape_sat,      NULL);
MENUITEM_SETTING(tape_tone,     &global_settings.tape_tone,     NULL);
MENUITEM_SETTING(tape_pause_fx, &global_settings.tape_pause_fx, NULL);
MENUITEM_SETTING(tape_ff_sound, &global_settings.tape_ff_sound, NULL);

MAKE_MENU(tape_mode_menu, ID2P(LANG_UIPOD_TAPE_MODE), NULL, Icon_NOICON,
          &tape_mode,
          &tape_wow,
          &tape_hiss,
          &tape_crinkle,
          &tape_sat,
          &tape_tone,
          &tape_pause_fx,
          &tape_ff_sound);

MENUITEM_SETTING(fx_reverb,          &global_settings.fx_reverb,          NULL);
MENUITEM_SETTING(fx_reverb_size,     &global_settings.fx_reverb_size,     NULL);
MENUITEM_SETTING(fx_reverb_damp,     &global_settings.fx_reverb_damp,     NULL);
MENUITEM_SETTING(fx_reverb_predelay, &global_settings.fx_reverb_predelay, NULL);
MENUITEM_SETTING(fx_reverb_width,    &global_settings.fx_reverb_width,    NULL);
MENUITEM_SETTING(fx_reverb_mix,      &global_settings.fx_reverb_mix,      NULL);

MAKE_MENU(reverb_menu, ID2P(LANG_UIPOD_REVERB), NULL, Icon_NOICON,
          &fx_reverb,
          &fx_reverb_size,
          &fx_reverb_damp,
          &fx_reverb_predelay,
          &fx_reverb_width,
          &fx_reverb_mix);

MENUITEM_SETTING(fx_delay,          &global_settings.fx_delay,          NULL);
MENUITEM_SETTING(fx_delay_time,     &global_settings.fx_delay_time,     NULL);
MENUITEM_SETTING(fx_delay_feedback, &global_settings.fx_delay_feedback, NULL);
MENUITEM_SETTING(fx_delay_tone,     &global_settings.fx_delay_tone,     NULL);
MENUITEM_SETTING(fx_delay_pingpong, &global_settings.fx_delay_pingpong, NULL);
MENUITEM_SETTING(fx_delay_mix,      &global_settings.fx_delay_mix,      NULL);

MAKE_MENU(delay_menu, ID2P(LANG_UIPOD_DELAY), NULL, Icon_NOICON,
          &fx_delay,
          &fx_delay_time,
          &fx_delay_feedback,
          &fx_delay_tone,
          &fx_delay_pingpong,
          &fx_delay_mix);

MENUITEM_SETTING(fx_chorus,       &global_settings.fx_chorus,       NULL);
MENUITEM_SETTING(fx_chorus_rate,  &global_settings.fx_chorus_rate,  NULL);
MENUITEM_SETTING(fx_chorus_depth, &global_settings.fx_chorus_depth, NULL);
MENUITEM_SETTING(fx_chorus_mix,   &global_settings.fx_chorus_mix,   NULL);

MAKE_MENU(chorus_menu, ID2P(LANG_UIPOD_CHORUS), NULL, Icon_NOICON,
          &fx_chorus,
          &fx_chorus_rate,
          &fx_chorus_depth,
          &fx_chorus_mix);

MENUITEM_SETTING(fx_flanger,          &global_settings.fx_flanger,          NULL);
MENUITEM_SETTING(fx_flanger_rate,     &global_settings.fx_flanger_rate,     NULL);
MENUITEM_SETTING(fx_flanger_depth,    &global_settings.fx_flanger_depth,    NULL);
MENUITEM_SETTING(fx_flanger_feedback, &global_settings.fx_flanger_feedback, NULL);
MENUITEM_SETTING(fx_flanger_mix,      &global_settings.fx_flanger_mix,      NULL);

MAKE_MENU(flanger_menu, ID2P(LANG_UIPOD_FLANGER), NULL, Icon_NOICON,
          &fx_flanger,
          &fx_flanger_rate,
          &fx_flanger_depth,
          &fx_flanger_feedback,
          &fx_flanger_mix);

MAKE_MENU(sound_effects_menu, ID2P(LANG_UIPOD_EFFECTS), lowlatency_callback,
          Icon_NOICON,
          &fx_enabled,
          &fx_speed,
          &tape_mode_menu,
          &reverb_menu,
          &delay_menu,
          &chorus_menu,
          &flanger_menu);

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

/* Crossfade & Auto-DJ submenu */
MENUITEM_SETTING(dj_mode,    &global_settings.dj_crossfade_mode,     NULL);
MENUITEM_SETTING(dj_len,     &global_settings.dj_transition_len,     NULL);
MENUITEM_SETTING(dj_stretch, &global_settings.dj_max_stretch,        NULL);
MENUITEM_SETTING(dj_gapless, &global_settings.dj_keep_album_gapless, NULL);
MENUITEM_SETTING(dj_qcut,    &global_settings.dj_quick_cut_next,     NULL);

MAKE_MENU(dj_crossfade_menu, ID2P(LANG_UIPOD_DJ_CROSSFADE), NULL, Icon_NOICON,
          &dj_mode,
          &dj_len,
          &dj_stretch,
          &dj_gapless,
          &dj_qcut);

MAKE_MENU(sound_settings, ID2P(LANG_UIPOD_SOUND), NULL, Icon_Audio,
          &equalizer_menu,
          &sound_effects_menu,
          &dj_crossfade_menu,
          &replaygain_settings_menu,
          &balance,
          &channel_config,
          &crossfeed,
          &menu_click);
