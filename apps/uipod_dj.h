/***************************************************************************
 * UIPod: Dual-Deck Smart DJ Crossfade C Coordinator (`uipod_dj.h`).
 *
 * Bridges Rockbox's cooperative `codec_thread` / `pcmbuf` / `playback` state
 * machine to the pure-Rust `uipod-dj` engine (Beatmatch WSOLA, Vinyl Sync,
 * Butterworth Bass Swap, and Filter Wash fallback).
 ****************************************************************************/
#ifndef UIPOD_DJ_H
#define UIPOD_DJ_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

struct mp3entry;
struct uipod_rs_dj_playhead;

typedef void (*uipod_dj_pcm_sink_fn)(const int16_t *stereo_s16,
                                     int frames,
                                     unsigned long elapsed,
                                     long offset);

/* Sync `global_settings.dj_*` into `uipod-dj`. */
void uipod_dj_apply_settings(void);

/* Cancel any staged or active DJ transition immediately (seek, stop, prev, double-skip). */
void uipod_dj_reset(void);

/* Returns true while `DualDeckMixer` is capturing or rendering a transition. */
bool uipod_dj_is_active(void);

/* Query speaker-latency-compensated audible playhead & telemetry for Now Playing. */
bool uipod_dj_query_playhead(struct uipod_rs_dj_playhead *out_ph,
                             const char **out_deck_a_path,
                             const char **out_deck_b_path);

/* Returns true when normal zero-copy single-track passthrough should run (0% CPU). */
bool uipod_dj_is_passthrough(const struct mp3entry *cur_id3);

/* Feed `44.1 kHz` stereo S16 decoded frames from `codec_pcmbuf_insert_callback`. */
void uipod_dj_process_codec_frames(const struct mp3entry *cur_id3,
                                   const int16_t *stereo_s16,
                                   int frames,
                                   uipod_dj_pcm_sink_fn sink_fn);

/* Called at the end of `codec_run_proc()` before closing a decoder. */
void uipod_dj_on_codec_eof(const struct mp3entry *cur_id3,
                           uipod_dj_pcm_sink_fn sink_fn);

/* Called inside `pcmbuf_start_track_change(TRACK_CHANGE_AUTO)`.
 * Returns true if `uipod_dj` owns the transition and will stamp `chunk_transidx`
 * at the `p = 0.50` Bass Swap midpoint (caller skips immediate `pcmbuf_monitor_track_change`). */
bool uipod_dj_on_auto_track_change(void);

/* Called inside `pcmbuf_start_track_change(TRACK_CHANGE_END_OF_DATA)`. */
void uipod_dj_on_end_of_data(void);

/* Record skip delta (`+1` = single R1 Next; anything else = instant cut). */
void uipod_dj_note_skip_delta(int toskip);

/* Called inside `pcmbuf_start_track_change(TRACK_CHANGE_MANUAL)`.
 * Returns true when a Quick DJ Cut transition was armed into `DualDeckMixer`. */
bool uipod_dj_on_manual_skip(void);

/* ---- Manual Dual-Deck DJ Host API (`ABI v11+`, `DJ Mixer` Plugin) ---- */
struct UIPodDjTelemetry;

bool   uipod_dj_manual_session_acquire(void);
void   uipod_dj_manual_session_release(void);
bool   uipod_dj_manual_deck_load(uint8_t deck, const char *path, bool start_paused);
void   uipod_dj_manual_deck_set_playing(uint8_t deck, bool playing, bool quantize);
void   uipod_dj_manual_deck_cue(uint8_t deck, uint8_t cue_action);
void   uipod_dj_manual_deck_seek_beats(uint8_t deck, int32_t delta_beats);
void   uipod_dj_manual_set_crossfader(float position, bool bass_swap_enabled);
void   uipod_dj_manual_deck_set_tempo(uint8_t deck,
                                      float target_bpm_ratio,
                                      uint8_t tempo_mode,
                                      float momentary_bend);
void   uipod_dj_manual_deck_set_sync(uint8_t deck, bool enabled);
void   uipod_dj_manual_deck_set_loop(uint8_t deck, uint8_t loop_beats);
void   uipod_dj_manual_deck_set_filter(uint8_t deck, float filter_value);
bool   uipod_dj_manual_get_telemetry(struct UIPodDjTelemetry *out);
size_t uipod_dj_source_track_count(uint8_t source_kind);
bool   uipod_dj_source_track_at(uint8_t source_kind,
                                size_t index,
                                char *path_out, size_t path_cap,
                                char *artist_out, size_t artist_cap,
                                char *title_out, size_t title_cap,
                                uint32_t *duration_ms_out);

#endif /* UIPOD_DJ_H */
