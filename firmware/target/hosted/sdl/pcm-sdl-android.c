/***************************************************************************
 * UIPod: Low-latency Android SDL2 (AAudio / OpenSLES / AudioTrack) PCM sink.
 *
 * Preserves 100% parity with `pcm-alsa.c`:
 *  - Exact sample-rate lock (`allowed_changes = 0` so SDL2 resamples cleanly
 *    whenever Android hardware runs at 48 kHz while codecs output 44.1 kHz).
 *  - Real-time `sdl_audio_callback` backed by a lock-free power-of-2 SPSC
 *    stereo ring buffer (`32768` frames) kept topped up by a `TIME_CRITICAL`
 *    writer thread in `1024`-frame periods (`while` burst refill, zero underruns).
 *  - Full Walkman `tape_deck_fx` DSP pipeline (steady run, Pause spin-down,
 *    Resume spin-up, FF/Rewind electro-mechanical motor sound, tail mute).
 *  - Sub-period delay-compensated stereo visualizer ring (`pcm_viz_latest`,
 *    `pcm_viz_read_synced`, `pcm_alsa_output_delay_us`) for the 10 GPU
 *    visualizers, `Sound Racer`, and `LCD Plasma`.
 *  - Built-in 3-mode damped iPod Click Wheel detent impulse (`pcm_alsa_menu_click`)
 *    mixed directly at the hardware callback output edge (~10 ms response).
 ****************************************************************************/
#include "autoconf.h"

#include <stdlib.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <math.h>
#include <pthread.h>
#include <SDL.h>

#include "config.h"
#include "debug.h"
#include "sound.h"
#include "audiohw.h"
#include "system.h"
#include "system-sdl.h"
#include "kernel.h"
#include "panic.h"
#include "trimpod_alsa.h"

#include "pcm.h"
#include "pcm-internal.h"
#include "pcm_sampr.h"
#include "pcm_mixer.h"
#include "pcm_sink.h"
#include "tape_deck_fx.h"

#define FRAME_BYTES          4U       /* S16_LE stereo */
#define ANDROID_PERIOD_FRMS  1024U    /* ~23.2 ms at 44.1 kHz (matches Rockbox mixer) */
#define RING_FRAMES          32768U   /* Power-of-2 lock-free SPSC ring (~743 ms max) */
#define RING_MASK            (RING_FRAMES - 1U)

static SDL_AudioDeviceID     s_dev = 0;
static unsigned int          real_sample_rate = 0;
static unsigned int          last_sample_rate = 44100U;
static volatile unsigned int req_sample_rate = 0;
static unsigned int          s_hw_period_frames = ANDROID_PERIOD_FRMS;
static unsigned int          s_target_frames = 4096U;

static const void           *pcm_data = NULL;
static size_t                pcm_size = 0;
static volatile bool         dev_playing = false;

static pthread_mutex_t       pcm_mtx;
static SDL_Thread           *writer_thread = NULL;
static SDL_sem              *writer_wake = NULL;
static volatile bool         writer_run = false;

static int16_t               period_buf[ANDROID_PERIOD_FRMS * 2U];

/* --- Lock-free SPSC stereo PCM ring between `writer_fn` and `sdl_audio_callback` --- */
static int16_t               s_ring_pcm[RING_FRAMES * 2U];
static uint32_t              s_ring_w = 0;
static uint32_t              s_ring_r = 0;

static inline uint32_t ring_avail_read(void)
{
    uint32_t w = __atomic_load_n(&s_ring_w, __ATOMIC_ACQUIRE);
    uint32_t r = __atomic_load_n(&s_ring_r, __ATOMIC_RELAXED);
    return w - r;
}

static inline uint32_t ring_avail_write(void)
{
    uint32_t w = __atomic_load_n(&s_ring_w, __ATOMIC_RELAXED);
    uint32_t r = __atomic_load_n(&s_ring_r, __ATOMIC_ACQUIRE);
    uint32_t used = w - r;
    return (used < RING_FRAMES) ? (RING_FRAMES - used) : 0U;
}

static inline uint32_t ring_used_from_writer(void)
{
    uint32_t w = __atomic_load_n(&s_ring_w, __ATOMIC_RELAXED);
    uint32_t r = __atomic_load_n(&s_ring_r, __ATOMIC_ACQUIRE);
    return w - r;
}

static void ring_reset(void)
{
    __atomic_store_n(&s_ring_r, 0U, __ATOMIC_SEQ_CST);
    __atomic_store_n(&s_ring_w, 0U, __ATOMIC_SEQ_CST);
}

