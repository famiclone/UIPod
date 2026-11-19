/* UIPod: the Playlists screen (see uipod_playlists.c).  A uipod_page that
 * lists the catalog's playlists plus a "+ Add Playlist" row, mirroring the
 * Music Folders settings page. */
#ifndef _UIPOD_PLAYLISTS_H
#define _UIPOD_PLAYLISTS_H

#include <stdbool.h>
#include <stddef.h>
#include "playlist.h"          /* struct playlist_insert_context */

/* root menu Playlists entry (a root_menu items[] function) */
int uipod_playlists_screen(void *param);

/* Consume a pending Play/Play Shuffled request and start playback.  Returns
 * 1 (started -> go to Now Playing), 0 (nothing pending -> show the file list),
 * or -1 (empty/failed -> back to the list).  Called by root_menu.c's
 * playlist_view() so the file list ends up behind Now Playing. */
int uipod_playlists_start_pending(void);

/* Consume a pending view request (tap A on a playlist row): fill `path` with
 * the playlist's full .m3u8 path and return true.  Called by playlist_view()
 * to open the playlist's track listing in the stock viewer. */
bool uipod_playlists_take_pending_view(char *path, size_t len);

/* Show the Playlists screen in "pick" mode: the chosen (or newly created)
 * playlist receives `sel` (a file or directory) via the stock Rockbox
 * catalog_insert_into() -- a directory is expanded into its tracks. */
void uipod_playlists_pick(const char *sel, int sel_attr);

/* Hold-A music context menu: Play / Play Shuffled (not on a track) / Play Next
 * / Add to Play Queue / Add to Playlist, all handled here.  Returns true when a
 * Play started a new queue: the caller goes to Now Playing.  The tracks form
 * takes a path set (a library artist/album has no single path to expand). */
bool uipod_music_context(const char *title, const char *path, int attr);
bool uipod_music_context_tracks(const char *title, char **paths, int count);
bool uipod_music_context_track_in_list(const char *title, char **paths,
                                       int count, int sel);

/* Put a track or directory (recursive) into the live queue at `position`:
 * PLAYLIST_INSERT (after the current track), PLAYLIST_INSERT_LAST (the end)
 * or PLAYLIST_REPLACE (confirm and start a new queue from it).  A new queue is
 * shuffled if `shuffle`.  Returns true when the queue started playing from
 * this call. */
bool uipod_queue_add(const char *path, int attr, int position, bool shuffle);

/* Tap-to-play: `path` alone becomes the new queue (an m4b resumes at its saved
 * position); the track already playing is shown, not restarted.  Returns true
 * when it is playing (caller -> Now Playing). */
bool uipod_play_track(const char *path);

/* Non-prompting single-track snippet start at `start_ms` (used by Music Quiz). */
bool uipod_play_single_at_ms(const char *path, unsigned long start_ms);

/* Build a full queue from `paths[0 .. count-1]` and start playback at
 * `start_idx` so album/playlist/folder tracks continue playing sequentially. */
bool uipod_play_tracks_at(char *const *paths, int count, int start_idx);

/* 3-by-3 windowed background prefetch coordinator for remote tracks:
 * - On queue start at `pos`: prefetches `[pos+1, pos+2]` (3-track initial window `[pos..pos+2]`).
 * - When the 3rd track (`pos+2`) begins playing: advances window by +3 and prefetches `[pos+3..pos+5]`. */
void uipod_prefetch_on_queue_start(void);
bool uipod_prefetch_on_track_change(void);
bool uipod_prefetch_allow_peek_offset(int peek_offset);

/* Now Playing's Shuffle: a fresh random order for the whole live queue every
 * time, played from its top (the current track stops). */
void uipod_queue_reshuffle(void);

/* A queue build in steps: begin() opens the insert at `position`
 * (PLAYLIST_REPLACE confirms the erase and starts fresh; false = declined or
 * no control file), add_path() fills it, end() starts a fresh queue from its
 * first track (shuffled first if `shuffle`) and returns whether playback
 * started.  add_path doubles as a uipod_library enumerator callback
 * (false = the engine refused). */
struct uipod_queue
{
    struct playlist_insert_context ctx;
    bool fresh;
    int pos;
    unsigned long elapsed, offset;   /* where the first track starts */
};
bool uipod_queue_begin(struct uipod_queue *q, int position);
bool uipod_queue_add_path(const char *path, void *q);
bool uipod_queue_end(struct uipod_queue *q, bool shuffle);

/* True when `path` is the loaded track, resuming it if paused: the caller
 * shows Now Playing instead of restarting it. */
bool uipod_resume_if_current(const char *path);

#endif /* _UIPOD_PLAYLISTS_H */
