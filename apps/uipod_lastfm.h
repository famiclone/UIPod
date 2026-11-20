/***************************************************************************
 * UIPod: Last.fm Scrobbling 2.0 & Offline Scrobble Cache integration.
 *
 * Supports:
 *  - QR-code / browser authorization (`auth.getToken` + `auth.getSession`)
 *  - Direct username + password login (`auth.getMobileSession`) & `lastfm.cfg`
 *  - Non-blocking `track.updateNowPlaying` & batched `track.scrobble` (50/req)
 *  - Persistent offline scrobble cache (`lastfm_queue.tsv`) with automatic
 *    background retry & manual sync in `Settings -> Accounts -> Last.fm`
 ****************************************************************************/
#ifndef UIPOD_LASTFM_H
#define UIPOD_LASTFM_H

#include <stdbool.h>
#include <stddef.h>

struct mp3entry;

void uipod_lastfm_init(void);
void uipod_lastfm_shutdown(void);
void uipod_lastfm_notify_online(void);

/* Non-blocking playback lifecycle hooks called from `playback.c` */
void uipod_lastfm_on_track_change(const struct mp3entry *id3);
void uipod_lastfm_on_track_finish(const struct mp3entry *id3);

/* `Settings -> Accounts -> Last.fm` UI screen */
int  uipod_lastfm_menu_page(void);

#endif /* UIPOD_LASTFM_H */