static void ring_push_frames(const int16_t *src, uint32_t nframes)
{
    uint32_t space = ring_avail_write();
    if (nframes > space)
        nframes = space;
    if (!nframes)
        return;

    uint32_t w = __atomic_load_n(&s_ring_w, __ATOMIC_RELAXED);
    uint32_t pos = w & RING_MASK;
    uint32_t first = RING_FRAMES - pos;
    if (first > nframes)
        first = nframes;

    memcpy(&s_ring_pcm[pos * 2U], src, (size_t)first * FRAME_BYTES);
    if (nframes > first)
    {
        memcpy(&s_ring_pcm[0], &src[first * 2U],
               (size_t)(nframes - first) * FRAME_BYTES);
    }

    __atomic_store_n(&s_ring_w, w + nframes, __ATOMIC_RELEASE);
}

static uint32_t ring_pop_frames(int16_t *dst, uint32_t want_frames)
{
    uint32_t avail = ring_avail_read();
    uint32_t got = (want_frames < avail) ? want_frames : avail;
    if (got > 0U)
    {
        uint32_t r = __atomic_load_n(&s_ring_r, __ATOMIC_RELAXED);
        uint32_t pos = r & RING_MASK;
        uint32_t first = RING_FRAMES - pos;
        if (first > got)
            first = got;

        memcpy(dst, &s_ring_pcm[pos * 2U], (size_t)first * FRAME_BYTES);
        if (got > first)
        {
            memcpy(&dst[first * 2U], &s_ring_pcm[0],
                   (size_t)(got - first) * FRAME_BYTES);
        }
        __atomic_store_n(&s_ring_r, r + got, __ATOMIC_RELEASE);
    }
    if (got < want_frames)
    {
        memset(&dst[got * 2U], 0, (size_t)(want_frames - got) * FRAME_BYTES);
    }
    return got;
}

/* --- Visualizer delay-compensated stereo PCM ring -------------------------- */
#define VIZ_PCM_FRAMES       16384U
#define VIZ_DISPLAY_LEAD_US  22000U

static int16_t               viz_pcm_ring[VIZ_PCM_FRAMES * 2U];
static volatile unsigned int viz_pcm_w = 0;
static volatile unsigned int out_delay_us = 95000U;
static volatile Uint32       out_delay_tick = 0;

extern void uipod_rs_intel_audio_tap_push(const int16_t *stereo_s16,
                                          size_t frames,
                                          uint32_t sample_rate,
                                          uint32_t output_delay_us,
                                          uint64_t now_ms);
extern int32_t  uipod_rs_manual_dj_is_active(void);
extern uint32_t uipod_rs_manual_dj_sample_rate(void);
extern size_t   uipod_rs_manual_dj_render_s16(int16_t *out_stereo_s16,
                                              size_t frames,
                                              uint32_t sample_rate);

static void viz_pcm_push(const void *src, unsigned nframes)
{
    const int16_t *s = (const int16_t *)src;
    unsigned w = viz_pcm_w;
    for (unsigned i = 0; i < nframes; i++, w++)
    {
        unsigned idx = (w & (VIZ_PCM_FRAMES - 1U)) * 2U;
        viz_pcm_ring[idx]     = s[2U * i];
        viz_pcm_ring[idx + 1] = s[2U * i + 1U];
    }
    viz_pcm_w = w;

    unsigned int sr = last_sample_rate ? last_sample_rate : real_sample_rate;
    if (!sr)
        sr = 44100U;
    uipod_rs_intel_audio_tap_push(s,
                                  (size_t)nframes,
                                  (uint32_t)sr,
                                  (uint32_t)out_delay_us,
                                  (uint64_t)SDL_GetTicks());
}

static unsigned int live_output_delay_us(void)
{
    if (!dev_playing || !s_dev)
        return 0U;
    unsigned int base = out_delay_us;
    Uint32 dt_ms = SDL_GetTicks() - out_delay_tick;
    unsigned int dt_us = (dt_ms < 250U) ? (dt_ms * 1000U) : 250000U;
    return (base > dt_us) ? (base - dt_us) : 0U;
}

unsigned pcm_viz_latest(int16_t *out, unsigned max_frames)
{
    if (!out || max_frames == 0)
        return 0;
    unsigned w = viz_pcm_w;
    unsigned n = max_frames < VIZ_PCM_FRAMES ? max_frames : VIZ_PCM_FRAMES;
    if (n > w)
        n = w;
    unsigned start = w - n;
    for (unsigned i = 0; i < n; i++)
    {
        unsigned idx = ((start + i) & (VIZ_PCM_FRAMES - 1U)) * 2U;
        out[2U * i]      = viz_pcm_ring[idx];
        out[2U * i + 1U] = viz_pcm_ring[idx + 1U];
    }
    return n;
}

