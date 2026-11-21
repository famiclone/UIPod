/* UIPod: SQLite-backed library index of the configured source folders.
 *
 * The DB (ROCKBOX_DIR/library.db) is a *derived index* of the SD-card files --
 * migrated across schema versions, never wholesale-rebuilt except when the file
 * is unreadable.  Reconcile is stat-gated: a no-change load costs one stat per
 * known directory (a blink); tags are parsed only for new/changed files.  It
 * powers Shuffle All (no filesystem walk) and the Artists/Albums browse. */
#ifndef _UIPOD_LIBRARY_H
#define _UIPOD_LIBRARY_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Open-or-create+migrate the index, then reconcile it against the source
 * folders.  Call once at startup, after settings are up. */
void uipod_library_init(void);
void uipod_library_close(void);

/* Stat-gated incremental reconcile: at startup and whenever a source folder is
 * added or removed.  force_full re-reads every directory (manual "Rescan
 * Library", for in-place retags that don't bump dir mtimes).  Returns the
 * number of track rows inserted/updated/deleted. */
int  uipod_library_reconcile(bool force_full);

/* USB unplugged: music may have arrived over MTP.  Marks the index stale (safe from
 * any event handler); library screens reconcile on next open, unless a computer is
 * attached, as a half-copied file would be indexed and never revisited. */
void uipod_library_mark_stale(void);
void uipod_library_refresh_if_stale(void);
void uipod_library_bump_remote_gen(void);

/* Shuffle All's track set: every Music path in title order, one callback per
 * row (return false to stop).  Past UIPOD_MAX_FILES_IN_PLAYLIST rows it is a
 * uniform random sample, so shuffle can reach any track. */
void uipod_library_music_paths(bool (*cb)(const char *path, void *ctx),
                                 void *ctx);

/* Browse enumerators: one callback per row, no size caps -- the caller owns
 * storage, so there is no arbitrary MAX to silently truncate against. */
void uipod_library_artists(int category,
        void (*cb)(const char *browse_artist, int track_count, void *ctx),
        void *ctx);
/* browse_artist==NULL -> every album in the library (the flat "Albums" facet). */
void uipod_library_albums(int category, const char *browse_artist,
        void (*cb)(const char *album, int year, void *ctx), void *ctx);
/* Tracks of one album (album==NULL -> the artist's whole library; with
 * browse_artist==NULL too -> every song, the "Songs" facet).  Rows are in the
 * order the browse queues them, so a row index maps 1:1 to a playlist index.
 * `title` may be "" (untagged) -> caller falls back to path's basename. */
void uipod_library_tracks(int category, const char *browse_artist,
        const char *album,
        void (*cb)(const char *title, const char *path, void *ctx), void *ctx);

/* Genres facet: distinct genres ("" for untagged) and tracks within one genre. */
void uipod_library_genres(int category,
        void (*cb)(const char *genre, int track_count, void *ctx), void *ctx);
void uipod_library_genre_tracks(int category, const char *genre,
        void (*cb)(const char *title, const char *path, void *ctx), void *ctx);

/* Indexed access into the deduplicated Music catalog (`TP_CAT_MUSIC`) for
 * plugins such as Music Quiz (`music_quiz.uipod`). */
int  uipod_library_music_count(void);
bool uipod_library_music_at(int index,
                            char *path_out, size_t path_cap,
                            char *artist_out, size_t artist_cap,
                            char *title_out, size_t title_cap,
                            uint32_t *duration_ms_out);

/* Expand a bare `.nsf`/`.nsfe` container path into canonical `<path>#<idx>` URIs
 * for all `TrackKind::Music` subtracks (using the SQLite index when available,
 * falling back to `uipod_rs_nsf_scan_container`).  If `fallback_if_empty` is
 * true and the file contains only SFX subtracks, emits `<path>#<start>` once.
 * Returns the number of URIs emitted. */
int  uipod_library_expand_nsf_music(const char *nsf_path,
                                    bool fallback_if_empty,
                                    bool (*cb)(const char *uri, void *ctx),
                                    void *ctx);

/* Bulk-safe variant of `uipod_library_expand_nsf_music` for unindexed multi-file
 * directory walks (1,000+ `.nsf` files): uses SQLite when indexed, otherwise
 * inspects only the 128-byte `.nsf` header / `NSFE` `plst` chunk without 6502
 * CPU emulation. */
int  uipod_library_expand_nsf_fast(const char *nsf_path,
                                   bool fallback_if_empty,
                                   bool (*cb)(const char *uri, void *ctx),
                                   void *ctx);

/* Fast RAM-cached count of `TrackKind::Music` subtracks inside a bare `.nsf`/`.nsfe`
 * container (used by File Browser rows to display `[24 ♪]` at 60 FPS). */
int  uipod_library_nsf_music_count(const char *nsf_path);

/* Look up `loop_point_ms` for an indexed `.nsf#idx` subtrack URI, or -1 if unknown. */
int  uipod_library_get_loop_point_ms(const char *track_uri);

struct uipod_rs_nsf_container_header;
struct uipod_rs_nsf_subtrack_info;

/* Load all subtracks (both `Music` and `Sfx`) for `nsf_path`, serving instantaneously
 * from SQLite `tracks` when indexed or falling back to `uipod_rs_nsf_scan_container`. */
size_t uipod_library_load_nsf_container(const char *nsf_path,
                                        struct uipod_rs_nsf_container_header *out_header,
                                        struct uipod_rs_nsf_subtrack_info *out_subtracks,
                                        size_t max_subtracks);

/* Enumerate all playable `TrackKind::Music` tracks (`track_kind == 0`, including
 * individual `.nsf#idx` subtracks and excluding SFX) under `dir_path` from the
 * SQLite index.  When `shuffle` is true, uniformly samples up to
 * `UIPOD_MAX_FILES_IN_PLAYLIST` subtracks in random order across all files.
 * Returns true if `dir_path` was served from the SQLite index, or false if
 * `dir_path` is outside indexed roots. */
bool uipod_library_folder_music_paths(const char *dir_path,
                                      bool shuffle,
                                      bool (*cb)(const char *uri, void *ctx),
                                      void *ctx);

/* Immediate File Manager cascade hooks so renaming, moving, or deleting a file
 * or folder updates/deletes all associated track & `.nsf#idx` subtrack rows. */
void uipod_library_on_path_renamed(const char *old_path,
                                   const char *new_path,
                                   bool is_dir);
void uipod_library_on_path_deleted(const char *path, bool is_dir);

#endif /* _UIPOD_LIBRARY_H */
