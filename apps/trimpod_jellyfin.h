/***************************************************************************
 * UIPod: Jellyfin integration bridge.
 *
 * Connects `trimpod-jellyfin` + local HTTP/TLS loopback stream proxy to:
 *  - Hardware video player (`trimpod_video_play_stream`) with automatic
 *    Direct Stream (`Static=true`) or H.264/AAC server transcoding (`stream.ts`)
 *  - Offline video downloads in `/mnt/SDCARD/Media/Videos/jellyfin`
 *  - SQLite music library (`Artists`, `Albums`, `Genres`, `Songs`, `Playlists`)
 *    and offline music downloads in `/mnt/SDCARD/Media/Music/jellyfin`
 ****************************************************************************/
#ifndef TRIMPOD_JELLYFIN_H
#define TRIMPOD_JELLYFIN_H

#include <stdbool.h>
#include <stddef.h>

struct sqlite3;
struct mp3entry;

/* ---- Lifecycle & Catalog Sync ----------------------------------------- */
void trimpod_jellyfin_init(void);
void trimpod_jellyfin_close(void);
bool trimpod_jellyfin_has_media(void);
bool trimpod_jellyfin_has_tracks(void);
bool trimpod_jellyfin_sync(bool show_summary_splash);

/* Thread-safe borrowed access to `/tmp/uipod-jellyfin.db` (`tracks` table)
 * for `trimpod_library.c` enumerators when `trimpod_source_is_jellyfin()`. */
struct sqlite3 *trimpod_jellyfin_db_lock(void);
void            trimpod_jellyfin_db_unlock(void);

/* ---- UI Screens ------------------------------------------------------- */
int trimpod_jellyfin_settings_page(bool from_source_picker);
int trimpod_jellyfin_menu_page(void);             /* MENUITEM_FUNCTION wrapper */
int trimpod_jellyfin_videos_browse(bool tv_only); /* Main -> Videos / Television */
int trimpod_jellyfin_files_browse(void);          /* Main -> Files in Jellyfin mode */

/* ---- Audio Playback, Metadata, Cover Art & Session Hooks -------------- */
bool trimpod_jellyfin_is_track_path(const char *path);
bool trimpod_jellyfin_prepare_track(const char *path, bool ui_blocking);
bool trimpod_jellyfin_download_track(const char *path);
bool trimpod_jellyfin_download_tracks(char *const *paths, int count);
bool trimpod_jellyfin_fill_id3(const char *path, struct mp3entry *id3);
void trimpod_jellyfin_enrich_id3(struct mp3entry *id3);
bool trimpod_jellyfin_get_cover_info(const char *path,
                                     char *cover_path_out,
                                     size_t cover_cap,
                                     int *out_bit_depth);
void trimpod_jellyfin_on_track_change(const char *cur_path);
void trimpod_jellyfin_on_track_finish(const struct mp3entry *id3);

#endif /* TRIMPOD_JELLYFIN_H */
