/***************************************************************************
 * UIPod: Dual-Deck Smart DJ Crossfade C Coordinator (`uipod_dj.c`).
 ****************************************************************************/
#include "config.h"
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

#include "kernel.h"
#include "metadata.h"
#include "pcm_mixer.h"
#include "pcmbuf.h"
#include "playback.h"
#include "settings.h"
#include "uipod_dj.h"
#include "uipod_rs_codecs.h"
#include "uipod_rs_track_intel.h"
#include "uipod_track_intel.h"

#define DJ_MIN_TRACK_DURATION_MS 9000UL
#define DJ_RENDER_CHUNK_FRAMES   1024
#define DJ_QUICK_CUT_MAX_FRAMES  (44100 * 2 + 8820) /* ~2.2s @ 44.1 kHz */

static unsigned long dj_tail_capture_lead_ms(void)
{
    int mode = global_settings.dj_crossfade_mode;
    int len  = global_settings.dj_transition_len;
    if (mode == 1) /* Classic Equal-Power Fade: 4s / 8s / 12s */
    {
        if (len == 0) return 6500UL;
        if (len == 2) return 14500UL;
        return 10500UL;
    }
    /* Smart DJ Beatmatch (4 Bars / 8 Bars) */
    return (len == 0) ? 12000UL : 17500UL;
}

static bool s_defer_track_stamp = false;
static long s_last_manual_skip_tick = 0;
static int  s_pending_skip_delta = 0;
static bool s_gapless_checked_for_track = false;
static char s_checked_path[MAX_PATH] = "";
static bool s_bypass_current_transition = false;

static int16_t s_mix_out_buf[DJ_RENDER_CHUNK_FRAMES * 2];
static int16_t s_spill_buf[DJ_RENDER_CHUNK_FRAMES * 2];

static bool is_nsf_track_id3(const struct mp3entry *id3)
{
    if (!id3)
        return false;
    if (id3->codectype == AFMT_NSF)
        return true;
    const char *p = id3->path;
    if (!p || !p[0])
        return false;
    char clean[MAX_PATH];
    if (uipod_rs_nsf_parse_uri(p, clean, sizeof(clean)) >= 0)
        return true;
    const char *dot = strrchr(p, '.');
    return dot && (strcasecmp(dot, ".nsf") == 0 || strcasecmp(dot, ".nsfe") == 0);
}

void uipod_dj_apply_settings(void)
{
    uipod_rs_dj_set_config((uint8_t)global_settings.dj_crossfade_mode,
                           (uint8_t)global_settings.dj_transition_len,
                           (uint8_t)global_settings.dj_max_stretch,
                           global_settings.dj_keep_album_gapless ? 1 : 0,
                           global_settings.dj_quick_cut_next ? 1 : 0);
    if (global_settings.dj_crossfade_mode == 0)
    {
        s_defer_track_stamp = false;
        s_gapless_checked_for_track = false;
        s_bypass_current_transition = false;
    }
}

void uipod_dj_reset(void)
{
    s_defer_track_stamp = false;
    s_gapless_checked_for_track = false;
    s_bypass_current_transition = false;
    s_checked_path[0] = '\0';
    uipod_rs_dj_reset();
}

static bool evaluate_bypass_for_track(const struct mp3entry *cur_id3)
{
    if (!cur_id3 || !cur_id3->path[0])
        return true;

    if (s_gapless_checked_for_track && strcmp(s_checked_path, cur_id3->path) == 0)
        return s_bypass_current_transition;

    const struct mp3entry *next_id3 = audio_next_track();
    if (!next_id3 || !next_id3->path[0])
        return true; /* Wait until next track metadata is buffered */

    strlcpy(s_checked_path, cur_id3->path, sizeof(s_checked_path));
    s_gapless_checked_for_track = true;

    /* Skip crossfade on single-track repeat, Single Mode auto-pause, or Endless NSF Loop */
    if (global_settings.repeat_mode == REPEAT_ONE ||
        global_settings.single_mode != SINGLE_MODE_OFF ||
        (is_nsf_track_id3(cur_id3) && uipod_rs_nsf_get_endless_loop() != 0))
    {
        s_bypass_current_transition = true;
        return true;
    }

    const char *alb_a = cur_id3->album ? cur_id3->album : "";
    const char *alb_b = next_id3->album ? next_id3->album : "";
    int trk_a = cur_id3->tracknum;
    int trk_b = next_id3->tracknum;

    s_bypass_current_transition = uipod_rs_dj_should_gapless_album(
        global_settings.playlist_shuffle ? 1 : 0,
        alb_a, trk_a, alb_b, trk_b) != 0;
    return s_bypass_current_transition;
}

