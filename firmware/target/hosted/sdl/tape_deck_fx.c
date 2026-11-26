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

/* Studio FX ring & delay line capacities (supports up to 192 kHz stereo) */
#define CHORUS_RING_FRAMES   8192U
#define CHORUS_RING_MASK     (CHORUS_RING_FRAMES - 1U)
#define FLANGER_RING_FRAMES  4096U
#define FLANGER_RING_MASK    (FLANGER_RING_FRAMES - 1U)
#define DELAY_RING_FRAMES    262144U
#define DELAY_RING_MASK      (DELAY_RING_FRAMES - 1U)
#define PREDELAY_RING_FRAMES 32768U
#define PREDELAY_RING_MASK   (PREDELAY_RING_FRAMES - 1U)

#define REV_NUM_COMBS        8U
#define REV_NUM_ALLPASS      4U
#define REV_COMB_CAP         8192U
#define REV_ALLPASS_CAP      4096U

static const uint16_t k_rev_comb_base[REV_NUM_COMBS] = {
    1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617
};
static const uint16_t k_rev_allpass_base[REV_NUM_ALLPASS] = {
    556, 441, 341, 225
};
#define REV_STEREO_SPREAD_44K 23.0f

enum tape_motor_mode
{
    TAPE_MOTOR_RUN = 0,
    TAPE_MOTOR_SPINDOWN,
    TAPE_MOTOR_STOPPED,
    TAPE_MOTOR_SPINUP,
};

static struct tape_deck_params s_cfg = {
    .master_enabled       = true,
    .enabled              = false,
    .wow_pct              = 55,
    .hiss_pct             = 45,
    .crinkle_pct          = 50,
    .sat_pct              = 50,
    .tone_pct             = 45,
    .pause_fx             = true,
    .ff_sound             = true,
    .chorus_on            = false,
    .chorus_rate_pct      = 35,
    .chorus_depth_pct     = 50,
    .chorus_mix_pct       = 40,
    .flanger_on           = false,
    .flanger_rate_pct     = 25,
    .flanger_depth_pct    = 60,
    .flanger_feedback_pct = 60,
    .flanger_mix_pct      = 45,
    .delay_on             = false,
    .delay_time_ms        = 360,
    .delay_feedback_pct   = 45,
    .delay_tone_pct       = 60,
    .delay_pingpong       = false,
    .delay_mix_pct        = 35,
    .reverb_on            = false,
    .reverb_size_pct      = 65,
    .reverb_damp_pct      = 45,
    .reverb_predelay_ms   = 20,
    .reverb_width_pct     = 100,
    .reverb_mix_pct       = 30,
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

    /* Studio FX: Stereo Chorus */
    uint32_t chorus_wp;
    float    chorus_ph1;
    float    chorus_ph2;
    float    chorus_inc1;
    float    chorus_inc2;
    float    chorus_center_samp;
    float    chorus_depth_samp;
    float    chorus_lp_alpha;
    float    chorus_lp_l;
    float    chorus_lp_r;
    float    chorus_dry_g;
    float    chorus_wet_g;

    /* Studio FX: Stereo Flanger */
    uint32_t flanger_wp;
    float    flanger_ph;
    float    flanger_inc;
    float    flanger_min_samp;
    float    flanger_span_samp;
    float    flanger_fb;
    float    flanger_dc_in_l;
    float    flanger_dc_out_l;
    float    flanger_dc_in_r;
    float    flanger_dc_out_r;
    float    flanger_dry_g;
    float    flanger_wet_g;

    /* Studio FX: Stereo / Ping-Pong Delay */
    uint32_t delay_wp;
    float    delay_cur_samp;
    float    delay_tgt_samp;
    float    delay_slew_alpha;
    float    delay_fb;
    float    delay_hp_alpha;
    float    delay_lp_alpha;
    float    delay_hp_l;
    float    delay_hp_r;
    float    delay_lp_l;
    float    delay_lp_r;
    float    delay_wet_g;

    /* Studio FX: True-Stereo Reverb */
    uint32_t rev_predly_wp;
    uint32_t rev_predly_samp;
    float    rev_in_hp_alpha;
    float    rev_in_hp_state;
    float    rev_feedback;
    float    rev_damp_alpha;
    float    rev_wet1;
    float    rev_wet2;
    float    rev_dry_g;
    uint32_t rev_comb_len_l[REV_NUM_COMBS];
    uint32_t rev_comb_len_r[REV_NUM_COMBS];
    uint32_t rev_comb_idx_l[REV_NUM_COMBS];
    uint32_t rev_comb_idx_r[REV_NUM_COMBS];
    float    rev_comb_lp_l[REV_NUM_COMBS];
    float    rev_comb_lp_r[REV_NUM_COMBS];
    uint32_t rev_ap_len_l[REV_NUM_ALLPASS];
    uint32_t rev_ap_len_r[REV_NUM_ALLPASS];
    uint32_t rev_ap_idx_l[REV_NUM_ALLPASS];
    uint32_t rev_ap_idx_r[REV_NUM_ALLPASS];

    /* Zero-lag 10-band Hardware-Sink Biquad EQ state */
    float    eq_z1_l[TAPE_EQ_BANDS];
    float    eq_z2_l[TAPE_EQ_BANDS];
    float    eq_z1_r[TAPE_EQ_BANDS];
    float    eq_z2_r[TAPE_EQ_BANDS];

    /* Dedicated smooth 3.5mm Headphone Low-Shelf Biquad state */
    float    hp35_z1_l;
    float    hp35_z2_l;
    float    hp35_z1_r;
    float    hp35_z2_r;
} st;

struct sink_eq_cfg
{
    bool enabled;
    int  precut_tenths;
    int  cutoff_hz[TAPE_EQ_BANDS];
    int  q_tenths[TAPE_EQ_BANDS];
    int  gain_tenths[TAPE_EQ_BANDS];
};

struct sink_eq_bank
{
    float    preamp_lin;
    uint32_t active_mask;
    float    b0[TAPE_EQ_BANDS];
    float    b1[TAPE_EQ_BANDS];
    float    b2[TAPE_EQ_BANDS];
    float    a1[TAPE_EQ_BANDS];
    float    a2[TAPE_EQ_BANDS];
};