unsigned pcm_viz_read_synced(int16_t *out, unsigned max_frames,
                             unsigned *sample_rate_out)
{
    unsigned sr = real_sample_rate ? real_sample_rate : last_sample_rate;
    if (!sr)
        sr = 44100U;
    if (sample_rate_out)
        *sample_rate_out = sr;
    if (!dev_playing || !out || max_frames == 0)
        return 0;

    unsigned w = viz_pcm_w;
    unsigned n = max_frames < (VIZ_PCM_FRAMES / 2U)
                     ? max_frames
                     : (VIZ_PCM_FRAMES / 2U);
    unsigned max_delay = VIZ_PCM_FRAMES - n - 1024U;
    unsigned int raw_dly_us = live_output_delay_us();
    unsigned int eff_dly_us = (raw_dly_us > VIZ_DISPLAY_LEAD_US)
                            ? (raw_dly_us - VIZ_DISPLAY_LEAD_US)
                            : 0U;
    unsigned delay_frames =
        (unsigned)(((uint64_t)eff_dly_us * (uint64_t)sr) / 1000000ULL);
    if (delay_frames > max_delay)
        delay_frames = max_delay;
    if (w <= delay_frames)
        return 0;

    unsigned speaker_frame = w - delay_frames;
    unsigned end = speaker_frame + (n / 2U);
    if (end > w)
        end = w;
    if (n > end)
        n = end;
    unsigned start = end - n;
    for (unsigned i = 0; i < n; i++)
    {
        unsigned idx = ((start + i) & (VIZ_PCM_FRAMES - 1U)) * 2U;
        out[2U * i]      = viz_pcm_ring[idx];
        out[2U * i + 1U] = viz_pcm_ring[idx + 1U];
    }
    return n;
}

/* --- iPod Click Wheel detent impulse mixer --------------------------------- */
#define CLICK_WAVE_FRAMES 96
#define CLICK_MIN_GAP_MS  22U

static int16_t       s_click_mono[CLICK_WAVE_FRAMES];
static int16_t       s_click_stereo[CLICK_WAVE_FRAMES * 2];
static volatile int  s_click_cursor = CLICK_WAVE_FRAMES; /* >= CLICK_WAVE_FRAMES => idle */
static Uint32        s_last_click_ms = 0;

static void init_click_waveform(void)
{
    double tmp[CLICK_WAVE_FRAMES];
    double sum = 0.0;
    const double pi = 3.14159265358979323846;

    for (int i = 0; i < CLICK_WAVE_FRAMES; i++)
    {
        double t = (double)i / 48000.0;
        double u = (double)i / (double)(CLICK_WAVE_FRAMES - 1);
        double atk  = 1.0 - exp(-t / 0.00004);
        double win  = 0.5 * (1.0 + cos(pi * u));
        double snap = 0.62 * sin(2.0 * pi * 5600.0 * t) * exp(-t / 0.00024);
        double body = 0.48 * sin(2.0 * pi * 2350.0 * t + 0.35) * exp(-t / 0.00042);
        double warm = 0.24 * sin(2.0 * pi * 1180.0 * t) * exp(-t / 0.00065);
        tmp[i] = (snap + body + warm) * atk * win;
        sum += tmp[i];
    }

    double dc = sum / (double)CLICK_WAVE_FRAMES;
    for (int i = 0; i < CLICK_WAVE_FRAMES; i++)
    {
        double v = (tmp[i] - dc) * 11500.0;
        if (v > 32000.0)  v = 32000.0;
        if (v < -32000.0) v = -32000.0;
        int16_t s = (int16_t)lrint(v);
        s_click_mono[i] = s;
        s_click_stereo[2 * i]     = s;
        s_click_stereo[2 * i + 1] = s;
    }
}

const void *pcm_alsa_click_samples(size_t *byte_size_out)
{
    if (byte_size_out)
        *byte_size_out = sizeof(s_click_stereo);
    return s_click_stereo;
}

bool pcm_alsa_menu_click(void)
{
    Uint32 now = SDL_GetTicks();
    if (now - s_last_click_ms < CLICK_MIN_GAP_MS)
        return true;
    s_last_click_ms = now;
    __atomic_store_n(&s_click_cursor, 0, __ATOMIC_RELEASE);
    if (!s_dev && writer_wake)
        SDL_SemPost(writer_wake);
    return true;
}

static void mix_click_into_period(int16_t *dst_stereo, int nframes)
{
    int cur = __atomic_load_n(&s_click_cursor, __ATOMIC_ACQUIRE);
    if (cur >= CLICK_WAVE_FRAMES)
        return;
    for (int i = 0; i < nframes && cur < CLICK_WAVE_FRAMES; i++, cur++)
    {
        int32_t c = s_click_mono[cur];
        int32_t l = (int32_t)dst_stereo[2 * i] + c;
        int32_t r = (int32_t)dst_stereo[2 * i + 1] + c;
        if (l > 32767)  l = 32767;
        if (l < -32768) l = -32768;
        if (r > 32767)  r = 32767;
        if (r < -32768) r = -32768;
        dst_stereo[2 * i]     = (int16_t)l;
        dst_stereo[2 * i + 1] = (int16_t)r;
    }
    __atomic_store_n(&s_click_cursor, cur, __ATOMIC_RELEASE);
}

/* --- Real-time SDL2 audio callback (AAudio / OpenSLES / AudioTrack) -------- */

