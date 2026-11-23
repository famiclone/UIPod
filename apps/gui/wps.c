/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * Copyright (C) 2002 Jerome Kuptz
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
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include "config.h"

#include "system.h"
#include "file.h"
#include "lcd.h"
#include "font.h"
#include "backlight.h"
#include "action.h"
#include "kernel.h"
#include "filetypes.h"
#include "settings.h"
#include "skin_engine/skin_engine.h"
#include "audio.h"
#include "usb.h"
#include "status.h"
#include "storage.h"
#include "screens.h"
#include "playlist.h"
#include "icons.h"
#include "lang.h"
#include "misc.h"
#include "sound.h"
#include "playback.h"
#include "splash.h"
#include "cuesheet.h"
#include "ata_idle_notify.h"
#include "root_menu.h"
#include "uipod_transition.h"   /* slide back to the browser on B */
#include "menu.h"                 /* do_menu / MENUITEM_VALUE (A: Now Playing menu) */
#include "powermgmt.h"            /* sleep timer (Now Playing knob) */
#include "uipod_playlists.h"    /* Add to Playlist, Shuffle */
#include "uipod_page.h"          /* Now Playing is a uipod_page */
#include "backdrop.h"
#include "appevents.h"
#include "viewport.h"
#include "pcmbuf.h"
#include "option_select.h"
#include "playlist_viewer.h"
#include "wps.h"
#include "uipod_visualizer.h"
#include "uipod_nowplaying.h"
#include "uipod_midi.h"
#include "uipod_nsf_ui.h"
#include "uipod_rs_codecs.h"
#include "uipod_rs_ui.h"
#include "uipod_ui.h"
#include "statusbar-skinned.h"
#include "skin_engine/wps_internals.h"


#define FF_REWIND_MAX_PERCENT 3 /* cap ff/rewind step size at max % of file */
                                /* 3% of 30min file == 54s step size */
#define MIN_FF_REWIND_STEP 500

static struct wps_state wps_state;

/* initial setup of wps_data  */
static void wps_state_init(void);
static void track_info_callback(unsigned short id, void *param);

#define WPS_DEFAULTCFG WPS_DIR "/rockbox_default.wps"
#define DEFAULT_WPS(screen) (WPS_DEFAULTCFG)

char* wps_default_skin(enum screen_type screen)
{
    static char *skin_buf[NB_SCREENS] = {
#if LCD_DEPTH > 1
            "%X(d)\n"
#endif
            "%s%?it<%?in<%in. |>%it|%fn>\n"
            "%s%?ia<%ia|%?d(2)<%d(2)|%(root%)>>\n"
            "%s%?id<%id|%?d(1)<%d(1)|%(root%)>> %?iy<%(%iy%)|>\n\n"
            "%al%pc/%pt%ar[%pp:%pe]\n"
            "%fbkBit %?fv<avg|> %?iv<%(id3v%iv%)|%(no id3%)>\n"
            "%pb\n%pm\n",
        };
    return skin_buf[screen];
}

static bool s_wps_theme_disabled = false;

static void update_non_static(void)
{
    if (s_wps_theme_disabled)
        uipod_nowplaying_draw();
}

void wps_do_action(enum wps_do_action_type action, bool updatewps)
{
    if (action == WPS_PLAYPAUSE) /* toggle the status */
    {
        struct wps_state *state = get_wps_state();
        if (state->paused)
            action = WPS_PLAY;

        state->paused = !state->paused;
    }

    if (action == WPS_PLAY) /* unpause_action */
    {
        audio_resume();
    }
    else /* WPS_PAUSE pause_action */
    {
        audio_pause();

        if (global_settings.pause_rewind) {
            unsigned long elapsed = audio_current_track()->elapsed;
            long newpos = elapsed - (global_settings.pause_rewind * 1000);

            audio_pre_ff_rewind();
            audio_ff_rewind(newpos > 0 ? newpos : 0);
        }
        if (action == WPS_PLAYPAUSE)
        {
            settings_save();
            call_storage_idle_notifys(true);   /* make sure resume info is saved */
        }
    }

    /* Bugfix only do a skin refresh if in one of the below screens */
    enum current_activity act = get_current_activity();

    bool refresh = (act == ACTIVITY_FM ||
                    act == ACTIVITY_WPS ||
                    act == ACTIVITY_RECORDING);

    if (updatewps && refresh)
        update_non_static();
}

