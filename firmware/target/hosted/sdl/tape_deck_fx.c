/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * UIPod: Real-Time Analog Cassette Deck DSP & Mechanical Transport Engine.
 *
 * Zero-libm, causal-delay (< 3.5 ms) real-time cassette processor:
 *   - Causal positive lookback ring (`target_seed = 2` frames = 0.04 ms) so
 *     there is virtually zero FIFO latency added to Visualizers or Sound Racer.
 *   - 32-sample control-rate LFO interpolation + branchless `fast_sin2pi` so
 *     inner sample loops make ZERO `libm` (`sinf`/`expf`/`sqrtf`) calls even
 *     under `-Os -fno-builtin`, running lock-free outside `pcm_mtx`.
 *
 * This program is free software; you can redistribute it and/or
 * modify it under the terms of the GNU General Public License
 * as published by the Free Software Foundation; either version 2
 * of the License, or (at your option) any later version.
 *
 ****************************************************************************/
#include "config.h"
#include "gcc_extensions.h"
#include "tape_deck_fx.h"

#include <math.h>
#include <string.h>

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

#define TAPE_RING_FRAMES 4096U
#define TAPE_RING_MASK   (TAPE_RING_FRAMES - 1U)
#define TAPE_SEED_FRAMES 2U
#define LFO_SUBBLOCK     32U

enum tape_motor_mode
{
    TAPE_MOTOR_RUN = 0,
    TAPE_MOTOR_SPINDOWN,
    TAPE_MOTOR_STOPPED,
    TAPE_MOTOR_SPINUP,
};

static struct tape_deck_params s_cfg = {
    .enabled     = false,
    .wow_pct     = 55,
    .hiss_pct    = 45,
    .crinkle_pct = 50,
    .sat_pct     = 50,
    .tone_pct    = 45,
    .pause_fx    = true,
    .ff_sound    = true,
};

static struct
{
    float    ring_l[TAPE_RING_FRAMES];
    float    ring_r[TAPE_RING_FRAMES];
    uint32_t write_pos;
    uint32_t seeded_frames;
    double   base_age;
    double   in_accum;

    /* Motor transport state */
    volatile enum tape_motor_mode motor_mode;
    volatile bool pause_req_pending;
    float    motor_speed;
    float    ramp_start_speed;
    float    ramp_prog;
    float    spinup_rate;

    /* Fast recursive mechanical button click oscillators (zero expf/sinf) */
    float    click_hi_env;
    float    click_lo_env;
    float    click_hi_ph;
    float    click_lo_ph;
    float    click_hi_dec;
    float    click_lo_dec;

    /* Sub-block interpolated Wow & Flutter LFOs */
    uint32_t lfo_rem;
    float    wow1_ph;
    float    wow2_ph;
    float    flut1_ph;
    float    flut2_ph;
    float    wow_drift;
    float    wow_drift_tgt;
    float    flut_drift;
    float    dly_mod_l;
    float    dly_mod_r;
    float    dly_step_l;
    float    dly_step_r;
    volatile unsigned int cur_delay_us;

    /* Mylar tape crinkle / crease & dropout state */
    float    crinkle_env;
    float    crinkle_dec;
    float    crinkle_snag_amp;
    float    crinkle_drop_l;
    float    crinkle_drop_r;
    float    crinkle_muffle_l[2];
    float    crinkle_muffle_r[2];
    float    crinkle_rustle_lp;
    uint32_t frames_until_first_crease;

    /* Magnetic head, saturation, tone & hiss filters */
    float    dc_in[2];
    float    dc_out[2];
    float    head_lp[2];
    float    tone_lp[2];
    float    hiss_lp[2];
    float    hiss_hp_lp[2];

    /* Cached sample-rate & parameter coefficients */
    unsigned int sr;
    float    inv_fs;
    float    sr_scale;
    float    dc_pole;
    float    head_alpha;
    float    tone_alpha;
    float    crinkle_lp_alpha;
    float    hiss_lp_alpha;
    float    hiss_hp_alpha;
    float    sat_amt;
    float    drive;
    float    bias;
    float    bias_norm;
    float    makeup;
    float    hiss_base_gain;

    uint32_t ctrl_cnt;
    uint32_t ctrl_period;
    uint32_t rng;

    /* Fast-Forward / Rewind electro-mechanical motor synthesizer */
    volatile int ff_dir;
    volatile bool was_manual_ff;
    int      ff_last_dir;
    float    ff_vel;
    float    ff_clack_hi_env;
    float    ff_clack_lo_env;
    float    ff_clack_nz_env;
    float    ff_clack_hi_ph;
    float    ff_clack_lo_ph;
    float    ff_motor_ph;
    float    ff_gear_ph;
    float    ff_hub_ph;
    float    ff_body_bp;
    float    ff_body_lp;
    float    ff_tape_bp;
    float    ff_tape_lp;
    uint32_t ff_rng;
} st;

