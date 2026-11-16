/***************************************************************************
 * Trimpod: Navidrome / Subsonic integration bridge.
 *
 * Thin C adapter connecting the pure-Rust `trimpod-navidrome` crate and
 * `navidrome-http-target` transport to Trimpod's SQLite library browse,
 * virtual `Files` folder browser, playlists, playback buffering/prefetch,
 * cover art, and Subsonic scrobbling.
 ****************************************************************************/
#ifndef TRIMPOD_NAVIDROME_H
#define TRIMPOD_NAVIDROME_H

#include <stdbool.h>
#include <stddef.h>

struct sqlite3;
struct mp3entry;

/* ---- Source type state (`ROCKBOX_DIR "/source.txt"`) ------------------ */
enum trimpod_source_kind {
    TRIMPOD_SOURCE_LOCAL     = 0,
    TRIMPOD_SOURCE_NAVIDROME = 1,
    TRIMPOD_SOURCE_JELLYFIN  = 2,
};

bool trimpod_source_is_configured(void);
enum trimpod_source_kind trimpod_source_get(void);
void trimpod_source_set(enum trimpod_source_kind kind);
bool trimpod_source_is_navidrome(void);
bool trimpod_source_is_jellyfin(void);
void trimpod_source_set_navidrome(bool navidrome);

/* ---- Lifecycle & Catalog Sync ----------------------------------------- */
void trimpod_navidrome_init(void);
void trimpod_navidrome_close(void);
bool trimpod_navidrome_has_tracks(void);
bool trimpod_navidrome_sync(bool show_summary_splash);

/* Thread-safe borrowed access to `/tmp/trimpod-navidrome.db` for
 * `trimpod_library.c` enumerators when `trimpod_source_is_navidrome()`. */
struct sqlite3 *trimpod_navidrome_db_lock(void);
void            trimpod_navidrome_db_unlock(void);

/* ---- UI Screens ------------------------------------------------------- */
/* Settings / Setup page (`from_source_picker = true` switches Source to
 * Navidrome and jumps to Main once connected & synced). */
int trimpod_navidrome_settings_page(bool from_source_picker);
int trimpod_navidrome_menu_page(void);      /* MENUITEM_FUNCTION wrapper */
int trimpod_navidrome_files_browse(void);   /* Main -> Files in Navidrome mode */

/* ---- Playback, Metadata, Cover Art & Scrobble Hooks ------------------- */
bool trimpod_navidrome_is_track_path(const char *path);

/* Ensures `path` is downloaded into the persistent SD cache.
 * When `ui_blocking` is true (called on the UI thread in `playlist_start`),
 * renders a cancellable `"Buffering..."` screen on cache miss. */
bool trimpod_navidrome_prepare_track(const char *path, bool ui_blocking);

/* Explicitly downloads track(s) + album `cover.jpg` to `/mnt/SDCARD/Media/Music/navidrome`
 * with foreground progress (`B` cancels) and pins them against cache eviction. */
bool trimpod_navidrome_download_track(const char *path);
bool trimpod_navidrome_download_tracks(char *const *paths, int count);

/* Populates `id3` from `navidrome.db` without touching network/audio files
 * (instantaneous playlist viewer browsing for uncached tracks). */
bool trimpod_navidrome_fill_id3(const char *path, struct mp3entry *id3);

/* Fills missing tags / duration on `id3` from `navidrome.db` (e.g. transcoded
 * MP3 streams whose stdout pipe omitted tags or VBR frame headers). */
void trimpod_navidrome_enrich_id3(struct mp3entry *id3);

/* Fetches/returns cached cover art `.jpg` path + original lossless bit depth
 * (`0` when streaming transcoded MP3) for Now Playing. */
bool trimpod_navidrome_get_cover_info(const char *path,
                                      char *cover_path_out,
                                      size_t cover_cap,
                                      int *out_bit_depth);

/* Playback lifecycle notifications from `playback.c` (audio thread). */
void trimpod_navidrome_on_track_change(const char *cur_path);
void trimpod_navidrome_on_track_finish(const struct mp3entry *id3);

#endif /* TRIMPOD_NAVIDROME_H */
