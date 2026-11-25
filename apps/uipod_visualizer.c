/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * UIPod: Native 4-Shade Audio-Reactive GPU Visualizer.
 *
 * Drives the pure-Rust `VizEngine` (`crates/uipod-ui/src/viz_gl.rs`) on its own
 * dedicated OS thread (`viz_render_thread`) synchronized to 60 Hz vsync while
 * tapping delay-compensated stereo PCM (`pcm_viz_read_synced` in `pcm-alsa.c`)
 * so beats land at the exact millisecond sound leaves the speaker or Bluetooth
 * headphones.  Every pixel is Bayer-dithered strictly into the 4 shades of the
 * active UIPod theme (`bg`, `fg1`, `fg2`, `fg`).
 *
 * Controls while visualizing:
 *   B / MENU     — Exit back to Now Playing (or previous screen)
 *   Left / Right — Previous / Next track (Hold to rewind / fast-forward)
 *   L2 / R2      — Previous / Next visualizer preset
 *   X / Y        — Toggle LCD pixel grain (4x4 Classic Macro-Pixel <-> 2x2 Fine)
 *   A / START    — Play / Pause music
 *   VOL+ / VOL-  — Adjust volume (SELECT + VOL+/- adjusts brightness)
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
#include "uipod_visualizer.h"

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

#include <SDL.h>
#include <SDL_thread.h>

#include "file.h"
#include "string-extra.h"
#include "action.h"
#include "audio.h"
#include "lcd.h"
#include "backlight.h"
#include "kernel.h"
#include "tick.h"
#include "button.h"
#include "settings.h"
#include "lang.h"
#include "misc.h"
#include "icon.h"
#include "status.h"
#include "system.h"
#include "metadata.h"
#include "tape_deck_fx.h"
#include "root_menu.h"
#include "gui/list.h"
#include "gui/wps.h"
#include "window-sdl.h"
#include "uipod_page.h"
#include "uipod_ui.h"
#include "uipod_rs_ui.h"
#include "uipod_track_intel.h"

/* Delay-compensated stereo PCM tap in firmware/target/hosted/sdl/pcm-alsa.c */
extern unsigned pcm_viz_read_synced(int16_t *out, unsigned max_frames,
                                    unsigned *sample_rate_out);

#define VIZ_STATE_FILE        ROCKBOX_DIR "/visualizers.txt"
#define MAX_PRESETS           32
#define ID_CAP                48
#define PCM_CHUNK             1024   /* matches 1024-point Hann FFT in viz_dsp.rs */
#define VIZ_FF_REWIND_MAX_PCT 3U
#define VIZ_MIN_FF_STEP_MS    500U

static char preset_ids[MAX_PRESETS][ID_CAP];
static bool preset_enabled[MAX_PRESETS];
static int  preset_count = 0;
static int  preset_cur   = 0;
static bool viz_ready    = false;

enum viz_fade_state { VIZ_NORMAL, VIZ_FADE_OUT, VIZ_FADE_IN };
#define VIZ_FADE_TICKS      (HZ / 2)   /* 0.50s idle/auto fade */
#define VIZ_MANUAL_TICKS    (HZ / 5)   /* 0.20s snappy manual L2/R2 fade */

static volatile enum viz_fade_state viz_fade = VIZ_NORMAL;
static long  viz_fade_ticks_cur = VIZ_FADE_TICKS;
static bool  viz_entry_faded    = false;
static long  viz_fade_start     = 0;
static volatile int  viz_pending_preset = -1; /* >= 0 explicit index, -2 random */
static long  viz_preset_tick    = 0;
static volatile bool viz_thread_stop = false;
static volatile int  viz_seek_dir    = 0;     /* -1 rewinding, 0 normal, +1 fast-forwarding */
static bool  viz_locked         = false;

static bool is_lang_uk(void)
{
    return strcmp((const char *)global_settings.lang_file, "ukrainian") == 0;
}

static char viz_state_buf[MAX_PRESETS * ID_CAP];

static void load_enabled_state(void)
{
    for (int i = 0; i < preset_count; i++)
        preset_enabled[i] = true;

    int fd = open(VIZ_STATE_FILE, O_RDONLY);
    if (fd < 0)
        return;
    int len = read(fd, viz_state_buf, sizeof(viz_state_buf) - 1);
    close(fd);
    if (len <= 0)
        return;
    viz_state_buf[len] = '\0';

    char *line = viz_state_buf, *nl;
    while (line && *line)
    {
        nl = strchr(line, '\n');
        if (nl) *nl = '\0';
        size_t L = strlen(line);
        if (L && line[L - 1] == '\r') line[--L] = '\0';
        if (*line)
        {
            for (int i = 0; i < preset_count; i++)
            {
                if (strcmp(preset_ids[i], line) == 0)
                {
                    preset_enabled[i] = false;
                    break;
                }
            }
        }
        line = nl ? nl + 1 : NULL;
    }
}

