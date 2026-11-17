/***************************************************************************
 * UIPod: screen transition animations (see uipod_transition.h).
 *
 * Two full-screen frame snapshots (`s_from` = the screen we leave, `s_to` = the
 * screen we arrive at) are composited into the live framebuffer across the
 * content band `[header_h, footer_y0)`, while the shared `Header` stays pinned.
 ****************************************************************************/
#include "config.h"
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include "system.h"
#include "kernel.h"
#include "lcd.h"
#include "settings.h"
#include "gui/viewport.h"
#include "window-sdl.h"   /* uipod_viz_active: projectM owns the GL window */
#include "uipod_transition.h"

/* full-screen frame snapshots (off the stack: ~2.25 MiB each at 1024x768x3) */
static fb_data s_from[LCD_FBWIDTH * LCD_FBHEIGHT];
static fb_data s_to[LCD_FBWIDTH * LCD_FBHEIGHT];
static bool    s_back_pending;
static bool    s_suppress_next;   /* skip the next screen-entry transition entirely */

/* 8x8 Bayer ordered dither matrix (0..63) for LCD pixel dissolve */
static const uint8_t s_bayer8[8][8] = {
    {  0, 32,  8, 40,  2, 34, 10, 42 },
    { 48, 16, 56, 24, 50, 18, 58, 26 },
    { 12, 44,  4, 36, 14, 46,  6, 38 },
    { 60, 28, 52, 20, 62, 30, 54, 22 },
    {  3, 35, 11, 43,  1, 33,  9, 41 },
    { 51, 19, 59, 27, 49, 17, 57, 25 },
    { 15, 47,  7, 39, 13, 45,  5, 37 },
    { 63, 31, 55, 23, 61, 29, 53, 21 },
};

static fb_data *fb_base(void)
{
    lcd_set_viewport(NULL);
    return FBADDR(0, 0);
}

static inline fb_data rgb_to_fb_pixel(int rgb)
{
    fb_data px;
    px.b = (unsigned char)(rgb & 0xFF);
    px.g = (unsigned char)((rgb >> 8) & 0xFF);
    px.r = (unsigned char)((rgb >> 16) & 0xFF);
    return px;
}

static int header_h(void)
{
    struct viewport vp = {0};
    viewport_set_defaults(&vp, SCREEN_MAIN);
    return (vp.y > 0 && vp.y < LCD_HEIGHT) ? vp.y : 0;
}

static int footer_y0(void)
{
    struct viewport vp = {0};
    viewport_set_defaults(&vp, SCREEN_MAIN);
    int y1 = vp.y + vp.height;
    return (y1 > 0 && y1 < LCD_HEIGHT) ? y1 : LCD_HEIGHT;
}

static void blit_header(fb_data *fb, const fb_data *src, int h)
{
    for (int y = 0; y < h; y++)
        memcpy(fb + y * LCD_FBWIDTH, src + y * LCD_FBWIDTH,
               (size_t)LCD_WIDTH * sizeof(fb_data));
}

static void blit_footer(fb_data *fb, const fb_data *src, int y0)
{
    for (int y = y0; y < LCD_HEIGHT; y++)
        memcpy(fb + y * LCD_FBWIDTH, src + y * LCD_FBWIDTH,
               (size_t)LCD_WIDTH * sizeof(fb_data));
}

static void blit_shifted(fb_data *fb, const fb_data *src, int xoff,
                         int y0, int y1)
{
    int aoff = xoff < 0 ? -xoff : xoff;
    int wpx  = LCD_WIDTH - aoff;
    if (wpx <= 0)
        return;
    int sx = xoff < 0 ? aoff : 0;
    int dx = xoff > 0 ? xoff : 0;
    for (int y = y0; y < y1; y++)
        memcpy(fb + y * LCD_FBWIDTH + dx, src + y * LCD_FBWIDTH + sx,
               (size_t)wpx * sizeof(fb_data));
}

