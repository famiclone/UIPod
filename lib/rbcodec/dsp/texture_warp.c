/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * UIPod: Ableton Live "Texture" Warp Mode granular time-stretcher.
 *
 * Synthesizes 6 overlapping stereo Hann-windowed grains with adaptive grain
 * size (~50 ms rhythmic near 1.0x up to ~128 ms ethereal cloud at 1/16x),
 * Flux read-position spray, subtle organic micro-shimmer detune, and
 * Catmull-Rom cubic interpolation so any song can be slowed down up to 16x
 * into a lush ambient soundscape without pitch drop or robotic artifacts.
 *
 * This program is free software; you can redistribute it and/or
 * modify it under the terms of the GNU General Public License
 * as published by the Free Software Foundation; either version 2
 * of the License, or (at your option) any later version.
 *
 ****************************************************************************/
#include "rbcodecconfig.h"
#include "platform.h"
#include "dsp_proc_entry.h"
#include "dsp_misc.h"
#include "texture_warp.h"
#include <math.h>
#include <string.h>

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

#define WARP_NUM_GRAINS   6
#define WARP_RING_FRAMES  16384U
#define WARP_RING_MASK    (WARP_RING_FRAMES - 1U)
#define WARP_OUT_FRAMES   256
#define WARP_LUT_SIZE     1024

static float   hann_lut[WARP_LUT_SIZE + 1] IBSS_ATTR;
static int32_t warp_out_bufs[2][WARP_OUT_FRAMES] IBSS_ATTR;
static float   warp_ring_l[WARP_RING_FRAMES] IBSS_ATTR;
static float   warp_ring_r[WARP_RING_FRAMES] IBSS_ATTR;

struct texture_warp_state
{
    struct dsp_buffer out_buf;
    uint32_t write_pos;
    uint32_t valid_frames;
    unsigned int fout;
    float    sr_scale;
    float    cur_ratio;
    float    target_ratio;
    double   in_accum;
    float    master_phase;
    float    grain_age[WARP_NUM_GRAINS];
    float    grain_rate[WARP_NUM_GRAINS];
    float    grain_pan_l[WARP_NUM_GRAINS];
    float    grain_pan_r[WARP_NUM_GRAINS];
    uint32_t rng;
};

static struct texture_warp_state warp_state IBSS_ATTR;
static volatile int s_warp_permille = 1000;

static inline uint32_t warp_xorshift(uint32_t *state)
{
    uint32_t r = *state;
    r ^= r << 13;
    r ^= r >> 17;
    r ^= r << 5;
    *state = r;
    return r;
}

static inline float warp_hann(float ph)
{
    float idx_f = ph * (float)WARP_LUT_SIZE;
    int idx = (int)idx_f;
    if (idx < 0)
        idx = 0;
    else if (idx >= WARP_LUT_SIZE)
        idx = WARP_LUT_SIZE - 1;
    float frac = idx_f - (float)idx;
    return hann_lut[idx] + (hann_lut[idx + 1] - hann_lut[idx]) * frac;
}

static inline float warp_soft_limit(float x)
{
    if (x > 3.2f)
        return 1.0f;
    if (x < -3.2f)
        return -1.0f;
    float x2 = x * x;
    return x * (27.0f + x2) / (27.0f + 9.0f * x2);
}

static inline float warp_sample_hermite(const float *ring, uint32_t wp, float age)
{
    int ia = (int)age;
    float frac = age - (float)ia;
    float u = 1.0f - frac;

    uint32_t i0 = (wp - 1U - (uint32_t)ia) & WARP_RING_MASK;
    uint32_t i1 = (i0 - 1U) & WARP_RING_MASK;
    uint32_t im1 = (i1 - 1U) & WARP_RING_MASK;
    uint32_t ip1 = (i0 + 1U) & WARP_RING_MASK;

    float y0 = ring[im1];
    float y1 = ring[i1];
    float y2 = ring[i0];
    float y3 = ring[ip1];

    float c1 = 0.5f * (y2 - y0);
    float c2 = y0 - 2.5f * y1 + 2.0f * y2 - 0.5f * y3;
    float c3 = 0.5f * (y3 - y0) + 1.5f * (y1 - y2);
    return ((c3 * u + c2) * u + c1) * u + y1;
}