static bool ffwd_rew(int button, bool seek_from_end)
{
    unsigned int step = 0;     /* current ff/rewind step */
    unsigned int max_step = 0; /* maximum ff/rewind step */
    int ff_rewind_count = 0;   /* current ff/rewind count (in ticks) */
    int direction = -1;         /* forward=1 or backward=-1 */
    bool exit = false;
    bool usb = false;
    bool ff_rewind = false;
    const long ff_rw_accel = (global_settings.ff_rewind_accel + 3);
    struct wps_state *gstate = get_wps_state();
    struct mp3entry *old_id3 = gstate->id3;

    if (button == ACTION_NONE)
    {
        status_set_ffmode(0);
        return usb;
    }
    while (!exit)
    {
        struct mp3entry *id3 = gstate->id3;
        if (id3 != old_id3)
        {
            ff_rewind = false;
            ff_rewind_count = 0;
            old_id3 = id3;
        }
        if (id3 && seek_from_end)
            id3->elapsed = id3->length;

        switch ( button )
        {
            case ACTION_WPS_SEEKFWD:
            case ACTION_WPS_SEEKBACK:
                direction = (button == ACTION_WPS_SEEKFWD) ? 1 : -1;
                if (ff_rewind)
                {
                    if (direction == 1)
                    {
                        /* fast forwarding, calc max step relative to end */
                        max_step = (id3->length -
                                    (id3->elapsed +
                                     ff_rewind_count)) *
                                     FF_REWIND_MAX_PERCENT / 100;
                    }
                    else
                    {
                        /* rewinding, calc max step relative to start */
                        max_step = (id3->elapsed + ff_rewind_count) *
                                   FF_REWIND_MAX_PERCENT / 100;
                    }

                    max_step = MAX(max_step, MIN_FF_REWIND_STEP);

                    if (step > max_step)
                        step = max_step;

                    ff_rewind_count += step * direction;

                    /* smooth seeking by multiplying step by: 1 + (2 ^ -accel) */
                    step += step >> ff_rw_accel;
                }
                else
                {
                    if ((audio_status() & AUDIO_STATUS_PLAY) && id3 && id3->length )
                    {
                        audio_pre_ff_rewind();
                        if (direction > 0)
                            status_set_ffmode(STATUS_FASTFORWARD);
                        else
                            status_set_ffmode(STATUS_FASTBACKWARD);

                        ff_rewind = true;

                        step = 1000 * global_settings.ff_rewind_min_step;
                        ff_rewind_count += step * direction;
                    }
                    else
                    {
                        exit = true;
                        break;
                    }
                }

                if (direction > 0) {
                    if ((id3->elapsed + ff_rewind_count) > id3->length)
                        ff_rewind_count = id3->length - id3->elapsed;
                }
                else {
                    if ((int)(id3->elapsed + ff_rewind_count) < 0)
                        ff_rewind_count = -id3->elapsed;
                }

                /* set the wps state ff_rewind_count so the progess info
                   displays corectly */
                gstate->ff_rewind_count = ff_rewind_count;

                uipod_nowplaying_draw();

                break;

            case ACTION_WPS_STOPSEEK:
                if (ff_rewind && id3)
                {
                    id3->elapsed = id3->elapsed + ff_rewind_count;
                    audio_ff_rewind(id3->elapsed);
                }
                gstate->ff_rewind_count = 0;
                ff_rewind = false;
                status_set_ffmode(0);
                uipod_nowplaying_draw();
                exit = true;
                break;

            default:
            {
                int handled = default_event_handler(button);
                if (handled == SYS_USB_CONNECTED ||
                    handled == SYS_POWEROFF || handled == SYS_REBOOT)
                {
                    if (ff_rewind)
                        audio_ff_rewind(id3 ? id3->elapsed : 0);
                    gstate->ff_rewind_count = 0;
                    status_set_ffmode(0);
                    usb = (handled == SYS_USB_CONNECTED);
                    exit = true;
                }
                break;
            }
        }
        if (!exit)
        {
            button = get_action(CONTEXT_WPS|ALLOW_SOFTLOCK, HZ);
            if (button != ACTION_WPS_SEEKFWD
                && button != ACTION_WPS_SEEKBACK
                && !IS_SYSEVENT(button))
                button = ACTION_WPS_STOPSEEK;
        }
    }
    return usb;
}