static void SDLCALL sdl_audio_callback(void *userdata, Uint8 *stream, int len)
{
    (void)userdata;
    if (len <= 0)
        return;

    uint32_t want_frames = (uint32_t)len / FRAME_BYTES;
    int16_t *out = (int16_t *)stream;

    ring_pop_frames(out, want_frames);

    int tail_bytes = len - (int)(want_frames * FRAME_BYTES);
    if (tail_bytes > 0)
        memset(stream + want_frames * FRAME_BYTES, 0, (size_t)tail_bytes);

    /* Mix Click Wheel impulse right at the hardware output edge (~10 ms latency) */
    mix_click_into_period(out, (int)want_frames);

    uint32_t rem = ring_avail_read();
    unsigned int sr = real_sample_rate ? real_sample_rate : 44100U;
    uint64_t total_frames = (uint64_t)rem + (uint64_t)want_frames;
    unsigned int q_us = (unsigned int)((total_frames * 1000000ULL) / (uint64_t)sr);
    out_delay_us = q_us + 15000U + tape_deck_fx_delay_us();
    out_delay_tick = SDL_GetTicks();

    if (writer_wake && (rem + ANDROID_PERIOD_FRMS <= s_target_frames))
        SDL_SemPost(writer_wake);
}

/* --- Sink locking & frame production -------------------------------------- */

static void sink_lock(void)
{
    pthread_mutex_lock(&pcm_mtx);
}

static void sink_unlock(void)
{
    pthread_mutex_unlock(&pcm_mtx);
}

#define TAPE_SCRATCH_FRAMES 1024
static int16_t tape_raw_in[TAPE_SCRATCH_FRAMES * 2];
static int16_t tape_scratch[TAPE_SCRATCH_FRAMES * 2];

static bool tape_pull_one_frame(int16_t *out_l, int16_t *out_r)
{
    while (dev_playing)
    {
        if (!pcm_size)
        {
            if (!pcm_play_dma_complete_callback(PCM_DMAST_OK, &pcm_data, &pcm_size))
                return false;
            pcm_size &= ~(size_t)(FRAME_BYTES - 1U);
            if (!pcm_size)
                continue;
            pcm_play_dma_status_callback(PCM_DMAST_STARTED);
        }

        const int16_t *s = (const int16_t *)pcm_data;
        *out_l = s[0];
        *out_r = s[1];
        pcm_data = (const uint8_t *)pcm_data + FRAME_BYTES;
        pcm_size -= FRAME_BYTES;
        return true;
    }
    return false;
}

