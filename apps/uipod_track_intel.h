/***************************************************************************
 * UIPod: C Firmware Coordinator for `uipod-track-intel` (`uipod_track_intel.c`).
 ****************************************************************************/
#ifndef UIPOD_TRACK_INTEL_H
#define UIPOD_TRACK_INTEL_H

#include <stdbool.h>
#include <stdint.h>
#include "metadata.h"
#include "uipod_rs_plugins.h"

void     uipod_track_intel_init(void);
void     uipod_track_intel_on_track_change(const struct mp3entry *id3);
void     uipod_track_intel_on_next_track(const struct mp3entry *next_id3);
uint64_t uipod_track_intel_key_for_id3(const struct mp3entry *id3);
void     uipod_track_intel_ensure_for_plugin(const char *plugin_id);
void     uipod_track_intel_tick_live(void);
bool     uipod_track_intel_get_analysis(struct UIPodPluginTrackAnalysis *out);
bool     uipod_track_intel_get_live(struct UIPodPluginLiveTelemetry *out);

#endif /* UIPOD_TRACK_INTEL_H */
