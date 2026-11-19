/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
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

#ifndef __WINDOWSDL_H__
#define __WINDOWSDL_H__

#include "SDL.h"

extern SDL_Surface *sim_lcd_surface; /* LCD content */

extern SDL_mutex *window_mutex;  /* prevent concurrent drawing from event thread &
                                    main thread on MS Windows */

/* Renders GUI texture. Sets up new texture, if necessary */
void sdl_window_render(void);

/* Updates size, aspect ratio, and re-renders window content */
bool sdl_window_adjust(void);

/* Needs to be called when the window size should change */
void sdl_window_adjustment_needed(void);

/* Creates window, GL context, and LCD surface when app launches */
void sdl_window_setup(void);

extern SDL_Window *sdlWindow;       /* the app window (GL-enabled on this target) */
extern volatile bool uipod_viz_active; /* visualizer owns the window/context */
/* Bind UIPod's GLES context to the calling thread before issuing GL calls. */
void sdl_gl_make_current(void);
/* The GLES context used for both the LCD presentation and the visualizer. */
SDL_GLContext sdl_gl_get_context(void);
/* Resolve GLES 3.0 entry points via SDL2 for the Rust GPU UI engine. */
const void *sdl_gl_get_proc_address(const char *name);
/* Present the current LCD framebuffer faded toward target_rgb (1=normal, 0=target). */
void sdl_gl_present_lcd_fade(float fade);
void sdl_gl_present_lcd_fade_rgb(float fade, uint32_t target_rgb);
/* Let a video show through the window, keeping the bottom overlay_rows LCD rows. */
void sdl_gl_set_video_through(bool on, int overlay_rows);
/* Query current GL drawable dimensions and swap directly for zero-copy GPU plugins. */
void sdl_gl_get_drawable_size(int *out_w, int *out_h);
void sdl_gl_swap_window(void);
bool sdl_gl_is_gpu_active(void);
void sdl_gl_set_gpu_active(bool on);
void sdl_gl_set_gpu_repaint_fn(void (*fn)(void));

#endif /* #ifndef __WINDOWSDL_H__ */