static void save_enabled_state(void)
{
    int used = 0;
    for (int i = 0; i < preset_count; i++)
    {
        if (!preset_enabled[i])
        {
            const char *nm = preset_ids[i];
            int L = (int)strlen(nm);
            if (used + L + 1 > (int)sizeof(viz_state_buf))
                break;
            memcpy(viz_state_buf + used, nm, (size_t)L);
            used += L;
            viz_state_buf[used++] = '\n';
        }
    }

    int fd = open(VIZ_STATE_FILE, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0)
        return;
    if (used > 0)
        write(fd, viz_state_buf, (size_t)used);
    close(fd);
}

static void scan_presets(void)
{
    size_t total = uipod_rs_viz_preset_count();
    if (total > MAX_PRESETS)
        total = MAX_PRESETS;
    preset_count = (int)total;
    for (int i = 0; i < preset_count; i++)
        uipod_rs_viz_preset_id((size_t)i, preset_ids[i], sizeof(preset_ids[i]));
    load_enabled_state();
}

static int enabled_count(void)
{
    int c = 0;
    for (int i = 0; i < preset_count; i++)
        if (preset_enabled[i])
            c++;
    return c;
}

static void activate_preset(int idx)
{
    if (preset_count <= 0)
        return;
    if (idx < 0 || idx >= preset_count)
        idx = 0;
    preset_cur = idx;
    uipod_rs_viz_set_preset((size_t)idx);
}

static void activate_random_preset(void)
{
    if (preset_count <= 0)
        return;
    int en = enabled_count();
    bool use_enabled = en > 0;
    int pool = use_enabled ? en : preset_count;
    int target = rand() % pool;

    int idx = 0, seen = 0;
    for (int i = 0; i < preset_count; i++)
    {
        if (!use_enabled || preset_enabled[i])
        {
            if (seen == target) { idx = i; break; }
            seen++;
        }
    }
    if (idx == preset_cur && pool > 1)
    {
        for (int j = 1; j < preset_count; j++)
        {
            int c = (idx + j) % preset_count;
            if (!use_enabled || preset_enabled[c]) { idx = c; break; }
        }
    }
    activate_preset(idx);
}

static int step_enabled_preset(int from, int delta)
{
    if (preset_count <= 0)
        return 0;
    bool use_enabled = enabled_count() > 0;
    int cur = (from >= 0 && from < preset_count) ? from : 0;
    for (int step = 1; step <= preset_count; step++)
    {
        int cand = (cur + delta * step + preset_count * 8) % preset_count;
        if (!use_enabled || preset_enabled[cand])
            return cand;
    }
    return cur;
}

static void viz_request_switch(int idx, bool fast)
{
    viz_pending_preset = idx;
    viz_fade_ticks_cur = fast ? VIZ_MANUAL_TICKS : VIZ_FADE_TICKS;
    if (viz_fade != VIZ_FADE_OUT)
    {
        viz_fade = VIZ_FADE_OUT;
        viz_fade_start = current_tick;
    }
}

static bool ensure_init(void)
{
    if (viz_ready)
        return true;

    sdl_gl_make_current();
    if (!uipod_rs_viz_init(sdl_gl_get_proc_address))
        return false;

    srand((unsigned)current_tick);
    viz_ready = true;
    return true;
}

