/***************************************************************************
 * UIPod: ListenBrainz Scrobbling & Offline Scrobble Cache integration.
 *
 * Supports:
 *  - User Token authentication & validation (`GET /1/validate-token`)
 *  - Custom `api_url` (`https://api.listenbrainz.org` or self-hosted Maloja/Koito)
 *  - Non-blocking `playing_now` (without `listened_at`) & `single`/`import` scrobbles
 *  - Persistent offline scrobble cache (`listenbrainz_queue.tsv`) with automatic
 *    background retry & manual sync in `Settings -> Accounts -> ListenBrainz`
 ****************************************************************************/
#ifndef UIPOD_LISTENBRAINZ_H
#define UIPOD_LISTENBRAINZ_H

#include <stdbool.h>
#include <stddef.h>

struct mp3entry;

void uipod_listenbrainz_init(void);
void uipod_listenbrainz_shutdown(void);
void uipod_listenbrainz_notify_online(void);

/* Non-blocking playback lifecycle hooks called from `playback.c` */
void uipod_listenbrainz_on_track_change(const struct mp3entry *id3);
void uipod_listenbrainz_on_track_finish(const struct mp3entry *id3);

/* Dynamic label helper (`ListenBrainz\x1e@user`) for `Settings -> Accounts` */
char *uipod_listenbrainz_menu_label(int selected_item, void *data,
                                    char *buffer, size_t buffer_len);

/* `Settings -> Accounts -> ListenBrainz` UI screen */
int  uipod_listenbrainz_menu_page(void);

#endif /* UIPOD_LISTENBRAINZ_H */