static struct sink_eq_cfg s_eq_cfg = {
    .enabled       = false,
    .precut_tenths = 0,
    .cutoff_hz     = { 31, 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000 },
    .q_tenths      = { 7, 14, 14, 14, 14, 14, 14, 14, 14, 7 },
    .gain_tenths   = { 0, 0, 0, 0, 0, 0, 0, 0, 0, 0 },
};
static struct sink_eq_bank s_eq_banks[2];
static volatile int        s_eq_bank_idx = 0;
static volatile bool       s_eq_bypass   = false;

struct hp35_shelf_bank
{
    bool  active;
    float b0, b1, b2, a1, a2;
};

static struct hp35_shelf_bank s_hp35_banks[2];
static volatile int           s_hp35_bank_idx       = 0;
static volatile int           s_hp35_profile        = 2;     /* 0=Off, 1=Subtle, 2=Warm, 3=Deep */
static volatile bool          s_hp35_jack_inserted  = false; /* 3.5mm SW_HEADPHONE_INSERT */
static volatile bool          s_hp35_route_is_codec = true;  /* TRIMPOD_SINK_SPEAKER */

/* Separate BSS arrays for studio FX delay lines so hot scalar state stays compact in L1 cache */
static struct
{
    float chorus_l[CHORUS_RING_FRAMES];
    float chorus_r[CHORUS_RING_FRAMES];
    float flanger_l[FLANGER_RING_FRAMES];
    float flanger_r[FLANGER_RING_FRAMES];
    float delay_l[DELAY_RING_FRAMES];
    float delay_r[DELAY_RING_FRAMES];
    float predly_m[PREDELAY_RING_FRAMES];
    float rev_comb_l[REV_NUM_COMBS][REV_COMB_CAP];
    float rev_comb_r[REV_NUM_COMBS][REV_COMB_CAP];
    float rev_ap_l[REV_NUM_ALLPASS][REV_ALLPASS_CAP];
    float rev_ap_r[REV_NUM_ALLPASS][REV_ALLPASS_CAP];
} fx_buf;

static inline bool tape_mode_active(void)
{
    return s_cfg.master_enabled && s_cfg.enabled;
}

static inline bool studio_fx_active(void)
{
    return s_cfg.master_enabled &&
           (s_cfg.chorus_on || s_cfg.flanger_on || s_cfg.delay_on || s_cfg.reverb_on);
}

static inline bool sink_eq_active(void)
{
    if (!s_eq_cfg.enabled || s_eq_bypass)
        return false;
    const struct sink_eq_bank *bk = &s_eq_banks[s_eq_bank_idx];
    return (bk->active_mask != 0U) || (s_eq_cfg.precut_tenths != 0);
}

static inline bool hp35_shelf_active(void)
{
    return s_hp35_profile > 0 && s_hp35_jack_inserted && s_hp35_route_is_codec &&
           s_hp35_banks[s_hp35_bank_idx].active;
}

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

static inline float sample_pow2_ring_linear(const float *ring,
                                            uint32_t mask,
                                            uint32_t wp,
                                            float age)
{
    float max_age = (float)(mask - 3U);
    if (age < 0.0f)
        age = 0.0f;
    else if (age > max_age)
        age = max_age;

    uint32_t ia = (uint32_t)age;
    float frac = age - (float)ia;
    uint32_t i0 = (wp - 1U - ia) & mask;
    uint32_t i1 = (i0 - 1U) & mask;
    return ring[i0] + (ring[i1] - ring[i0]) * frac;
}

static inline float clamp01f(int pct)
{
    if (pct <= 0)
        return 0.0f;
    if (pct >= 100)
        return 1.0f;
    return (float)pct * 0.01f;
}

static void clear_chorus_state(void)
{
    memset(fx_buf.chorus_l, 0, sizeof(fx_buf.chorus_l));
    memset(fx_buf.chorus_r, 0, sizeof(fx_buf.chorus_r));
    st.chorus_wp = 0;
    st.chorus_lp_l = 0.0f;
    st.chorus_lp_r = 0.0f;
}

static void clear_flanger_state(void)
{
    memset(fx_buf.flanger_l, 0, sizeof(fx_buf.flanger_l));
    memset(fx_buf.flanger_r, 0, sizeof(fx_buf.flanger_r));
    st.flanger_wp = 0;
    st.flanger_dc_in_l = st.flanger_dc_out_l = 0.0f;
    st.flanger_dc_in_r = st.flanger_dc_out_r = 0.0f;
}

static void clear_delay_state(void)
{
    memset(fx_buf.delay_l, 0, sizeof(fx_buf.delay_l));
    memset(fx_buf.delay_r, 0, sizeof(fx_buf.delay_r));
    st.delay_wp = 0;
    st.delay_hp_l = st.delay_hp_r = 0.0f;
    st.delay_lp_l = st.delay_lp_r = 0.0f;
    st.delay_cur_samp = st.delay_tgt_samp;
}

static void clear_reverb_state(void)
{
    memset(fx_buf.predly_m, 0, sizeof(fx_buf.predly_m));
    memset(fx_buf.rev_comb_l, 0, sizeof(fx_buf.rev_comb_l));
    memset(fx_buf.rev_comb_r, 0, sizeof(fx_buf.rev_comb_r));
    memset(fx_buf.rev_ap_l, 0, sizeof(fx_buf.rev_ap_l));
    memset(fx_buf.rev_ap_r, 0, sizeof(fx_buf.rev_ap_r));
    memset(st.rev_comb_idx_l, 0, sizeof(st.rev_comb_idx_l));
    memset(st.rev_comb_idx_r, 0, sizeof(st.rev_comb_idx_r));
    memset(st.rev_comb_lp_l, 0, sizeof(st.rev_comb_lp_l));
    memset(st.rev_comb_lp_r, 0, sizeof(st.rev_comb_lp_r));
    memset(st.rev_ap_idx_l, 0, sizeof(st.rev_ap_idx_l));
    memset(st.rev_ap_idx_r, 0, sizeof(st.rev_ap_idx_r));
    st.rev_predly_wp = 0;
    st.rev_in_hp_state = 0.0f;
}

static void clear_eq_state(void)
{
    memset(st.eq_z1_l, 0, sizeof(st.eq_z1_l));
    memset(st.eq_z2_l, 0, sizeof(st.eq_z2_l));
    memset(st.eq_z1_r, 0, sizeof(st.eq_z1_r));
    memset(st.eq_z2_r, 0, sizeof(st.eq_z2_r));
}