static inline uint32_t xorshift32(uint32_t *s)
{
    uint32_t r = *s;
    if (r == 0)
        r = 0xA341316CU;
    r ^= r << 13;
    r ^= r >> 17;
    r ^= r << 5;
    *s = r;
    return r;
}

/* Branchless high-accuracy sin(2*pi*ph) for ph >= 0 (error < 0.0007, 0 libm calls) */
static inline float fast_sin2pi(float ph)
{
    float u = ph - (float)(int)ph;
    if (u < 0.0f)
        u += 1.0f;
    float sign = 1.0f;
    if (u >= 0.5f)
    {
        u -= 0.5f;
        sign = -1.0f;
    }
    float t = u * 4.0f - 1.0f;
    float p = (1.0f - t * t) * sign;
    float ap = (p >= 0.0f) ? p : -p;
    return p * (0.775f + 0.225f * ap);
}

static inline float gb_pade_tanh(float x)
{
    if (x > 3.2f)
        return 1.0f;
    if (x < -3.2f)
        return -1.0f;
    float x2 = x * x;
    return x * (27.0f + x2) / (27.0f + 9.0f * x2);
}

static inline int16_t float_to_s16(float x)
{
    float v = x * 32768.0f;
    if (v >= 32767.0f)
        return 32767;
    if (v <= -32768.0f)
        return -32768;
    return (int16_t)v;
}

static inline float sample_ring_linear(const float *ring, uint32_t wp, float age)
{
    if (age < 0.0f)
        age = 0.0f;
    else if (age > (float)(TAPE_RING_FRAMES - 4U))
        age = (float)(TAPE_RING_FRAMES - 4U);

    uint32_t ia = (uint32_t)age;
    float frac = age - (float)ia;
    uint32_t i0 = (wp - 1U - ia) & TAPE_RING_MASK;
    uint32_t i1 = (i0 - 1U) & TAPE_RING_MASK;
    return ring[i0] + (ring[i1] - ring[i0]) * frac;
}

static void fire_mech_click(float amp)
{
    st.click_hi_env = amp;
    st.click_lo_env = amp;
    st.click_hi_ph = 0.0f;
    st.click_lo_ph = 0.0f;
}

static void update_coeffs(unsigned int sr)
{
    if (sr < 8000U)
        sr = 44100U;
    st.sr = sr;

    float fs = (float)sr;
    st.inv_fs = 1.0f / fs;
    st.sr_scale = fs * (1.0f / 44100.0f);

    st.ctrl_period = (uint32_t)(1024.0f * st.sr_scale);
    if (st.ctrl_period < 64U)
        st.ctrl_period = 64U;

    st.click_hi_dec = expf(-950.0f / fs);
    st.click_lo_dec = expf(-420.0f / fs);

    float tone = (float)s_cfg.tone_pct * 0.01f;
    if (tone < 0.0f) tone = 0.0f;
    if (tone > 1.0f) tone = 1.0f;
    float fc_tone = 16500.0f * powf(0.094f, tone);
    if (fc_tone > fs * 0.44f)
        fc_tone = fs * 0.44f;
    st.tone_alpha = 1.0f - expf((float)(-2.0 * M_PI) * fc_tone / fs);

    st.crinkle_lp_alpha = 1.0f - expf((float)(-2.0 * M_PI * 520.0) / fs);
    st.head_alpha       = 1.0f - expf((float)(-2.0 * M_PI * 95.0) / fs);
    st.dc_pole          = powf(0.995f, 44100.0f / fs);
    st.hiss_lp_alpha    = 1.0f - expf((float)(-2.0 * M_PI * 2900.0) / fs);
    st.hiss_hp_alpha    = 1.0f - expf((float)(-2.0 * M_PI * 220.0) / fs);

    float sat = (float)s_cfg.sat_pct * 0.01f;
    if (sat < 0.0f) sat = 0.0f;
    if (sat > 1.0f) sat = 1.0f;
    st.sat_amt   = sat * 0.92f;
    st.drive     = 1.0f + st.sat_amt * 3.35f;
    st.bias      = st.sat_amt * 0.105f;
    st.bias_norm = gb_pade_tanh(st.drive * st.bias);
    float denom  = fabsf(gb_pade_tanh(st.drive));
    if (denom < 0.001f)
        denom = 1.0f;
    st.makeup = (1.0f / denom) * (1.0f / (1.0f + 0.26f * st.sat_amt));

    float h = (float)s_cfg.hiss_pct * 0.01f;
    if (h < 0.0f) h = 0.0f;
    if (h > 1.0f) h = 1.0f;
    st.hiss_base_gain = (0.20f * h + 0.80f * h * h) * 0.068f;

    float w = (float)s_cfg.wow_pct * 0.01f;
    if (w < 0.0f) w = 0.0f;
    if (w > 1.0f) w = 1.0f;
    float mean_dly_frames = 2.0f + (0.35f * w + 0.65f * w * w) * 260.0f * st.sr_scale;
    st.cur_delay_us = s_cfg.enabled
                    ? (unsigned int)(mean_dly_frames * 1000000.0f / fs)
                    : 0U;
}