static void warp_spawn_grain(struct texture_warp_state *st, int g,
                             float ratio, float grain_len, float ambient)
{
    uint32_t r = warp_xorshift(&st->rng);
    float u01 = (float)(r & 0xFFFFU) * (1.0f / 65535.0f);
    float bi  = ((float)((int)((r >> 16) & 0xFFFFU) - 32768)) * (1.0f / 32768.0f);

    /* Subtle micro-shimmer pitch detune (+/- 3.8 cents max at deep ambient slowdowns) */
    float detune = bi * ambient * 0.0022f;
    float rate = 1.0f + detune;
    st->grain_rate[g] = rate;

    /* Subtle stereo cloud spread on alternate grains in deep ambient range */
    float pan_amt = ambient * 0.16f * ((g & 1) ? 1.0f : -1.0f);
    st->grain_pan_l[g] = 1.0f - pan_amt;
    st->grain_pan_r[g] = 1.0f + pan_amt;

    /* Causal offset behind `write_pos` so grain never overtakes incoming audio */
    float catchup = (rate > ratio) ? ((rate - ratio) * grain_len) : 0.0f;
    float flux_spray = u01 * (ambient * ambient * 1800.0f * st->sr_scale);
    float spawn_age = catchup + 48.0f * st->sr_scale + flux_spray;

    float max_age = (st->valid_frames > 8U)
                  ? (float)(st->valid_frames - 4U)
                  : 2.0f;
    if (max_age > (float)(WARP_RING_FRAMES - 16U))
        max_age = (float)(WARP_RING_FRAMES - 16U);
    if (spawn_age > max_age)
        spawn_age = max_age;
    if (spawn_age < 2.0f)
        spawn_age = 2.0f;

    st->grain_age[g] = spawn_age;
}

static void warp_reset_grains(struct texture_warp_state *st)
{
    st->write_pos = 0;
    st->valid_frames = 0;
    st->in_accum = 0.0;
    st->master_phase = 0.0f;
    st->cur_ratio = st->target_ratio;
    if (st->rng == 0)
        st->rng = 0x6D2B79F5U;

    memset(warp_ring_l, 0, sizeof(warp_ring_l));
    memset(warp_ring_r, 0, sizeof(warp_ring_r));

    for (int g = 0; g < WARP_NUM_GRAINS; g++)
    {
        st->grain_age[g] = 4.0f + (float)g * 12.0f;
        st->grain_rate[g] = 1.0f;
        st->grain_pan_l[g] = 1.0f;
        st->grain_pan_r[g] = 1.0f;
    }
}

static void warp_flush(struct dsp_proc_entry *this)
{
    struct texture_warp_state *st = (struct texture_warp_state *)this->data;
    st->out_buf.remcount = 0;
    warp_reset_grains(st);
}

static inline void warp_push_frame(struct texture_warp_state *st,
                                   float s_l, float s_r)
{
    uint32_t wp = st->write_pos;
    warp_ring_l[wp] = s_l;
    warp_ring_r[wp] = s_r;
    st->write_pos = (wp + 1U) & WARP_RING_MASK;
    if (st->valid_frames < WARP_RING_FRAMES)
        st->valid_frames++;

    for (int g = 0; g < WARP_NUM_GRAINS; g++)
        st->grain_age[g] += 1.0f;
}

static void texture_warp_process(struct dsp_proc_entry *this,
                                 struct dsp_buffer **buf_p)
{
    struct texture_warp_state *st = (struct texture_warp_state *)this->data;
    struct dsp_buffer *src = *buf_p;
    struct dsp_buffer *dst = &st->out_buf;

    *buf_p = dst;

    if (dst->remcount > 0)
        return;

    dst->remcount = 0;
    dst->p32[0] = warp_out_bufs[0];
    dst->p32[1] = warp_out_bufs[1];
    dst->proc_mask = src->proc_mask;

    int avail = src->remcount;
    if (avail <= 0)
        return;

    const int32_t *src_l = src->p32[0];
    const int32_t *src_r = (src->format.num_channels >= 2) ? src->p32[1] : src->p32[0];
    const float full_scale = 32768.0f * (float)(1U << src->format.output_scale);
    const float inv_scale = 1.0f / full_scale;