static void compute_single_eq_biquad(int band,
                                     double fs,
                                     int cutoff_hz,
                                     int q_tenths,
                                     int gain_tenths,
                                     double *out_b0,
                                     double *out_b1,
                                     double *out_b2,
                                     double *out_a1,
                                     double *out_a2)
{
    double f0 = (double)cutoff_hz;
    if (f0 < 16.0)
        f0 = 16.0;
    if (f0 > fs * 0.46)
        f0 = fs * 0.46;

    double q = (double)q_tenths * 0.1;
    if (q < 0.25)
        q = 0.25;
    if (q > 18.0)
        q = 18.0;

    double A = pow(10.0, (double)gain_tenths / 400.0);
    double w0 = 2.0 * M_PI * f0 / fs;
    double cw = cos(w0);
    double sw = sin(w0);
    double alpha = sw / (2.0 * q);

    double b0, b1, b2, a0, a1, a2;
    if (band == 0)
    {
        double sq = 2.0 * sqrt(A) * alpha;
        b0 = A * ((A + 1.0) - (A - 1.0) * cw + sq);
        b1 = 2.0 * A * ((A - 1.0) - (A + 1.0) * cw);
        b2 = A * ((A + 1.0) - (A - 1.0) * cw - sq);
        a0 = (A + 1.0) + (A - 1.0) * cw + sq;
        a1 = -2.0 * ((A - 1.0) + (A + 1.0) * cw);
        a2 = (A + 1.0) + (A - 1.0) * cw - sq;
    }
    else if (band == TAPE_EQ_BANDS - 1)
    {
        double sq = 2.0 * sqrt(A) * alpha;
        b0 = A * ((A + 1.0) + (A - 1.0) * cw + sq);
        b1 = -2.0 * A * ((A - 1.0) + (A + 1.0) * cw);
        b2 = A * ((A + 1.0) + (A - 1.0) * cw - sq);
        a0 = (A + 1.0) - (A - 1.0) * cw + sq;
        a1 = 2.0 * ((A - 1.0) - (A + 1.0) * cw);
        a2 = (A + 1.0) - (A - 1.0) * cw - sq;
    }
    else
    {
        b0 = 1.0 + alpha * A;
        b1 = -2.0 * cw;
        b2 = 1.0 - alpha * A;
        a0 = 1.0 + alpha / A;
        a1 = -2.0 * cw;
        a2 = 1.0 - alpha / A;
    }

    double inv_a0 = (fabs(a0) > 1e-12) ? (1.0 / a0) : 1.0;
    *out_b0 = b0 * inv_a0;
    *out_b1 = b1 * inv_a0;
    *out_b2 = b2 * inv_a0;
    *out_a1 = a1 * inv_a0;
    *out_a2 = a2 * inv_a0;
}

static void update_eq_coeffs(unsigned int sr)
{
    double fs = (sr >= 8000U) ? (double)sr : 44100.0;
    int next_idx = 1 - s_eq_bank_idx;
    struct sink_eq_bank *bk = &s_eq_banks[next_idx];

    int pc = s_eq_cfg.precut_tenths;
    if (pc < -240) pc = -240;
    if (pc > 240)  pc = 240;
    bk->preamp_lin = (float)pow(10.0, -(double)pc / 200.0);

    uint32_t mask = 0U;
    for (int b = 0; b < TAPE_EQ_BANDS; b++)
    {
        int g = s_eq_cfg.gain_tenths[b];
        if (g == 0)
        {
            bk->b0[b] = 1.0f;
            bk->b1[b] = bk->b2[b] = bk->a1[b] = bk->a2[b] = 0.0f;
            continue;
        }
        if (g < -240) g = -240;
        if (g > 240)  g = 240;
        double b0, b1, b2, a1, a2;
        compute_single_eq_biquad(b, fs, s_eq_cfg.cutoff_hz[b], s_eq_cfg.q_tenths[b], g,
                                 &b0, &b1, &b2, &a1, &a2);
        bk->b0[b] = (float)b0;
        bk->b1[b] = (float)b1;
        bk->b2[b] = (float)b2;
        bk->a1[b] = (float)a1;
        bk->a2[b] = (float)a2;
        mask |= (1U << b);
    }
    bk->active_mask = mask;
    s_eq_bank_idx = next_idx;
}

static void clear_hp35_state(void)
{
    st.hp35_z1_l = 0.0f;
    st.hp35_z2_l = 0.0f;
    st.hp35_z1_r = 0.0f;
    st.hp35_z2_r = 0.0f;
}