void tape_deck_fx_init(void)
{
    memset(&st, 0, sizeof(st));
    st.rng    = 0xA341316CU;
    st.ff_rng = 0x6D2B79F5U;
    st.motor_mode  = TAPE_MOTOR_RUN;
    st.motor_speed = 1.0f;
    st.base_age = (double)TAPE_SEED_FRAMES;
    update_coeffs(44100U);
    st.frames_until_first_crease = 44100U * 3U;
}

void tape_deck_fx_set_params(const struct tape_deck_params *p)
{
    if (!p)
        return;
    bool was_enabled = s_cfg.enabled;
    s_cfg = *p;
    update_coeffs(st.sr ? st.sr : 44100U);
    if (!was_enabled && s_cfg.enabled)
    {
        st.seeded_frames = 0;
        st.in_accum = 0.0;
        st.base_age = (double)TAPE_SEED_FRAMES;
        st.lfo_rem = 0;
        st.motor_mode = TAPE_MOTOR_RUN;
        st.motor_speed = 1.0f;
        st.frames_until_first_crease = (uint32_t)(2.8f * (float)(st.sr ? st.sr : 44100U));
    }
}

bool tape_deck_fx_is_enabled(void)
{
    return s_cfg.enabled;
}

bool tape_deck_fx_pause_fx_enabled(void)
{
    return s_cfg.enabled && s_cfg.pause_fx;
}

float tape_deck_fx_visual_speed(void)
{
    if (!s_cfg.enabled || !s_cfg.pause_fx)
        return 1.0f;
    return st.motor_speed;
}

unsigned int tape_deck_fx_delay_us(void)
{
    return s_cfg.enabled ? st.cur_delay_us : 0U;
}

bool tape_deck_fx_is_steady_run(void)
{
    return st.motor_mode == TAPE_MOTOR_RUN &&
           st.seeded_frames >= TAPE_SEED_FRAMES &&
           !st.pause_req_pending;
}

bool tape_deck_fx_begin_pause(void)
{
    if (!s_cfg.enabled || !s_cfg.pause_fx)
        return false;

    st.ramp_start_speed = (st.motor_speed > 0.08f) ? st.motor_speed : 1.0f;
    st.ramp_prog = 0.0f;
    st.pause_req_pending = true;
    st.motor_mode = TAPE_MOTOR_SPINDOWN;
    fire_mech_click(0.12f);
    return true;
}

bool tape_deck_fx_is_stopped_idle(void)
{
    return st.motor_mode == TAPE_MOTOR_STOPPED &&
           st.click_lo_env <= 0.0005f &&
           !st.pause_req_pending;
}

bool tape_deck_fx_take_pause_request(void)
{
    if (st.pause_req_pending &&
        st.motor_mode == TAPE_MOTOR_STOPPED &&
        st.click_lo_env <= 0.0005f)
    {
        st.pause_req_pending = false;
        return true;
    }
    return false;
}

void tape_deck_fx_begin_resume(void)
{
    st.pause_req_pending = false;

    if (!s_cfg.enabled || !s_cfg.pause_fx)
    {
        st.motor_mode = TAPE_MOTOR_RUN;
        st.motor_speed = 1.0f;
        return;
    }

    bool mid_spindown = (st.motor_mode == TAPE_MOTOR_SPINDOWN &&
                         st.motor_speed > 0.14f);
    if (!mid_spindown)
    {
        st.seeded_frames = 0;
        st.in_accum = 0.0;
        st.base_age = (double)TAPE_SEED_FRAMES;
    }

    float start_v = mid_spindown ? st.motor_speed : 0.14f;
    st.ramp_start_speed = start_v;
    st.motor_speed = start_v;
    st.ramp_prog = 0.0f;
    st.spinup_rate = 1.0f / (0.28f * (float)(st.sr ? st.sr : 44100U));
    st.motor_mode = TAPE_MOTOR_SPINUP;
    fire_mech_click(0.11f);
}

void tape_deck_fx_on_seek_or_stop(void)
{
    st.pause_req_pending = false;
    st.seeded_frames = 0;
    st.in_accum = 0.0;
    st.base_age = (double)TAPE_SEED_FRAMES;
    st.crinkle_env = 0.0f;

    /* Only do pinch-roller spin-up if user just released manual FF/Rewind */
    if (st.was_manual_ff && s_cfg.enabled && s_cfg.pause_fx)
    {
        st.was_manual_ff = false;
        st.ramp_start_speed = 0.36f;
        st.motor_speed = 0.36f;
        st.ramp_prog = 0.0f;
        st.spinup_rate = 1.0f / (0.11f * (float)(st.sr ? st.sr : 44100U));
        st.motor_mode = TAPE_MOTOR_SPINUP;
    }
    else
    {
        st.was_manual_ff = false;
        st.motor_mode = TAPE_MOTOR_RUN;
        st.motor_speed = 1.0f;
    }
}

