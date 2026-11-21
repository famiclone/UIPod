/***************************************************************************
 * UIPod: MIDI SoundFont (.sf2) persistence & flat SoundFont selector.
 ****************************************************************************/
#ifndef UIPOD_MIDI_H
#define UIPOD_MIDI_H

#include <stdbool.h>

/* Load saved custom SoundFont path (if any) from ROCKBOX_DIR "/soundfont.cfg". */
void uipod_midi_init(void);

/* True ONLY when `path` is a SoundFont-synthesized MIDI track (.mid/.midi/.kar/.rmi). */
bool uipod_midi_is_midi_path(const char *path);

/* Apply `sf2_path` (or NULL / "" to restore default `Mobile_0100_base.sf2`),
 * persist to `soundfont.cfg`, and show an error splash if invalid. */
bool uipod_midi_apply_soundfont(const char *sf2_path);

/* Open the flat SoundFont selector page (Default + `.sf2` files from
 * Settings -> Library -> SoundFont Folders). */
void uipod_midi_pick_soundfont(void);

#endif /* UIPOD_MIDI_H */
