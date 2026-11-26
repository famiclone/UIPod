/***************************************************************************
 *             __________               __   ___
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * Copyright (C) 2017 Marcin Bukat
 * Copyright (C) 2019 by Roman Stolyarov
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
#ifdef __ANDROID__
#include <jni.h>
#include <SDL.h>
#include <stdbool.h>
#include "config.h"
#include "lcd.h"
#include "backlight-target.h"

static void android_set_window_brightness(int step)
{
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    if (!env)
        return;
    jclass cls = (*env)->FindClass(env, "dev/famiclone/uipod/UIPodActivity");
    if (!cls)
    {
        (*env)->ExceptionClear(env);
        return;
    }
    jmethodID mid = (*env)->GetStaticMethodID(env, cls, "setAppBrightness", "(I)V");
    if (mid)
        (*env)->CallStaticVoidMethod(env, cls, mid, (jint)step);
    else
        (*env)->ExceptionClear(env);
    (*env)->DeleteLocalRef(env, cls);
}

static int s_saved_step = DEFAULT_BRIGHTNESS_SETTING;

bool backlight_hw_init(void)
{
    lcd_set_panel_dark(false);
    return true;
}

void backlight_hw_on(void)
{
    lcd_set_panel_dark(false);
    android_set_window_brightness(s_saved_step);
}

void backlight_hw_off(void)
{
    lcd_set_panel_dark(true);
    android_set_window_brightness(0);
}

void backlight_hw_brightness(int brightness)
{
    if (brightness > MAX_BRIGHTNESS_SETTING)
        brightness = MAX_BRIGHTNESS_SETTING;
    if (brightness < MIN_BRIGHTNESS_SETTING)
        brightness = MIN_BRIGHTNESS_SETTING;
    s_saved_step = brightness;
    android_set_window_brightness(brightness);
}

void backlight_hw_complete_wake(void)
{
}

#else /* !__ANDROID__ */

#include <sys/types.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <stdbool.h>
#include <unistd.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <SDL.h>
#include <SDL_thread.h>
#include "config.h"
#include "lcd.h"                /* lcd_set_panel_dark: stop presenting while dark */
#include "backlight-target.h"
#include "system-sdl.h"
#include "window-sdl.h"
#include "sysfs.h"
#include "panic.h"

typedef enum {
  BL_TYPE1 = 0,
  BL_TYPE2 = 1,
} backlight_t;

static char* get_bl_env(const char *env_key)
{
    char *p = getenv(env_key);
    return (p && *p) ? p : NULL;
}

static void set_type1_brightness(int value) {
  sysfs_set_string(get_bl_env("SYSFS_BL_COMMAND"), get_bl_env("BL_COMMAND"));
  sysfs_set_string(get_bl_env("SYSFS_BL_NAME"), get_bl_env("BL_NAME"));
  sysfs_set_int(get_bl_env("SYSFS_BL_PARAM"), value);
  sysfs_set_int(get_bl_env("SYSFS_BL_START"), 1);
}

/* LCD_ON/OFF_COMMAND (set per device in its .sys) power the panel itself:
 * off stops the 60Hz scanout of a dark screen, on re-inits it (~300ms). */
static void set_type1_lcd(const char *cmd_env) {
  char *cmd = get_bl_env(cmd_env);
  if (!cmd)
    return;
  sysfs_set_string(get_bl_env("SYSFS_BL_COMMAND"), cmd);
  sysfs_set_string(get_bl_env("SYSFS_BL_NAME"), get_bl_env("BL_NAME"));
  sysfs_set_int(get_bl_env("SYSFS_BL_START"), 1);
}

static void set_type2_brightness(int value) {
  sysfs_set_int(get_bl_env("SYSFS_BL_BRIGHTNESS"), value);
}

static int get_type2_bl_step(void) {
  int max = 0;

  if (!sysfs_get_int(get_bl_env("SYSFS_BL_MAX"), &max)) {
    return 0;
  }

  return max / 17;
}