bool uipod_dj_is_passthrough(const struct mp3entry *cur_id3)
{
    /* Always check active mixer phase FIRST:
     * - When `Crossfade = OFF` and `Idle`, `uipod_rs_dj_get_phase()` is a single
     *   lock-free atomic load returning `0` -> pure 0% CPU Rockbox gapless bypass.
     * - If `Crossfade` was switched `Off` mid-capture, `phase` stays `> 0` until
     *   100% of already-captured `Deck A` frames finish draining to `pcmbuf`! */
    int phase = uipod_rs_dj_get_phase();
    if (phase != 0)
        return false;

    if (global_settings.dj_crossfade_mode == 0)
        return true;

    if (!cur_id3 || cur_id3->length < DJ_MIN_TRACK_DURATION_MS)
        return true;

    unsigned long len_ms = cur_id3->length;
    unsigned long el_ms  = cur_id3->elapsed;
    if (is_nsf_track_id3(cur_id3))
    {
        uint32_t rs_dur = uipod_rs_codec_duration_ms();
        uint32_t rs_el  = uipod_rs_codec_elapsed_ms();
        if (rs_dur >= DJ_MIN_TRACK_DURATION_MS)
        {
            len_ms = rs_dur;
            el_ms  = rs_el;
        }
    }

    if (el_ms + dj_tail_capture_lead_ms() < len_ms)
        return true;

    if (evaluate_bypass_for_track(cur_id3))
        return true;

    return false;
}

static void drain_ready_mix_to_sink(const struct mp3entry *cur_id3,
                                    uipod_dj_pcm_sink_fn sink_fn)
{
    long off = cur_id3 ? (long)cur_id3->offset : 0;

    while (uipod_rs_dj_get_phase() >= 3)
    {
        int mid_crossed = 0;
        uint32_t chunk_el_ms = 0;
        size_t got = uipod_rs_dj_render_s16_timed(s_mix_out_buf,
                                                  DJ_RENDER_CHUNK_FRAMES,
                                                  &mid_crossed,
                                                  &chunk_el_ms);
        if (got == 0)
            break;

        sink_fn(s_mix_out_buf, (int)got, (unsigned long)chunk_el_ms, off);

        if (mid_crossed && s_defer_track_stamp)
        {
            s_defer_track_stamp = false;
            pcmbuf_dj_commit_midpoint_handoff();
        }
    }
}

