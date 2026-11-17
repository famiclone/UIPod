/***************************************************************************
 * UIPod: Persistent Non-Blocking Background Download Queue & Offline Mode.
 ****************************************************************************/
#ifndef UIPOD_DOWNLOADS_H
#define UIPOD_DOWNLOADS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum uipod_dl_kind
{
    UIPOD_DL_NAV_AUDIO = 0,
    UIPOD_DL_JF_AUDIO  = 1,
    UIPOD_DL_JF_VIDEO  = 2,
};

enum uipod_dl_state
{
    UIPOD_DL_STATE_QUEUED      = 0,
    UIPOD_DL_STATE_DOWNLOADING = 1,
    UIPOD_DL_STATE_DONE        = 2,
    UIPOD_DL_STATE_FAILED      = 3,
};

struct uipod_dl_jf_video_req
{
    const char *title;
    const char *local_path;
    const char *item_id;
    const char *media_source_id;
    const char *container;
    const char *video_codec;
    const char *audio_codec;
    uint32_t    width;
    uint32_t    height;
    uint32_t    bit_depth;
    uint32_t    bitrate;
};

/* ---- Offline Mode (`ROCKBOX_DIR "/offline_mode.txt"`) ----------------- */

void uipod_offline_mode_init(void);
bool uipod_offline_mode_get(void);
void uipod_offline_mode_set(bool enabled);

extern const struct menu_value_cb uipod_offline_mode_value_cb;

/* ---- Persistent Background Download Queue (`downloads.tsv`) ----------- */

void uipod_downloads_init(void);
void uipod_downloads_shutdown(void);

/* Header status: `out_active` is true while queued/in-flight downloads run;
 * `out_done` / `out_total` report the active batch progress (`DONE/TOTAL`). */
void uipod_downloads_header_status(bool *out_active,
                                   uint32_t *out_done,
                                   uint32_t *out_total);

/* Monotonically incremented each time a background download completes. */
uint32_t uipod_downloads_completed_seq(void);

bool uipod_downloads_enqueue_nav_track(const char *local_path,
                                       const char *song_id,
                                       const char *cover_id,
                                       const char *title);

bool uipod_downloads_enqueue_jf_audio(const char *local_path,
                                      const char *song_id,
                                      const char *suffix,
                                      const char *cover_id,
                                      const char *title);

bool uipod_downloads_enqueue_jf_video(const struct uipod_dl_jf_video_req *req);

/* Settings -> Downloads interactive queue screen */
int uipod_downloads_page(void);

#endif /* UIPOD_DOWNLOADS_H */