static void gwps_caption_backlight(struct wps_state *state)
{
    if (state->id3)
    {
        if (global_settings.caption_backlight)
        {
            /* turn on backlight n seconds before track ends, and turn it off n
               seconds into the new track. n == backlight_timeout, or 5s */
            int n = global_settings.backlight_timeout * 1000;

            if ( n < 1000 )
                n = 5000; /* use 5s if backlight is always on or off */

            if (((state->id3->elapsed < 1000) ||
                 ((state->id3->length - state->id3->elapsed) < (unsigned)n)) &&
                (state->paused == false))
                backlight_on();
        }
    }
}

/* UIPod: chapters (cuesheet subtracks) never remap the D-pad -- prev/next
 * stay whole-file skips; chapter navigation is L2/R2 (ACTION_TP_CHAP_*). */
static void next_track(void)
{
    audio_next();
}

static void play_hop(int direction)
{
    struct wps_state *state = get_wps_state();
    long step = global_settings.skip_length*1000;
    long elapsed = state->id3->elapsed;
    long remaining = state->id3->length - elapsed;

    if (step < 0)
    {
        if (direction < 0)
        {
            audio_prev_or_restart();
            return;
        }
        else if (remaining < DEFAULT_SKIP_THRESH*2)
        {
            next_track();
            return;
        }
        else
            elapsed += (remaining - DEFAULT_SKIP_THRESH*2);
    }
    else if (!global_settings.prevent_skip &&
           (!step ||
            (direction > 0 && step >= remaining) ||
            (direction < 0 && elapsed < DEFAULT_SKIP_THRESH)))
    {   /* Do normal track skipping */
        if (direction > 0)
            next_track();
        else if (direction < 0)
        {
            if (step > 0 && global_settings.rewind_across_tracks && elapsed < DEFAULT_SKIP_THRESH && playlist_check(-1))
            {
                bool audio_paused = (audio_status() & AUDIO_STATUS_PAUSE)?true:false;
                if (!audio_paused)
                    audio_pause();
                audio_prev();
                audio_ff_rewind(-step);
                if (!audio_paused)
                    audio_resume();
                return;
            }

            audio_prev_or_restart();
        }
        return;
    }
    else if (direction == 1 && step >= remaining)
    {
        system_sound_play(SOUND_TRACK_NO_MORE);
        return;
    }
    else if (direction == -1 && elapsed < step)
    {
        elapsed = 0;
    }
    else
    {
        elapsed += step * direction;
    }
    audio_pre_ff_rewind();
    audio_ff_rewind(elapsed);
}


static void gwps_leave_wps(void)
{
    if (s_wps_theme_disabled)
    {
        s_wps_theme_disabled = false;
        lcd_set_update_suppressed(true);
        FOR_NB_SCREENS(i)
        {
            screens[i].scroll_stop();
            viewportmanager_theme_undo(i, false);
        }
        lcd_set_update_suppressed(false);
    }

    /* unhandle statusbar update delay */
    sb_skin_set_update_delay(DEFAULT_UPDATE_DELAY);
}

/*
 * display the wps on entering or restoring */
static void gwps_enter_wps(void)
{
    if (!s_wps_theme_disabled)
    {
        FOR_NB_SCREENS(i)
        {
            screens[i].scroll_stop();
            sb_set_title_text(NULL, Icon_NOICON, i);
            viewportmanager_theme_enable(i, false, NULL);
        }
        s_wps_theme_disabled = true;
    }
    uipod_nowplaying_draw();
}

static long do_wps_exit(void)
{
    audio_pause();
    update_non_static();
    audio_stop();

    gwps_leave_wps();
    /* GO_TO_PREVIOUS, like the ACTION_WPS_BROWSE exit: _BROWSER resolves a
     * stale last_browser (default 0 = filetree) and lands in the file browser. */
    return GO_TO_PREVIOUS;
}

/* The WPS can be left in two ways:
 *      a)  call a function, which draws over the wps. In this case, the wps
 *          will be still active (i.e. the below function didn't return)
 *      b)  return with a value evaluated by root_menu.c, in this case the wps
 *          is really left, and root_menu will handle the next screen
 *
 * In either way, call gwps_leave_wps(), in order to restore the correct
 * "main screen" backdrops and statusbars
 */
