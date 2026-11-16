/* UIPod: full-screen video playback with Now Playing style overlays. */
#ifndef _UIPOD_VIDEO_H
#define _UIPOD_VIDEO_H

#include <stdbool.h>

struct uipod_video_stream_opts
{
    const char *url;             /* local path or http://127.0.0.1:<port>/... */
    const char *resume_id;       /* optional canonical key (e.g. "jellyfin://video/<id>") */
    int         duration_ms;     /* known duration when container omits it (e.g. MPEG-TS) */
    int         server_resume_ms;/* fallback server resume point if no local entry */
    int         width;
    int         height;
    int         ratio_num;
    int         ratio_den;
    bool        is_transcoded;   /* true => seek restarts transcode at StartTimeTicks */
    bool        force_from_start;/* true => start from 0:00 regardless of saved resume */
    const int  *chapter_ms;
    int         chapters;
    bool      (*on_reopen_at)(void *ctx, int start_ms);
    void      (*on_progress)(void *ctx, int position_ms, bool paused, bool stopped);
    void       *cb_ctx;
};

/* Plays one file until it ends or the user leaves.  Stops any music first. */
void uipod_video_play(const char *path);

/* Plays a streamed or Jellyfin-managed video with pre-probed metadata & callbacks. */
void uipod_video_play_stream(const struct uipod_video_stream_opts *opts);

#endif /* _UIPOD_VIDEO_H */