static int viz_render_thread(void *param)
{
    (void)param;
    int16_t pcm[PCM_CHUNK * 2];
    Uint32 prev_ms = SDL_GetTicks();

    SDL_GL_MakeCurrent(sdlWindow, sdl_gl_get_context());
    SDL_GL_SetSwapInterval(1);

    while (!viz_thread_stop)
    {
        if (lcd_panel_is_dark())
        {
            SDL_Delay(100);
            prev_ms = SDL_GetTicks();
            continue;
        }

        Uint32 now_ms = SDL_GetTicks();
        float dt = (float)(now_ms - prev_ms) * 0.001f;
        prev_ms = now_ms;
        if (dt <= 0.0f || dt > 0.1f)
            dt = 1.0f / 60.0f;

        unsigned sr = 44100;
        unsigned frames = pcm_viz_read_synced(pcm, PCM_CHUNK, &sr);

        /* Wall-clock preset auto-cycling while on screen (0 = Never). */
        if (!viz_locked && global_settings.viz_transition > 0 &&
            viz_fade == VIZ_NORMAL &&
            current_tick - viz_preset_tick >= (long)global_settings.viz_transition * HZ)
        {
            viz_request_switch(-2, false);
        }

        float fade = 1.0f;
        long span = viz_fade_ticks_cur > 0 ? viz_fade_ticks_cur : 1;
        if (viz_fade == VIZ_FADE_OUT)
        {
            long el = current_tick - viz_fade_start;
            if (el >= span)
            {
                int req = viz_pending_preset;
                viz_pending_preset = -1;
                if (req >= 0)
                    activate_preset(req);
                else
                    activate_random_preset();
                viz_preset_tick = current_tick;
                viz_fade = VIZ_FADE_IN;
                viz_fade_start = current_tick;
                fade = 0.0f;
            }
            else
            {
                fade = 1.0f - (float)el / (float)span;
            }
        }
        else if (viz_fade == VIZ_FADE_IN)
        {
            long el = current_tick - viz_fade_start;
            if (el >= span)
                viz_fade = VIZ_NORMAL;
            else
                fade = (float)el / (float)span;
        }

        int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
        SDL_GL_GetDrawableSize(sdlWindow, &win_w, &win_h);

        float track_prog = 0.0f;
        float transport_speed = 0.0f;
        int st_now = audio_status();
        int sdir = viz_seek_dir;
        struct wps_state *ws = get_wps_state();
        struct mp3entry *id3 = ws ? ws->id3 : NULL;
        if ((st_now & AUDIO_STATUS_PLAY) && id3 && id3->length > 0)
        {
            long pos_ms = (long)id3->elapsed + (ws ? ws->ff_rewind_count : 0);
            long len_ms = (long)id3->length;
            if (pos_ms < 0) pos_ms = 0;
            if (pos_ms > len_ms) pos_ms = len_ms;
            track_prog = (float)pos_ms / (float)len_ms;
        }
        float base_spd = (global_settings.fx_speed > 0)
                       ? ((float)global_settings.fx_speed * 0.001f)
                       : 1.0f;
        if (sdir != 0)
            transport_speed = (sdir > 0) ? 6.5f : -6.5f;
        else if (st_now & AUDIO_STATUS_PLAY)
        {
            if (tape_deck_fx_pause_fx_enabled())
                transport_speed = base_spd * tape_deck_fx_visual_speed();
            else if (!(st_now & AUDIO_STATUS_PAUSE))
                transport_speed = base_spd;
        }

        uipod_track_intel_tick_live();

        uipod_rs_viz_render_frame(pcm,
                                  (size_t)frames,
                                  (uint32_t)sr,
                                  dt,
                                  (int32_t)win_w,
                                  (int32_t)win_h,
                                  (uint32_t)(global_settings.bg_color  & 0xFFFFFF),
                                  (uint32_t)(global_settings.fg_color  & 0xFFFFFF),
                                  (uint32_t)(global_settings.fg1_color & 0xFFFFFF),
                                  (uint32_t)(global_settings.fg2_color & 0xFFFFFF),
                                  fade,
                                  track_prog,
                                  transport_speed);

        (void)uipod_rs_ui_render_volume_popup_over_screen(
            (uint32_t)(win_w > 0 ? win_w : LCD_WIDTH),
            (uint32_t)(win_h > 0 ? win_h : LCD_HEIGHT),
            (uint32_t)(global_settings.bg_color  & 0xFFFFFF),
            (uint32_t)(global_settings.fg_color  & 0xFFFFFF),
            (uint32_t)(global_settings.fg1_color & 0xFFFFFF),
            (uint32_t)(global_settings.fg2_color & 0xFFFFFF),
            (uint64_t)sdl_get_ms());

        SDL_GL_SwapWindow(sdlWindow);
    }

    SDL_GL_MakeCurrent(sdlWindow, NULL);
    return 0;
}

static void viz_finish_seek(bool *seeking, int *seek_delta,
                            struct mp3entry **seek_id3, bool commit)
{
    if (!*seeking)
        return;