static void fill_period(int16_t *frames, ssize_t period_size, unsigned int sr)
{
    ssize_t frames_left = period_size;
    bool new_buffer = false;
    if (!sr)
        sr = 44100U;

    if (uipod_rs_manual_dj_is_active())
    {
        ssize_t rem = period_size;
        int16_t *out_cursor = frames;
        while (rem > 0)
        {
            ssize_t chunk = MIN(rem, (ssize_t)TAPE_SCRATCH_FRAMES);
            uipod_rs_manual_dj_render_s16(tape_raw_in, (size_t)chunk, (uint32_t)sr);
            const int16_t *src_blk = tape_raw_in;
            if (tape_deck_fx_is_enabled() && !tape_deck_fx_ff_active())
            {
                tape_deck_fx_process_steady_block(tape_raw_in, tape_scratch,
                                                  (int)chunk, sr);
                src_blk = tape_scratch;
            }
            viz_pcm_push(src_blk, (unsigned)chunk);
            sink_lock();
            pcm_copy_buffer(out_cursor, src_blk, (size_t)chunk * FRAME_BYTES);
            sink_unlock();
            out_cursor += chunk * 2;
            rem -= chunk;
        }
        return;
    }

    sink_lock();

    if (tape_deck_fx_is_enabled() &&
        !tape_deck_fx_is_stopped_idle() &&
        !tape_deck_fx_ff_active() &&
        dev_playing)
    {
        int seed = tape_deck_fx_need_seed_frames(sr);
        while (seed > 0)
        {
            int16_t sl = 0, sr_in = 0;
            if (!tape_pull_one_frame(&sl, &sr_in))
                break;
            tape_deck_fx_push_input(sl, sr_in, true);
            seed--;
        }

        if (tape_deck_fx_is_steady_run())
        {
            while (frames_left > 0 && dev_playing)
            {
                if (!pcm_size)
                {
                    new_buffer = true;
                    if (!pcm_play_dma_complete_callback(PCM_DMAST_OK, &pcm_data, &pcm_size))
                        break;
                }

                if (pcm_size % FRAME_BYTES)
                {
                    pcm_size &= ~(size_t)(FRAME_BYTES - 1U);
                    if (!pcm_size)
                        break;
                }

                ssize_t nframes = MIN((ssize_t)(pcm_size / FRAME_BYTES), frames_left);
                if (nframes > (ssize_t)TAPE_SCRATCH_FRAMES)
                    nframes = (ssize_t)TAPE_SCRATCH_FRAMES;

                memcpy(tape_raw_in, pcm_data, (size_t)nframes * FRAME_BYTES);
                pcm_data = (const uint8_t *)pcm_data + (size_t)nframes * FRAME_BYTES;
                pcm_size -= (size_t)nframes * FRAME_BYTES;

                if (new_buffer)
                {
                    new_buffer = false;
                    pcm_play_dma_status_callback(PCM_DMAST_STARTED);
                }

                sink_unlock();
                tape_deck_fx_process_steady_block(tape_raw_in, tape_scratch,
                                                  (int)nframes, sr);
                viz_pcm_push(tape_scratch, (unsigned)nframes);
                sink_lock();

                int16_t *dst = &frames[2 * (period_size - frames_left)];
                pcm_copy_buffer(dst, tape_scratch, (size_t)nframes * FRAME_BYTES);
                frames_left -= nframes;

                if (!tape_deck_fx_is_steady_run())
                    break;
            }
        }
        else
        {
            bool paused_now = false;
            while (frames_left > 0 && dev_playing && !paused_now)
            {
                ssize_t chunk = MIN(frames_left, (ssize_t)TAPE_SCRATCH_FRAMES);
                ssize_t produced = 0;

                while (produced < chunk)
                {
                    bool exhausted = false;
                    while (tape_deck_fx_in_demand() >= 1.0)
                    {
                        int16_t sl = 0, sr_in = 0;
                        if (!tape_pull_one_frame(&sl, &sr_in))
                        {
                            exhausted = true;
                            break;
                        }
                        tape_deck_fx_push_input(sl, sr_in, false);
                    }
                    if (exhausted)
                        break;

                    tape_deck_fx_render_one(&tape_scratch[2 * produced],
                                            &tape_scratch[2 * produced + 1]);
                    produced++;

                    if (tape_deck_fx_take_pause_request())
                    {
                        mixer_channel_play_pause(PCM_MIXER_CHAN_PLAYBACK, false);
                        pcm_size = 0;
                        paused_now = true;
                        break;
                    }
                }

                if (produced <= 0)
                    break;

                int16_t *dst = &frames[2 * (period_size - frames_left)];
                pcm_copy_buffer(dst, tape_scratch, (size_t)produced * FRAME_BYTES);
                viz_pcm_push(tape_scratch, (unsigned)produced);
                frames_left -= produced;
            }
        }
    }
    else
    {
        bool mute_stopped_tape = tape_deck_fx_is_enabled() &&
                                 tape_deck_fx_is_stopped_idle() &&
                                 !tape_deck_fx_ff_active() &&
                                 (mixer_channel_status(PCM_MIXER_CHAN_BEEP) != CHANNEL_PLAYING);

        while (frames_left > 0 && dev_playing)
        {
            if (!pcm_size)
            {
                new_buffer = true;
                if (!pcm_play_dma_complete_callback(PCM_DMAST_OK, &pcm_data, &pcm_size))
                    break;
            }

            if (pcm_size % FRAME_BYTES)
            {
                pcm_size &= ~(size_t)(FRAME_BYTES - 1U);
                if (!pcm_size)
                    break;
            }

            ssize_t nframes = MIN((ssize_t)(pcm_size / FRAME_BYTES), frames_left);
            int16_t *dst = &frames[2 * (period_size - frames_left)];

            if (mute_stopped_tape)
            {
                memset(dst, 0, (size_t)nframes * FRAME_BYTES);
            }
            else
            {
                pcm_copy_buffer(dst, pcm_data, (size_t)nframes * FRAME_BYTES);
                viz_pcm_push(pcm_data, (unsigned)nframes);
            }

            pcm_data    = (const uint8_t *)pcm_data + (size_t)nframes * FRAME_BYTES;
            pcm_size    -= (size_t)nframes * FRAME_BYTES;
            frames_left -= nframes;

            if (new_buffer)
            {
                new_buffer = false;
                pcm_play_dma_status_callback(PCM_DMAST_STARTED);
            }
        }
    }

    if (frames_left > 0)
        memset(&frames[2 * (period_size - frames_left)], 0,
               (size_t)frames_left * FRAME_BYTES);

    if (tape_deck_fx_ff_active())
    {
        ssize_t rem = period_size;
        while (rem > 0)
        {
            ssize_t chunk = MIN(rem, (ssize_t)TAPE_SCRATCH_FRAMES);
            int16_t *dst = &frames[2 * (period_size - rem)];
            tape_deck_fx_render_ff_period(tape_scratch, (int)chunk, sr);
            pcm_copy_buffer(dst, tape_scratch, (size_t)chunk * FRAME_BYTES);
            rem -= chunk;
        }
    }

    sink_unlock();
}

/* --- 16-tap Nuttall Polyphase Windowed-Sinc Stereo Resampler --------------- */
#define RS_TAPS        16
#define RS_PHASES      256
#define RS_RING_FRAMES 2048U
#define RS_RING_MASK   (RS_RING_FRAMES - 1U)
#define RS_PULL_CHUNK  256

