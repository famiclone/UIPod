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

#include <SDL_loadso.h>
#include "kernel.h"
#include "load_code.h"
#include "metadata.h"
#include "pcm_mixer.h"
#include "pcmbuf.h"
#include "playback.h"
#include "playlist.h"
#include "playlist_catalog.h"
#include "rbpaths.h"
#include "settings.h"
#include "string-extra.h"
#include "tape_deck_fx.h"
#include "button-sdl.h"
#include "uipod_dj.h"
#include "uipod_jellyfin.h"
#include "uipod_library.h"
#include "uipod_navidrome.h"
#include "uipod_playlists.h"
#include "uipod_rs_codecs.h"
#include "uipod_rs_plugins.h"
#include "uipod_rs_track_intel.h"
#include "uipod_track_intel.h"
#include <stdio.h>

extern unsigned int pcm_alsa_output_delay_us(void);
extern void         pcm_alsa_wake_writer(void);
extern const char  *handle_special_dirs(const char *dir, unsigned flags,
                                        char *buf, const size_t bufsize);

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
static uint64_t s_dj_last_pcmbuf_frame = 0;
static char s_deck_a_path[MAX_PATH] = "";
static char s_deck_b_path[MAX_PATH] = "";

static int16_t s_mix_out_buf[DJ_RENDER_CHUNK_FRAMES * 2];
static int16_t s_spill_buf[DJ_RENDER_CHUNK_FRAMES * 2];

static inline void commit_dj_sink(uipod_dj_pcm_sink_fn sink_fn,
                                  const int16_t *stereo_s16,
                                  int frames,
                                  unsigned long elapsed,
                                  long offset)
{
    sink_fn(stereo_s16, frames, elapsed, offset);
    __atomic_store_n(&s_dj_last_pcmbuf_frame,
                     pcmbuf_total_committed_frames(),
                     __ATOMIC_RELEASE);
}

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
    s_deck_a_path[0] = '\0';
    s_deck_b_path[0] = '\0';
    __atomic_store_n(&s_dj_last_pcmbuf_frame, 0, __ATOMIC_RELEASE);
    uipod_rs_dj_reset();
}

static uint64_t current_speaker_pcmbuf_frame(uint64_t *out_hw_delay_frames)
{
    unsigned long mix_sr = mixer_get_frequency();
    if (mix_sr == 0)
        mix_sr = 44100UL;

    uint64_t dly_us = (uint64_t)pcm_alsa_output_delay_us() +
                      (uint64_t)tape_deck_fx_delay_us();
    uint64_t hw_delay_frames = (dly_us * (uint64_t)mix_sr) / 1000000ULL;
    if (out_hw_delay_frames)
        *out_hw_delay_frames = hw_delay_frames;

    uint64_t played = pcmbuf_total_played_frames();
    return (played > hw_delay_frames) ? (played - hw_delay_frames) : 0ULL;
}

bool uipod_dj_is_active(void)
{
    if (uipod_rs_dj_get_phase() != 0 || s_defer_track_stamp)
        return true;
    uint64_t last_dj = __atomic_load_n(&s_dj_last_pcmbuf_frame, __ATOMIC_ACQUIRE);
    if (last_dj == 0)
        return false;
    return current_speaker_pcmbuf_frame(NULL) < last_dj;
}