void tape_deck_fx_set_ffmode(int dir)
{
    int prev = st.ff_dir;
    if (dir == prev)
        return;

    st.ff_dir = dir;
    if (dir != 0)
        st.was_manual_ff = true;

    if (!s_cfg.enabled || !s_cfg.ff_sound)
        return;

    if (dir != 0)
    {
        st.ff_last_dir = dir;
        if (prev == 0)
        {
            st.ff_clack_hi_env = 0.10f;
            st.ff_clack_lo_env = 0.08f;
            st.ff_clack_nz_env = 0.06f;
            st.ff_clack_hi_ph = 0.0f;
            st.ff_clack_lo_ph = 0.0f;
        }
        pcm_alsa_wake_writer();
    }
    else if (prev != 0)
    {
        st.ff_clack_hi_env = 0.09f;
        st.ff_clack_lo_env = 0.075f;
        st.ff_clack_nz_env = 0.05f;
        st.ff_clack_hi_ph = 0.0f;
        st.ff_clack_lo_ph = 0.0f;
        pcm_alsa_wake_writer();
    }
}

bool tape_deck_fx_ff_active(void)
{
    if (!s_cfg.enabled || !s_cfg.ff_sound)
        return false;
    return (st.ff_dir != 0) || (st.ff_vel > 0.01f) || (st.ff_clack_lo_env > 0.001f);
}

int tape_deck_fx_need_seed_frames(unsigned int sample_rate)
{
    if (sample_rate != st.sr)
        update_coeffs(sample_rate);
    if (st.seeded_frames >= TAPE_SEED_FRAMES)
        return 0;
    return (int)(TAPE_SEED_FRAMES - st.seeded_frames);
}

double tape_deck_fx_in_demand(void)
{
    return st.in_accum;
}

void tape_deck_fx_push_input(int16_t in_l, int16_t in_r, bool is_seed)
{
    const float inv32768 = 1.0f / 32768.0f;
    uint32_t wp = st.write_pos;
    st.ring_l[wp] = (float)in_l * inv32768;
    st.ring_r[wp] = (float)in_r * inv32768;
    st.write_pos = (wp + 1U) & TAPE_RING_MASK;

    if (is_seed)
    {
        st.seeded_frames++;
        st.base_age = (double)TAPE_SEED_FRAMES;
        st.in_accum = 0.0;
    }
    else
    {
        st.base_age += 1.0;
        st.in_accum -= 1.0;
    }
}

static void trigger_crinkle(float c)
{
    uint32_t r = xorshift32(&st.rng);
    float u1 = (float)(r & 0xFFFFU) * (1.0f / 65535.0f);
    float u2 = (float)((r >> 16) & 0xFFFFU) * (1.0f / 65535.0f);

    float dur_s = 0.17f + u1 * 0.19f;
    st.crinkle_env = 1.0f;
    st.crinkle_dec = 1.0f / (dur_s * (float)st.sr);

    /* Strictly positive causal tension snag (85 .. 255 samples lookback) */
    st.crinkle_snag_amp = (85.0f + (100.0f + 70.0f * u2) * c) * st.sr_scale;

    float depth = 0.42f + 0.48f * c;
    if ((r & 1U) == 0)
    {
        st.crinkle_drop_l = depth;
        st.crinkle_drop_r = depth * (0.45f + 0.35f * u2);
    }
    else
    {
        st.crinkle_drop_l = depth * (0.45f + 0.35f * u2);
        st.crinkle_drop_r = depth;
    }
}

static void step_control_block(void)
{
    uint32_t r = xorshift32(&st.rng);
    float bi1 = ((float)((int)(r & 0xFFFFU) - 32768)) * (1.0f / 32768.0f);
    float bi2 = ((float)((int)((r >> 16) & 0xFFFFU) - 32768)) * (1.0f / 32768.0f);

    st.wow_drift_tgt = st.wow_drift_tgt * 0.78f + bi1 * 0.55f;
    if (st.wow_drift_tgt > 1.0f) st.wow_drift_tgt = 1.0f;
    if (st.wow_drift_tgt < -1.0f) st.wow_drift_tgt = -1.0f;
    st.flut_drift = st.flut_drift * 0.72f + bi2 * 0.28f;

    float c = (float)s_cfg.crinkle_pct * 0.01f;
    if (c > 0.02f && st.crinkle_env <= 0.001f && st.motor_speed > 0.5f)
    {
        if (st.frames_until_first_crease > 0)
        {
            if (st.frames_until_first_crease <= st.ctrl_period)
            {
                st.frames_until_first_crease = 0;
                if (c >= 0.15f)
                {
                    trigger_crinkle(c);
                    return;
                }
            }
            else
            {
                st.frames_until_first_crease -= st.ctrl_period;
            }
        }

        float prob = (0.0022f + 0.0165f * c * c) * c;
        uint32_t r2 = xorshift32(&st.rng);
        float u = (float)(r2 & 0xFFFFU) * (1.0f / 65535.0f);
        if (u < prob)
            trigger_crinkle(c);
    }
}