/* ---- Now Playing as a uipod_page ------------------------------------- *
 * The While Playing Screen runs on the shared uipod_page_run() loop
 * (poll -> on_action -> draw), with its state in struct wps_page, so it is a
 * page like every other UIPod screen.  It drives the stock skin-engine
 * renderer (refresh classes, peak-meter fast path, scroll thread). */
struct wps_page
{
    struct uipod_page base;
    long button;          /* last (party/ab-processed) action, for storage_spin */
    long last_left, last_right;
    long result;          /* GO_TO_* handed back to the root dispatch */
    bool restore;         /* (re-)enter the WPS theme on the next draw */
    bool exit;            /* leave via do_wps_exit() */
    bool update;          /* force a non-static skin update on the next draw */
};

static void wps_page_draw(struct uipod_page *p)
{
    struct wps_page *w = (struct wps_page *)p;
    struct wps_state *state = get_wps_state();

    bool audio_paused = (audio_status() & AUDIO_STATUS_PAUSE)?true:false;
    /* did someone else (i.e power thread) change audio pause mode? */
    if (state->paused != audio_paused) {
        state->paused = audio_paused;

        /* if another thread paused audio, we are probably in car mode,
           about to shut down. lets save the settings. */
        if (state->paused) {
            settings_save();
            call_storage_idle_notifys(true);
        }
    }

    if (w->restore)
    {
        w->restore = false;
        sb_skin_set_update_delay(0);
        w->update = true;
        gwps_enter_wps();
    }
    else
    {
        gwps_caption_backlight(state);
        uipod_nowplaying_draw();
        w->update = false;
    }
}

static int wps_page_poll(struct uipod_page *p, int timeout)
{
    struct wps_page *w = (struct wps_page *)p;
    (void)timeout;

    if (w->button && !IS_SYSEVENT(w->button) )
        storage_spin();

    struct uipod_rs_dj_telemetry dj_tel;
    bool dj_live = (uipod_rs_dj_get_telemetry(&dj_tel) != 0);
    int poll_tmo = lcd_panel_is_dark()
        ? HZ
        : ((dj_live || uipod_rs_nsf_get_active_chip_flags() >= 0) ? (HZ / 15) : (HZ / 5));
    long button = get_action(CONTEXT_WPS|ALLOW_SOFTLOCK, poll_tmo);

    /* Exit if audio has stopped playing. This happens e.g. at end of
       playlist or if using the sleep timer. */
    if (!(audio_status() & AUDIO_STATUS_PLAY))
        w->exit = true;

    w->button = button;
    return button;
}

/* Hold-A on Now Playing opens this menu (do_menu returns NP_PAUSE / NP_SHUFFLE
 * / NP_ADD_TO_PLAYLIST / NP_START_VIZ; GO_TO_PREVIOUS on cancel).  Sleep Timer
 * is an inline knob: LEFT/RIGHT (or A) cycle Off/5/10.../90 min and arm the
 * timer live; the value shows minutes left.  Opened like the context menu
 * (gwps_leave_wps -> menu -> restore) so it themes. */
enum { NP_PAUSE = 0, NP_SHUFFLE, NP_ADD_TO_PLAYLIST, NP_START_VIZ, NP_SOUNDFONT };

static const int np_sleep_min[] = { 0, 5, 10, 15, 30, 45, 60, 75, 90 };
#define NP_SLEEP_N ((int)(sizeof np_sleep_min / sizeof np_sleep_min[0]))

/* Stateless: the current preset is the one nearest the live time remaining. */
static int np_sleep_cur_index(void)
{
    int sec = get_sleep_timer();
    if (sec <= 0)
        return 0;                       /* Off */
    int m = (sec + 59) / 60, best = 1, bestd = 1000000;
    for (int i = 1; i < NP_SLEEP_N; i++)
    {
        int d = np_sleep_min[i] - m;
        if (d < 0) d = -d;
        if (d < bestd) { bestd = d; best = i; }
    }
    return best;
}

static const char *np_sleep_get(void *ctx, char *buf, int len)
{
    (void)ctx;
    int sec = get_sleep_timer();
    if (sec <= 0)
        return str(LANG_OFF);
    snprintf(buf, len, "%dm", (sec + 59) / 60);   /* minutes left, round up; 'm' = our selector standard */
    return buf;
}