/* Smooth RBJ Low-Shelf compensation tailored for AC-coupled 3.5mm headphone stage */
static void update_hp35_shelf_coeffs(unsigned int sr)
{
    double fs = (sr >= 8000U) ? (double)sr : 44100.0;
    int next_idx = 1 - s_hp35_bank_idx;
    struct hp35_shelf_bank *bk = &s_hp35_banks[next_idx];
    int prof = s_hp35_profile;

    double f0 = 95.0, q = 0.58, gain_db = 0.0, trim_db = 0.0;
    if (prof == 1)
    {
        f0 = 90.0;  q = 0.55; gain_db = 2.5; trim_db = -0.9;
    }
    else if (prof == 2)
    {
        f0 = 95.0;  q = 0.58; gain_db = 4.0; trim_db = -1.6;
    }
    else if (prof >= 3)
    {
        f0 = 105.0; q = 0.60; gain_db = 6.0; trim_db = -2.6;
    }

    if (prof <= 0 || gain_db <= 0.0)
    {
        bk->active = false;
        bk->b0 = 1.0f;
        bk->b1 = bk->b2 = bk->a1 = bk->a2 = 0.0f;
        s_hp35_bank_idx = next_idx;
        return;
    }

    if (f0 > fs * 0.45)
        f0 = fs * 0.45;
    double A = pow(10.0, gain_db / 40.0);
    double trim = pow(10.0, trim_db / 20.0);
    double w0 = 2.0 * M_PI * f0 / fs;
    double cw = cos(w0);
    double sw = sin(w0);
    double alpha = sw / (2.0 * q);
    double sq = 2.0 * sqrt(A) * alpha;

    double b0 = A * ((A + 1.0) - (A - 1.0) * cw + sq);
    double b1 = 2.0 * A * ((A - 1.0) - (A + 1.0) * cw);
    double b2 = A * ((A + 1.0) - (A - 1.0) * cw - sq);
    double a0 = (A + 1.0) + (A - 1.0) * cw + sq;
    double a1 = -2.0 * ((A - 1.0) + (A + 1.0) * cw);
    double a2 = (A + 1.0) + (A - 1.0) * cw - sq;
    double inv_a0 = (fabs(a0) > 1e-12) ? (1.0 / a0) : 1.0;

    bk->b0 = (float)(b0 * inv_a0 * trim);
    bk->b1 = (float)(b1 * inv_a0 * trim);
    bk->b2 = (float)(b2 * inv_a0 * trim);
    bk->a1 = (float)(a1 * inv_a0);
    bk->a2 = (float)(a2 * inv_a0);
    bk->active = true;
    s_hp35_bank_idx = next_idx;
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
    st.cur_delay_us = tape_mode_active()
                    ? (unsigned int)(mean_dly_frames * 1000000.0f / fs)
                    : 0U;

    /* 1. Stereo Chorus coefficients */
    float ch_rate = clamp01f(s_cfg.chorus_rate_pct);
    float ch_depth = clamp01f(s_cfg.chorus_depth_pct);
    float ch_mix = clamp01f(s_cfg.chorus_mix_pct);
    float ch_hz = 0.15f + 3.05f * ch_rate * ch_rate;
    st.chorus_inc1 = ch_hz * st.inv_fs;
    st.chorus_inc2 = (ch_hz * 0.37f) * st.inv_fs;
    st.chorus_center_samp = 0.0145f * fs;
    st.chorus_depth_samp  = (0.25f * ch_depth + 0.75f * ch_depth * ch_depth) * 0.0065f * fs;
    float fc_ch = 9500.0f;
    if (fc_ch > fs * 0.44f) fc_ch = fs * 0.44f;
    st.chorus_lp_alpha = 1.0f - expf((float)(-2.0 * M_PI) * fc_ch / fs);
    st.chorus_dry_g = 1.0f - 0.38f * ch_mix;
    st.chorus_wet_g = ch_mix * 0.86f;

    /* 2. Stereo Flanger coefficients */
    float fl_rate = clamp01f(s_cfg.flanger_rate_pct);
    float fl_depth = clamp01f(s_cfg.flanger_depth_pct);
    float fl_mix = clamp01f(s_cfg.flanger_mix_pct);
    int fb_raw = s_cfg.flanger_feedback_pct;
    if (fb_raw < -90) fb_raw = -90;
    if (fb_raw > 90)  fb_raw = 90;
    float fl_hz = 0.07f + 2.45f * fl_rate * fl_rate;
    st.flanger_inc = fl_hz * st.inv_fs;
    st.flanger_min_samp  = 0.00055f * fs;
    st.flanger_span_samp = (0.20f * fl_depth + 0.80f * fl_depth * fl_depth) * 0.0058f * fs;
    st.flanger_fb = ((float)fb_raw * 0.01f) * 0.96f;
    st.flanger_dry_g = 1.0f - 0.35f * fl_mix;
    st.flanger_wet_g = fl_mix * 0.85f;

    /* 3. Stereo / Ping-Pong Delay coefficients */
    int dly_ms = s_cfg.delay_time_ms;
    if (dly_ms < 20)   dly_ms = 20;
    if (dly_ms > 1200) dly_ms = 1200;
    float tgt_samp = ((float)dly_ms * 0.001f) * fs;
    if (tgt_samp > (float)(DELAY_RING_FRAMES - 8U))
        tgt_samp = (float)(DELAY_RING_FRAMES - 8U);
    st.delay_tgt_samp = tgt_samp;
    if (st.delay_cur_samp <= 1.0f)
        st.delay_cur_samp = tgt_samp;
    st.delay_slew_alpha = 1.0f - expf(-1.0f / (0.045f * fs));
    int dfb_raw = s_cfg.delay_feedback_pct;
    if (dfb_raw < 0)  dfb_raw = 0;
    if (dfb_raw > 92) dfb_raw = 92;
    st.delay_fb = (float)dfb_raw * 0.01f;
    float dtone = clamp01f(s_cfg.delay_tone_pct);
    float fc_dly = 1100.0f * powf(12.5f, dtone);
    if (fc_dly > fs * 0.44f) fc_dly = fs * 0.44f;
    st.delay_lp_alpha = 1.0f - expf((float)(-2.0 * M_PI) * fc_dly / fs);
    st.delay_hp_alpha = 1.0f - expf((float)(-2.0 * M_PI * 85.0) / fs);
    st.delay_wet_g = clamp01f(s_cfg.delay_mix_pct) * 0.92f;

    /* 4. True-Stereo Studio Reverb coefficients */
    float rv_size = clamp01f(s_cfg.reverb_size_pct);
    float rv_damp = clamp01f(s_cfg.reverb_damp_pct);
    float rv_width = clamp01f(s_cfg.reverb_width_pct);
    float rv_mix = clamp01f(s_cfg.reverb_mix_pct);
    int pre_ms = s_cfg.reverb_predelay_ms;
    if (pre_ms < 0)   pre_ms = 0;
    if (pre_ms > 150) pre_ms = 150;
    uint32_t pre_samp = (uint32_t)(((float)pre_ms * 0.001f) * fs);
    if (pre_samp > PREDELAY_RING_FRAMES - 4U)
        pre_samp = PREDELAY_RING_FRAMES - 4U;
    st.rev_predly_samp = pre_samp;
    st.rev_in_hp_alpha = 1.0f - expf((float)(-2.0 * M_PI * 115.0) / fs);
    st.rev_feedback = 0.70f + 0.268f * rv_size;
    /* Sample-rate-invariant HF absorption (1.6 kHz heavy damp .. 14 kHz bright) */
    float fc_damp = 14000.0f * powf(0.115f, rv_damp);
    if (fc_damp > fs * 0.44f) fc_damp = fs * 0.44f;
    st.rev_damp_alpha = 1.0f - expf((float)(-2.0 * M_PI) * fc_damp / fs);
    float wet_tot = rv_mix * 0.42f;
    st.rev_wet1 = wet_tot * (rv_width * 0.5f + 0.5f);
    st.rev_wet2 = wet_tot * ((1.0f - rv_width) * 0.5f);
    st.rev_dry_g = 1.0f - 0.22f * rv_mix;

    uint32_t spread = (uint32_t)(REV_STEREO_SPREAD_44K * st.sr_scale + 0.5f);
    if (spread < 1U) spread = 1U;
    for (uint32_t c = 0; c < REV_NUM_COMBS; c++)
    {
        uint32_t ll = (uint32_t)((float)k_rev_comb_base[c] * st.sr_scale + 0.5f);
        if (ll < 16U) ll = 16U;
        if (ll >= REV_COMB_CAP - 2U) ll = REV_COMB_CAP - 2U;
        uint32_t lr = ll + spread;
        if (lr >= REV_COMB_CAP - 2U) lr = REV_COMB_CAP - 2U;
        st.rev_comb_len_l[c] = ll;
        st.rev_comb_len_r[c] = lr;
        if (st.rev_comb_idx_l[c] >= ll) st.rev_comb_idx_l[c] = 0;
        if (st.rev_comb_idx_r[c] >= lr) st.rev_comb_idx_r[c] = 0;
    }
    for (uint32_t a = 0; a < REV_NUM_ALLPASS; a++)
    {
        uint32_t al = (uint32_t)((float)k_rev_allpass_base[a] * st.sr_scale + 0.5f);
        if (al < 8U) al = 8U;
        if (al >= REV_ALLPASS_CAP - 2U) al = REV_ALLPASS_CAP - 2U;
        uint32_t ar = al + spread;
        if (ar >= REV_ALLPASS_CAP - 2U) ar = REV_ALLPASS_CAP - 2U;
        st.rev_ap_len_l[a] = al;
        st.rev_ap_len_r[a] = ar;
        if (st.rev_ap_idx_l[a] >= al) st.rev_ap_idx_l[a] = 0;
        if (st.rev_ap_idx_r[a] >= ar) st.rev_ap_idx_r[a] = 0;
    }

    update_eq_coeffs(sr);
    update_hp35_shelf_coeffs(sr);
}