    int src_idx = 0;

    /* Seed initial ring samples on fresh start / seek so cubic lookup has history */
    while (st->valid_frames < 64U && src_idx < avail)
    {
        warp_push_frame(st,
                        (float)src_l[src_idx] * inv_scale,
                        (float)src_r[src_idx] * inv_scale);
        src_idx++;
    }
    if (st->valid_frames < 64U)
    {
        if (src_idx > 0)
            dsp_advance_buffer32(src, src_idx);
        return;
    }

    int permille = s_warp_permille;
    float tgt_ratio = (float)permille * 0.001f;
    if (tgt_ratio < 0.05f)
        tgt_ratio = 0.05f;
    if (tgt_ratio > 2.0f)
        tgt_ratio = 2.0f;
    st->target_ratio = tgt_ratio;

    int32_t *out_l = warp_out_bufs[0];
    int32_t *out_r = warp_out_bufs[1];
    int produced = 0;

    while (produced < WARP_OUT_FRAMES)
    {
        while (st->in_accum >= 1.0 && src_idx < avail)
        {
            warp_push_frame(st,
                            (float)src_l[src_idx] * inv_scale,
                            (float)src_r[src_idx] * inv_scale);
            src_idx++;
            st->in_accum -= 1.0;
        }
        if (st->in_accum >= 1.0)
            break;

        /* Smooth knob glide (~25 ms) so live speed adjustments never click */
        st->cur_ratio += 0.0018f * (st->target_ratio - st->cur_ratio);
        float ratio = st->cur_ratio;

        float ambient = 0.0f;
        if (ratio < 1.0f)
        {
            ambient = (1.0f - ratio) * (1.0f / 0.88f);
            if (ambient > 1.0f)
                ambient = 1.0f;
        }

        /* Adaptive Ableton Texture grain length: ~50 ms near 1.0x -> ~127 ms deep ambient */
        float grain_len = (2200.0f + ambient * 3400.0f) * st->sr_scale;
        float phase_inc = 1.0f / grain_len;

        float prev_m = st->master_phase;
        float curr_m = prev_m + phase_inc;
        if (curr_m >= 1.0f)
            curr_m -= 1.0f;
        st->master_phase = curr_m;

        float mix_l = 0.0f;
        float mix_r = 0.0f;
        float max_valid_age = (st->valid_frames > 4U)
                            ? (float)(st->valid_frames - 3U)
                            : 1.0f;
        if (max_valid_age > (float)(WARP_RING_FRAMES - 6U))
            max_valid_age = (float)(WARP_RING_FRAMES - 6U);

        for (int g = 0; g < WARP_NUM_GRAINS; g++)
        {
            float off = (float)g * (1.0f / (float)WARP_NUM_GRAINS);
            float ph_prev = prev_m + off;
            if (ph_prev >= 1.0f)
                ph_prev -= 1.0f;
            float ph = curr_m + off;
            if (ph >= 1.0f)
                ph -= 1.0f;

            /* Grain wrapped 1.0 -> 0.0 at exact zero-amplitude Hann boundary */
            if (ph < ph_prev)
                warp_spawn_grain(st, g, ratio, grain_len, ambient);

            float w = warp_hann(ph);
            float age = st->grain_age[g];
            if (age < 1.0f)
                age = 1.0f;
            else if (age > max_valid_age)
                age = max_valid_age;

            float gl = warp_sample_hermite(warp_ring_l, st->write_pos, age);
            float gr = warp_sample_hermite(warp_ring_r, st->write_pos, age);

            mix_l += gl * w * st->grain_pan_l[g];
            mix_r += gr * w * st->grain_pan_r[g];

            st->grain_age[g] = age - st->grain_rate[g];
        }

        /* Constant-loudness blend between coherent (1/3.0) and diffuse Flux cloud (1/1.72) */
        float norm = 1.0f / (3.0f - 1.28f * ambient);
        mix_l *= norm;
        mix_r *= norm;

        if (mix_l > 0.92f || mix_l < -0.92f)
            mix_l = warp_soft_limit(mix_l);
        if (mix_r > 0.92f || mix_r < -0.92f)
            mix_r = warp_soft_limit(mix_r);

        out_l[produced] = (int32_t)(mix_l * full_scale);
        out_r[produced] = (int32_t)(mix_r * full_scale);
        produced++;
        st->in_accum += (double)ratio;
    }