void uipod_dj_process_codec_frames(const struct mp3entry *cur_id3,
                                   const int16_t *stereo_s16,
                                   int frames,
                                   uipod_dj_pcm_sink_fn sink_fn)
{
    if (!stereo_s16 || frames <= 0 || !sink_fn)
        return;

    int phase = uipod_rs_dj_get_phase();
    if (phase == 0)
    {
        /* Transition into `CapturingDeckA` (`phase = 1`) */
        pcmbuf_set_low_latency(false);
        uint32_t sr = (uint32_t)mixer_get_frequency();
        if (sr == 0)
            sr = 44100;
        const char *alb_a = (cur_id3 && cur_id3->album) ? cur_id3->album : "";
        int trk_a = cur_id3 ? cur_id3->tracknum : 0;
        unsigned long len_ms = cur_id3 ? cur_id3->length : 0;
        unsigned long el_ms  = cur_id3 ? cur_id3->elapsed : 0;
        if (is_nsf_track_id3(cur_id3))
        {
            uint32_t rs_dur = uipod_rs_codec_duration_ms();
            uint32_t rs_el  = uipod_rs_codec_elapsed_ms();
            if (rs_dur > 0)
            {
                len_ms = rs_dur;
                el_ms  = rs_el;
            }
        }
        uipod_rs_dj_begin_deck_a(sr,
                                 alb_a,
                                 trk_a,
                                 global_settings.playlist_shuffle ? 1 : 0,
                                 0);
        uipod_rs_dj_set_track_timing((uint32_t)el_ms, (uint32_t)len_ms, 0);
        float bpm_a = 0.0f, conf_a = 0.0f;
        uint64_t key_a = uipod_track_intel_key_for_id3(cur_id3);
        if (key_a != 0)
            (void)uipod_rs_intel_lookup_bpm(key_a, &bpm_a, &conf_a);
        uipod_rs_dj_set_bpm_hints(bpm_a, conf_a, 0.0f, 0.0f);
        phase = 1;
    }

    if (phase == 1)
    {
        /* Losslessly append into `Deck A` tail and drain any spilled older frames
         * stamped with their exact source audio `spill_el_ms` (never `Track1.length`!). */
        long off = cur_id3 ? (long)cur_id3->offset : 0;
        uipod_rs_dj_push_deck_a_frames(stereo_s16, (size_t)frames);
        for (;;)
        {
            uint32_t spill_el_ms = 0;
            size_t spilled = uipod_rs_dj_pop_spill_s16(
                s_spill_buf, DJ_RENDER_CHUNK_FRAMES, &spill_el_ms);
            if (spilled == 0)
                break;
            sink_fn(s_spill_buf, (int)spilled, (unsigned long)spill_el_ms, off);
        }
        return;
    }

    if (phase >= 3)
    {
        /* Draining captured frames after Crossfade was switched Off mid-capture */
        drain_ready_mix_to_sink(cur_id3, sink_fn);
        if (uipod_rs_dj_get_phase() == 0)
        {
            unsigned long el = cur_id3 ? cur_id3->elapsed : 0;
            long off = cur_id3 ? (long)cur_id3->offset : 0;
            sink_fn(stereo_s16, frames, el, off);
            return;
        }
    }

    /* Phase >= 2: feeding incoming Deck B & draining rendered Dual-Deck mix */
    int ready = uipod_rs_dj_push_deck_b(stereo_s16, (size_t)frames, 0);
    if (ready)
        drain_ready_mix_to_sink(cur_id3, sink_fn);

    /* If Deck B finished draining its transition intro, remaining future calls hit passthrough */
}

void uipod_dj_on_codec_eof(const struct mp3entry *cur_id3,
                           uipod_dj_pcm_sink_fn sink_fn)
{
    s_gapless_checked_for_track = false;
    s_checked_path[0] = '\0';
    s_bypass_current_transition = false;

    int phase = uipod_rs_dj_get_phase();
    const struct mp3entry *next_id3 = audio_next_track();
    bool no_next = (!next_id3 || !next_id3->path[0] ||
                    global_settings.dj_crossfade_mode == 0 ||
                    global_settings.single_mode != SINGLE_MODE_OFF);
    if (phase == 1 && sink_fn && no_next)
    {
        /* Playlist has no Deck B (or Crossfade disabled): drain 100% of captured Deck A tail! */
        uipod_rs_dj_flush_to_gapless_drain();
        drain_ready_mix_to_sink(cur_id3, sink_fn);
        return;
    }
    if (phase >= 2 && sink_fn)
    {
        /* Deck B reached EOF while transition/drain was still active: flush everything! */
        (void)uipod_rs_dj_push_deck_b(NULL, 0, 1);
        drain_ready_mix_to_sink(cur_id3, sink_fn);
        if (s_defer_track_stamp)
        {
            s_defer_track_stamp = false;
            pcmbuf_dj_commit_midpoint_handoff();
        }
    }
}

bool uipod_dj_on_auto_track_change(void)
{
    if (global_settings.dj_crossfade_mode == 0)
        return false;

    int phase = uipod_rs_dj_get_phase();
    if (phase != 1)
        return false;

    const struct mp3entry *next_id3 = audio_next_track();
    const char *alb_b = (next_id3 && next_id3->album) ? next_id3->album : "";
    int trk_b = next_id3 ? next_id3->tracknum : 0;
    unsigned long len_b_ms = next_id3 ? next_id3->length : 0;
    bool next_is_nsf = is_nsf_track_id3(next_id3);

    float bpm_b = 0.0f, conf_b = 0.0f;
    uint64_t key_b = uipod_track_intel_key_for_id3(next_id3);
    if (key_b != 0)
        (void)uipod_rs_intel_lookup_bpm(key_b, &bpm_b, &conf_b);
    uipod_rs_dj_set_bpm_hints(0.0f, 0.0f, bpm_b, conf_b);

    uipod_rs_dj_finish_deck_a_begin_deck_b(alb_b, trk_b, next_is_nsf ? 1 : 0);
    uipod_rs_dj_set_track_timing(0, 0, (uint32_t)len_b_ms);
    s_defer_track_stamp = true;
    return true;
}