/* Updates Wow & Flutter delay target once every 32 samples (~1.4 kHz control rate) */
static inline void update_wow_subblock(float capstan_v)
{
    float w = (float)s_cfg.wow_pct * 0.01f;
    if (w <= 0.001f || capstan_v <= 0.001f)
    {
        st.dly_step_l = (0.0f - st.dly_mod_l) * (1.0f / (float)LFO_SUBBLOCK);
        st.dly_step_r = (0.0f - st.dly_mod_r) * (1.0f / (float)LFO_SUBBLOCK);
        st.lfo_rem = LFO_SUBBLOCK;
        return;
    }

    const float dt_sub = (float)LFO_SUBBLOCK * st.inv_fs * capstan_v;
    st.wow_drift += 0.008f * (st.wow_drift_tgt - st.wow_drift);

    st.wow1_ph += 0.58f * dt_sub;
    if (st.wow1_ph >= 1.0f) st.wow1_ph -= 1.0f;
    st.wow2_ph += 1.37f * dt_sub;
    if (st.wow2_ph >= 1.0f) st.wow2_ph -= 1.0f;
    st.flut1_ph += 7.4f * dt_sub;
    if (st.flut1_ph >= 1.0f) st.flut1_ph -= 1.0f;
    st.flut2_ph += 12.9f * dt_sub;
    if (st.flut2_ph >= 1.0f) st.flut2_ph -= 1.0f;

    float wow_shape = 0.35f * w + 0.65f * w * w;
    float wow_span  = wow_shape * 520.0f * st.sr_scale; /* 0 .. wow_span causal lookback */
    float flut_span = wow_shape * 52.0f * st.sr_scale;

    /* Normalized [0..1] positive lookback so 0 seed buffer delay is needed */
    float wow_raw = 0.50f * fast_sin2pi(st.wow1_ph)
                  + 0.25f * fast_sin2pi(st.wow2_ph + 0.17f)
                  + 0.25f * st.wow_drift;
    float wow_u = 0.5f + 0.5f * wow_raw;
    if (wow_u < 0.0f) wow_u = 0.0f;

    float fl_l = 0.5f + 0.35f * fast_sin2pi(st.flut1_ph)
                      + 0.15f * fast_sin2pi(st.flut2_ph);
    float fl_r = 0.5f + 0.35f * fast_sin2pi(st.flut1_ph + 0.07f)
                      - 0.15f * fast_sin2pi(st.flut2_ph + 0.03f);

    float tgt_l = wow_u * wow_span + fl_l * flut_span;
    float tgt_r = wow_u * (wow_span * 0.985f) + fl_r * flut_span + w * 3.5f * st.sr_scale;

    st.dly_step_l = (tgt_l - st.dly_mod_l) * (1.0f / (float)LFO_SUBBLOCK);
    st.dly_step_r = (tgt_r - st.dly_mod_r) * (1.0f / (float)LFO_SUBBLOCK);
    st.lfo_rem = LFO_SUBBLOCK;
}