static backlight_t get_type(void)
{
    char *bl_type = getenv("BL_TYPE");
    if (!bl_type) return BL_TYPE1;

    if (strcmp(bl_type, "TYPE1") == 0) return BL_TYPE1;
    if (strcmp(bl_type, "TYPE2") == 0) return BL_TYPE2;

    return BL_TYPE1;
}

/* Offload dispdbg sysfs writes (especially LCD_ON/OFF_COMMAND, which stalls
 * ~300ms inside sunxi disp2 MIPI-DSI init/sleep) onto a dedicated OS thread so
 * cooperative Rockbox fibers (codec_thread / low-latency NSF & MIDI) never
 * freeze while the panel powers down or up. */
static SDL_mutex *s_bl_mtx = NULL;
static SDL_cond  *s_bl_cond = NULL;
static bool s_worker_ready = false;

static bool s_want_on = true;
static int  s_want_step = DEFAULT_BRIGHTNESS_SETTING;
static bool s_present_done = true;

static void hw_apply_brightness_sync(backlight_t type, int step)
{
    switch (type) {
        case BL_TYPE1: set_type1_brightness(step * 15); break;
        case BL_TYPE2: set_type2_brightness(step * get_type2_bl_step()); break;
    }
}

static void hw_apply_off_sync(backlight_t type)
{
    switch (type) {
        case BL_TYPE1: set_type1_brightness(0); break;
        case BL_TYPE2: sysfs_set_int(get_bl_env("SYSFS_BL_POWER"), 1); break;
    }
    if (type == BL_TYPE1)
        set_type1_lcd("LCD_OFF_COMMAND");
}

static int bl_worker_fn(void *arg)
{
    (void)arg;
    bool hw_on = true;
    int  hw_step = DEFAULT_BRIGHTNESS_SETTING;

    for (;;)
    {
        SDL_LockMutex(s_bl_mtx);
        while (!((!s_want_on && hw_on) ||
                 (s_want_on && !hw_on) ||
                 (s_want_on && hw_on && s_want_step != hw_step)))
        {
            SDL_CondWait(s_bl_cond, s_bl_mtx);
        }

        if (!s_want_on && hw_on)
        {
            SDL_UnlockMutex(s_bl_mtx);
            hw_apply_off_sync(get_type());
            hw_on = false;
            hw_step = -1;
            continue;
        }

        if (s_want_on && !hw_on)
        {
            s_present_done = false;
            SDL_UnlockMutex(s_bl_mtx);

            /* 1. Re-enable panel scanout (~300ms in kernel) while LED is still 0
             *    and lcd_panel_is_dark() is still true (no dead-vsync swap). */
            backlight_t type = get_type();
            if (type == BL_TYPE1)
                set_type1_lcd("LCD_ON_COMMAND");
            hw_on = true;

            SDL_LockMutex(s_bl_mtx);
            if (!s_want_on)
            {
                SDL_UnlockMutex(s_bl_mtx);
                continue;
            }
            SDL_UnlockMutex(s_bl_mtx);

            /* 2. TCON now produces 60Hz vsync; ask backlight_thread on the
             *    Rockbox/GL thread to flip lcd_set_panel_dark(false) and
             *    present the up-to-date frame BEFORE lighting the LED. */
            sim_enter_irq_handler();
            backlight_notify_hw_woke();
            sim_exit_irq_handler();

            SDL_LockMutex(s_bl_mtx);
            Uint32 deadline = SDL_GetTicks() + 50;
            while (s_want_on && !s_present_done)
            {
                Uint32 now = SDL_GetTicks();
                if ((Sint32)(deadline - now) <= 0)
                    break;
                SDL_CondWaitTimeout(s_bl_cond, s_bl_mtx, deadline - now);
            }
            if (!s_want_on)
            {
                SDL_UnlockMutex(s_bl_mtx);
                continue;
            }
            int step = s_want_step;
            SDL_UnlockMutex(s_bl_mtx);

            if (type == BL_TYPE2)
                sysfs_set_int(get_bl_env("SYSFS_BL_POWER"), 0);
            hw_apply_brightness_sync(type, step);
            hw_step = step;
            continue;
        }

        /* Already on: apply updated brightness step */
        int step = s_want_step;
        SDL_UnlockMutex(s_bl_mtx);
        hw_apply_brightness_sync(get_type(), step);
        hw_step = step;
    }
    return 0;
}