    struct wps_state *gstate = get_wps_state();
    struct mp3entry *id3 = gstate ? gstate->id3 : NULL;
    if (commit && id3 && id3 == *seek_id3)
    {
        long target = (long)id3->elapsed + *seek_delta;
        if (target < 0)
            target = 0;
        if (target > (long)id3->length)
            target = (long)id3->length;
        id3->elapsed = (unsigned long)target;
        audio_ff_rewind(id3->elapsed);
    }
    else if (id3)
    {
        audio_ff_rewind(id3->elapsed);
    }

    if (gstate)
        gstate->ff_rewind_count = 0;
    status_set_ffmode(0);
    *seeking = false;
    *seek_delta = 0;
    *seek_id3 = NULL;
    viz_seek_dir = 0;
}

static void visualizer_session(int locked_idx)
{
    viz_locked = (locked_idx >= 0);

    if (!ensure_init())
    {
        uipod_viz_active = false;
        viz_entry_faded = false;
        backlight_set_timeout(global_settings.backlight_timeout);
        lcd_update();
        return;
    }

    scan_presets();
    button_clear_queue();
    sdl_gl_make_current();
    uipod_viz_active = true;

    /* Suspend Auto Screen Off while actively visualizing (restored on exit). */
    backlight_set_timeout(0);

    if (viz_locked)
        activate_preset(locked_idx);
    else
        activate_random_preset();

    viz_fade_ticks_cur = VIZ_FADE_TICKS;
    if (viz_entry_faded)
    {
        viz_fade = VIZ_FADE_IN;
        viz_fade_start = current_tick;
        viz_entry_faded = false;
    }
    else
    {
        viz_fade = VIZ_NORMAL;
    }
    viz_pending_preset = -1;
    viz_preset_tick = current_tick;
    viz_seek_dir = 0;

    /* Hand the GLES context to `viz_render_thread` on its own OS core while the
     * main Rockbox thread services audio decoding, network prefetch, and input. */
    SDL_GL_MakeCurrent(sdlWindow, NULL);
    viz_thread_stop = false;
    SDL_Thread *rt = SDL_CreateThread(viz_render_thread, "uipod_viz", NULL);
    if (rt)
    {
        bool seeking = false;
        unsigned int seek_step = 0;
        int seek_delta = 0;
        struct mp3entry *seek_id3 = NULL;
        const long ff_rw_accel = global_settings.ff_rewind_accel + 3;

        for (;;)
        {
            int act = get_action(CONTEXT_WPS, HZ / 20);
            if (power_display_off())
            {
                viz_finish_seek(&seeking, &seek_delta, &seek_id3, true);
                break;
            }

            if (act & SYS_EVENT)
            {
                long handled = default_event_handler(act);
                if (handled == SYS_USB_CONNECTED ||
                    handled == SYS_POWEROFF ||
                    handled == SYS_REBOOT)
                {
                    viz_finish_seek(&seeking, &seek_delta, &seek_id3, false);
                    break;
                }
                continue;
            }

            if (seeking)
            {
                if (act == ACTION_WPS_STOPSEEK ||
                    (act == ACTION_NONE &&
                     (button_status() & (BUTTON_LEFT | BUTTON_RIGHT)) == 0))
                {
                    viz_finish_seek(&seeking, &seek_delta, &seek_id3, true);
                    continue;
                }
                if (act != ACTION_NONE &&
                    act != ACTION_WPS_SEEKBACK &&
                    act != ACTION_WPS_SEEKFWD)
                {
                    viz_finish_seek(&seeking, &seek_delta, &seek_id3, true);
                }
            }

            if (act == ACTION_WPS_BROWSE ||
                act == ACTION_STD_CANCEL ||
                act == ACTION_STD_CONTEXT)
            {
                break;
            }
            else if (act == ACTION_WPS_SEEKBACK || act == ACTION_WPS_SEEKFWD)
            {
                int dir = (act == ACTION_WPS_SEEKFWD) ? 1 : -1;
                struct wps_state *gstate = get_wps_state();
                struct mp3entry *id3 = gstate ? gstate->id3 : NULL;

                if (seeking && id3 != seek_id3)
                    viz_finish_seek(&seeking, &seek_delta, &seek_id3, false);

                if ((audio_status() & AUDIO_STATUS_PLAY) && id3 && id3->length > 0)
                {
                    if (!seeking)
                    {
                        audio_pre_ff_rewind();
                        status_set_ffmode(dir > 0 ? STATUS_FASTFORWARD
                                                  : STATUS_FASTBACKWARD);
                        seeking = true;
                        seek_id3 = id3;
                        unsigned int min_s = global_settings.ff_rewind_min_step > 0
                                           ? (unsigned int)global_settings.ff_rewind_min_step
                                           : 1U;
                        seek_step = 1000U * min_s;
                        seek_delta = (int)seek_step * dir;
                        viz_seek_dir = dir;
                    }
                    else
                    {
                        long cur_pos = (long)id3->elapsed + seek_delta;
                        if (cur_pos < 0) cur_pos = 0;
                        if (cur_pos > (long)id3->length) cur_pos = (long)id3->length;

                        unsigned long span_ms = (dir > 0)
                            ? (id3->length - (unsigned long)cur_pos)
                            : (unsigned long)cur_pos;
                        unsigned int max_step = (unsigned int)(
                            (span_ms * VIZ_FF_REWIND_MAX_PCT) / 100UL);
                        if (max_step < VIZ_MIN_FF_STEP_MS)
                            max_step = VIZ_MIN_FF_STEP_MS;
                        if (seek_step > max_step)
                            seek_step = max_step;

                        seek_delta += (int)seek_step * dir;
                        seek_step += seek_step >> ff_rw_accel;
                        viz_seek_dir = dir;
                    }

                    if ((long)id3->elapsed + seek_delta > (long)id3->length)
                        seek_delta = (int)(id3->length - id3->elapsed);
                    else if ((long)id3->elapsed + seek_delta < 0)
                        seek_delta = -(int)id3->elapsed;

                    gstate->ff_rewind_count = seek_delta;
                }
            }
            else if (act == ACTION_WPS_SKIPPREV)
            {
                if (audio_status() & AUDIO_STATUS_PLAY)
                    audio_prev_or_restart();
            }
            else if (act == ACTION_WPS_SKIPNEXT)
            {
                if (audio_status() & AUDIO_STATUS_PLAY)
                    audio_next();
            }
            else if (act == ACTION_TP_CHAP_PREV)
            {
                int target = step_enabled_preset(preset_cur, -1);
                viz_request_switch(target, true);
            }
            else if (act == ACTION_TP_CHAP_NEXT)
            {
                int target = step_enabled_preset(preset_cur, 1);
                viz_request_switch(target, true);
            }
            else if (act == ACTION_WPS_HOTKEY)
            {
                (void)uipod_rs_viz_toggle_grain();
            }
            else if (act == ACTION_WPS_PLAY)
            {
                int st = audio_status();
                if (st & AUDIO_STATUS_PLAY)
                {
                    if (st & AUDIO_STATUS_PAUSE)
                        audio_resume();
                    else
                        audio_pause();
                }
            }
        }
        viz_finish_seek(&seeking, &seek_delta, &seek_id3, true);
        viz_thread_stop = true;
        SDL_WaitThread(rt, NULL);
    }
    sdl_gl_make_current();

    uipod_viz_active = false;
    if (power_display_off())
        backlight_set_timeout_quiet(global_settings.backlight_timeout);
    else
        backlight_set_timeout(global_settings.backlight_timeout);
    button_clear_queue();
    button_touch_activity();

    lcd_update();
}