bool uipod_dj_query_playhead(struct uipod_rs_dj_playhead *out_ph,
                             const char **out_deck_a_path,
                             const char **out_deck_b_path)
{
    if (out_deck_a_path)
        *out_deck_a_path = s_deck_a_path;
    if (out_deck_b_path)
        *out_deck_b_path = s_deck_b_path;
    if (!out_ph)
        return false;

    uint64_t hw_delay_frames = 0;
    uint64_t speaker_frame = current_speaker_pcmbuf_frame(&hw_delay_frames);
    uint64_t last_dj = __atomic_load_n(&s_dj_last_pcmbuf_frame, __ATOMIC_ACQUIRE);
    int phase = uipod_rs_dj_get_phase();
    bool downstream_draining = (last_dj > 0 && speaker_frame < last_dj);

    if (phase == 0 && !downstream_draining)
    {
        memset(out_ph, 0, sizeof(*out_ph));
        return false;
    }

    size_t downstream_frames;
    if (last_dj > 0)
    {
        downstream_frames = (last_dj > speaker_frame)
                          ? (size_t)(last_dj - speaker_frame)
                          : 0U;
    }
    else
    {
        downstream_frames = pcmbuf_unplayed_frames() + (size_t)hw_delay_frames;
    }

    return uipod_rs_dj_get_playhead_synced(out_ph, downstream_frames) != 0;
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

        commit_dj_sink(sink_fn, s_mix_out_buf, (int)got, (unsigned long)chunk_el_ms, off);

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
        strlcpy(s_deck_a_path,
                (cur_id3 && cur_id3->path[0]) ? cur_id3->path : "",
                sizeof(s_deck_a_path));
        s_deck_b_path[0] = '\0';
        __atomic_store_n(&s_dj_last_pcmbuf_frame,
                         pcmbuf_total_committed_frames(),
                         __ATOMIC_RELEASE);

        uipod_rs_dj_begin_deck_a(sr,
                                 alb_a,
                                 trk_a,
                                 global_settings.playlist_shuffle ? 1 : 0,
                                 0);
        uipod_rs_dj_set_track_timing((uint32_t)el_ms, (uint32_t)len_ms, 0);
        float bpm_a = 0.0f, conf_a = 0.0f;
        uint32_t first_beat_a_ms = 0;
        int32_t fb_a = -1;
        uint64_t key_a = uipod_track_intel_key_for_id3(cur_id3);
        if (key_a != 0 &&
            uipod_rs_intel_lookup_tempo(key_a, &bpm_a, &conf_a, &first_beat_a_ms) != 0)
        {
            fb_a = (int32_t)first_beat_a_ms;
        }
        uipod_rs_dj_set_intel_hints(bpm_a, conf_a, fb_a, 0.0f, 0.0f, -1);
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
            commit_dj_sink(sink_fn, s_spill_buf, (int)spilled, (unsigned long)spill_el_ms, off);
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
    {
        drain_ready_mix_to_sink(cur_id3, sink_fn);
    }
    else if (pcmbuf_is_lowdata())
    {
        /* Deck B is still buffering (e.g. slow HTTP stream): yield safe pre-mix
         * Deck A prefix frames so `pcmbuf` never underruns mid-track! */
        long off = cur_id3 ? (long)cur_id3->offset : 0;
        for (int k = 0; k < 2; k++)
        {
            uint32_t pre_el_ms = 0;
            size_t popped = uipod_rs_dj_pop_safe_premix_a_s16(
                s_spill_buf, DJ_RENDER_CHUNK_FRAMES, &pre_el_ms);
            if (popped == 0)
                break;
            commit_dj_sink(sink_fn, s_spill_buf, (int)popped, (unsigned long)pre_el_ms, off);
        }
    }

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

    strlcpy(s_deck_b_path,
            (next_id3 && next_id3->path[0]) ? next_id3->path : "",
            sizeof(s_deck_b_path));

    float bpm_b = 0.0f, conf_b = 0.0f;
    uint32_t first_beat_b_ms = 0;
    int32_t fb_b = -1;
    uint64_t key_b = uipod_track_intel_key_for_id3(next_id3);
    if (key_b != 0 &&
        uipod_rs_intel_lookup_tempo(key_b, &bpm_b, &conf_b, &first_beat_b_ms) != 0)
    {
        fb_b = (int32_t)first_beat_b_ms;
    }
    uipod_rs_dj_set_intel_hints(0.0f, 0.0f, -1, bpm_b, conf_b, fb_b);

    uipod_rs_dj_set_track_timing(0, 0, (uint32_t)len_b_ms);
    uipod_rs_dj_finish_deck_a_begin_deck_b(alb_b, trk_b, next_is_nsf ? 1 : 0);
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
    strlcpy(s_deck_a_path,
            (cur_id3 && cur_id3->path[0]) ? cur_id3->path : "",
            sizeof(s_deck_a_path));
    strlcpy(s_deck_b_path,
            (next_id3 && next_id3->path[0]) ? next_id3->path : "",
            sizeof(s_deck_b_path));
    uipod_rs_dj_begin_deck_a(sr, "", 0, 1, 1);
    uipod_rs_dj_set_track_timing((uint32_t)el_a_ms,
                                 (uint32_t)len_a_ms,
                                 (uint32_t)len_b_ms);
    uipod_rs_dj_push_deck_a_frames(peek_buf, got);
    uipod_rs_dj_finish_deck_a_begin_deck_b("", 0, 0);
    __atomic_store_n(&s_dj_last_pcmbuf_frame,
                     pcmbuf_total_committed_frames(),
                     __ATOMIC_RELEASE);
    free(peek_buf);
    return true;
}

