/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * Copyright (C) Markus Braun (2002)
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

#include "scrollbar.h"
#include "config.h"
#include <stdint.h>
#include "limits.h"
#include "bmp.h"

/* calculates the start and size of the knob */
static void scrollbar_helper(int min_shown, int max_shown, int items,
                             int inner_len, int *size, int *start)
{
    int min, max, range;

    /* min should be min */
    if(min_shown < max_shown) {
        min = min_shown;
        max = max_shown;
    }
    else {
        min = max_shown;
        max = min_shown;
    }

    /* limit min and max */
    if(min < 0)
        min = 0;
    if(min > items)
        min = items;

    if(max < 0)
        max = 0;
    if(max > items)
        max = items;

    range = max - min;

    /* avoid overflows */
    while (items > (INT_MAX / inner_len)) {
        items >>= 1;
        range >>= 1;
    }

    /* calc start and end of the knob */
    if (items > 0 && items > range) {
        *size = inner_len * range / items;
        int min_knob = (inner_len >= 6) ? 3 : 1;
        if (*size < min_knob) { /* width of knob is too thin */
            *size = min_knob;
            *start = (inner_len - min_knob) * min / items;
        } else {
            *start = (inner_len - *size) * min / (items - range);
        }
    } else {  /* if null draw full bar */
        *size = inner_len;
        *start = 0;
    }

    return;
}

void gui_scrollbar_draw(struct screen * screen, int x, int y,
                        int width, int height, int items,
                        int min_shown, int max_shown,
                        unsigned flags)
{
    int inner_x, inner_y, inner_wd, inner_ht, inner_len;
    int start, size;
#ifdef HAVE_LCD_COLOR
    int infill;
#endif

    if (flags & INVERTFILL)
    {
        min_shown = items - max_shown;
        max_shown = items;
    }

    const int bw = (width >= 12 && height >= 12) ? 3 : 1;

    if (flags & BORDER_NOFILL)
    {
        inner_x  = x;
        inner_y  = y;
        inner_wd = width;
        inner_ht = height;
    }
    else
    {
        inner_x  = x + bw;
        inner_y  = y + bw;
        inner_wd = width  - 2 * bw;
        inner_ht = height - 2 * bw;
    }
    /* Boundary check to make sure that height is reasonable, otherwise nothing
     *  to do 
     */
    if ((inner_wd | inner_ht) < 0)
        return;

    if (flags & HORIZONTAL)
        inner_len = inner_wd;
    else
        inner_len = inner_ht;

    scrollbar_helper(min_shown, max_shown, items, inner_len, &size, &start);

    /* draw box */
    if (!(flags & BORDER_NOFILL))
    {
#ifdef HAVE_LCD_COLOR
        /* must avoid corners if case of (flags & FOREGROUND) */
        screen->fillrect(inner_x, y, inner_wd, bw);
        screen->fillrect(inner_x, y + height - bw, inner_wd, bw);
        screen->fillrect(x, inner_y, bw, inner_ht);
        screen->fillrect(x + width - bw, inner_y, bw, inner_ht);
#else
        for (int b = 0; b < bw; b++)
            screen->drawrect(x + b, y + b, width - 2 * b, height - 2 * b);
#endif
    }
    screen->set_drawmode(DRMODE_SOLID | DRMODE_INVERSEVID);

#ifdef HAVE_LCD_COLOR
    infill = flags & (screen->depth > 1 ? INNER_FILL_MASK : INNER_FILL);

    if (!(flags & FOREGROUND))
    {
#endif
        /* clear corner pixels */
        screen->fillrect(x, y, bw, bw);
        screen->fillrect(x + width - bw, y, bw, bw);
        screen->fillrect(x, y + height - bw, bw, bw);
        screen->fillrect(x + width - bw, y + height - bw, bw, bw);

#ifdef HAVE_LCD_COLOR
        if (infill != INNER_BGFILL)
            infill = INNER_FILL;
    }

    if (infill == INNER_FILL)
#endif
    {
        /* clear pixels in progress bar */
        screen->fillrect(inner_x, inner_y, inner_wd, inner_ht);
    }

    screen->set_drawmode(DRMODE_SOLID);

    if (flags & INNER_NOFILL)
        return;

#ifdef HAVE_LCD_COLOR
    if (infill == INNER_BGFILL)
    {
        /* fill inner area with current background color */
        unsigned fg = screen->get_foreground();
        screen->set_foreground(screen->get_background());
        screen->fillrect(inner_x, inner_y, inner_wd, inner_ht);
        screen->set_foreground(fg);
    }
#endif

    if (flags & HORIZONTAL)
    {
        inner_x += start;
        inner_wd = size;
    }
    else
    {
        inner_y += start;
        inner_ht = size;
    }

    screen->fillrect(inner_x, inner_y, inner_wd, inner_ht);
}