bool backlight_hw_init(void)
{
    backlight_t type = get_type();
    if (type == BL_TYPE1)
        set_type1_lcd("LCD_ON_COMMAND");
    lcd_set_panel_dark(false);
    if (type == BL_TYPE2)
        sysfs_set_int(get_bl_env("SYSFS_BL_POWER"), 0);
    hw_apply_brightness_sync(type, DEFAULT_BRIGHTNESS_SETTING);

    s_want_on = true;
    s_want_step = DEFAULT_BRIGHTNESS_SETTING;
    s_present_done = true;

    s_bl_mtx = SDL_CreateMutex();
    s_bl_cond = SDL_CreateCond();
    if (s_bl_mtx && s_bl_cond &&
        SDL_CreateThread(bl_worker_fn, "uipod-bl", NULL) != NULL)
    {
        s_worker_ready = true;
    }
    return true;
}

static int last_bl = 1;

void backlight_hw_complete_wake(void)
{
    if (!s_worker_ready)
        return;

    bool should_present = false;
    SDL_LockMutex(s_bl_mtx);
    if (s_want_on)
        should_present = true;
    SDL_UnlockMutex(s_bl_mtx);

    if (should_present)
        lcd_set_panel_dark(false);

    SDL_LockMutex(s_bl_mtx);
    s_present_done = true;
    SDL_CondSignal(s_bl_cond);
    SDL_UnlockMutex(s_bl_mtx);
}

void backlight_hw_on(void)
{
    if (sdl_window_is_headless())
        return;
    if (!s_worker_ready)
    {
        if (last_bl != 1)
        {
            backlight_t type = get_type();
            if (type == BL_TYPE1)
                set_type1_lcd("LCD_ON_COMMAND");
            lcd_set_panel_dark(false);
            switch (type) {
                case BL_TYPE1: set_type1_brightness(15); break;
                case BL_TYPE2: sysfs_set_int(get_bl_env("SYSFS_BL_POWER"), 0); break;
            }
            last_bl = 1;
        }
        return;
    }

    bool already_awake = false;
    SDL_LockMutex(s_bl_mtx);
    if (!s_want_on)
        s_want_step = MIN_BRIGHTNESS_SETTING;
    else if (s_present_done)
        already_awake = true;
    s_want_on = true;
    SDL_CondSignal(s_bl_cond);
    SDL_UnlockMutex(s_bl_mtx);

    if (already_awake)
        lcd_set_panel_dark(false);
}

void backlight_hw_off(void)
{
    if (sdl_window_is_headless())
        return;
    /* Mark dark immediately on the Rockbox thread so SDL/GL stops swapping
     * before the worker disables the panel's TCON scanout. */
    lcd_set_panel_dark(true);

    if (!s_worker_ready)
    {
        if (last_bl != 0)
        {
            hw_apply_off_sync(get_type());
            last_bl = 0;
        }
        return;
    }

    SDL_LockMutex(s_bl_mtx);
    s_want_on = false;
    SDL_CondSignal(s_bl_cond);
    SDL_UnlockMutex(s_bl_mtx);
}

void backlight_hw_brightness(int brightness)
{
    if (sdl_window_is_headless())
        return;
    /* cap range, just in case */
    if (brightness > MAX_BRIGHTNESS_SETTING)
        brightness = MAX_BRIGHTNESS_SETTING;
    if (brightness < MIN_BRIGHTNESS_SETTING)
        brightness = MIN_BRIGHTNESS_SETTING;

    if (!s_worker_ready)
    {
        hw_apply_brightness_sync(get_type(), brightness);
        return;
    }

    SDL_LockMutex(s_bl_mtx);
    s_want_step = brightness;
    SDL_CondSignal(s_bl_cond);
    SDL_UnlockMutex(s_bl_mtx);
}

#endif /* !__ANDROID__ */