    dst->remcount = produced;
    if (src_idx > 0)
        dsp_advance_buffer32(src, src_idx);
}

static intptr_t texture_warp_new_format(struct dsp_proc_entry *this,
                                        struct dsp_config *dsp,
                                        struct sample_format *format)
{
    struct texture_warp_state *st = (struct texture_warp_state *)this->data;
    struct dsp_buffer *dst = &st->out_buf;

    if (dst->remcount > 0)
        return PROC_NEW_FORMAT_TRANSITION;

    unsigned int fout = dsp_get_output_frequency(dsp);
    if (fout < 8000U)
        fout = 44100U;

    int permille = s_warp_permille;
    bool active = (permille != 1000);
    bool was_active = dsp_proc_active(dsp, DSP_PROC_TEXTURE_WARP);

    if (st->fout != fout)
    {
        st->fout = fout;
        st->sr_scale = (float)fout * (1.0f / 44100.0f);
    }

    dst->format = *format;
    dst->format.num_channels = 2; /* Stereo cloud output even for mono sources */

    dsp_proc_activate(dsp, DSP_PROC_TEXTURE_WARP, active);

    if (active)
    {
        st->target_ratio = (float)permille * 0.001f;
        if (!was_active)
            warp_reset_grains(st);
        return PROC_NEW_FORMAT_OK;
    }

    return PROC_NEW_FORMAT_DEACTIVATED;
}

void dsp_set_texture_warp_speed(int permille)
{
    if (permille <= 0)
        permille = 1000;
    else if (permille < 50)
        permille = 50;
    else if (permille > 2000)
        permille = 2000;

    if (permille == s_warp_permille)
        return;

    s_warp_permille = permille;
    warp_state.target_ratio = (float)permille * 0.001f;

    struct dsp_config *dsp = dsp_get_config(CODEC_IDX_AUDIO);
    if (dsp)
        dsp_proc_want_format_update(dsp, DSP_PROC_TEXTURE_WARP);
}

int dsp_get_texture_warp_speed(void)
{
    return s_warp_permille;
}

void dsp_texture_warp_init(struct dsp_config *dsp, unsigned int dsp_id)
{
    if (dsp_id != CODEC_IDX_AUDIO)
        return;

    for (int i = 0; i <= WARP_LUT_SIZE; i++)
    {
        double ph = (double)i / (double)WARP_LUT_SIZE;
        hann_lut[i] = (float)(0.5 * (1.0 - cos(2.0 * M_PI * ph)));
    }

    warp_state.fout = DSP_OUT_DEFAULT_HZ;
    warp_state.sr_scale = (float)DSP_OUT_DEFAULT_HZ * (1.0f / 44100.0f);
    warp_state.cur_ratio = 1.0f;
    warp_state.target_ratio = 1.0f;
    warp_state.rng = 0x6D2B79F5U;
    warp_reset_grains(&warp_state);

    dsp_proc_enable(dsp, DSP_PROC_TEXTURE_WARP, true);
}

static void texture_warp_proc_init(struct dsp_proc_entry *this,
                                   struct dsp_config *dsp)
{
    this->data = (intptr_t)&warp_state;
    dsp_proc_set_in_place(dsp, DSP_PROC_TEXTURE_WARP, false);
    this->process = texture_warp_process;
}

static intptr_t texture_warp_configure(struct dsp_proc_entry *this,
                                       struct dsp_config *dsp,
                                       unsigned int setting,
                                       intptr_t value)
{
    switch (setting)
    {
    case DSP_FLUSH:
        warp_flush(this);
        break;

    case DSP_PROC_INIT:
        texture_warp_proc_init(this, dsp);
        break;

    case DSP_PROC_NEW_FORMAT:
        return texture_warp_new_format(this, dsp, (struct sample_format *)value);

    case DSP_SET_OUT_FREQUENCY:
        dsp_proc_want_format_update(dsp, DSP_PROC_TEXTURE_WARP);
        break;
    }
    return 0;
}

DSP_PROC_DB_ENTRY(TEXTURE_WARP,
                  texture_warp_configure);