/* ==========================================================================
 * Manual Dual-Deck DJ Session Coordinator (`ABI v11+`, `DJ Mixer` Plugin)
 * ========================================================================== */

#define DJ_FAV_CACHE_CAP 512

struct dj_fav_entry
{
    char path[MAX_PATH];
    char title[128];
    char artist[96];
    uint32_t duration_ms;
};

static struct dj_fav_entry *s_fav_cache = NULL;
static size_t               s_fav_count = 0;

static void basename_without_ext(const char *path, char *out, size_t cap)
{
    if (!out || cap == 0)
        return;
    if (!path || !path[0])
    {
        out[0] = '\0';
        return;
    }
    const char *slash = strrchr(path, '/');
    const char *base  = slash ? (slash + 1) : path;
    strlcpy(out, base, cap);
    char *hash = strrchr(out, '#');
    if (hash)
        *hash = '\0';
    char *dot = strrchr(out, '.');
    if (dot && dot != out)
        *dot = '\0';
}

static void enrich_track_display_meta(const char *path,
                                      char *title_out, size_t title_cap,
                                      char *artist_out, size_t artist_cap,
                                      uint32_t *dur_out)
{
    if (title_out && title_cap > 0)
        title_out[0] = '\0';
    if (artist_out && artist_cap > 0)
        artist_out[0] = '\0';
    if (dur_out)
        *dur_out = 0;
    if (!path || !path[0])
        return;

    struct mp3entry id3;
    memset(&id3, 0, sizeof(id3));
    bool filled = false;
    if (uipod_navidrome_is_track_path(path))
        filled = uipod_navidrome_fill_id3(path, &id3);
    else if (uipod_jellyfin_is_track_path(path))
        filled = uipod_jellyfin_fill_id3(path, &id3);

    if (filled)
    {
        if (title_out && title_cap > 0 && id3.title && id3.title[0])
            strlcpy(title_out, id3.title, title_cap);
        if (artist_out && artist_cap > 0 && id3.artist && id3.artist[0])
            strlcpy(artist_out, id3.artist, artist_cap);
        if (dur_out && id3.length > 0)
            *dur_out = (uint32_t)id3.length;
    }

    if (title_out && title_cap > 0 && !title_out[0])
        basename_without_ext(path, title_out, title_cap);
}

static void append_m3u8_to_fav_cache(const char *m3u_path)
{
    if (!m3u_path || !m3u_path[0] || !s_fav_cache || s_fav_count >= DJ_FAV_CACHE_CAP)
        return;
    FILE *fp = fopen(m3u_path, "r");
    if (!fp)
        return;

    char line[MAX_PATH + 64];
    while (s_fav_count < DJ_FAV_CACHE_CAP && fgets(line, sizeof(line), fp))
    {
        size_t len = strlen(line);
        while (len > 0 && (line[len - 1] == '\n' || line[len - 1] == '\r' || line[len - 1] == ' '))
            line[--len] = '\0';
        if (len == 0 || line[0] == '#')
            continue;

        bool dup = false;
        for (size_t i = 0; i < s_fav_count; i++)
        {
            if (strcmp(s_fav_cache[i].path, line) == 0)
            {
                dup = true;
                break;
            }
        }
        if (dup)
            continue;

        struct dj_fav_entry *e = &s_fav_cache[s_fav_count++];
        strlcpy(e->path, line, sizeof(e->path));
        enrich_track_display_meta(e->path,
                                  e->title, sizeof(e->title),
                                  e->artist, sizeof(e->artist),
                                  &e->duration_ms);
    }
    fclose(fp);
}

static void refresh_fav_cache(void)
{
    s_fav_count = 0;
    if (!s_fav_cache)
        s_fav_cache = calloc(DJ_FAV_CACHE_CAP, sizeof(*s_fav_cache));
    if (!s_fav_cache)
        return;

    const char *nav_fav = uipod_navidrome_favourites_path();
    if (nav_fav && nav_fav[0])
        append_m3u8_to_fav_cache(nav_fav);

    char pdir[MAX_PATH];
    catalog_get_directory(pdir, sizeof(pdir));
    if (pdir[0])
    {
        char f1[MAX_PATH], f2[MAX_PATH];
        snprintf(f1, sizeof(f1), "%s/Favourites.m3u8", pdir);
        snprintf(f2, sizeof(f2), "%s/Favorites.m3u8", pdir);
        append_m3u8_to_fav_cache(f1);
        append_m3u8_to_fav_cache(f2);
    }
}