static inline void process_core_sample(float base_age_f,
                                       float capstan_v,
                                       float stop_fade,
                                       int16_t *out_l,
                                       int16_t *out_r)
{
    if (++st.ctrl_cnt >= st.ctrl_period)
    {
        st.ctrl_cnt = 0;
        step_control_block();
    }

    if (st.lfo_rem == 0)
        update_wow_subblock(capstan_v);
    st.lfo_rem--;
    st.dly_mod_l += st.dly_step_l;
    st.dly_mod_r += st.dly_step_r;

    float crease_env = 0.0f;
    float crease_snag = 0.0f;
    float rustle = 0.0f;

    if (UNLIKELY(st.crinkle_env > 0.0f) && capstan_v > 0.02f)
    {
        float p = 1.0f - st.crinkle_env;
        float env_main = fast_sin2pi(p * 0.5f);
        float ripple = 0.62f + 0.38f * fast_sin2pi(p * 2.5f);
        crease_env = env_main * (ripple * ripple);

        /* Positive causal tension snag (0 .. snag_amp .. 0) */
        float snag_wave = 0.5f - 0.5f * fast_sin2pi(p + 0.25f);
        crease_snag = st.crinkle_snag_amp * snag_wave * env_main;

        uint32_t rn = xorshift32(&st.rng);
        float wn = ((float)((int)(rn & 0xFFFFU) - 32768)) * (1.0f / 32768.0f);
        st.crinkle_rustle_lp += 0.08f * (wn - st.crinkle_rustle_lp);
        rustle = st.crinkle_rustle_lp * crease_env *
                 ((float)s_cfg.crinkle_pct * 0.01f) * 0.032f;

        st.crinkle_env -= st.crinkle_dec * capstan_v;
        if (st.crinkle_env < 0.0f)
            st.crinkle_env = 0.0f;
    }

    float s_l = 0.0f;
    float s_r = 0.0f;

    if (LIKELY(stop_fade > 0.0f))
    {
        float dly_l = base_age_f + st.dly_mod_l + crease_snag;
        float dly_r = base_age_f + st.dly_mod_r + crease_snag * 0.93f;

        s_l = sample_ring_linear(st.ring_l, st.write_pos, dly_l);
        s_r = sample_ring_linear(st.ring_r, st.write_pos, dly_r);

        if (st.sat_amt > 0.001f)
        {
            float sat_l = (gb_pade_tanh((s_l + st.bias) * st.drive) - st.bias_norm) * st.makeup;
            float sat_r = (gb_pade_tanh((s_r + st.bias) * st.drive) - st.bias_norm) * st.makeup;
            s_l = s_l * (1.0f - st.sat_amt) + sat_l * st.sat_amt;
            s_r = s_r * (1.0f - st.sat_amt) + sat_r * st.sat_amt;

            float dc_l = s_l - st.dc_in[0] + st.dc_pole * st.dc_out[0];
            st.dc_in[0] = s_l;
            st.dc_out[0] = dc_l;
            s_l = dc_l;

            float dc_r = s_r - st.dc_in[1] + st.dc_pole * st.dc_out[1];
            st.dc_in[1] = s_r;
            st.dc_out[1] = dc_r;
            s_r = dc_r;

            float hb_gain = 0.28f * st.sat_amt;
            st.head_lp[0] += st.head_alpha * (s_l - st.head_lp[0]);
            st.head_lp[1] += st.head_alpha * (s_r - st.head_lp[1]);
            s_l += st.head_lp[0] * hb_gain;
            s_r += st.head_lp[1] * hb_gain;
        }

        st.tone_lp[0] += st.tone_alpha * (s_l - st.tone_lp[0]);
        st.tone_lp[1] += st.tone_alpha * (s_r - st.tone_lp[1]);
        s_l = st.tone_lp[0];
        s_r = st.tone_lp[1];

        if (UNLIKELY(crease_env > 0.001f))
        {
            st.crinkle_muffle_l[0] += st.crinkle_lp_alpha * (s_l - st.crinkle_muffle_l[0]);
            st.crinkle_muffle_l[1] += st.crinkle_lp_alpha * (st.crinkle_muffle_l[0] - st.crinkle_muffle_l[1]);
            st.crinkle_muffle_r[0] += st.crinkle_lp_alpha * (s_r - st.crinkle_muffle_r[0]);
            st.crinkle_muffle_r[1] += st.crinkle_lp_alpha * (st.crinkle_muffle_r[0] - st.crinkle_muffle_r[1]);

            float muf_mix = crease_env * 0.90f;
            if (muf_mix > 1.0f) muf_mix = 1.0f;
            s_l = s_l * (1.0f - muf_mix) + st.crinkle_muffle_l[1] * muf_mix;
            s_r = s_r * (1.0f - muf_mix) + st.crinkle_muffle_r[1] * muf_mix;

            float g_l = 1.0f - st.crinkle_drop_l * crease_env;
            float g_r = 1.0f - st.crinkle_drop_r * crease_env;
            if (g_l < 0.08f) g_l = 0.08f;
            if (g_r < 0.08f) g_r = 0.08f;
            s_l = s_l * g_l + rustle;
            s_r = s_r * g_r + rustle * 0.85f;
        }
        else
        {
            st.crinkle_muffle_l[0] = st.crinkle_muffle_l[1] = s_l;
            st.crinkle_muffle_r[0] = st.crinkle_muffle_r[1] = s_r;
        }

        if (st.hiss_base_gain > 0.00001f)
        {
            uint32_t rh = xorshift32(&st.rng);
            float wn_l = ((float)((int)(rh & 0xFFFFU) - 32768)) * (1.0f / 32768.0f);
            float wn_r = ((float)((int)((rh >> 16) & 0xFFFFU) - 32768)) * (1.0f / 32768.0f);

            st.hiss_lp[0]    += st.hiss_lp_alpha * (wn_l - st.hiss_lp[0]);
            st.hiss_lp[1]    += st.hiss_lp_alpha * (wn_r - st.hiss_lp[1]);
            st.hiss_hp_lp[0] += st.hiss_hp_alpha * (wn_l - st.hiss_hp_lp[0]);
            st.hiss_hp_lp[1] += st.hiss_hp_alpha * (wn_r - st.hiss_hp_lp[1]);

            float air_l = (wn_l - st.hiss_hp_lp[0]) * 0.38f + st.hiss_lp[0] * 0.62f;
            float air_r = (wn_r - st.hiss_hp_lp[1]) * 0.38f + st.hiss_lp[1] * 0.62f;

            float speed_gate = (capstan_v >= 1.0f) ? 1.0f : capstan_v;
            float hiss_gain = st.hiss_base_gain * speed_gate;
            s_l += air_l * hiss_gain;
            s_r += air_r * hiss_gain;
        }

        s_l *= stop_fade;
        s_r *= stop_fade;
    }

    if (UNLIKELY(st.click_lo_env > 0.0005f))
    {
        st.click_hi_ph += 1450.0f * st.inv_fs;
        st.click_lo_ph += 460.0f * st.inv_fs;
        float cl = fast_sin2pi(st.click_hi_ph) * st.click_hi_env * 0.55f
                 + fast_sin2pi(st.click_lo_ph) * st.click_lo_env * 0.45f;
        s_l += cl;
        s_r += cl * 0.92f;
        st.click_hi_env *= st.click_hi_dec;
        st.click_lo_env *= st.click_lo_dec;
    }

    if (s_l > 0.96f || s_l < -0.96f)
        s_l = gb_pade_tanh(s_l);
    if (s_r > 0.96f || s_r < -0.96f)
        s_r = gb_pade_tanh(s_r);

    *out_l = float_to_s16(s_l);
    *out_r = float_to_s16(s_r);
}