static float s_sinc_tbl[RS_PHASES + 1][RS_TAPS];
static struct
{
    float        ring_l[RS_RING_FRAMES];
    float        ring_r[RS_RING_FRAMES];
    int16_t      pull_buf[RS_PULL_CHUNK * 2];
    uint32_t     w_pos;
    uint32_t     r_pos;
    double       frac;
    bool         seeded;
    uint32_t     drain_zeros_pushed;
    unsigned int aa_sr;
    unsigned int aa_dst_sr;
    float        aa_b0, aa_b1, aa_b2, aa_a1, aa_a2;
    float        aa_z1_l, aa_z2_l, aa_z1_r, aa_z2_r;
} s_rs;

static void android_rs_reset(void)
{
    s_rs.w_pos = 0;
    s_rs.r_pos = 0;
    s_rs.frac = 0.0;
    s_rs.seeded = false;
    s_rs.drain_zeros_pushed = 0;
    s_rs.aa_z1_l = s_rs.aa_z2_l = 0.0f;
    s_rs.aa_z1_r = s_rs.aa_z2_r = 0.0f;
}

static void init_android_resampler(void)
{
    const double pi = 3.14159265358979323846;
    const double fc = 0.455;
    for (int p = 0; p <= RS_PHASES; p++)
    {
        double frac = (double)p / (double)RS_PHASES;
        double sum = 0.0;
        double row[RS_TAPS];
        for (int k = 0; k < RS_TAPS; k++)
        {
            double x = (double)k - (7.0 + frac);
            double u = 0.5 + x / (double)RS_TAPS;
            double win = 0.0;
            if (u > 0.0 && u < 1.0)
            {
                win = 0.355768
                    - 0.487396 * cos(2.0 * pi * u)
                    + 0.144232 * cos(4.0 * pi * u)
                    - 0.012604 * cos(6.0 * pi * u);
            }
            double sinc = (fabs(x) < 1e-9)
                        ? (2.0 * fc)
                        : (sin(2.0 * pi * fc * x) / (pi * x));
            row[k] = sinc * win;
            sum += row[k];
        }
        double inv = (fabs(sum) > 1e-12) ? (1.0 / sum) : 1.0;
        for (int k = 0; k < RS_TAPS; k++)
            s_sinc_tbl[p][k] = (float)(row[k] * inv);
    }
    android_rs_reset();
}

static void android_rs_update_aa(unsigned int src_sr, unsigned int dst_sr)
{
    if (s_rs.aa_sr == src_sr && s_rs.aa_dst_sr == dst_sr)
        return;
    s_rs.aa_sr = src_sr;
    s_rs.aa_dst_sr = dst_sr;
    s_rs.aa_z1_l = s_rs.aa_z2_l = 0.0f;
    s_rs.aa_z1_r = s_rs.aa_z2_r = 0.0f;
    if (src_sr <= dst_sr || dst_sr == 0)
        return;
    const double pi = 3.14159265358979323846;
    double w0 = 2.0 * pi * (0.4375 * (double)dst_sr) / (double)src_sr;
    double cw = cos(w0);
    double alpha = sin(w0) / 1.4142135623730951;
    double b0 = (1.0 - cw) * 0.5;
    double b1 = 1.0 - cw;
    double b2 = (1.0 - cw) * 0.5;
    double inv_a0 = 1.0 / (1.0 + alpha);
    s_rs.aa_b0 = (float)(b0 * inv_a0);
    s_rs.aa_b1 = (float)(b1 * inv_a0);
    s_rs.aa_b2 = (float)(b2 * inv_a0);
    s_rs.aa_a1 = (float)((-2.0 * cw) * inv_a0);
    s_rs.aa_a2 = (float)((1.0 - alpha) * inv_a0);
}

static void android_rs_push_chunk(unsigned int src_sr, unsigned int dst_sr)
{
    fill_period(s_rs.pull_buf, RS_PULL_CHUNK, src_sr);
    bool use_aa = (src_sr > dst_sr);
    if (use_aa)
        android_rs_update_aa(src_sr, dst_sr);
    for (int i = 0; i < RS_PULL_CHUNK; i++)
    {
        float sl = (float)s_rs.pull_buf[2 * i];
        float sr = (float)s_rs.pull_buf[2 * i + 1];
        if (use_aa)
        {
            float yl = s_rs.aa_b0 * sl + s_rs.aa_z1_l;
            s_rs.aa_z1_l = s_rs.aa_b1 * sl - s_rs.aa_a1 * yl + s_rs.aa_z2_l;
            s_rs.aa_z2_l = s_rs.aa_b2 * sl - s_rs.aa_a2 * yl;
            sl = yl;

            float yr = s_rs.aa_b0 * sr + s_rs.aa_z1_r;
            s_rs.aa_z1_r = s_rs.aa_b1 * sr - s_rs.aa_a1 * yr + s_rs.aa_z2_r;
            s_rs.aa_z2_r = s_rs.aa_b2 * sr - s_rs.aa_a2 * yr;
            sr = yr;
        }
        uint32_t m = s_rs.w_pos & RS_RING_MASK;
        s_rs.ring_l[m] = sl;
        s_rs.ring_r[m] = sr;
        s_rs.w_pos++;
    }
}