static bool prepare_deck_local_file(const char *path, bool ui_blocking)
{
    if (!path || !path[0])
        return false;
    if (uipod_navidrome_is_track_path(path))
        return uipod_navidrome_prepare_track(path, ui_blocking);
    if (uipod_jellyfin_is_track_path(path))
        return uipod_jellyfin_prepare_track(path, ui_blocking);
    return true;
}

static char s_manual_deck_vfs_path[2][MAX_PATH];
static bool s_opus_registered = false;
static void *s_opus_handle = NULL;

static void ensure_opus_decoder_registered(void)
{
    if (s_opus_registered)
        return;
    s_opus_registered = true;

    s_opus_handle = lc_open(CODECS_DIR "/opus.codec", NULL, 0);
    if (!s_opus_handle)
        return;

    int (*fn_get_size)(int) =
        (int (*)(int))SDL_LoadFunction(s_opus_handle, "opus_decoder_get_size");
    int (*fn_init)(void *, int32_t, int) =
        (int (*)(void *, int32_t, int))SDL_LoadFunction(s_opus_handle, "opus_decoder_init");
    int (*fn_decode)(void *, const uint8_t *, int32_t, int16_t *, int, int) =
        (int (*)(void *, const uint8_t *, int32_t, int16_t *, int, int))
            SDL_LoadFunction(s_opus_handle, "opus_decode");

    if (fn_get_size && fn_init && fn_decode)
        uipod_rs_manual_dj_register_opus(fn_get_size, fn_init, fn_decode);
}

static const char *resolve_deck_os_path(uint8_t deck,
                                        const char *vfs_path,
                                        char *os_buf,
                                        size_t os_cap)
{
    uint8_t idx = deck & 1u;
    if (vfs_path && vfs_path[0])
        strlcpy(s_manual_deck_vfs_path[idx], vfs_path, sizeof(s_manual_deck_vfs_path[idx]));
    else
        s_manual_deck_vfs_path[idx][0] = '\0';

    if (!vfs_path || !vfs_path[0])
        return "";
    const char *resolved = handle_special_dirs(vfs_path, 0, os_buf, os_cap);
    return (resolved && resolved[0]) ? resolved : vfs_path;
}