void tape_deck_fx_init(void)
{
    memset(&st, 0, sizeof(st));
    memset(&fx_buf, 0, sizeof(fx_buf));
    st.rng    = 0xA341316CU;
    st.ff_rng = 0x6D2B79F5U;
    st.motor_mode  = TAPE_MOTOR_RUN;
    st.motor_speed = 1.0f;
    st.base_age = (double)TAPE_SEED_FRAMES;
    update_coeffs(44100U);
    st.delay_cur_samp = st.delay_tgt_samp;
    st.frames_until_first_crease = 44100U * 3U;
}

void tape_deck_fx_set_params(const struct tape_deck_params *p)
{
    if (!p)
        return;
    bool prev_tape    = tape_mode_active();
    bool prev_chorus  = s_cfg.master_enabled && s_cfg.chorus_on;
    bool prev_flanger = s_cfg.master_enabled && s_cfg.flanger_on;
    bool prev_delay   = s_cfg.master_enabled && s_cfg.delay_on;
    bool prev_reverb  = s_cfg.master_enabled && s_cfg.reverb_on;

    s_cfg = *p;
    bool now_tape    = tape_mode_active();
    bool now_chorus  = s_cfg.master_enabled && s_cfg.chorus_on;
    bool now_flanger = s_cfg.master_enabled && s_cfg.flanger_on;
    bool now_delay   = s_cfg.master_enabled && s_cfg.delay_on;
    bool now_reverb  = s_cfg.master_enabled && s_cfg.reverb_on;

    update_coeffs(st.sr ? st.sr : 44100U);

    if (!prev_tape && now_tape)
    {
        st.seeded_frames = 0;
        st.in_accum = 0.0;
        st.base_age = (double)TAPE_SEED_FRAMES;
        st.lfo_rem = 0;
        st.motor_mode = TAPE_MOTOR_RUN;
        st.motor_speed = 1.0f;
        st.frames_until_first_crease = (uint32_t)(2.8f * (float)(st.sr ? st.sr : 44100U));
    }
    else if (prev_tape && !now_tape)
    {
        st.pause_req_pending = false;
        st.motor_mode = TAPE_MOTOR_RUN;
        st.motor_speed = 1.0f;
    }

    if (!prev_chorus && now_chorus)
        clear_chorus_state();
    if (!prev_flanger && now_flanger)
        clear_flanger_state();
    if (!prev_delay && now_delay)
        clear_delay_state();
    if (!prev_reverb && now_reverb)
        clear_reverb_state();
}

void tape_deck_fx_set_eq(bool enabled,
                         int precut_tenths,
                         const int cutoff_hz[TAPE_EQ_BANDS],
                         const int q_tenths[TAPE_EQ_BANDS],
                         const int gain_tenths[TAPE_EQ_BANDS])
{
    bool was_on = s_eq_cfg.enabled;
    s_eq_cfg.enabled       = enabled;
    s_eq_cfg.precut_tenths = precut_tenths;
    if (cutoff_hz && q_tenths && gain_tenths)
    {
        for (int i = 0; i < TAPE_EQ_BANDS; i++)
        {
            s_eq_cfg.cutoff_hz[i]   = cutoff_hz[i];
            s_eq_cfg.q_tenths[i]    = q_tenths[i];
            s_eq_cfg.gain_tenths[i] = gain_tenths[i];
        }
    }
    if (!was_on && enabled)
        clear_eq_state();
    update_eq_coeffs(st.sr ? st.sr : 44100U);
}

void tape_deck_fx_set_eq_bypass(bool bypass)
{
    s_eq_bypass = bypass;
}

void tape_deck_fx_set_hp35_profile(int profile)
{
    if (profile < 0) profile = 0;
    if (profile > 3) profile = 3;
    bool was_active = hp35_shelf_active();
    s_hp35_profile = profile;
    update_hp35_shelf_coeffs(st.sr ? st.sr : 44100U);
    if (!was_active && hp35_shelf_active())
        clear_hp35_state();
}

void tape_deck_fx_set_hp35_jack(bool inserted)
{
    bool was_active = hp35_shelf_active();
    s_hp35_jack_inserted = inserted;
    if (!was_active && hp35_shelf_active())
        clear_hp35_state();
}