/* 0: Classic iPod horizontal push-slide (100 ms, quad ease-out) */
static void run_slide(enum uipod_transition_dir dir, int hh, int fy)
{
    fb_data *fb = fb_base();
    int d = (dir == UIPOD_TRANS_FORWARD) ? 1 : -1;
    unsigned long start = sdl_get_ms();

    for (;;)
    {
        unsigned long el = sdl_get_ms() - start;
        if (el >= UIPOD_TRANSITION_MS)
            break;
        int t   = (int)(el * 1000 / UIPOD_TRANSITION_MS);
        int inv = 1000 - t;
        int p   = 1000 - inv * inv / 1000;
        int W   = LCD_WIDTH;
        int off_from = -d * p * W / 1000;
        int off_to   =  d * (1000 - p) * W / 1000;
        blit_shifted(fb, s_from, off_from, hh, fy);
        blit_shifted(fb, s_to,   off_to,   hh, fy);
        lcd_update();
    }
}

/* 1: Parallax card stack (`Cover`, 135 ms, cubic ease-out + 4px fg1 edge) */
static void run_cover(enum uipod_transition_dir dir, int hh, int fy)
{
    const unsigned long dur_ms = 135;
    fb_data *fb = fb_base();
    const fb_data edge_px = rgb_to_fb_pixel(global_settings.fg1_color);
    const int W = LCD_WIDTH;
    unsigned long start = sdl_get_ms();

    for (;;)
    {
        unsigned long el = sdl_get_ms() - start;
        if (el >= dur_ms)
            break;
        int t   = (int)(el * 1000 / dur_ms);
        int inv = 1000 - t;
        int p   = 1000 - ((inv * inv / 1000) * inv) / 1000;

        int split_x;
        const fb_data *left_src;
        const fb_data *right_src;
        int left_sx;

        if (dir == UIPOD_TRANS_FORWARD)
        {
            /* `s_to` slides over from right [split_x, W); `s_from` shifts 28% left */
            split_x   = (1000 - p) * W / 1000;
            left_src  = s_from;
            left_sx   = (p * (W * 28 / 100)) / 1000;
            right_src = s_to;
        }
        else
        {
            /* `s_from` slides off to right [split_x, W), revealing `s_to` recovering */
            split_x   = p * W / 1000;
            left_src  = s_to;
            left_sx   = ((1000 - p) * (W * 28 / 100)) / 1000;
            right_src = s_from;
        }

        if (split_x < 0) split_x = 0;
        if (split_x > W) split_x = W;
        if (left_sx + split_x > W)
            left_sx = W - split_x;

        int edge_x0 = (split_x >= 4 && split_x < W) ? (split_x - 4) : split_x;

        for (int y = hh; y < fy; y++)
        {
            fb_data *dst_row = fb + y * LCD_FBWIDTH;
            if (edge_x0 > 0)
            {
                memcpy(dst_row, left_src + y * LCD_FBWIDTH + left_sx,
                       (size_t)edge_x0 * sizeof(fb_data));
            }
            for (int x = edge_x0; x < split_x; x++)
                dst_row[x] = edge_px;
            if (split_x < W)
            {
                memcpy(dst_row + split_x, right_src + y * LCD_FBWIDTH,
                       (size_t)(W - split_x) * sizeof(fb_data));
            }
        }
        lcd_update();
    }
}

/* 2: Smooth alpha crossfade (`Fade`, 130 ms) */
static void run_fade(int hh, int fy)
{
    const unsigned long dur_ms = 130;
    fb_data *fb = fb_base();
    const size_t start_byte = (size_t)hh * LCD_FBWIDTH * sizeof(fb_data);
    const size_t end_byte   = (size_t)fy * LCD_FBWIDTH * sizeof(fb_data);
    uint8_t       *dst = (uint8_t *)fb + start_byte;
    const uint8_t *src_a = (const uint8_t *)s_from + start_byte;
    const uint8_t *src_b = (const uint8_t *)s_to   + start_byte;
    const size_t   nbytes = (end_byte > start_byte) ? (end_byte - start_byte) : 0;

    unsigned long start = sdl_get_ms();
    for (;;)
    {
        unsigned long el = sdl_get_ms() - start;
        if (el >= dur_ms)
            break;
        int t   = (int)(el * 1000 / dur_ms);
        int inv = 1000 - t;
        int p   = 1000 - inv * inv / 1000;
        uint32_t alpha     = (uint32_t)((p * 256) / 1000);
        uint32_t inv_alpha = 256u - alpha;

        for (size_t i = 0; i < nbytes; i++)
            dst[i] = (uint8_t)((src_a[i] * inv_alpha + src_b[i] * alpha) >> 8);

        lcd_update();
    }
}

