/* Trimpod: runs TrimUI's stock hardware video player (tplayerslave) as a child. */
#ifndef _VIDEO_TARGET_H_
#define _VIDEO_TARGET_H_

#include <stdbool.h>
#include <stddef.h>

#define RETROHH_VIDEO_MAX_CHAPTERS 99
#define RETROHH_VIDEO_MAX_TRACKS   16

enum retrohh_video_aspect
{
    RETROHH_VIDEO_FIT = 0,   /* whole picture, black bars */
    RETROHH_VIDEO_STRETCH,   /* fills the screen, shape distorted */
    RETROHH_VIDEO_ASPECT_COUNT,
};

struct retrohh_video_track
{
    int  stream_index;
    char label[48];
    bool external;
    char ext_path[256];
};

struct retrohh_video_info
{
    bool hevc;
    int  width, height;
    int  ratio_num, ratio_den;     /* display shape, or 0 if the file doesn't say */
    int  chapter_ms[RETROHH_VIDEO_MAX_CHAPTERS];
    int  chapters;
    struct retrohh_video_track audio_tracks[RETROHH_VIDEO_MAX_TRACKS];
    int  audio_count;
    struct retrohh_video_track sub_tracks[RETROHH_VIDEO_MAX_TRACKS];
    int  sub_count;
};

/* Reads the file with ffprobe; false if it couldn't be read. */
bool retrohh_video_probe(const char *path, struct retrohh_video_info *info);

/* start_ms > 0 starts partway through the file. */
bool retrohh_video_start(const char *path, int start_ms);
void retrohh_video_stop(void);
bool retrohh_video_running(void);

/* False while the player hasn't reported yet. */
bool retrohh_video_status(int *position_ms, int *duration_ms, bool *playing);

void retrohh_video_pause(bool paused);
void retrohh_video_seek(int position_ms);
void retrohh_video_set_aspect(const struct retrohh_video_info *info,
                              enum retrohh_video_aspect aspect);
bool retrohh_video_set_audio_track(int track_ordinal);
bool retrohh_video_extract_subtitle_srt(const char *video_path,
                                        const struct retrohh_video_track *sub,
                                        char *out_srt_path,
                                        size_t out_cap);

/* Show or hide the video through the app window; the bottom overlay_rows LCD rows
 * stay on top of it. */
void retrohh_video_set_overlay(bool video_visible, int overlay_rows);

#endif /* _VIDEO_TARGET_H_ */
