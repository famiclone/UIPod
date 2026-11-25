/***************************************************************************
 * UIPod: C Firmware Coordinator for `uipod-track-intel`.
 *
 * - Initializes persistent `ROCKBOX_DIR "/track_intel.db"` (`UTI1` 512-byte
 *   binary records) and `pcmbuf` stream-analysis fallback.
 * - Silently dispatches background analysis on every track / `.cue` subtrack
 *   change (plus `NEXTTRACK_ID3` prefetch for Auto-DJ).
 * - Implements the synchronous `"Processing..."` / `"Обробка..."` stacked HUD
 *   gate when launching rhythm-driven plugins (`Beat Stack`, `Sound Racer`,
 *   `Pulsar Vanguard`) on an uncached track.
 * - Drives lock-free 60+ Hz `TrackLiveFrame` snapshots for GPU Visualizers
 *   and `.uipod` plugins.
 ****************************************************************************/
#include "config.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

#include "audio.h"
#include "backlight.h"
#include "cuesheet.h"
#include "kernel.h"
#include "metadata.h"
#include "pcm_mixer.h"
#include "pcmbuf.h"
#include "rbpaths.h"
#include "settings.h"
#include "string-extra.h"
#include "tape_deck_fx.h"
#include "uipod_rs_codecs.h"
#include "uipod_rs_track_intel.h"
#include "uipod_rs_ui.h"
#include "uipod_track_intel.h"
#include "window-sdl.h"

extern unsigned int pcm_alsa_output_delay_us(void);

#define INTEL_DB_PATH ROCKBOX_DIR "/track_intel.db"

static volatile uint64_t s_active_key_hash = 0;

static bool intel_is_ukrainian(void)
{
    return strcmp((const char *)global_settings.lang_file, "ukrainian") == 0;
}

static size_t intel_pcm_fallback_cb(int16_t *out_stereo,
                                    size_t max_frames,
                                    uint32_t *out_sr)
{
    unsigned long sr = mixer_get_frequency();
    if (sr == 0)
        sr = 44100;
    if (out_sr)
        *out_sr = (uint32_t)sr;
    if (!out_stereo || max_frames == 0)
        return 0;
    return pcmbuf_peek_ahead_s16(0, out_stereo, max_frames);
}

struct extracted_id3_fields
{
    char        base_path[MAX_PATH];
    uint64_t    file_size;
    int64_t     mtime_sec;
    int32_t     cue_subtrack;
    uint32_t    slice_start_ms;
    uint32_t    duration_ms;
    uint32_t    sample_rate;
    const char *title;
    const char *artist;
    const char *album;
    const char *genre;
};

static bool extract_id3_fields(const struct mp3entry *id3,
                               struct extracted_id3_fields *out)
{
    if (!id3 || !id3->path[0] || !out)
        return false;

    memset(out, 0, sizeof(*out));
    out->cue_subtrack = -1;
    out->sample_rate  = id3->frequency > 0 ? (uint32_t)id3->frequency : 44100u;
    out->duration_ms  = (uint32_t)id3->length;
    out->title        = (id3->title && id3->title[0]) ? id3->title : "";
    out->artist       = (id3->artist && id3->artist[0]) ? id3->artist : "";
    out->album        = (id3->album && id3->album[0]) ? id3->album : "";
    out->genre        = (id3->genre_string && id3->genre_string[0]) ? id3->genre_string : "";

    /* Resolve clean filesystem path (stripping `.nsf#<idx>` if present) for `stat()` */
    if (uipod_rs_nsf_parse_uri(id3->path, out->base_path, sizeof(out->base_path)) >= 0)
    {
        uint32_t rs_dur = uipod_rs_codec_duration_ms();
        if (rs_dur > 0)
            out->duration_ms = rs_dur;
    }
    else
    {
        strlcpy(out->base_path, id3->path, sizeof(out->base_path));
    }

    struct stat st;
    if (stat(out->base_path, &st) == 0 && st.st_size > 0)
    {
        out->file_size = (uint64_t)st.st_size;
        out->mtime_sec = (int64_t)st.st_mtime;
    }
    else
    {
        out->file_size = (uint64_t)id3->filesize;
        out->mtime_sec = 0;
    }