/* 3: 4x4 HiDPI Bayer LCD pixel dissolve (`Pixels`, 145 ms — preserves exact 4-color palette) */
static void run_pixels(enum uipod_transition_dir dir, int hh, int fy)
{
    const unsigned long dur_ms = 145;
    const int CELL = 4;
    const int NCOLS = LCD_WIDTH / CELL; /* 256 */
    fb_data *fb = fb_base();
    uint8_t mask[8][256];

    unsigned long start = sdl_get_ms();
    for (;;)
    {
        unsigned long el = sdl_get_ms() - start;
        if (el >= dur_ms)
            break;
        int t   = (int)(el * 1000 / dur_ms);
        int inv = 1000 - t;
        int p   = 1000 - inv * inv / 1000;

        /* Precompute the 8x256 macro-pixel decision table for this frame */
        for (int br = 0; br < 8; br++)
        {
            for (int bx = 0; bx < NCOLS; bx++)
            {
                int bayer = (int)s_bayer8[br][bx & 7] * 10; /* 0..630 */
                int sweep = (dir == UIPOD_TRANS_FORWARD)
                                ? ((NCOLS - 1 - bx) * 370) / (NCOLS - 1)
                                : (bx * 370) / (NCOLS - 1);
                mask[br][bx] = (p > bayer + sweep) ? 1 : 0;
            }
        }

        for (int y0 = hh; y0 < fy; y0 += CELL)
        {
            int y1 = y0 + CELL;
            if (y1 > fy)
                y1 = fy;
            const uint8_t *mrow = mask[((y0 - hh) / CELL) & 7];

            int bx = 0;
            while (bx < NCOLS)
            {
                uint8_t use_to = mrow[bx];
                int run_end = bx + 1;
                while (run_end < NCOLS && mrow[run_end] == use_to)
                    run_end++;

                int px_x = bx * CELL;
                int px_w = (run_end - bx) * CELL;
                const fb_data *src = use_to ? s_to : s_from;
                for (int y = y0; y < y1; y++)
                {
                    memcpy(fb + y * LCD_FBWIDTH + px_x,
                           src + y * LCD_FBWIDTH + px_x,
                           (size_t)px_w * sizeof(fb_data));
                }
                bx = run_end;
            }
        }
        lcd_update();
    }
}

