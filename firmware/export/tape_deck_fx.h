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
 * This program is free software; you can redistribute it and/or
 * modify it under the terms of the GNU General Public License
 * as published by the Free Software Foundation; either version 2
 * of the License, or (at your option) any later version.
 *
 ****************************************************************************/
#ifndef TAPE_DECK_FX_H
#define TAPE_DECK_FX_H

#include <stdbool.h>
#include <stdint.h>

struct tape_deck_params
{
    bool master_enabled; /* Global Effects master switch (context menu & Settings) */

    /* Analog Cassette Tape Mode */
    bool enabled;
    int  wow_pct;      /* 0..100: Wow & Flutter ("Плавання швидкості") */
    int  hiss_pct;     /* 0..100: Analog Tape Hiss ("Шум стрічки") */
    int  crinkle_pct;  /* 0..100: Mylar Creases & Dropouts ("Заломи стрічки") */
    int  sat_pct;      /* 0..100: Magnetic Saturation & Head Bump ("Сатурація") */
    int  tone_pct;     /* 0..100: HF Roll-Off ("Зріз ВЧ") */
    bool pause_fx;     /* Capstan spin-down on Pause & spin-up on Resume */
    bool ff_sound;     /* Electro-mechanical motor & spool whir on FF/Rewind */

    /* Stereo Chorus */
    bool chorus_on;
    int  chorus_rate_pct;  /* 0..100 */
    int  chorus_depth_pct; /* 0..100 */
    int  chorus_mix_pct;   /* 0..100 */

    /* Stereo Flanger */
    bool flanger_on;
    int  flanger_rate_pct;     /* 0..100 */
    int  flanger_depth_pct;    /* 0..100 */
    int  flanger_feedback_pct; /* -90..+90 */
    int  flanger_mix_pct;      /* 0..100 */

    /* Stereo / Ping-Pong Delay */
    bool delay_on;
    int  delay_time_ms;      /* 40..1000 ms */
    int  delay_feedback_pct; /* 0..90 */
    int  delay_tone_pct;     /* 0..100 */
    bool delay_pingpong;
    int  delay_mix_pct;      /* 0..100 */

    /* True-Stereo Studio Reverb */
    bool reverb_on;
    int  reverb_size_pct;    /* 0..100 */
    int  reverb_damp_pct;    /* 0..100 */
    int  reverb_predelay_ms; /* 0..120 ms */
    int  reverb_width_pct;   /* 0..100 */
    int  reverb_mix_pct;     /* 0..100 */
};

#define TAPE_EQ_BANDS 10

void tape_deck_fx_init(void);
void tape_deck_fx_set_params(const struct tape_deck_params *p);
void tape_deck_fx_set_eq(bool enabled,
                         int precut_tenths,
                         const int cutoff_hz[TAPE_EQ_BANDS],
                         const int q_tenths[TAPE_EQ_BANDS],
                         const int gain_tenths[TAPE_EQ_BANDS]);
void tape_deck_fx_set_eq_bypass(bool bypass);
int  tape_deck_fx_eval_eq_curve_tenths_db(double freq_hz);
bool tape_deck_fx_is_enabled(void);
bool tape_deck_fx_pause_fx_enabled(void);
float tape_deck_fx_visual_speed(void);
unsigned int tape_deck_fx_delay_us(void);

/* Pause spin-down / Resume spin-up transport hooks */
bool tape_deck_fx_begin_pause(void);
bool tape_deck_fx_is_stopped_idle(void);
bool tape_deck_fx_is_steady_run(void);
bool tape_deck_fx_take_pause_request(void);
void tape_deck_fx_begin_resume(void);
void tape_deck_fx_on_seek_or_stop(void);

/* Fast-Forward (+1) / Rewind (-1) / Normal (0) motor sound hook */
void tape_deck_fx_set_ffmode(int dir);
bool tape_deck_fx_ff_active(void);

/* Lock-free fast block processor for steady 1.0x playback (1 in = 1 out) */
void tape_deck_fx_process_steady_block(const int16_t *in_interleaved,
                                       int16_t *out_interleaved,
                                       int frames,
                                       unsigned int sample_rate);

/* Variable-rate capstan hooks used only during ~300 ms Pause/Resume ramps */
int    tape_deck_fx_need_seed_frames(unsigned int sample_rate);
double tape_deck_fx_in_demand(void);
void   tape_deck_fx_push_input(int16_t in_l, int16_t in_r, bool is_seed);
void   tape_deck_fx_render_one(int16_t *out_l, int16_t *out_r);

void   tape_deck_fx_render_ff_period(int16_t *out_interleaved,
                                     int frames,
                                     unsigned int sample_rate);

/* Implemented in `pcm-alsa.c` to wake the ALSA writer thread immediately */
void pcm_alsa_wake_writer(void);

#endif /* TAPE_DECK_FX_H */