bool uipod_dj_manual_session_acquire(void)
{
    uipod_dj_reset();
    ensure_opus_decoder_registered();
    sdl_set_analog_stick_dpad_suppressed(true);
    refresh_fav_cache();
    s_manual_deck_vfs_path[0][0] = '\0';
    s_manual_deck_vfs_path[1][0] = '\0';

    uint32_t sr = (uint32_t)mixer_get_frequency();
    if (sr < 8000u)
        sr = 48000u;

    /* Initialize ManualDjCore with `MANUAL_DJ_ACTIVE = false` so ALSA/SDL keeps playing
     * uninterrupted from Rockbox while Deck A (`>= 1.0s`) and Deck B pre-buffer! */
    if (!uipod_rs_manual_dj_session_begin(sr))
    {
        sdl_set_analog_stick_dpad_suppressed(false);
        return false;
    }

    int st = audio_status();
    if (st & AUDIO_STATUS_PLAY)
    {
        struct mp3entry *cur = audio_current_track();
        bool was_paused = (st & AUDIO_STATUS_PAUSE) != 0;
        bool loaded_a = false;
        bool is_nsf_a = false;

        if (cur && cur->path[0])
        {
            uint32_t dly_ms = (pcm_alsa_output_delay_us() + tape_deck_fx_delay_us()) / 1000u;
            uint32_t raw_el = (uint32_t)cur->elapsed;
            uint32_t raw_len = (uint32_t)cur->length;
            is_nsf_a = is_nsf_track_id3(cur);
            if (is_nsf_a)
            {
                uint32_t rs_dur = uipod_rs_codec_duration_ms();
                uint32_t rs_el  = uipod_rs_codec_elapsed_ms();
                if (rs_dur > 0)
                {
                    raw_len = rs_dur;
                    raw_el  = rs_el;
                }
            }
            /* When active, ALSA already holds `[raw_el - dly_ms .. raw_el)` in its hardware DMA
             * ring, so the next sample ALSA pulls from `ManualDjCore` is `raw_el` (`chunk_ridx`)! */
            uint32_t start_el_ms = was_paused
                                 ? ((raw_el > dly_ms) ? (raw_el - dly_ms) : 0u)
                                 : raw_el;

            (void)prepare_deck_local_file(cur->path, false);

            size_t seed_cap = (size_t)sr * 4u;
            int16_t *seed_buf = NULL;
            size_t seed_got = 0;
            if (!was_paused && is_nsf_a)
            {
                seed_buf = malloc(seed_cap * 4u);
                if (seed_buf)
                    seed_got = pcmbuf_peek_ahead_s16(0, seed_buf, seed_cap);
            }

            const char *t_a = (cur->title && cur->title[0]) ? cur->title : "";
            const char *a_a = (cur->artist && cur->artist[0]) ? cur->artist : "";
            uint64_t key_a = uipod_track_intel_ensure_path(cur->path, t_a, a_a, raw_len, true);

            char os_path_a[MAX_PATH];
            const char *real_a = resolve_deck_os_path(0, cur->path, os_path_a, sizeof(os_path_a));

            loaded_a = uipod_rs_manual_dj_load_deck(0,
                                                    real_a,
                                                    t_a,
                                                    a_a,
                                                    raw_len,
                                                    start_el_ms,
                                                    was_paused,
                                                    key_a,
                                                    seed_buf,
                                                    seed_got);
            if (seed_buf)
                free(seed_buf);
        }

        /* Pre-load queued next playlist track paused at its Cue point into Deck B BEFORE handoff */
        struct mp3entry *nxt = audio_next_track();
        char next_path[MAX_PATH] = "";
        char next_title[128]     = "";
        char next_artist[96]     = "";
        uint32_t next_len_ms     = 0;

        if (nxt && nxt->path[0])
        {
            strlcpy(next_path, nxt->path, sizeof(next_path));
            if (nxt->title && nxt->title[0])
                strlcpy(next_title, nxt->title, sizeof(next_title));
            if (nxt->artist && nxt->artist[0])
                strlcpy(next_artist, nxt->artist, sizeof(next_artist));
            next_len_ms = (uint32_t)nxt->length;
        }
        else if (playlist_amount() > 1)
        {
            int cur_disp = playlist_get_display_index(); /* 1-based */
            int next_idx = (cur_disp >= 1 && cur_disp < playlist_amount()) ? cur_disp : 0;
            struct playlist_track_info pti;
            if (playlist_get_track_info(NULL, next_idx, &pti) >= 0 && pti.filename[0])
            {
                strlcpy(next_path, pti.filename, sizeof(next_path));
                enrich_track_display_meta(next_path,
                                          next_title, sizeof(next_title),
                                          next_artist, sizeof(next_artist),
                                          &next_len_ms);
            }
        }

        if (next_path[0] && prepare_deck_local_file(next_path, false))
        {
            uint64_t key_b = uipod_track_intel_ensure_path(
                next_path, next_title, next_artist, next_len_ms, true);
            char os_path_b[MAX_PATH];
            const char *real_b = resolve_deck_os_path(1, next_path, os_path_b, sizeof(os_path_b));
            (void)uipod_rs_manual_dj_load_deck(1,
                                               real_b,
                                               next_title,
                                               next_artist,
                                               next_len_ms,
                                               0,
                                               true,
                                               key_b,
                                               NULL,
                                               0);
        }

        /* Snap Deck A to the exact live `chunk_ridx` timestamp right at the instant of handoff */
        if (loaded_a && !was_paused && !is_nsf_a && cur)
        {
            uint32_t now_raw_el = (uint32_t)cur->elapsed;
            uipod_rs_manual_dj_sync_playhead_ms(0, now_raw_el);
        }

        uipod_rs_manual_dj_session_activate(true);

        /* Quiesce Rockbox `pcmbuf` & `tape_deck_fx` completely for strict single-consumer routing */
        if (!was_paused)
        {
            audio_pause();
            tape_deck_fx_on_seek_or_stop();
            pcmbuf_fade(false, true);
            pcmbuf_pause(true);
            for (int i = 0; i < 4; i++)
                yield();
        }
    }
    else
    {
        /* Player was stopped: pre-cue first two library tracks paused if available */
        int lib_n = uipod_library_music_count();
        for (int d = 0; d < 2 && d < lib_n; d++)
        {
            char p[MAX_PATH] = "", a[96] = "", t[128] = "";
            uint32_t dur = 0;
            if (uipod_library_music_at(d, p, sizeof(p), a, sizeof(a), t, sizeof(t), &dur) &&
                p[0] && prepare_deck_local_file(p, false))
            {
                uint64_t key = uipod_track_intel_ensure_path(p, t, a, dur, true);
                char os_p[MAX_PATH];
                const char *real_p = resolve_deck_os_path((uint8_t)d, p, os_p, sizeof(os_p));
                (void)uipod_rs_manual_dj_load_deck((uint8_t)d, real_p, t, a, dur, 0, true, key, NULL, 0);
            }
        }
        uipod_rs_manual_dj_session_activate(true);
    }

    pcm_alsa_wake_writer();
    return true;
}