void uipod_dj_on_end_of_data(void)
{
    /* Playlist ended with no Deck B: drain 100% of any buffered Deck A tail to `pcmbuf`
     * before `pcmbuf_start_track_change` marks end-of-data! */
    s_defer_track_stamp = false;
    if (uipod_rs_dj_get_phase() != 0)
    {
        uipod_rs_dj_flush_to_gapless_drain();
        while (uipod_rs_dj_get_phase() >= 3)
        {
            int mid_crossed = 0;
            uint32_t chunk_el_ms = 0;
            size_t got = uipod_rs_dj_render_s16_timed(s_mix_out_buf,
                                                      DJ_RENDER_CHUNK_FRAMES,
                                                      &mid_crossed,
                                                      &chunk_el_ms);
            if (got == 0)
                break;

            const int16_t *src = s_mix_out_buf;
            int rem = (int)got;
            int guard_waits = 0;
            while (rem > 0)
            {
                int req = rem;
                int16_t *dst = (int16_t *)pcmbuf_request_buffer(&req);
                if (!dst || req <= 0)
                {
                    pcmbuf_play_start();
                    if (++guard_waits > 600) /* ~24s safety ceiling */
                        break;
                    sleep(HZ / 25);
                    continue;
                }
                guard_waits = 0;
                int take = (req < rem) ? req : rem;
                memcpy(dst, src, (size_t)take * 4);
                pcmbuf_write_complete(take, (unsigned long)chunk_el_ms, 0);
                src += take * 2;
                rem -= take;
            }
            if (rem > 0)
                break;
        }
    }
    uipod_rs_dj_reset();
}

void uipod_dj_note_skip_delta(int toskip)
{
    s_pending_skip_delta = toskip;
}

bool uipod_dj_on_manual_skip(void)
{
    int delta = s_pending_skip_delta;
    s_pending_skip_delta = 0;

    long now = current_tick;
    bool rapid_double_tap = (now - s_last_manual_skip_tick) < (HZ * 3 / 4);
    s_last_manual_skip_tick = now;

    if (delta != 1 ||
        global_settings.dj_crossfade_mode == 0 ||
        !global_settings.dj_quick_cut_next ||
        rapid_double_tap ||
        uipod_rs_dj_get_phase() >= 2)
    {
        uipod_dj_reset();
        return false;
    }

    int16_t *peek_buf = malloc((size_t)DJ_QUICK_CUT_MAX_FRAMES * 4);
    if (!peek_buf)
    {
        uipod_dj_reset();
        return false;
    }

    size_t got = pcmbuf_peek_ahead_s16(10, peek_buf, DJ_QUICK_CUT_MAX_FRAMES);
    if (got < 8192)
    {
        free(peek_buf);
        uipod_dj_reset();
        return false;
    }

    uint32_t sr = (uint32_t)mixer_get_frequency();
    if (sr == 0)
        sr = 44100;

    const struct mp3entry *cur_id3  = audio_current_track();
    const struct mp3entry *next_id3 = audio_next_track();
    unsigned long el_a_ms  = cur_id3 ? cur_id3->elapsed : 0;
    unsigned long len_a_ms = cur_id3 ? cur_id3->length : 0;
    unsigned long len_b_ms = next_id3 ? next_id3->length : 0;

    s_defer_track_stamp = false;
    uipod_rs_dj_begin_deck_a(sr, "", 0, 1, 1);
    uipod_rs_dj_set_track_timing((uint32_t)el_a_ms,
                                 (uint32_t)len_a_ms,
                                 (uint32_t)len_b_ms);
    uipod_rs_dj_push_deck_a_frames(peek_buf, got);
    uipod_rs_dj_finish_deck_a_begin_deck_b("", 0, 0);
    free(peek_buf);
    return true;
}