static void copy_period_resampled(int16_t *dst_frames, unsigned int nframes)
{
    unsigned int dst_sr = real_sample_rate ? real_sample_rate : 48000U;
    unsigned int src_sr = uipod_rs_manual_dj_is_active()
                        ? (unsigned int)uipod_rs_manual_dj_sample_rate()
                        : (last_sample_rate ? last_sample_rate : dst_sr);
    bool active_src = dev_playing || tape_deck_fx_ff_active() ||
                      uipod_rs_manual_dj_is_active();
    if (active_src)
    {
        s_rs.drain_zeros_pushed = 0;
    }
    else if (!s_rs.seeded || s_rs.drain_zeros_pushed >= (uint32_t)(RS_PULL_CHUNK * 2))
    {
        if (s_rs.seeded)
            android_rs_reset();
        memset(dst_frames, 0, (size_t)nframes * FRAME_BYTES);
        return;
    }

    if (src_sr == dst_sr && !s_rs.seeded)
    {
        fill_period(dst_frames, (ssize_t)nframes, dst_sr);
        return;
    }

    if (!s_rs.seeded)
    {
        android_rs_reset();
        for (uint32_t z = 0; z < 8U; z++)
        {
            s_rs.ring_l[z] = 0.0f;
            s_rs.ring_r[z] = 0.0f;
        }
        s_rs.w_pos = 8U;
        s_rs.seeded = true;
    }

    const double step = (double)src_sr / (double)dst_sr;
    for (unsigned int i = 0; i < nframes; i++)
    {
        while ((s_rs.w_pos - s_rs.r_pos) < (uint32_t)(RS_TAPS + 4))
        {
            android_rs_push_chunk(src_sr, dst_sr);
            if (!active_src)
                s_rs.drain_zeros_pushed += (uint32_t)RS_PULL_CHUNK;
        }
        float pf = (float)(s_rs.frac * (double)RS_PHASES);
        int pi = (int)pf;
        float mu = pf - (float)pi;
        if (pi >= RS_PHASES)
        {
            pi = RS_PHASES - 1;
            mu = 1.0f;
        }
        const float *c0 = s_sinc_tbl[pi];
        const float *c1 = s_sinc_tbl[pi + 1];
        float acc_l = 0.0f, acc_r = 0.0f;
        uint32_t base = s_rs.r_pos;
        for (int k = 0; k < RS_TAPS; k++)
        {
            float w = c0[k] + (c1[k] - c0[k]) * mu;
            uint32_t m = (base + (uint32_t)k) & RS_RING_MASK;
            acc_l += s_rs.ring_l[m] * w;
            acc_r += s_rs.ring_r[m] * w;
        }
        if (acc_l > 32767.0f)       acc_l = 32767.0f;
        else if (acc_l < -32768.0f) acc_l = -32768.0f;
        if (acc_r > 32767.0f)       acc_r = 32767.0f;
        else if (acc_r < -32768.0f) acc_r = -32768.0f;
        dst_frames[2 * i]     = (int16_t)acc_l;
        dst_frames[2 * i + 1] = (int16_t)acc_r;
        s_rs.frac += step;
        uint32_t adv = (uint32_t)s_rs.frac;
        s_rs.r_pos += adv;
        s_rs.frac -= (double)adv;
    }
}

static bool open_sdl_audio(unsigned int want_sr)
{
    if (s_dev)
        return true;

    ring_reset();

    if (!(SDL_WasInit(SDL_INIT_AUDIO) & SDL_INIT_AUDIO))
    {
        if (SDL_InitSubSystem(SDL_INIT_AUDIO) < 0)
            return false;
    }

    unsigned int target_sr = want_sr ? want_sr : 48000U;

    SDL_AudioSpec want, have;
    SDL_zero(want);
    SDL_zero(have);
    want.freq     = (int)target_sr;
    want.format   = AUDIO_S16LSB;
    want.channels = 2;
    want.samples  = ANDROID_PERIOD_FRMS;
    want.callback = sdl_audio_callback;

    s_dev = SDL_OpenAudioDevice(NULL, 0, &want, &have, 0);
    if (!s_dev)
        return false;

    real_sample_rate = (have.freq > 0) ? (unsigned int)have.freq : target_sr;
    if (!last_sample_rate)
        last_sample_rate = real_sample_rate;
    s_hw_period_frames = (have.samples > 0) ? (unsigned int)have.samples : ANDROID_PERIOD_FRMS;

    unsigned int min_target = s_hw_period_frames * 4U;
    if (min_target < 4096U)
        min_target = 4096U;
    if (min_target > RING_FRAMES - (ANDROID_PERIOD_FRMS * 2U))
        min_target = RING_FRAMES - (ANDROID_PERIOD_FRMS * 2U);
    s_target_frames = min_target;

    SDL_PauseAudioDevice(s_dev, 0);
    return true;
}