/* 4: 8-band horizontal shutter wipe (`Blinds`, 135 ms) */
static void run_blinds(enum uipod_transition_dir dir, int hh, int fy)
{
    const unsigned long dur_ms = 135;
    const int NBANDS = 8;
    fb_data *fb = fb_base();
    const fb_data lip_px = rgb_to_fb_pixel(global_settings.fg1_color);
    fb_data lip_line[LCD_WIDTH];
    for (int x = 0; x < LCD_WIDTH; x++)
        lip_line[x] = lip_px;

    int total_h = fy - hh;
    if (total_h <= 0)
        return;
    int band_h = (total_h + NBANDS - 1) / NBANDS;

    unsigned long start = sdl_get_ms();
    for (;;)
    {
        unsigned long el = sdl_get_ms() - start;
        if (el >= dur_ms)
            break;
        int t   = (int)(el * 1000 / dur_ms);
        int inv = 1000 - t;
        int p   = 1000 - inv * inv / 1000;

        for (int b = 0; b < NBANDS; b++)
        {
            int by0 = hh + b * band_h;
            int by1 = by0 + band_h;
            if (by0 >= fy)
                break;
            if (by1 > fy)
                by1 = fy;
            int bh = by1 - by0;

            int stagger_idx = (dir == UIPOD_TRANS_FORWARD) ? b : (NBANDS - 1 - b);
            int local_p = (p * 1280 - stagger_idx * 40) / 1000;
            if (local_p < 0)    local_p = 0;
            if (local_p > 1000) local_p = 1000;

            int reveal = (bh * local_p) / 1000;

            for (int i = 0; i < bh; i++)
            {
                int y = by0 + i;
                fb_data *dst_row = fb + y * LCD_FBWIDTH;
                if (dir == UIPOD_TRANS_FORWARD)
                {
                    if (i < reveal)
                        memcpy(dst_row, s_to + y * LCD_FBWIDTH,
                               (size_t)LCD_WIDTH * sizeof(fb_data));
                    else if (i < reveal + 2 && reveal > 0 && reveal < bh)
                        memcpy(dst_row, lip_line,
                               (size_t)LCD_WIDTH * sizeof(fb_data));
                    else
                        memcpy(dst_row, s_from + y * LCD_FBWIDTH,
                               (size_t)LCD_WIDTH * sizeof(fb_data));
                }
                else
                {
                    int split = bh - reveal;
                    if (i >= split)
                        memcpy(dst_row, s_to + y * LCD_FBWIDTH,
                               (size_t)LCD_WIDTH * sizeof(fb_data));
                    else if (i >= split - 2 && reveal > 0 && reveal < bh)
                        memcpy(dst_row, lip_line,
                               (size_t)LCD_WIDTH * sizeof(fb_data));
                    else
                        memcpy(dst_row, s_from + y * LCD_FBWIDTH,
                               (size_t)LCD_WIDTH * sizeof(fb_data));
                }
            }
        }
        lcd_update();
    }
}

void uipod_transition_animate(enum uipod_transition_dir dir,
                              uipod_render_fn render, void *ctx)
{
    s_back_pending = false;                 /* consumed by this animate */

    if (s_suppress_next)
    {
        s_suppress_next = false;
        if (render)
            render(ctx);
        if (!uipod_viz_active)
            lcd_update();
        return;
    }

    if (uipod_viz_active)
    {
        if (render)
            render(ctx);
        return;
    }

    int style = global_settings.transition_style;
    if (style == UIPOD_TRANS_STYLE_OFF)
    {
        if (render)
            render(ctx);
        lcd_update();
        return;
    }

    /* snapshot the screen we leave, then render the destination off-screen */
    memcpy(s_from, fb_base(), FRAMEBUFFER_SIZE);
    lcd_set_update_suppressed(true);
    if (render)
        render(ctx);
    memcpy(s_to, fb_base(), FRAMEBUFFER_SIZE);
    memcpy(fb_base(), s_from, FRAMEBUFFER_SIZE);

    int hh = header_h();
    int fy = footer_y0();
    blit_header(fb_base(), s_to, hh);
    blit_footer(fb_base(), s_to, fy);
    lcd_set_update_suppressed(false);
    lcd_update();

    switch (style)
    {
        case UIPOD_TRANS_STYLE_COVER:
            run_cover(dir, hh, fy);
            break;
        case UIPOD_TRANS_STYLE_FADE:
            run_fade(hh, fy);
            break;
        case UIPOD_TRANS_STYLE_PIXELS:
            run_pixels(dir, hh, fy);
            break;
        case UIPOD_TRANS_STYLE_BLINDS:
            run_blinds(dir, hh, fy);
            break;
        case UIPOD_TRANS_STYLE_SLIDE:
        default:
            run_slide(dir, hh, fy);
            break;
    }

    /* settle exactly on the destination frame */
    memcpy(fb_base(), s_to, FRAMEBUFFER_SIZE);
    lcd_update();
}

bool uipod_transition_first_screen(void)
{
    static bool first = true;
    bool f = first;
    first = false;
    return f;
}

void uipod_transition_arm_back(void)
{
    s_back_pending = true;
}

bool uipod_transition_take_back(void)
{
    bool b = s_back_pending;
    s_back_pending = false;
    return b;
}

void uipod_transition_suppress_next(void)
{
    s_suppress_next = true;
}