void tape_deck_fx_set_hp35_route_codec(bool is_codec_route)
{
    bool was_active = hp35_shelf_active();
    s_hp35_route_is_codec = is_codec_route;
    if (!was_active && hp35_shelf_active())
        clear_hp35_state();
}

int tape_deck_fx_eval_eq_curve_tenths_db(double freq_hz)
{
    const double fs = 48000.0;
    if (freq_hz < 16.0)
        freq_hz = 16.0;
    if (freq_hz > fs * 0.46)
        freq_hz = fs * 0.46;

    double w = 2.0 * M_PI * freq_hz / fs;
    double c1 = cos(w);
    double s1 = sin(w);
    double c2 = cos(2.0 * w);
    double s2 = sin(2.0 * w);

    double total_mag_sq = 1.0;
    for (int b = 0; b < TAPE_EQ_BANDS; b++)
    {
        int g = s_eq_cfg.gain_tenths[b];
        if (g == 0)
            continue;
        double b0, b1, b2, a1, a2;
        compute_single_eq_biquad(b, fs, s_eq_cfg.cutoff_hz[b], s_eq_cfg.q_tenths[b], g,
                                 &b0, &b1, &b2, &a1, &a2);
        double nr = b0 + b1 * c1 + b2 * c2;
        double ni = -(b1 * s1 + b2 * s2);
        double dr = 1.0 + a1 * c1 + a2 * c2;
        double di = -(a1 * s1 + a2 * s2);
        double den = dr * dr + di * di;
        if (den > 1e-18)
            total_mag_sq *= (nr * nr + ni * ni) / den;
    }

    if (total_mag_sq <= 1e-12)
        return -240;
    double db = 10.0 * log10(total_mag_sq);
    int tenths = (int)floor(db * 10.0 + 0.5);
    if (tenths < -240) tenths = -240;
    if (tenths > 240)  tenths = 240;
    return tenths;
}

bool tape_deck_fx_is_enabled(void)
{
    return sink_eq_active() || hp35_shelf_active() ||
           tape_mode_active() || studio_fx_active();
}

bool tape_deck_fx_pause_fx_enabled(void)
{
    return tape_mode_active() && s_cfg.pause_fx;
}

float tape_deck_fx_visual_speed(void)
{
    if (!tape_mode_active() || !s_cfg.pause_fx)
        return 1.0f;
    return st.motor_speed;
}

unsigned int tape_deck_fx_delay_us(void)
{
    return tape_mode_active() ? st.cur_delay_us : 0U;
}

bool tape_deck_fx_is_steady_run(void)
{
    if (!tape_mode_active())
        return true;
    return st.motor_mode == TAPE_MOTOR_RUN &&
           st.seeded_frames >= TAPE_SEED_FRAMES &&
           !st.pause_req_pending;
}

bool tape_deck_fx_begin_pause(void)
{
    if (!tape_mode_active() || !s_cfg.pause_fx)
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
    if (!tape_mode_active())
        return false;
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

    if (!tape_mode_active() || !s_cfg.pause_fx)
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

    if (s_cfg.master_enabled)
    {
        if (s_cfg.chorus_on)
            clear_chorus_state();
        if (s_cfg.flanger_on)
            clear_flanger_state();
        if (s_cfg.delay_on)
            clear_delay_state();
        if (s_cfg.reverb_on)
            clear_reverb_state();
    }
    if (sink_eq_active())
        clear_eq_state();

    /* Only do pinch-roller spin-up if user just released manual FF/Rewind */
    if (st.was_manual_ff && tape_mode_active() && s_cfg.pause_fx)
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

    if (!tape_mode_active() || !s_cfg.ff_sound)
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
    if (!tape_mode_active() || !s_cfg.ff_sound)
        return false;
    return (st.ff_dir != 0) || (st.ff_vel > 0.01f) || (st.ff_clack_lo_env > 0.001f);
}

