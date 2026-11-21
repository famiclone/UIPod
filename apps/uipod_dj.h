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

typedef void (*uipod_dj_pcm_sink_fn)(const int16_t *stereo_s16,
                                     int frames,
                                     unsigned long elapsed,
                                     long offset);

/* Sync `global_settings.dj_*` into `uipod-dj`. */
void uipod_dj_apply_settings(void);

/* Cancel any staged or active DJ transition immediately (seek, stop, prev, double-skip). */
void uipod_dj_reset(void);

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

/* Called inside `pcmbuf_start_track_change(TRACK_CHANGE_MANUAL)` BEFORE `pcmbuf_play_stop()`. */
void uipod_dj_on_manual_skip(void);

#endif /* UIPOD_DJ_H */