void uipod_dj_manual_session_release(void)
{
    sdl_set_analog_stick_dpad_suppressed(false);
    if (!uipod_rs_manual_dj_is_active())
        return;

    uint8_t  dom_deck = 0;
    char     win_path[MAX_PATH];
    uint32_t win_el_ms = 0;
    uint8_t  win_playing = 0;
    win_path[0] = '\0';

    uipod_rs_manual_dj_session_end(&dom_deck, win_path, sizeof(win_path),
                                   &win_el_ms, &win_playing);

    if (s_manual_deck_vfs_path[dom_deck & 1u][0])
        strlcpy(win_path, s_manual_deck_vfs_path[dom_deck & 1u], sizeof(win_path));
    s_manual_deck_vfs_path[0][0] = '\0';
    s_manual_deck_vfs_path[1][0] = '\0';

    if (s_fav_cache)
    {
        free(s_fav_cache);
        s_fav_cache = NULL;
        s_fav_count = 0;
    }

    tape_deck_fx_on_seek_or_stop();
    pcmbuf_fade(false, true);

    if (!win_path[0])
        return;

    int st = audio_status();
    struct mp3entry *cur = (st & AUDIO_STATUS_PLAY) ? audio_current_track() : NULL;
    struct mp3entry *nxt = (st & AUDIO_STATUS_PLAY) ? audio_next_track() : NULL;

    if (cur && strcmp(cur->path, win_path) == 0)
    {
        audio_pre_ff_rewind();
        audio_ff_rewind((long)win_el_ms);
        if (win_playing)
            audio_resume();
        else
            audio_pause();
        for (int i = 0; i < 16; i++)
            yield();
    }
    else if (nxt && strcmp(nxt->path, win_path) == 0)
    {
        audio_next();
        for (int i = 0; i < 16; i++)
            yield();
        if (win_el_ms > 80u)
        {
            audio_pre_ff_rewind();
            audio_ff_rewind((long)win_el_ms);
        }
        if (win_playing)
            audio_resume();
        else
            audio_pause();
        for (int i = 0; i < 16; i++)
            yield();
    }
    else
    {
        if (uipod_play_single_at_ms(win_path, (unsigned long)win_el_ms))
        {
            for (int i = 0; i < 16; i++)
                yield();
            if (!win_playing)
                audio_pause();
        }
    }
}

bool uipod_dj_manual_deck_load(uint8_t deck, const char *path, bool start_paused)
{
    if (!path || !path[0])
        return false;
    if (!prepare_deck_local_file(path, true))
        return false;

    char title[128] = "";
    char artist[96] = "";
    uint32_t dur_ms = 0;

    /* Search library first for rich artist/title/duration metadata */
    int lib_n = uipod_library_music_count();
    for (int i = 0; i < lib_n; i++)
    {
        char lp[MAX_PATH];
        if (uipod_library_music_at(i, lp, sizeof(lp),
                                   artist, sizeof(artist),
                                   title, sizeof(title),
                                   &dur_ms) &&
            strcmp(lp, path) == 0)
        {
            break;
        }
        title[0] = '\0';
        artist[0] = '\0';
        dur_ms = 0;
    }
    if (!title[0])
        enrich_track_display_meta(path, title, sizeof(title), artist, sizeof(artist), &dur_ms);

    uint64_t key = uipod_track_intel_ensure_path(path, title, artist, dur_ms, true);
    char os_path[MAX_PATH];
    const char *real_path = resolve_deck_os_path(deck, path, os_path, sizeof(os_path));
    bool ok = uipod_rs_manual_dj_load_deck(deck, real_path, title, artist,
                                           dur_ms, 0, start_paused,
                                           key, NULL, 0);
    pcm_alsa_wake_writer();
    return ok;
}