int tape_deck_fx_need_seed_frames(unsigned int sample_rate)
{
    if (sample_rate != st.sr)
        update_coeffs(sample_rate);
    if (!tape_mode_active())
        return 0;
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

static inline void process_sink_eq_sample(float *io_l, float *io_r)
{
    const struct sink_eq_bank *bk = &s_eq_banks[s_eq_bank_idx];
    float s_l = *io_l * bk->preamp_lin;
    float s_r = *io_r * bk->preamp_lin;
    uint32_t mask = bk->active_mask;
    if (mask != 0U)
    {
        for (int b = 0; b < TAPE_EQ_BANDS; b++)
        {
            if ((mask & (1U << b)) == 0U)
                continue;
            const float b0 = bk->b0[b];
            const float b1 = bk->b1[b];
            const float b2 = bk->b2[b];
            const float a1 = bk->a1[b];
            const float a2 = bk->a2[b];

            float yl = b0 * s_l + st.eq_z1_l[b];
            st.eq_z1_l[b] = b1 * s_l - a1 * yl + st.eq_z2_l[b];
            st.eq_z2_l[b] = b2 * s_l - a2 * yl;
            s_l = yl;

            float yr = b0 * s_r + st.eq_z1_r[b];
            st.eq_z1_r[b] = b1 * s_r - a1 * yr + st.eq_z2_r[b];
            st.eq_z2_r[b] = b2 * s_r - a2 * yr;
            s_r = yr;
        }
    }
    *io_l = s_l;
    *io_r = s_r;
}

static inline void process_hp35_shelf_sample(float *io_l, float *io_r)
{
    const struct hp35_shelf_bank *bk = &s_hp35_banks[s_hp35_bank_idx];
    if (!bk->active)
        return;
    float s_l = *io_l;
    float s_r = *io_r;

    float yl = bk->b0 * s_l + st.hp35_z1_l;
    st.hp35_z1_l = bk->b1 * s_l - bk->a1 * yl + st.hp35_z2_l;
    st.hp35_z2_l = bk->b2 * s_l - bk->a2 * yl;

    float yr = bk->b0 * s_r + st.hp35_z1_r;
    st.hp35_z1_r = bk->b1 * s_r - bk->a1 * yr + st.hp35_z2_r;
    st.hp35_z2_r = bk->b2 * s_r - bk->a2 * yr;

    *io_l = yl;
    *io_r = yr;
}

/* Zero-libm real-time studio rack: Chorus -> Flanger -> Delay -> Reverb */
static inline void process_studio_fx_sample(float *io_l, float *io_r)
{
    float s_l = *io_l;
    float s_r = *io_r;

    /* 1. Multi-Voice Stereo Chorus */
    if (s_cfg.chorus_on)
    {
        uint32_t wp = st.chorus_wp;
        fx_buf.chorus_l[wp] = s_l;
        fx_buf.chorus_r[wp] = s_r;
        uint32_t next_wp = (wp + 1U) & CHORUS_RING_MASK;
        st.chorus_wp = next_wp;

        st.chorus_ph1 += st.chorus_inc1;
        if (st.chorus_ph1 >= 1.0f)
            st.chorus_ph1 -= 1.0f;
        st.chorus_ph2 += st.chorus_inc2;
        if (st.chorus_ph2 >= 1.0f)
            st.chorus_ph2 -= 1.0f;

        float mod_slow = 0.25f * fast_sin2pi(st.chorus_ph2);
        float l1 = st.chorus_center_samp +
                   st.chorus_depth_samp * (0.75f * fast_sin2pi(st.chorus_ph1) + mod_slow);
        float l2 = st.chorus_center_samp * 1.28f +
                   st.chorus_depth_samp * (0.75f * fast_sin2pi(st.chorus_ph1 + 0.33f) - mod_slow);
        float r1 = st.chorus_center_samp * 1.04f +
                   st.chorus_depth_samp * (0.75f * fast_sin2pi(st.chorus_ph1 + 0.50f) - mod_slow);
        float r2 = st.chorus_center_samp * 1.32f +
                   st.chorus_depth_samp * (0.75f * fast_sin2pi(st.chorus_ph1 + 0.83f) + mod_slow);

        float wet_l = 0.55f * sample_pow2_ring_linear(fx_buf.chorus_l, CHORUS_RING_MASK, next_wp, l1) +
                      0.45f * sample_pow2_ring_linear(fx_buf.chorus_r, CHORUS_RING_MASK, next_wp, l2);
        float wet_r = 0.55f * sample_pow2_ring_linear(fx_buf.chorus_r, CHORUS_RING_MASK, next_wp, r1) +
                      0.45f * sample_pow2_ring_linear(fx_buf.chorus_l, CHORUS_RING_MASK, next_wp, r2);

        st.chorus_lp_l += st.chorus_lp_alpha * (wet_l - st.chorus_lp_l);
        st.chorus_lp_r += st.chorus_lp_alpha * (wet_r - st.chorus_lp_r);

        s_l = s_l * st.chorus_dry_g + st.chorus_lp_l * st.chorus_wet_g;
        s_r = s_r * st.chorus_dry_g + st.chorus_lp_r * st.chorus_wet_g;
    }

    /* 2. Quadrature Stereo Jet Flanger */
    if (s_cfg.flanger_on)
    {
        st.flanger_ph += st.flanger_inc;
        if (st.flanger_ph >= 1.0f)
            st.flanger_ph -= 1.0f;

        float u_l = 0.5f + 0.5f * fast_sin2pi(st.flanger_ph);
        float u_r = 0.5f + 0.5f * fast_sin2pi(st.flanger_ph + 0.25f);
        float dly_l = st.flanger_min_samp + u_l * st.flanger_span_samp;
        float dly_r = st.flanger_min_samp + u_r * st.flanger_span_samp;

        uint32_t wp = st.flanger_wp;
        float tap_l = sample_pow2_ring_linear(fx_buf.flanger_l, FLANGER_RING_MASK, wp, dly_l);
        float tap_r = sample_pow2_ring_linear(fx_buf.flanger_r, FLANGER_RING_MASK, wp, dly_r);

        float fb_l = gb_pade_tanh(s_l + tap_l * st.flanger_fb);
        float fb_r = gb_pade_tanh(s_r + tap_r * st.flanger_fb);

        /* DC blocker in feedback loop prevents low-end offset buildup at high feedback */
        float dc_l = fb_l - st.flanger_dc_in_l + st.dc_pole * st.flanger_dc_out_l;
        st.flanger_dc_in_l = fb_l;
        st.flanger_dc_out_l = dc_l;

        float dc_r = fb_r - st.flanger_dc_in_r + st.dc_pole * st.flanger_dc_out_r;
        st.flanger_dc_in_r = fb_r;
        st.flanger_dc_out_r = dc_r;

        fx_buf.flanger_l[wp] = dc_l;
        fx_buf.flanger_r[wp] = dc_r;
        st.flanger_wp = (wp + 1U) & FLANGER_RING_MASK;

        s_l = s_l * st.flanger_dry_g + tap_l * st.flanger_wet_g;
        s_r = s_r * st.flanger_dry_g + tap_r * st.flanger_wet_g;
    }

    /* 3. Stereo / Ping-Pong Delay with Tape-Echo Time Slew & Filtered Feedback */
    if (s_cfg.delay_on)
    {
        st.delay_cur_samp += st.delay_slew_alpha * (st.delay_tgt_samp - st.delay_cur_samp);
        uint32_t wp = st.delay_wp;

        float tap_l = sample_pow2_ring_linear(fx_buf.delay_l, DELAY_RING_MASK, wp, st.delay_cur_samp);
        float tap_r = sample_pow2_ring_linear(fx_buf.delay_r, DELAY_RING_MASK, wp, st.delay_cur_samp);

        /* 85 Hz HPF + Tone LPF inside echo feedback path */
        st.delay_hp_l += st.delay_hp_alpha * (tap_l - st.delay_hp_l);
        st.delay_hp_r += st.delay_hp_alpha * (tap_r - st.delay_hp_r);
        float hp_l = tap_l - st.delay_hp_l;
        float hp_r = tap_r - st.delay_hp_r;

        st.delay_lp_l += st.delay_lp_alpha * (hp_l - st.delay_lp_l);
        st.delay_lp_r += st.delay_lp_alpha * (hp_r - st.delay_lp_r);
        float filt_l = st.delay_lp_l;
        float filt_r = st.delay_lp_r;

        float wr_l, wr_r;
        if (s_cfg.delay_pingpong)
        {
            float mono_in = (s_l + s_r) * 0.5f;
            wr_l = gb_pade_tanh(mono_in + filt_r * st.delay_fb);
            wr_r = gb_pade_tanh(filt_l * st.delay_fb);
        }
        else
        {
            wr_l = gb_pade_tanh(s_l + filt_l * st.delay_fb);
            wr_r = gb_pade_tanh(s_r + filt_r * st.delay_fb);
        }

        fx_buf.delay_l[wp] = wr_l;
        fx_buf.delay_r[wp] = wr_r;
        st.delay_wp = (wp + 1U) & DELAY_RING_MASK;

        s_l += filt_l * st.delay_wet_g;
        s_r += filt_r * st.delay_wet_g;
    }

    /* 4. True-Stereo Studio Reverb (Pre-Delay + 8 LP-Comb/ch + 4 Allpass/ch) */
    if (s_cfg.reverb_on)
    {
        float mono_raw = (s_l + s_r) * 0.5f;
        st.rev_in_hp_state += st.rev_in_hp_alpha * (mono_raw - st.rev_in_hp_state);
        float rev_in_clean = (mono_raw - st.rev_in_hp_state) * 0.032f;

        uint32_t pwp = st.rev_predly_wp;
        fx_buf.predly_m[pwp] = rev_in_clean;
        uint32_t prp = (pwp - st.rev_predly_samp) & PREDELAY_RING_MASK;
        float pre_out = (st.rev_predly_samp == 0U) ? rev_in_clean : fx_buf.predly_m[prp];
        st.rev_predly_wp = (pwp + 1U) & PREDELAY_RING_MASK;

        float out_l = 0.0f;
        float out_r = 0.0f;
        const float fb = st.rev_feedback;
        const float damp = st.rev_damp_alpha;

        for (uint32_t c = 0; c < REV_NUM_COMBS; c++)
        {
            uint32_t il = st.rev_comb_idx_l[c];
            float yn_l = fx_buf.rev_comb_l[c][il];
            float lp_l = st.rev_comb_lp_l[c] + damp * (yn_l - st.rev_comb_lp_l[c]);
            st.rev_comb_lp_l[c] = lp_l;
            fx_buf.rev_comb_l[c][il] = pre_out + lp_l * fb;
            if (++il >= st.rev_comb_len_l[c])
                il = 0;
            st.rev_comb_idx_l[c] = il;
            out_l += yn_l;

            uint32_t ir = st.rev_comb_idx_r[c];
            float yn_r = fx_buf.rev_comb_r[c][ir];
            float lp_r = st.rev_comb_lp_r[c] + damp * (yn_r - st.rev_comb_lp_r[c]);
            st.rev_comb_lp_r[c] = lp_r;
            fx_buf.rev_comb_r[c][ir] = pre_out + lp_r * fb;
            if (++ir >= st.rev_comb_len_r[c])
                ir = 0;
            st.rev_comb_idx_r[c] = ir;
            out_r += yn_r;
        }

        for (uint32_t a = 0; a < REV_NUM_ALLPASS; a++)
        {
            uint32_t il = st.rev_ap_idx_l[a];
            float buf_l = fx_buf.rev_ap_l[a][il];
            float x_l = out_l;
            out_l = -x_l + buf_l;
            fx_buf.rev_ap_l[a][il] = x_l + buf_l * 0.5f;
            if (++il >= st.rev_ap_len_l[a])
                il = 0;
            st.rev_ap_idx_l[a] = il;

            uint32_t ir = st.rev_ap_idx_r[a];
            float buf_r = fx_buf.rev_ap_r[a][ir];
            float x_r = out_r;
            out_r = -x_r + buf_r;
            fx_buf.rev_ap_r[a][ir] = x_r + buf_r * 0.5f;
            if (++ir >= st.rev_ap_len_r[a])
                ir = 0;
            st.rev_ap_idx_r[a] = ir;
        }

        s_l = s_l * st.rev_dry_g + out_l * st.rev_wet1 + out_r * st.rev_wet2;
        s_r = s_r * st.rev_dry_g + out_r * st.rev_wet1 + out_l * st.rev_wet2;
    }

    *io_l = s_l;
    *io_r = s_r;
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

        if (sink_eq_active())
            process_sink_eq_sample(&s_l, &s_r);
        if (hp35_shelf_active())
            process_hp35_shelf_sample(&s_l, &s_r);

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

    if (studio_fx_active())
        process_studio_fx_sample(&s_l, &s_r);

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

    if (!tape_mode_active())
    {
        const bool eq_on   = sink_eq_active();
        const bool hp35_on = hp35_shelf_active();
        const bool fx_on   = studio_fx_active();
        if (!eq_on && !hp35_on && !fx_on)
        {
            if (out_interleaved != in_interleaved && frames > 0)
                memcpy(out_interleaved, in_interleaved, (size_t)frames * 2U * sizeof(int16_t));
            return;
        }
        for (int i = 0; i < frames; i++)
        {
            float s_l = (float)in_interleaved[2 * i]     * inv32768;
            float s_r = (float)in_interleaved[2 * i + 1] * inv32768;
            if (eq_on)
                process_sink_eq_sample(&s_l, &s_r);
            if (hp35_on)
                process_hp35_shelf_sample(&s_l, &s_r);
            if (fx_on)
                process_studio_fx_sample(&s_l, &s_r);
            if (s_l > 0.96f || s_l < -0.96f)
                s_l = gb_pade_tanh(s_l);
            if (s_r > 0.96f || s_r < -0.96f)
                s_r = gb_pade_tanh(s_r);
            out_interleaved[2 * i]     = float_to_s16(s_l);
            out_interleaved[2 * i + 1] = float_to_s16(s_r);
        }
        return;
    }

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
    const float k_ff_motor_gain = 0.40f; /* -8 dB quieter FF/RW motor sound */

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

            float mech = (motor_body * 0.055f + tape_whir * 0.048f) * vel * k_ff_motor_gain;
            float pan = (st.ff_last_dir < 0) ? -0.14f : 0.14f;
            sig_l += mech * (1.0f - pan);
            sig_r += mech * (1.0f + pan);
        }

        if (UNLIKELY(st.ff_clack_lo_env > 0.0005f))
        {
            st.ff_clack_hi_ph += 1720.0f * inv_fs;
            st.ff_clack_lo_ph += 540.0f * inv_fs;
            float clack = (fast_sin2pi(st.ff_clack_hi_ph) * st.ff_clack_hi_env
                         + fast_sin2pi(st.ff_clack_lo_ph) * st.ff_clack_lo_env
                         + wn * st.ff_clack_nz_env) * k_ff_motor_gain;
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
