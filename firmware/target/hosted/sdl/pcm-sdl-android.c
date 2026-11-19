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

static void fill_period(int16_t *frames, ssize_t period_size)
{
    ssize_t frames_left = period_size;
    bool new_buffer = false;
    unsigned int sr = real_sample_rate ? real_sample_rate : last_sample_rate;
    if (!sr)
        sr = 44100U;

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

static bool open_sdl_audio(unsigned int want_sr)
{
    if (s_dev)
    {
        SDL_CloseAudioDevice(s_dev);
        s_dev = 0;
    }

    ring_reset();

    if (!(SDL_WasInit(SDL_INIT_AUDIO) & SDL_INIT_AUDIO))
    {
        if (SDL_InitSubSystem(SDL_INIT_AUDIO) < 0)
            return false;
    }

    unsigned int target_sr = want_sr ? want_sr : 44100U;

    SDL_AudioSpec want, have;
    SDL_zero(want);
    SDL_zero(have);
    want.freq     = (int)target_sr;
    want.format   = AUDIO_S16LSB;
    want.channels = 2;
    want.samples  = ANDROID_PERIOD_FRMS;
    want.callback = sdl_audio_callback;

    /* Pass allowed_changes = 0 so SDL2 guarantees exact `want.freq` / S16 stereo
     * to Rockbox's mixer and runs SDL2's internal resampler when Android's
     * AAudio / OpenSLES / AudioTrack hardware opens at 48 kHz. */
    s_dev = SDL_OpenAudioDevice(NULL, 0, &want, &have, 0);
    if (!s_dev)
        return false;

    real_sample_rate = (have.freq > 0) ? (unsigned int)have.freq : target_sr;
    last_sample_rate = real_sample_rate;
    s_hw_period_frames = (have.samples > 0) ? (unsigned int)have.samples : ANDROID_PERIOD_FRMS;

    /* Always keep >= 4 hardware callback blocks (and at least 4096 frames ≈ 93 ms)
     * pre-buffered in the SPSC ring so neither AAudio bursts nor AudioTrack
     * `minBufferSize` pulls can ever underrun. */
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

    /* Open the output stream right away and keep it warm while UIPod is running
     * so track starts, pauses, and Click Wheel clicks never pop or stall. */
    open_sdl_audio(last_sample_rate ? last_sample_rate : 44100U);

    while (writer_run)
    {
        unsigned int want_sr = req_sample_rate;
        if (want_sr && (!s_dev || want_sr != last_sample_rate))
        {
            req_sample_rate = 0;
            if (!open_sdl_audio(want_sr))
            {
                SDL_Delay(100);
                continue;
            }
        }

        if (!s_dev)
        {
            if (!open_sdl_audio(last_sample_rate ? last_sample_rate : 44100U))
            {
                SDL_Delay(200);
                continue;
            }
        }

        bool producing = dev_playing || tape_deck_fx_ff_active();
        if (!producing)
        {
            SDL_SemWaitTimeout(writer_wake, 20);
            continue;
        }

        /* Burst-fill the SPSC ring up to `s_target_frames` in one wakeup,
         * matching `pcm-alsa.c` (`while (space >= period_size)`). */
        bool wrote_any = false;
        while (writer_run && !req_sample_rate &&
               (dev_playing || tape_deck_fx_ff_active()))
        {
            uint32_t used = ring_used_from_writer();
            uint32_t space = ring_avail_write();
            if (used >= s_target_frames || space < ANDROID_PERIOD_FRMS)
                break;

            fill_period(period_buf, (ssize_t)ANDROID_PERIOD_FRMS);
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