    if (id3->cuesheet && id3->cuesheet->track_count > 1 &&
        id3->cuesheet->curr_track_idx >= 0 &&
        id3->cuesheet->curr_track_idx < id3->cuesheet->track_count)
    {
        int idx = id3->cuesheet->curr_track_idx;
        out->cue_subtrack   = (int32_t)idx;
        out->slice_start_ms = (uint32_t)id3->cuesheet->tracks[idx].offset;
        if (idx + 1 < id3->cuesheet->track_count)
        {
            unsigned long next_off = id3->cuesheet->tracks[idx + 1].offset;
            if (next_off > id3->cuesheet->tracks[idx].offset)
                out->duration_ms = (uint32_t)(next_off - id3->cuesheet->tracks[idx].offset);
        }
        else if (id3->length > id3->cuesheet->tracks[idx].offset)
        {
            out->duration_ms = (uint32_t)(id3->length - id3->cuesheet->tracks[idx].offset);
        }
        if (id3->cuesheet->tracks[idx].title[0])
            out->title = id3->cuesheet->tracks[idx].title;
        if (id3->cuesheet->tracks[idx].performer[0])
            out->artist = id3->cuesheet->tracks[idx].performer;
    }

    return true;
}

void uipod_track_intel_init(void)
{
    uipod_rs_intel_init(INTEL_DB_PATH, intel_pcm_fallback_cb);
}

uint64_t uipod_track_intel_key_for_id3(const struct mp3entry *id3)
{
    struct extracted_id3_fields f;
    if (!extract_id3_fields(id3, &f))
        return 0;
    return uipod_rs_intel_compute_key(id3->path,
                                      f.file_size,
                                      f.mtime_sec,
                                      f.cue_subtrack,
                                      f.duration_ms);
}

void uipod_track_intel_on_track_change(const struct mp3entry *id3)
{
    struct extracted_id3_fields f;
    if (!extract_id3_fields(id3, &f))
    {
        __atomic_store_n(&s_active_key_hash, 0ULL, __ATOMIC_RELEASE);
        uipod_rs_intel_clear_active();
        return;
    }

    uint64_t key = uipod_rs_intel_compute_key(id3->path,
                                              f.file_size,
                                              f.mtime_sec,
                                              f.cue_subtrack,
                                              f.duration_ms);
    __atomic_store_n(&s_active_key_hash, key, __ATOMIC_RELEASE);

    (void)uipod_rs_intel_on_track_change(id3->path,
                                         f.file_size,
                                         f.mtime_sec,
                                         f.cue_subtrack,
                                         f.slice_start_ms,
                                         f.duration_ms,
                                         f.sample_rate,
                                         f.title,
                                         f.artist,
                                         f.album,
                                         f.genre);
}

void uipod_track_intel_on_next_track(const struct mp3entry *next_id3)
{
    struct extracted_id3_fields f;
    if (!extract_id3_fields(next_id3, &f))
        return;

    uipod_rs_intel_prefetch_track(next_id3->path,
                                  f.file_size,
                                  f.mtime_sec,
                                  f.cue_subtrack,
                                  f.slice_start_ms,
                                  f.duration_ms,
                                  f.sample_rate,
                                  f.title,
                                  f.artist,
                                  f.album,
                                  f.genre);
}

static bool is_music_driven_plugin(const char *plugin_id)
{
    if (!plugin_id || !plugin_id[0])
        return false;
    return strcmp(plugin_id, "beat_stack") == 0 ||
           strcmp(plugin_id, "sound_racer") == 0 ||
           strcmp(plugin_id, "pulsar_vanguard") == 0;
}