/* Classic iPod (1G-4G / Mini) vertical list scrollbar:
 * closed square-cornered frame with an inner background margin around the
 * solid foreground thumb on all four sides. */
void gui_ipod_scrollbar_draw(struct screen *screen, int x, int y,
                             int width, int height, int items,
                             int min_shown, int max_shown)
{
    const int bw  = 4;  /* frame border thickness (matches 76px HiDPI strokes) */
    const int pad = 4;  /* inner gap between frame border and thumb */

    if (width <= 2 * (bw + pad) || height <= 2 * (bw + pad))
        return;

    int min = (min_shown < max_shown) ? min_shown : max_shown;
    int max = (min_shown < max_shown) ? max_shown : min_shown;
    if (min < 0) min = 0;
    if (min > items) min = items;
    if (max < 0) max = 0;
    if (max > items) max = items;
    int range = max - min;

    /* Clear interior to background colour */
    screen->set_drawmode(DRMODE_SOLID | DRMODE_INVERSEVID);
    screen->fillrect(x + bw, y + bw, width - 2 * bw, height - 2 * bw);

    /* Closed square-cornered outer frame */
    screen->set_drawmode(DRMODE_SOLID);
    screen->fillrect(x, y, width, bw);
    screen->fillrect(x, y + height - bw, width, bw);
    screen->fillrect(x, y + bw, bw, height - 2 * bw);
    screen->fillrect(x + width - bw, y + bw, bw, height - 2 * bw);

    /* Inner thumb channel */
    int tx = x + bw + pad;
    int ty = y + bw + pad;
    int tw = width  - 2 * (bw + pad);
    int th = height - 2 * (bw + pad);

    int knob_h = th;
    int knob_off = 0;
    if (items > 0 && items > range)
    {
        int min_knob = (th >= 24) ? 24 : th;
        knob_h = (int)((int64_t)th * range / items);
        if (knob_h < min_knob)
            knob_h = min_knob;
        if (knob_h > th)
            knob_h = th;
        knob_off = (int)(((int64_t)(th - knob_h) * min + (items - range) / 2) /
                         (items - range));
    }

    screen->fillrect(tx, ty + knob_off, tw, knob_h);
}

void gui_bitmap_scrollbar_draw(struct screen * screen, struct bitmap *bm, int x, int y,
                        int width, int height, int items,
                        int min_shown, int max_shown,
                        unsigned flags)
{
    int start;
    int size;
    int inner_len;
    int startx = 0, starty = 0;

    screen->set_drawmode(DRMODE_SOLID|DRMODE_INVERSEVID);

    /* clear pixels in progress bar */
    if ((flags&DONT_CLEAR_EXCESS) == 0)
        screen->fillrect(x, y, width, height);

    screen->set_drawmode(DRMODE_SOLID);

    if (flags & INNER_NOFILL)
        return;

    if (flags & INVERTFILL)
    {
        min_shown = items - max_shown;
        max_shown = items;
    }

    if (flags & HORIZONTAL)
        inner_len = width;
    else
        inner_len = height;

    scrollbar_helper(min_shown, max_shown, items, inner_len, &size, &start);

    if (flags & HORIZONTAL) {
        x += start;
        width = size;
        if (flags & INVERTFILL)
            startx = start;
    } else {
        y += start;
        height = size;
        if (flags & INVERTFILL)
            starty = start;
    }

    if (bm->width < startx)
        width = 0;
    else if (bm->width < startx + width)
        width = bm->width - startx;
    if (bm->height < starty)
        height = 0;
    else if (bm->height < starty + height)
        height = bm->height - starty;

    screen->bmp_part(bm, startx, starty, x, y, width, height);
}

void show_busy_slider(struct screen *s, int x, int y, int width, int height)
{
    static int start = 0, dir = 1;
    gui_scrollbar_draw(s, x, y, width, height, 100,
                           start, start+20, HORIZONTAL);
#if NB_SCREENS > 1
    if (s->screen_type == SCREEN_MAIN)
#endif
    {
        start += (dir*2);
        if (start > 79)
            dir = -1;
        else if (start < 1)
            dir = 1;
    }
}