void uipod_visualizer_run(void)
{
    visualizer_session(-1);
}

bool uipod_visualizer_fade_to_black(void)
{
    uint32_t bg_rgb = (uint32_t)(global_settings.bg_color & 0xFFFFFF);
    bool was_gpu = uipod_gpu_is_active();
    backlight_set_timeout(0);
    sdl_gl_make_current();
    SDL_GL_SetSwapInterval(1);
    uipod_viz_active = true;

    long start = current_tick;
    for (;;)
    {
        long dt = current_tick - start;
        float fade = (dt >= VIZ_FADE_TICKS)
                         ? 0.0f
                         : 1.0f - (float)dt / (float)VIZ_FADE_TICKS;
        if (was_gpu && uipod_rs_ui_is_ready())
        {
            int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
            sdl_gl_make_current();
            sdl_gl_get_drawable_size(&win_w, &win_h);
            if (uipod_rs_ui_present_fade_rgb_gpu((uint32_t)win_w,
                                                 (uint32_t)win_h,
                                                 fade,
                                                 bg_rgb))
            {
                SDL_GL_SwapWindow(sdlWindow);
            }
        }
        else
        {
            sdl_gl_present_lcd_fade_rgb(fade, bg_rgb);
        }

        int act = get_action(CONTEXT_STD, TIMEOUT_NOBLOCK);
        bool blanked = power_display_off();

        if (act & SYS_EVENT)
            act = default_event_handler(act) ? ACTION_STD_CANCEL : ACTION_NONE;

        if (blanked || (act != ACTION_NONE && act != ACTION_REDRAW))
        {
            if (blanked)
                backlight_set_timeout_quiet(global_settings.backlight_timeout);
            else
                backlight_set_timeout(global_settings.backlight_timeout);
            if (was_gpu && uipod_rs_ui_is_ready())
            {
                int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
                sdl_gl_make_current();
                sdl_gl_get_drawable_size(&win_w, &win_h);
                if (uipod_rs_ui_present_fade_rgb_gpu((uint32_t)win_w,
                                                     (uint32_t)win_h,
                                                     1.0f,
                                                     bg_rgb))
                {
                    SDL_GL_SwapWindow(sdlWindow);
                }
            }
            else
            {
                sdl_gl_present_lcd_fade_rgb(1.0f, bg_rgb);
            }
            uipod_viz_active = false;
            return true;
        }
        if (dt >= VIZ_FADE_TICKS)
            break;
        yield();
    }
    viz_entry_faded = true;
    return false;
}