void uipod_track_intel_ensure_for_plugin(const char *plugin_id)
{
    if (!is_music_driven_plugin(plugin_id))
        return;

    /* Case C — Nothing playing: start immediately with 120.0 BPM fallback */
    if (!(audio_status() & AUDIO_STATUS_PLAY))
        return;

    struct mp3entry *id3 = audio_current_track();
    struct extracted_id3_fields f;
    if (!extract_id3_fields(id3, &f))
        return;

    uint64_t key = uipod_rs_intel_compute_key(id3->path,
                                              f.file_size,
                                              f.mtime_sec,
                                              f.cue_subtrack,
                                              f.duration_ms);
    if (key == 0)
        return;

    __atomic_store_n(&s_active_key_hash, key, __ATOMIC_RELEASE);

    /* Case A — Record already in DB: 0 ms, zero popup, start immediately! */
    if (uipod_rs_intel_is_ready(key))
    {
        uipod_track_intel_tick_live();
        return;
    }

    /* Case B — Cache miss or background worker still running:
     * Show stacked HUD `"Processing..."` / `"Обробка..."`, boost worker priority,
     * animate HUD smoothly at 60 FPS, persist to DB, then dismiss HUD cleanly! */
    const char *hud_title = intel_is_ukrainian() ? "Обробка..." : "Processing...";
    const char *hud_detail = (f.title && f.title[0]) ? f.title : NULL;
    uint32_t hud = uipod_hud_progress_begin(hud_title, hud_detail, -1.0f);
    backlight_on();

    /* Enqueue/boost immediately (timeout_ms = 0 returns right after boosting) */
    (void)uipod_rs_intel_ensure_current(id3->path,
                                        f.file_size,
                                        f.mtime_sec,
                                        f.cue_subtrack,
                                        f.slice_start_ms,
                                        f.duration_ms,
                                        f.sample_rate,
                                        f.title,
                                        f.artist,
                                        f.album,
                                        f.genre,
                                        0);

    uint64_t start_ms = (uint64_t)sdl_get_ms();
    while (!uipod_rs_intel_is_ready(key))
    {
        uint64_t elapsed = (uint64_t)sdl_get_ms() - start_ms;
        if (elapsed >= 1800u)
            break;
        sleep(1);
        if (hud != 0)
            uipod_hud_progress_update(hud, hud_title, hud_detail, -1.0f);
    }

    /* Ensure the card settles visibly if shown for < 140 ms so it never flickers 1 frame */
    if (hud != 0)
    {
        while ((uint64_t)sdl_get_ms() - start_ms < 140u)
        {
            sleep(1);
            uipod_hud_progress_update(hud, hud_title, hud_detail, -1.0f);
        }
        uipod_hud_progress_end(hud);
    }

    uipod_track_intel_tick_live();
}

void uipod_track_intel_tick_live(void)
{
    int raw_st = audio_status();
    uint8_t st = 0u;
    if (raw_st & AUDIO_STATUS_PLAY)
        st = (raw_st & AUDIO_STATUS_PAUSE) ? 2u : 1u;

    uint64_t now_ms = (uint64_t)sdl_get_ms();
    uint64_t key = 0;
    uint32_t playhead_ms = 0;
    uint32_t dur_ms = 0;
    float lat_ms = 25.0f;

    if (st != 0u)
    {
        struct mp3entry *id3 = audio_current_track();
        if (id3 && id3->path[0])
        {
            uint64_t cur_key = uipod_track_intel_key_for_id3(id3);
            uint64_t prev_key = __atomic_load_n(&s_active_key_hash, __ATOMIC_ACQUIRE);
            if (cur_key != 0 && cur_key != prev_key)
                uipod_track_intel_on_track_change(id3);
            key = cur_key;

            uint32_t dly_us = pcm_alsa_output_delay_us() + tape_deck_fx_delay_us();
            lat_ms = (float)dly_us * 0.001f;
            uint32_t dly_ms = dly_us / 1000u;

            uint32_t raw_el = (uint32_t)id3->elapsed;
            uint32_t raw_len = (uint32_t)id3->length;
            if (id3->codectype == AFMT_NSF)
            {
                uint32_t rs_dur = uipod_rs_codec_duration_ms();
                uint32_t rs_el  = uipod_rs_codec_elapsed_ms();
                if (rs_dur > 0)
                {
                    raw_len = rs_dur;
                    raw_el  = rs_el;
                }
            }
            if (id3->cuesheet && id3->cuesheet->track_count > 1 &&
                id3->cuesheet->curr_track_idx >= 0 &&
                id3->cuesheet->curr_track_idx < id3->cuesheet->track_count)
            {
                int idx = id3->cuesheet->curr_track_idx;
                uint32_t off = (uint32_t)id3->cuesheet->tracks[idx].offset;
                raw_el = (raw_el > off) ? (raw_el - off) : 0u;
                if (idx + 1 < id3->cuesheet->track_count &&
                    id3->cuesheet->tracks[idx + 1].offset > off)
                {
                    raw_len = (uint32_t)(id3->cuesheet->tracks[idx + 1].offset - off);
                }
                else if (raw_len > off)
                {
                    raw_len -= off;
                }
            }

            playhead_ms = (raw_el > dly_ms) ? (raw_el - dly_ms) : 0u;
            dur_ms      = raw_len;
        }
    }

    uipod_rs_intel_step_live(st, key, playhead_ms, dur_ms, lat_ms, now_ms);
}

bool uipod_track_intel_get_analysis(struct UIPodPluginTrackAnalysis *out)
{
    return uipod_rs_intel_get_active_analysis(out);
}

bool uipod_track_intel_get_live(struct UIPodPluginLiveTelemetry *out)
{
    uipod_track_intel_tick_live();
    return uipod_rs_intel_get_live_telemetry(out);
}