static void np_sleep_cycle(void *ctx, int dir)       /* resets + arms live */
{
    (void)ctx;
    int idx = np_sleep_cur_index() + (dir < 0 ? -1 : 1);
    if (idx < 0) idx = 0;
    if (idx >= NP_SLEEP_N) idx = NP_SLEEP_N - 1;
    set_sleeptimer_duration(np_sleep_min[idx]);      /* 0 = Off/disarm */

    char toast_msg[96];
    if (np_sleep_min[idx] <= 0)
        snprintf(toast_msg, sizeof(toast_msg), "%s: %s",
                 str(LANG_SLEEP_TIMER), str(LANG_OFF));
    else
        snprintf(toast_msg, sizeof(toast_msg), "%s: %dm",
                 str(LANG_SLEEP_TIMER), np_sleep_min[idx]);
    uipod_hud_toast_ex("sleep_timer", toast_msg, 1800);
}

static const struct menu_value_cb np_sleep_value =
    { np_sleep_get, np_sleep_cycle, NULL };

/* Pause/Play: label-only value row. A (or LEFT/RIGHT) toggles playback in place
 * and the row relabels; the menu stays open until B. */
static const char *np_pause_get(void *ctx, char *buf, int len)
{
    (void)ctx; (void)buf; (void)len;
    return (audio_status() & AUDIO_STATUS_PAUSE) ? str(LANG_PLAY) : str(LANG_PAUSE);
}
static void np_pause_cycle(void *ctx, int dir)
{
    (void)ctx; (void)dir;
    get_wps_state()->paused = (audio_status() & AUDIO_STATUS_PAUSE) != 0;
    wps_do_action(WPS_PLAYPAUSE, false);
}
static const struct menu_value_cb np_pause_value =
    { np_pause_get, np_pause_cycle, NULL };

static const char *np_large_art_get(void *ctx, char *buf, int len)
{
    (void)ctx; (void)buf; (void)len;
    return uipod_toggle_str(uipod_nowplaying_is_alt());
}
static void np_large_art_cycle(void *ctx, int dir)
{
    (void)ctx; (void)dir;
    uipod_nowplaying_toggle_alt();
}
static const struct menu_value_cb np_large_art_value =
    { np_large_art_get, np_large_art_cycle, NULL };

static const char *np_effects_get(void *ctx, char *buf, int len)
{
    (void)ctx; (void)buf; (void)len;
    return uipod_toggle_str(global_settings.fx_enabled);
}
static void np_effects_cycle(void *ctx, int dir)
{
    (void)ctx; (void)dir;
    global_settings.fx_enabled = !global_settings.fx_enabled;
    uipod_tape_fx_apply();
    settings_save();
}
static const struct menu_value_cb np_effects_value =
    { np_effects_get, np_effects_cycle, NULL };

static int np_soundfont_cb(int action, const struct menu_item_ex *this_item,
                           struct gui_synclist *this_list)
{
    (void)this_item;
    (void)this_list;
    if (action == ACTION_REQUEST_MENUITEM)
    {
        struct wps_state *st = get_wps_state();
        bool uses_sf2 = st && st->id3 &&
                        (st->id3->codectype == AFMT_MIDI ||
                         uipod_midi_is_midi_path(st->id3->path));
        if (!uses_sf2)
            return ACTION_EXIT_MENUITEM;
    }
    return action;
}

static int np_nsf_cb(int action, const struct menu_item_ex *this_item,
                     struct gui_synclist *this_list)
{
    (void)this_item;
    (void)this_list;
    if (action == ACTION_REQUEST_MENUITEM)
    {
        struct wps_state *st = get_wps_state();
        bool is_nsf = (uipod_rs_nsf_get_active_chip_flags() >= 0) ||
                      (st && st->id3 && st->id3->codectype == AFMT_NSF);
        if (!is_nsf)
            return ACTION_EXIT_MENUITEM;
    }
    return action;
}

static char *np_nsf_endless_label(int selected_item,
                                  void *data,
                                  char *buffer,
                                  size_t buffer_len)
{
    (void)selected_item;
    (void)data;
    bool uk = strcmp((const char *)global_settings.lang_file, "ukrainian") == 0;
    snprintf(buffer, buffer_len, "%s", uk ? "Нескінченний цикл" : "Endless Loop");
    return buffer;
}

