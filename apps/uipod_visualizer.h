/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * UIPod: Native 4-Shade Audio-Reactive GPU Visualizer, started from the Now
 * Playing menu or auto-started after idle while music plays (any screen).
 * See apps/uipod_visualizer.c and crates/uipod-ui/src/viz_gl.rs.
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
#ifndef UIPOD_VISUALIZER_H
#define UIPOD_VISUALIZER_H

#include <stdbool.h>

/* Run the fullscreen 4-shade visualizer (auto-cycling enabled presets) until
 * the user presses Back (B).  Blocks the calling UI thread; audio plays on. */
void uipod_visualizer_run(void);

/* Fade the current screen into the active theme's background shade, then arm
 * a fade-in so the visualizer blooms smoothly out of `theme.bg`.  Returns true
 * if a keypress cancelled the fade (caller should NOT launch the visualizer). */
bool uipod_visualizer_fade_to_black(void);

/* True when the idle auto-start timer has expired: a delay is configured, the
 * display is on, and music is actively playing (not paused). */
bool uipod_visualizer_autostart_due(void);

/* Idle auto-start for a UI loop's timeout path: when due, fade to `theme.bg`
 * and run the visualizer.  Returns true if the screen needs a repaint. */
bool uipod_visualizer_maybe_autostart(void);

/* Settings -> Visualizers: flat on/off toggle list of built-in 4-shade presets.
 * A toggles the highlighted preset; Hold A / MENU opens Preview; B leaves. */
int uipod_visualizer_menu(void);

#endif /* UIPOD_VISUALIZER_H */
