/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * UIPod: audio spectrum (FFT-style bar graph) drawn on the Now Playing
 * screen in place of the volume bar. See apps/uipod_spectrum.c.
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
#ifndef UIPOD_SPECTRUM_H
#define UIPOD_SPECTRUM_H

struct screen;
struct viewport;

/* Draw a frequency-band bar graph for the currently playing audio, filling the
 * given viewport (assumed to be the current viewport on `display`). */
void uipod_spectrum_draw(struct screen *display, struct viewport *vp);

#endif /* UIPOD_SPECTRUM_H */