/* Lock-free batch processor for 99.9% steady 1.0x playback */
void tape_deck_fx_process_steady_block(const int16_t *in_interleaved,
                                       int16_t *out_interleaved,
                                       int frames,
                                       unsigned int sample_rate)
{
    if (UNLIKELY(sample_rate != st.sr))
        update_coeffs(sample_rate);

    const float inv32768 = 1.0f / 32768.0f;
    const float age_f = (float)st.base_age;

    for (int i = 0; i < frames; i++)
    {
        uint32_t wp = st.write_pos;
        st.ring_l[wp] = (float)in_interleaved[2 * i]     * inv32768;
        st.ring_r[wp] = (float)in_interleaved[2 * i + 1] * inv32768;
        st.write_pos = (wp + 1U) & TAPE_RING_MASK;

        process_core_sample(age_f,
                            1.0f,
                            1.0f,
                            &out_interleaved[2 * i],
                            &out_interleaved[2 * i + 1]);
    }
}

/* Variable-rate single-sample processor for ~300 ms Pause Spin-Down / Resume Spin-Up */
void tape_deck_fx_render_one(int16_t *out_l, int16_t *out_r)
{
    float capstan_v = 1.0f;
    float stop_fade = 1.0f;

    if (st.motor_mode == TAPE_MOTOR_SPINDOWN)
    {
        st.ramp_prog += st.inv_fs * (1.0f / 0.34f);
        if (st.ramp_prog >= 1.0f)
        {
            st.ramp_prog = 1.0f;
            st.motor_speed = 0.0f;
            st.motor_mode = TAPE_MOTOR_STOPPED;
            st.click_hi_env = 0.0f;
            st.click_lo_env = 0.0f;
            capstan_v = 0.0f;
            stop_fade = 0.0f;
        }
        else
        {
            float rem = 1.0f - st.ramp_prog;
            /* rem^1.5 approximated without libm sqrtf: rem * (0.35 + 0.65 * rem) */
            capstan_v = st.ramp_start_speed * rem * (0.35f + 0.65f * rem);
            st.motor_speed = capstan_v;
            if (rem < 0.18f)
            {
                float u = rem * (1.0f / 0.18f);
                stop_fade = u * u * (3.0f - 2.0f * u);
            }
        }
    }
    else if (st.motor_mode == TAPE_MOTOR_STOPPED)
    {
        capstan_v = 0.0f;
        stop_fade = 0.0f;
        st.motor_speed = 0.0f;
    }
    else if (st.motor_mode == TAPE_MOTOR_SPINUP)
    {
        st.ramp_prog += st.spinup_rate;
        if (st.ramp_prog >= 1.0f)
        {
            st.ramp_prog = 1.0f;
            st.motor_mode = TAPE_MOTOR_RUN;
            st.motor_speed = 1.0f;
            capstan_v = 1.0f;
        }
        else
        {
            float u = st.ramp_prog;
            float s_curve = u * u * (3.0f - 2.0f * u);
            float bounce = fast_sin2pi(u * 0.5f) * u * 0.038f;
            capstan_v = st.ramp_start_speed +
                        (1.0f - st.ramp_start_speed) * s_curve + bounce;
            st.motor_speed = capstan_v;
        }
    }
    else
    {
        st.motor_speed = 1.0f;
        capstan_v = 1.0f;
    }

    float age_f = (float)st.base_age;
    if (stop_fade > 0.0f)
    {
        st.base_age -= (double)capstan_v;
        st.in_accum += (double)capstan_v;
    }

    process_core_sample(age_f, capstan_v, stop_fade, out_l, out_r);
}