void uipod_dj_manual_deck_set_playing(uint8_t deck, bool playing, bool quantize)
{
    uipod_rs_manual_dj_set_playing(deck, playing, quantize);
    if (playing)
        pcm_alsa_wake_writer();
}

void uipod_dj_manual_deck_cue(uint8_t deck, uint8_t cue_action)
{
    uipod_rs_manual_dj_cue(deck, cue_action);
    pcm_alsa_wake_writer();
}

void uipod_dj_manual_deck_seek_beats(uint8_t deck, int32_t delta_beats)
{
    uipod_rs_manual_dj_seek_beats(deck, delta_beats);
}

void uipod_dj_manual_set_crossfader(float position, bool bass_swap_enabled)
{
    uipod_rs_manual_dj_set_crossfader(position, bass_swap_enabled);
}

void uipod_dj_manual_deck_set_tempo(uint8_t deck,
                                    float target_bpm_ratio,
                                    uint8_t tempo_mode,
                                    float momentary_bend)
{
    uipod_rs_manual_dj_set_tempo(deck, target_bpm_ratio, tempo_mode, momentary_bend);
}

void uipod_dj_manual_deck_set_sync(uint8_t deck, bool enabled)
{
    uipod_rs_manual_dj_set_sync(deck, enabled);
}

void uipod_dj_manual_deck_set_loop(uint8_t deck, uint8_t loop_beats)
{
    uipod_rs_manual_dj_set_loop(deck, loop_beats);
}

void uipod_dj_manual_deck_set_filter(uint8_t deck, float filter_value)
{
    uipod_rs_manual_dj_set_filter(deck, filter_value);
}

bool uipod_dj_manual_get_telemetry(struct UIPodDjTelemetry *out)
{
    int16_t lx = 0, ly = 0, rx = 0, ry = 0;
    sdl_get_analog_sticks(&lx, &ly, &rx, &ry);
    return uipod_rs_manual_dj_get_telemetry(out, lx, ly, rx, ry);
}

size_t uipod_dj_source_track_count(uint8_t source_kind)
{
    switch (source_kind)
    {
        case UIPOD_DJ_SRC_QUEUE:
        {
            int n = playlist_amount();
            return (n > 0) ? (size_t)n : 0u;
        }
        case UIPOD_DJ_SRC_FAVOURITES:
            return s_fav_count;
        default:
        {
            int n = uipod_library_music_count();
            return (n > 0) ? (size_t)n : 0u;
        }
    }
}

bool uipod_dj_source_track_at(uint8_t source_kind,
                              size_t index,
                              char *path_out, size_t path_cap,
                              char *artist_out, size_t artist_cap,
                              char *title_out, size_t title_cap,
                              uint32_t *duration_ms_out)
{
    switch (source_kind)
    {
        case UIPOD_DJ_SRC_QUEUE:
        {
            int n = playlist_amount();
            if ((int)index < 0 || (int)index >= n)
                return false;
            struct playlist_track_info pti;
            if (playlist_get_track_info(NULL, (int)index, &pti) < 0 || !pti.filename[0])
                return false;
            if (path_out && path_cap > 0)
                strlcpy(path_out, pti.filename, path_cap);
            enrich_track_display_meta(pti.filename,
                                      title_out, title_cap,
                                      artist_out, artist_cap,
                                      duration_ms_out);
            return true;
        }
        case UIPOD_DJ_SRC_FAVOURITES:
        {
            if (!s_fav_cache || index >= s_fav_count)
                return false;
            const struct dj_fav_entry *e = &s_fav_cache[index];
            if (path_out && path_cap > 0)
                strlcpy(path_out, e->path, path_cap);
            if (artist_out && artist_cap > 0)
                strlcpy(artist_out, e->artist, artist_cap);
            if (title_out && title_cap > 0)
                strlcpy(title_out, e->title, title_cap);
            if (duration_ms_out)
                *duration_ms_out = e->duration_ms;
            return true;
        }
        default:
            return uipod_library_music_at((int)index,
                                          path_out, path_cap,
                                          artist_out, artist_cap,
                                          title_out, title_cap,
                                          duration_ms_out);
    }
}
