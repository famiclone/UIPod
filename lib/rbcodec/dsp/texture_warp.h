/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * UIPod: Ableton Texture-style multi-grain time-stretching DSP stage.
 *
 * This program is free software; you can redistribute it and/or
 * modify it under the terms of the GNU General Public License
 * as published by the Free Software Foundation; either version 2
 * of the License, or (at your option) any later version.
 *
 ****************************************************************************/
#ifndef TEXTURE_WARP_H
#define TEXTURE_WARP_H

#include "dsp_core.h"

/* Set playback speed in permille (1000 = 1.00x bypass, 62 = 1/16x ambient .. 1500 = 1.50x). */
void dsp_set_texture_warp_speed(int permille);
int  dsp_get_texture_warp_speed(void);

void dsp_texture_warp_init(struct dsp_config *dsp,
                           unsigned int dsp_id) INIT_ATTR;

#endif /* TEXTURE_WARP_H */
