/***************************************************************************
 * UIPod: C FFI declarations for `crates/uipod-track-intel` (`libuipod_ffi.a`).
 ****************************************************************************/
#ifndef UIPOD_RS_TRACK_INTEL_H
#define UIPOD_RS_TRACK_INTEL_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "uipod_rs_plugins.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef size_t (*uipod_intel_pcm_fallback_fn)(int16_t *out_stereo,
                                              size_t max_frames,
                                              uint32_t *out_sr);

void uipod_rs_intel_init(const char *db_path,
                         uipod_intel_pcm_fallback_fn pcm_fallback);

uint64_t uipod_rs_intel_compute_key(const char *raw_path,
                                    uint64_t file_size,
                                    int64_t mtime_sec,
                                    int32_t cue_subtrack,
                                    uint32_t duration_ms);

int32_t uipod_rs_intel_on_track_change(const char *raw_path,
                                       uint64_t file_size,
                                       int64_t mtime_sec,
                                       int32_t cue_subtrack,
                                       uint32_t slice_start_ms,
                                       uint32_t duration_ms,
                                       uint32_t sample_rate,
                                       const char *title,
                                       const char *artist,
                                       const char *album,
                                       const char *genre);

void uipod_rs_intel_prefetch_track(const char *raw_path,
                                   uint64_t file_size,
                                   int64_t mtime_sec,
                                   int32_t cue_subtrack,
                                   uint32_t slice_start_ms,
                                   uint32_t duration_ms,
                                   uint32_t sample_rate,
                                   const char *title,
                                   const char *artist,
                                   const char *album,
                                   const char *genre);

int32_t uipod_rs_intel_is_ready(uint64_t key_hash);

int32_t uipod_rs_intel_ensure_current(const char *raw_path,
                                      uint64_t file_size,
                                      int64_t mtime_sec,
                                      int32_t cue_subtrack,
                                      uint32_t slice_start_ms,
                                      uint32_t duration_ms,
                                      uint32_t sample_rate,
                                      const char *title,
                                      const char *artist,
                                      const char *album,
                                      const char *genre,
                                      uint32_t timeout_ms);

void uipod_rs_intel_clear_active(void);

int32_t uipod_rs_intel_lookup_bpm(uint64_t key_hash,
                                  float *out_bpm,
                                  float *out_conf);

int32_t uipod_rs_intel_lookup_tempo(uint64_t key_hash,
                                    float *out_bpm,
                                    float *out_conf,
                                    uint32_t *out_first_beat_ms);

void uipod_rs_intel_audio_tap_push(const int16_t *stereo_s16,
                                   size_t frames,
                                   uint32_t sample_rate,
                                   uint32_t output_delay_us,
                                   uint64_t now_ms);

void uipod_rs_intel_step_live(uint8_t status,
                              uint64_t track_key_hash,
                              uint32_t playhead_ms,
                              uint32_t duration_ms,
                              float output_latency_ms,
                              uint64_t now_ms);

bool uipod_rs_intel_get_active_analysis(struct UIPodPluginTrackAnalysis *out);
bool uipod_rs_intel_get_live_telemetry(struct UIPodPluginLiveTelemetry *out);

void uipod_rs_dj_set_bpm_hints(float bpm_a, float conf_a, float bpm_b, float conf_b);
void uipod_rs_dj_set_intel_hints(float bpm_a,
                                 float conf_a,
                                 int32_t first_beat_a_ms,
                                 float bpm_b,
                                 float conf_b,
                                 int32_t first_beat_b_ms);

/* Manual Dual-Deck DJ Engine (`ABI v11+`, `DJ Mixer` Plugin) */
int32_t uipod_rs_manual_dj_is_active(void);
int32_t uipod_rs_manual_dj_session_begin(uint32_t sample_rate);
void    uipod_rs_manual_dj_session_end(uint8_t *out_dom_deck,
                                       char *out_path,
                                       size_t path_cap,
                                       uint32_t *out_elapsed_ms,
                                       uint8_t *out_playing);
bool    uipod_rs_manual_dj_load_deck(uint8_t deck,
                                     const char *path,
                                     const char *title,
                                     const char *artist,
                                     uint32_t duration_ms,
                                     uint32_t start_ms,
                                     bool start_paused,
                                     uint64_t intel_key_hash,
                                     const int16_t *seeded_pcm,
                                     size_t seeded_frames);
void    uipod_rs_manual_dj_set_playing(uint8_t deck, bool playing, bool quantize);
void    uipod_rs_manual_dj_cue(uint8_t deck, uint8_t cue_action);
void    uipod_rs_manual_dj_seek_beats(uint8_t deck, int32_t delta_beats);
void    uipod_rs_manual_dj_set_crossfader(float position, bool bass_swap_enabled);
void    uipod_rs_manual_dj_set_tempo(uint8_t deck,
                                     float target_bpm_ratio,
                                     uint8_t tempo_mode,
                                     float momentary_bend);
void    uipod_rs_manual_dj_set_sync(uint8_t deck, bool enabled);
void    uipod_rs_manual_dj_set_loop(uint8_t deck, uint8_t loop_beats);
void    uipod_rs_manual_dj_set_filter(uint8_t deck, float filter_value);
size_t  uipod_rs_manual_dj_render_s16(int16_t *out_stereo_s16,
                                      size_t frames,
                                      uint32_t sample_rate);
bool    uipod_rs_manual_dj_get_telemetry(struct UIPodDjTelemetry *out,
                                         int16_t analog_lx,
                                         int16_t analog_ly,
                                         int16_t analog_rx,
                                         int16_t analog_ry);

#ifdef __cplusplus
}
#endif

#endif /* UIPOD_RS_TRACK_INTEL_H */