bool uipod_visualizer_autostart_due(void)
{
    int status = audio_status();
    return global_settings.viz_start_delay > 0
        && !power_display_off()
        && is_backlight_on(true)
        && (status & AUDIO_STATUS_PLAY)
        && !(status & AUDIO_STATUS_PAUSE)
        && TIME_AFTER(current_tick, button_last_activity_tick()
                      + global_settings.viz_start_delay * HZ);
}

bool uipod_visualizer_maybe_autostart(void)
{
    if (!uipod_visualizer_autostart_due())
        return false;
    if (!uipod_visualizer_fade_to_black())
        uipod_visualizer_run();
    return true;
}

/* ---- Settings -> Visualizers: localized [x]/[ ] toggle list --------------- */

struct viz_menu
{
    struct uipod_page base;
    struct gui_synclist lists;
};

static const char *vm_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    (void)data;
    if (sel < 0 || sel >= preset_count)
        return "";
    uipod_rs_viz_preset_name((size_t)sel, is_lang_uk(), buf, buf_len);
    return buf;
}

static const char *vm_get_indicator(int sel, void *data)
{
    (void)data;
    if (sel < 0 || sel >= preset_count)
        return "";
    return uipod_toggle_str(preset_enabled[sel]);
}

static void vm_preview(int sel)
{
    if (sel < 0 || sel >= preset_count)
        return;
    visualizer_session(sel);
}

static void vm_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct viz_menu *)self)->lists);
}

static int vm_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct viz_menu *)self)->lists, &action);
    return action;
}

static enum uipod_page_result vm_on_action(struct uipod_page *self, int action)
{
    struct viz_menu *p = (struct viz_menu *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);
    bool have = (preset_count > 0 && sel >= 0 && sel < preset_count);

    switch (action)
    {
        case ACTION_STD_OK:
            if (have)
            {
                preset_enabled[sel] = !preset_enabled[sel];
                save_enabled_state();
                gui_synclist_draw(&p->lists);
            }
            return UIPOD_PAGE_STAY;

        case ACTION_STD_CONTEXT:
            if (have)
            {
                static const char *const opts[] = { ID2P(LANG_UIPOD_PREVIEW) };
                char title[64];
                if (uipod_context_menu(vm_get_name(sel, NULL, title,
                                                   sizeof(title)), opts, 1) == 0)
                    vm_preview(sel);
                gui_synclist_draw(&p->lists);
            }
            return UIPOD_PAGE_STAY;

        case ACTION_STD_CANCEL:
            return UIPOD_PAGE_DONE;
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable vm_vtable =
{
    .legend    = NULL,
    .draw      = vm_draw,
    .poll      = vm_poll,
    .on_action = vm_on_action,
};

int uipod_visualizer_menu(void)
{
    struct viz_menu p =
    {
        .base = { .vt = &vm_vtable, .context = CONTEXT_LIST },
    };

    scan_presets();

    gui_synclist_init(&p.lists, vm_get_name, NULL, false, 1, NULL);
    gui_synclist_set_indicator_callback(&p.lists, vm_get_indicator);
    gui_synclist_set_title(&p.lists, str(LANG_UIPOD_VISUALIZERS), Icon_NOICON);
    gui_synclist_set_nb_items(&p.lists, preset_count);
    gui_synclist_select_item(&p.lists, 0);

    uipod_page_run(&p.base);
    return GO_TO_PREVIOUS;
}
