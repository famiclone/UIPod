/* UIPod: Phase 3 Native NSF/NSFe 1024x768 UI screens & helpers
 *  - Now Playing `Channels ›` live hardware APU channel toggle submenu
 *  - File Browser `.nsf` / `.nsfe` virtual folder drill-down (`Shuffle Game Music`,
 *    indexed `TrackKind::Music` subtracks, collapsed `Sound Effects (N) ›` subfolder,
 *    and per-subtrack `Add to Playlist...` / `Track Info` context menu) */
#ifndef _UIPOD_NSF_UI_H
#define _UIPOD_NSF_UI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* True when `path` ends with `.nsf` or `.nsfe` (case-insensitive) and does NOT
 * carry a `#<subtrack>` fragment. */
bool uipod_nsf_is_container_file(const char *path);

/* Primary Famicom expansion chip badge string (`"2A03"`, `"VRC6"`, `"MMC5"`,
 * `"FDS"`, `"VRC7"`, `"N163"`, `"5B"`). */
const char *uipod_nsf_chip_badge_str(uint8_t chip_flags);

/* Dynamic row label + summary badge (`"Channels\x1e3/5 ON"`) for the Now Playing
 * context menu. */
char *uipod_nsf_channels_menu_label(int selected_item,
                                    void *data,
                                    char *buffer,
                                    size_t buffer_len);

/* `MENUITEM_FUNCTION_DYNTEXT` callback for Now Playing -> `Channels ›`. */
int  uipod_nsf_channels_menu_fn(void);
void uipod_nsf_channels_screen(void);

/* Virtual `.nsf`/`.nsfe` container drill-down page pushed onto the File Browser
 * navigation stack.  Returns `GO_TO_WPS` when playback starts, or `GO_TO_ROOT` /
 * `0` when backing out to the parent directory. */
int  uipod_nsf_container_screen(const char *nsf_path,
                                const char *info_file_path);

#endif /* _UIPOD_NSF_UI_H */