static int writer_fn(void *unused)
{
    (void)unused;

    SDL_SetThreadPriority(SDL_THREAD_PRIORITY_TIME_CRITICAL);

    /* Open Android AAudio/AudioTrack once at native 48 kHz and keep it open
     * permanently — sample-rate switches resample seamlessly in `s_rs` with
     * zero `SDL_CloseAudioDevice` / `ring_reset()` dropouts! */
    open_sdl_audio(48000U);

    while (writer_run)
    {
        unsigned int want_sr = req_sample_rate;
        if (want_sr)
        {
            req_sample_rate = 0;
            last_sample_rate = want_sr;
        }

        if (!s_dev)
        {
            if (!open_sdl_audio(48000U))
            {
                SDL_Delay(200);
                continue;
            }
        }

        bool producing = dev_playing || tape_deck_fx_ff_active() ||
                         uipod_rs_manual_dj_is_active() || s_rs.seeded;
        if (!producing)
        {
            SDL_SemWaitTimeout(writer_wake, 20);
            continue;
        }

        /* Burst-fill the SPSC ring up to `s_target_frames` in one wakeup,
         * matching `pcm-alsa.c` (`while (space >= period_size)`). */
        bool wrote_any = false;
        while (writer_run &&
               (dev_playing || tape_deck_fx_ff_active() ||
                uipod_rs_manual_dj_is_active() || s_rs.seeded))
        {
            unsigned int pending_sr = req_sample_rate;
            if (pending_sr)
            {
                req_sample_rate = 0;
                last_sample_rate = pending_sr;
            }

            uint32_t used = ring_used_from_writer();
            uint32_t space = ring_avail_write();
            if (used >= s_target_frames || space < ANDROID_PERIOD_FRMS)
                break;

            copy_period_resampled(period_buf, ANDROID_PERIOD_FRMS);
            ring_push_frames(period_buf, ANDROID_PERIOD_FRMS);
            wrote_any = true;
        }

        if (!wrote_any)
            SDL_SemWaitTimeout(writer_wake, 8);
    }

    if (s_dev)
    {
        SDL_CloseAudioDevice(s_dev);
        s_dev = 0;
    }
    return 0;
}

static void sink_set_freq(uint16_t freq)
{
    unsigned int sr = hw_freq_sampr[freq];
    if (sr != last_sample_rate)
    {
        req_sample_rate = sr;
        if (writer_wake)
            SDL_SemPost(writer_wake);
    }
}

static void sink_dma_start(const void *addr, size_t size)
{
    pcm_data = addr;
    pcm_size = size;
    dev_playing = true;
    if (writer_wake)
        SDL_SemPost(writer_wake);
}

static void sink_dma_stop(void)
{
    dev_playing = false;
    pcm_size = 0;
}

static void sink_dma_init(void)
{
    pthread_mutexattr_t attr;
    pthread_mutexattr_init(&attr);
    pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);
    pthread_mutex_init(&pcm_mtx, &attr);

    last_sample_rate = hw_freq_sampr[HW_FREQ_DEFAULT];
    init_android_resampler();
    init_click_waveform();
    tape_deck_fx_init();
    ring_reset();

    writer_wake = SDL_CreateSemaphore(0);
    writer_run = true;
    writer_thread = SDL_CreateThread(writer_fn, "uipod-pcm-android", NULL);
    if (!writer_thread)
        panicf("Could not create Android audio thread");
}

static void sink_dma_postinit(void)
{
}

unsigned int pcm_alsa_output_delay_us(void)
{
    return live_output_delay_us();
}

void pcm_alsa_wake_writer(void)
{
    if (writer_wake)
        SDL_SemPost(writer_wake);
}

void pcm_alsa_set_direct_hw(bool enabled)
{
    (void)enabled;
}

void pcm_alsa_shutdown(void)
{
    if (!writer_thread)
        return;
    writer_run = false;
    if (writer_wake)
        SDL_SemPost(writer_wake);
    SDL_WaitThread(writer_thread, NULL);
    writer_thread = NULL;
}

struct pcm_sink builtin_pcm_sink = {
    .caps = {
        .samprs       = hw_freq_sampr,
        .num_samprs   = HW_NUM_FREQ,
        .default_freq = HW_FREQ_DEFAULT,
    },
    .ops = {
        .init     = sink_dma_init,
        .postinit = sink_dma_postinit,
        .set_freq = sink_set_freq,
        .lock     = sink_lock,
        .unlock   = sink_unlock,
        .play     = sink_dma_start,
        .stop     = sink_dma_stop,
    },
};