static const char *np_nsf_endless_get(void *ctx, char *buf, int len)
{
    (void)ctx; (void)buf; (void)len;
    return uipod_toggle_str(uipod_rs_nsf_get_endless_loop() != 0);
}

static void np_nsf_endless_cycle(void *ctx, int dir)
{
    (void)ctx; (void)dir;
    int cur = uipod_rs_nsf_get_endless_loop();
    uipod_rs_nsf_set_endless_loop(cur == 0 ? 1 : 0);
}

static const struct menu_value_cb np_nsf_endless_value =
    { np_nsf_endless_get, np_nsf_endless_cycle, NULL };

MENUITEM_VALUE(np_pause, "", &np_pause_value, Icon_NOICON);
MENUITEM_FUNCTION_DYNTEXT(np_nsf_channels, MENU_SHOW_CHEVRON,
                          uipod_nsf_channels_menu_fn,
                          uipod_nsf_channels_menu_label, NULL, NULL,
                          np_nsf_cb, Icon_NOICON);
MENUITEM_VALUE_DYNTEXT_CB(np_nsf_endless, &np_nsf_endless_value,
                          np_nsf_endless_label, np_nsf_cb, Icon_NOICON);
MENUITEM_VALUE(np_effects, ID2P(LANG_UIPOD_EFFECTS), &np_effects_value, Icon_NOICON);
MENUITEM_RETURNVALUE_CHEVRON(np_soundfont, ID2P(LANG_UIPOD_SOUNDFONT), NP_SOUNDFONT,
                             np_soundfont_cb, Icon_NOICON);
MENUITEM_VALUE(np_sleeptimer, ID2P(LANG_SLEEP_TIMER), &np_sleep_value, Icon_NOICON);
MENUITEM_VALUE(np_large_art, ID2P(LANG_UIPOD_LARGE_ART), &np_large_art_value, Icon_NOICON);
/* Shuffle: an action, not a mode -- every pick reshuffles the live queue. */
MENUITEM_RETURNVALUE(np_shuffle, ID2P(LANG_SHUFFLE), NP_SHUFFLE,
                     NULL, Icon_NOICON);
MENUITEM_RETURNVALUE(np_addpl, ID2P(LANG_UIPOD_ADD_TO_PLAYLIST), NP_ADD_TO_PLAYLIST,
                     NULL, Icon_NOICON);
MENUITEM_RETURNVALUE(np_viz, ID2P(LANG_UIPOD_START_VISUALIZER), NP_START_VIZ, NULL, Icon_NOICON);
/* Repeat: an inline setting row (LEFT/RIGHT cycle, applied live to the
 * running playlist via its settings_list callback); auto-labelled. */
MENUITEM_SETTING(np_repeat, &global_settings.repeat_mode, NULL);

MAKE_MENU(nowplaying_menu, ID2P(LANG_NOW_PLAYING), NULL, Icon_NOICON,
          &np_pause, &np_nsf_channels, &np_nsf_endless, &np_effects, &np_soundfont,
          &np_shuffle, &np_repeat, &np_sleeptimer, &np_large_art, &np_addpl, &np_viz);