void tape_deck_fx_render_ff_period(int16_t *out_interleaved,
                                   int frames,
                                   unsigned int sample_rate)
{
    if (sample_rate < 8000U)
        sample_rate = 44100U;
    const float fs = (float)sample_rate;
    const float inv_fs = 1.0f / fs;

    const float accel = 1.0f - expf(-inv_fs / 0.085f);
    const float brake = 1.0f - expf(-inv_fs / 0.018f);
    const float dec_hi = expf(-780.0f * inv_fs);
    const float dec_lo = expf(-340.0f * inv_fs);
    const float dec_nz = expf(-1100.0f * inv_fs);

    const float f_body = 2.0f * sinf((float)M_PI * 690.0f * inv_fs);
    const float q_body = 0.55f;
    const float f_tape = 2.0f * sinf((float)M_PI * 2150.0f * inv_fs);
    const float q_tape = 0.85f;

    for (int i = 0; i < frames; i++)
    {
        int dir = st.ff_dir;
        float tgt_vel = (dir != 0) ? 1.0f : 0.0f;
        st.ff_vel += (tgt_vel - st.ff_vel) * ((dir != 0) ? accel : brake);
        if (st.ff_vel < 0.0005f && dir == 0)
            st.ff_vel = 0.0f;

        float vel = st.ff_vel;
        float sig_l = 0.0f;
        float sig_r = 0.0f;

        uint32_t r = xorshift32(&st.ff_rng);
        float wn = ((float)((int)(r & 0xFFFFU) - 32768)) * (1.0f / 32768.0f);

        if (vel > 0.001f)
        {
            float dir_pitch = (st.ff_last_dir < 0) ? 1.07f : 1.00f;
            float speed_pitch = (0.42f + 0.58f * vel) * dir_pitch;

            st.ff_hub_ph += (14.5f * speed_pitch) * inv_fs;
            if (st.ff_hub_ph >= 1.0f)
                st.ff_hub_ph -= 1.0f;
            float hub_wobble = fast_sin2pi(st.ff_hub_ph);

            float spline_wave = fast_sin2pi(st.ff_hub_ph * 6.0f + 0.25f);
            float spline_tick = (spline_wave > 0.86f) ? ((spline_wave - 0.86f) * 7.0f) : 0.0f;

            float jitter = 1.0f + 0.018f * hub_wobble + 0.008f * wn;
            st.ff_motor_ph += (225.0f * speed_pitch * jitter) * inv_fs;
            if (st.ff_motor_ph >= 1.0f)
                st.ff_motor_ph -= 1.0f;
            st.ff_gear_ph += (585.0f * speed_pitch * jitter) * inv_fs;
            if (st.ff_gear_ph >= 1.0f)
                st.ff_gear_ph -= 1.0f;

            float comm = fast_sin2pi(st.ff_motor_ph)
                       + 0.42f * fast_sin2pi(st.ff_motor_ph * 2.0f)
                       + 0.22f * fast_sin2pi(st.ff_gear_ph);
            float motor_raw = gb_pade_tanh(comm * 0.85f + wn * 0.28f);
            float hp_b = motor_raw - st.ff_body_lp - q_body * st.ff_body_bp;
            st.ff_body_bp += f_body * hp_b;
            st.ff_body_lp += f_body * st.ff_body_bp;
            float motor_body = st.ff_body_bp * 0.72f + st.ff_body_lp * 0.35f;

            float hp_t = wn - st.ff_tape_lp - q_tape * st.ff_tape_bp;
            st.ff_tape_bp += f_tape * hp_t;
            st.ff_tape_lp += f_tape * st.ff_tape_bp;
            float tape_whir = st.ff_tape_bp * (0.68f + 0.32f * hub_wobble)
                            + spline_tick * wn * 0.30f;

            float mech = (motor_body * 0.055f + tape_whir * 0.048f) * vel;
            float pan = (st.ff_last_dir < 0) ? -0.14f : 0.14f;
            sig_l += mech * (1.0f - pan);
            sig_r += mech * (1.0f + pan);
        }

        if (UNLIKELY(st.ff_clack_lo_env > 0.0005f))
        {
            st.ff_clack_hi_ph += 1720.0f * inv_fs;
            st.ff_clack_lo_ph += 540.0f * inv_fs;
            float clack = fast_sin2pi(st.ff_clack_hi_ph) * st.ff_clack_hi_env
                        + fast_sin2pi(st.ff_clack_lo_ph) * st.ff_clack_lo_env
                        + wn * st.ff_clack_nz_env;
            sig_l += clack;
            sig_r += clack;
            st.ff_clack_hi_env *= dec_hi;
            st.ff_clack_lo_env *= dec_lo;
            st.ff_clack_nz_env *= dec_nz;
        }

        out_interleaved[2 * i]     = float_to_s16(sig_l);
        out_interleaved[2 * i + 1] = float_to_s16(sig_r);
    }
}