static enum uipod_page_result wps_page_on_action(struct uipod_page *p,
                                                   int button)
{
    struct wps_page *w = (struct wps_page *)p;
    struct wps_state *state = get_wps_state();

        switch(button)
        {
            case ACTION_WPS_BROWSE:
                gwps_leave_wps();
                /* B backs out of Now Playing -> slide the previous screen in L->R.
                 * GO_TO_PREVIOUS (not _BROWSER): the root dispatch resolves the
                 * real previous screen via last_screen. */
                uipod_transition_arm_back();
                w->result = GO_TO_PREVIOUS;
                return UIPOD_PAGE_DONE;

                /* A tap: play/pause (the tap/hold standard: tap executes).
                 * Sync paused first in case another thread changed it. */
            case ACTION_WPS_PLAY:
                state->paused = (audio_status() & AUDIO_STATUS_PAUSE) != 0;
                wps_do_action(WPS_PLAYPAUSE, true);
                break;

                /* Hold A: the Now Playing menu (pause/shuffle/repeat/sleep/
                 * add-to-playlist/visualizer): leave the skin so the menu
                 * themes + tweens, act on the choice, then restore the WPS. */
            case ACTION_STD_CONTEXT:
            {
                gwps_leave_wps();
                int sel = do_menu(&nowplaying_menu, NULL, NULL, false);
                if (sel == NP_SHUFFLE)
                {
                    uipod_queue_reshuffle();
                    uipod_transition_arm_back();   /* slide back, as B does */
                }
                else if (sel == NP_SOUNDFONT)
                    uipod_midi_pick_soundfont();
                else if (sel == NP_ADD_TO_PLAYLIST &&
                         state->id3 && state->id3->path[0])
                    uipod_playlists_pick(state->id3->path, FILE_ATTR_AUDIO);
                else if (sel == NP_START_VIZ)
                    uipod_visualizer_run();   /* B returns to Now Playing */

                if (!audio_status())   /* music stopped while away -> leave WPS */
                    { w->result = GO_TO_ROOT; return UIPOD_PAGE_DONE; }
                w->restore = true;
                break;
            }

            case ACTION_WPS_VOLUP: /* fall through */
            case ACTION_WPS_VOLDOWN:
                if (button == ACTION_WPS_VOLUP)
                    adjust_volume(1);
                else
                    adjust_volume(-1);

                setvol();
                uipod_nowplaying_draw();
                w->update = false;
                break;
            /* Hold D-pad Left / Right: smooth accelerating rewind / fast-forward. */
            case ACTION_WPS_SEEKFWD:
            case ACTION_WPS_SEEKBACK:
                if (ffwd_rew(button, false))
                {
                    w->result = GO_TO_ROOT;
                    return UIPOD_PAGE_DONE;
                }
                w->update = false;
                break;

                /* prev / restart */
            case ACTION_WPS_SKIPPREV:
                w->last_left = current_tick;
                play_hop(-1);
                break;

                /* next */
            case ACTION_WPS_SKIPNEXT:
                w->last_right = current_tick;
                play_hop(1);
                break;

            /* L2 / R2: previous / next chapter for cuesheet/m4b audiobooks,
             * or +/-15s in-track seek for standard audio & .nsf#idx subtracks. */
            case ACTION_TP_CHAP_PREV:
            case ACTION_TP_CHAP_NEXT:
                if ((audio_status() & AUDIO_STATUS_PLAY) && state->id3)
                {
                    if (state->id3->cuesheet)
                    {
                        curr_cuesheet_skip(state->id3->cuesheet,
                                           button == ACTION_TP_CHAP_NEXT ? 1 : -1,
                                           state->id3->elapsed);
                    }
                    else
                    {
                        long delta  = (button == ACTION_TP_CHAP_NEXT) ? 15000L : -15000L;
                        long target = (long)state->id3->elapsed + delta;
                        long max_ms = (long)state->id3->length;
                        if (target < 0)
                            target = 0;
                        if (max_ms > 0 && target > max_ms)
                            target = max_ms;
                        state->id3->elapsed = (unsigned long)target;
                        audio_pre_ff_rewind();
                        audio_ff_rewind(target);
                    }
                    w->update = true;
                }
                break;
            case ACTION_WPS_HOTKEY:
                uipod_nowplaying_toggle_alt();
                uipod_nowplaying_draw();
                w->update = false;
                break;

            /* menu key functions */
            case ACTION_WPS_MENU:
                gwps_leave_wps();
                w->result = GO_TO_ROOT;
                return UIPOD_PAGE_DONE;



                /* screen settings */

                /* stop and exit wps */
            case ACTION_WPS_STOP:
                w->exit = true;
                break;

             /* this case is used by the softlock feature
              * it requests a full update here */
            case ACTION_REDRAW:
                skin_request_full_update(WPS);
                skin_request_full_update(CUSTOM_STATUSBAR); /* if SBS is used */
                break;
            case ACTION_NONE: /* Timeout, do a partial update */
                /* Auto-start the visualizer after the configured idle while
                 * music plays (0 = Never).  Runs the sequence inline rather
                 * than via uipod_visualizer_maybe_autostart() because the
                 * skin must be left/restored around the run. */
                if (uipod_visualizer_autostart_due())
                {
                    /* Fade Now Playing to black first (hides the viz load).  A
                     * keypress during the fade cancels -> stay here. */
                    if (uipod_visualizer_fade_to_black())
                    {
                        w->update = true;   /* repaint Now Playing */
                        break;
                    }
                    gwps_leave_wps();
                    uipod_visualizer_run();   /* loads while black, fades in; B returns */
                    if (!audio_status())
                        { w->result = GO_TO_ROOT; return UIPOD_PAGE_DONE; }
                    w->restore = true;
                    break;
                }
                w->update = true;
                ffwd_rew(button, false); /* hopefully fix the ffw/rwd bug */
                break;
            case ACTION_WPS_VIEW_PLAYLIST:
                gwps_leave_wps();
                w->result = GO_TO_PLAYLIST_VIEWER;
                return UIPOD_PAGE_DONE;
            default:                /* system events: the page loop's */
                w->update = true;
                break;
        }

    /* ACTION_WPS_STOP, or audio stopped on its own: leave via do_wps_exit so
       the pause/stop bookkeeping runs exactly as it always did. */
    if (w->exit)
    {
        w->result = do_wps_exit();
        return UIPOD_PAGE_DONE;
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable wps_page_vtable =
{
    .legend    = NULL,            /* the skin engine paints the SBS header */
    .draw      = wps_page_draw,
    .poll      = wps_page_poll,
    .on_action = wps_page_on_action,
};

long gui_wps_show(void)
{
/* NOTE: if USBAudio ever gets its own DSP channel, this block can go away! */
    struct wps_page w =
    {
        .base = {
            .vt                = &wps_page_vtable,
            .context           = CONTEXT_WPS,
            /* Slide in like every other page (only the very first screen at app
             * launch skips it).  The slide can flicker or show missing data
             * while a track loads -- the destination frame is captured before
             * id3 is ready; left visible rather than hidden by snapping. */
            .no_enter_anim     = uipod_transition_first_screen(),
            .no_arm_back       = true,  /* B arms its own back slide to browser */
            .no_header_refresh = true,  /* the skin engine owns the SBS header */
            .animated          = true,  /* counters/EQ/spectrum/viz repaint live */
        },
        .result        = GO_TO_ROOT,
        .restore       = true,
    };

    wps_state_init();

    uipod_page_run(&w.base);
    if (w.base.home)
    {
        /* Hold-B home: the run loop breaks out before on_action can leave the
         * skin.  w.restore doubles as "the skin is currently left" -- leave
         * only when still entered. */
        if (!w.restore)
            gwps_leave_wps();
        return GO_TO_ROOT;
    }
    return w.result;
}

struct wps_state *get_wps_state(void)
{
    return &wps_state;
}

/* this is called from the playback thread so NO DRAWING! */
static void track_info_callback(unsigned short id, void *param)
{
    struct wps_state *state = get_wps_state();

    if (id == PLAYBACK_EVENT_TRACK_CHANGE || id == PLAYBACK_EVENT_CUR_TRACK_READY)
    {
        state->id3 = ((struct track_event *)param)->id3;
        if (state->id3->cuesheet)
        {
            cue_find_current_track(state->id3->cuesheet, state->id3->elapsed);
        }
    }
#ifdef AUDIO_FAST_SKIP_PREVIEW
    else if (id == PLAYBACK_EVENT_TRACK_SKIP)
    {
        state->id3 = audio_current_track();
    }
#endif
    if (id == PLAYBACK_EVENT_NEXTTRACKID3_AVAILABLE)
        state->nid3 = audio_next_track();
    skin_request_full_update(WPS);
}

static void wps_state_init(void)
{
    struct wps_state *state = get_wps_state();
    state->paused = false;
    if(audio_status() & AUDIO_STATUS_PLAY)
    {
        state->id3 = audio_current_track();
        state->nid3 = audio_next_track();
    }
    else
    {
        state->id3 = NULL;
        state->nid3 = NULL;
    }
    /* We'll be updating due to restore initialized with true */
    skin_request_full_update(WPS);
    /* add the WPS track event callbacks */
    add_event(PLAYBACK_EVENT_TRACK_CHANGE, track_info_callback);
    add_event(PLAYBACK_EVENT_NEXTTRACKID3_AVAILABLE, track_info_callback);
    /* Use the same callback as ..._TRACK_CHANGE for when remaining handles have
       finished */
    add_event(PLAYBACK_EVENT_CUR_TRACK_READY, track_info_callback);
#ifdef AUDIO_FAST_SKIP_PREVIEW
    add_event(PLAYBACK_EVENT_TRACK_SKIP, track_info_callback);
#endif
}
