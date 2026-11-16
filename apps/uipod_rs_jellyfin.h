/***************************************************************************
 * UIPod: C ABI header for the pure-Rust Jellyfin crate (`uipod-jellyfin`).
 *
 * Zero Rockbox dependencies cross this boundary.
 ****************************************************************************/
#ifndef UIPOD_RS_JELLYFIN_H
#define UIPOD_RS_JELLYFIN_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum tp_jf_video_quality {
    TP_JF_VQ_AUTO_720P      = 0,
    TP_JF_VQ_TRANSCODE_720P = 1,
    TP_JF_VQ_TRANSCODE_480P = 2,
    TP_JF_VQ_ORIGINAL_1080P = 3,
};

enum tp_jf_audio_quality {
    TP_JF_AQ_ORIGINAL = 0,
    TP_JF_AQ_MP3_320  = 1,
    TP_JF_AQ_MP3_192  = 2,
};

enum tp_jf_item_type {
    TP_JF_ITEM_MOVIE   = 1,
    TP_JF_ITEM_SERIES  = 2,
    TP_JF_ITEM_SEASON  = 3,
    TP_JF_ITEM_EPISODE = 4,
    TP_JF_ITEM_VIDEO   = 5,
};

typedef uint8_t (*tp_jf_cancel_fn)(void *ctx);

typedef struct TpJfHttpVtable {
    int32_t (*http_request)(
        uint8_t is_https,
        const char *host,
        uint16_t port,
        const char *method,
        const char *path_and_query,
        const char *extra_headers,
        const uint8_t *req_body,
        size_t req_body_len,
        uint8_t allow_self_signed,
        int32_t timeout_ms,
        int32_t *out_status,
        uint8_t **out_data,
        size_t *out_len,
        char *err_buf,
        size_t err_cap
    );
    void (*http_free)(uint8_t *ptr);
    int32_t (*http_download_ex)(
        uint8_t is_https,
        const char *host,
        uint16_t port,
        const char *path_and_query,
        const char *extra_headers,
        uint8_t allow_self_signed,
        const char *dest_path,
        int32_t timeout_ms,
        tp_jf_cancel_fn cancel_cb,
        void *cancel_ctx,
        char *err_buf,
        size_t err_cap
    );
} TpJfHttpVtable;

typedef struct TpJfConfigC {
    char     url[256];
    char     username[128];
    char     password[128];
    uint8_t  video_quality;     /* enum tp_jf_video_quality */
    uint8_t  audio_quality;     /* enum tp_jf_audio_quality */
    uint8_t  allow_self_signed; /* 0 or 1 */
    uint8_t  _pad;
    uint32_t cache_limit_mb;    /* 0 = unlimited */
} TpJfConfigC;

typedef struct TpJfViewC {
    const char *id;
    const char *name;
    const char *collection_type;
    uint8_t     view_kind; /* 0 = mixed, 1 = video, 2 = music */
} TpJfViewC;

typedef struct TpJfVideoItemC {
    const char *id;
    const char *view_id;
    uint8_t     item_type; /* enum tp_jf_item_type */
    uint8_t     played;
    const char *name;
    const char *series_id;
    const char *series_name;
    const char *season_id;
    const char *season_name;
    int32_t     season_number;
    int32_t     episode_number;
    int32_t     year;
    uint32_t    duration_ms;
    uint32_t    resume_ms;
    const char *media_source_id;
    const char *container;
    const char *video_codec;
    const char *audio_codec;
    uint32_t    width;
    uint32_t    height;
    uint32_t    ratio_num;
    uint32_t    ratio_den;
    uint32_t    bit_depth;
    uint32_t    bitrate;
    const char *chapters_csv;
    const char *local_path;
} TpJfVideoItemC;

typedef struct TpJfAudioSongC {
    const char *id;
    const char *local_track_path;
    const char *local_cover_path;
    const char *cover_id;
    const char *server_path;
    const char *suffix;
    const char *title;
    const char *artist;
    const char *album_artist;
    const char *album;
    const char *genre;
    int32_t     year;
    int32_t     track;
    int32_t     disc_number;
    uint32_t    duration_secs;
    uint64_t    size;
    uint32_t    bit_rate;
    uint32_t    bit_depth;
    uint32_t    sampling_rate;
} TpJfAudioSongC;

typedef struct TpJfPlannedStreamC {
    uint8_t  is_https;
    uint8_t  allow_self_signed;
    uint8_t  is_transcoded;
    uint8_t  _pad;
    uint16_t port;
    uint16_t _pad2;
    char     host[256];
    char     path_and_query[1024];
    char     extra_headers[512];
    char     download_ext[16];
} TpJfPlannedStreamC;

typedef void (*tp_jf_on_view_fn)(void *ctx, const TpJfViewC *view);
typedef void (*tp_jf_on_video_fn)(void *ctx, const TpJfVideoItemC *item);
typedef void (*tp_jf_on_song_fn)(void *ctx, const TpJfAudioSongC *song);
typedef void (*tp_jf_on_playlist_fn)(void *ctx, const char *playlist_name,
                                     const char *m3u8_content);
typedef void (*tp_jf_on_progress_fn)(void *ctx, uint32_t synced_videos,
                                     uint32_t synced_tracks);

bool   tp_jellyfin_init(const char *video_root_utf8,
                        const char *audio_root_utf8,
                        const TpJfHttpVtable *vtable);
void   tp_jellyfin_shutdown(void);

bool   tp_jellyfin_load_config_text(const char *ini_utf8);
size_t tp_jellyfin_format_config_text(char *out_buf, size_t out_cap);
bool   tp_jellyfin_get_config(TpJfConfigC *out_cfg);
bool   tp_jellyfin_set_config(const TpJfConfigC *in_cfg);
bool   tp_jellyfin_is_configured(void);

int32_t tp_jellyfin_sync_catalog(tp_jf_on_view_fn on_view,
                                 tp_jf_on_video_fn on_video,
                                 tp_jf_on_song_fn on_song,
                                 tp_jf_on_playlist_fn on_playlist,
                                 tp_jf_on_progress_fn on_progress,
                                 void *cb_ctx,
                                 tp_jf_cancel_fn cancel_cb,
                                 void *cancel_ctx,
                                 uint32_t *out_videos,
                                 uint32_t *out_tracks,
                                 char *err_buf,
                                 size_t err_cap);

int32_t tp_jellyfin_plan_video_stream(const char *item_id_utf8,
                                      const char *media_source_id_utf8,
                                      const char *container_utf8,
                                      const char *video_codec_utf8,
                                      const char *audio_codec_utf8,
                                      uint32_t width,
                                      uint32_t height,
                                      uint32_t bit_depth,
                                      uint32_t bitrate,
                                      uint32_t start_ms,
                                      const char *play_session_id_utf8,
                                      TpJfPlannedStreamC *out_plan,
                                      char *err_buf,
                                      size_t err_cap);

size_t tp_jellyfin_resolve_downloaded_video(const char *planned_local_path_utf8,
                                            char *out_buf,
                                            size_t out_cap);
bool   tp_jellyfin_delete_downloaded_video(const char *planned_local_path_utf8);

int32_t tp_jellyfin_download_video(const char *planned_local_path_utf8,
                                   const char *item_id_utf8,
                                   const char *media_source_id_utf8,
                                   const char *container_utf8,
                                   const char *video_codec_utf8,
                                   const char *audio_codec_utf8,
                                   uint32_t width,
                                   uint32_t height,
                                   uint32_t bit_depth,
                                   uint32_t bitrate,
                                   tp_jf_cancel_fn cancel_cb,
                                   void *cancel_ctx,
                                   char *err_buf,
                                   size_t err_cap);

void tp_jellyfin_stop_encoding(const char *play_session_id_utf8);
void tp_jellyfin_report_session(uint8_t endpoint_kind,
                                const char *item_id_utf8,
                                const char *media_source_id_utf8,
                                const char *play_session_id_utf8,
                                uint32_t position_ms,
                                uint8_t is_paused);

bool    tp_jellyfin_is_audio_track_path(const char *path_utf8);
int32_t tp_jellyfin_ensure_audio_cached(const char *local_path_utf8,
                                        const char *song_id_utf8,
                                        const char *suffix_utf8,
                                        const char *cover_id_utf8,
                                        uint8_t pin,
                                        tp_jf_cancel_fn cancel_cb,
                                        void *cancel_ctx,
                                        char *err_buf,
                                        size_t err_cap);
void    tp_jellyfin_prefetch_audio(const char *local_path_utf8,
                                   const char *song_id_utf8,
                                   const char *suffix_utf8,
                                   const char *cover_id_utf8);
size_t  tp_jellyfin_cover_path_for(const char *local_track_path_utf8,
                                   const char *cover_id_utf8,
                                   char *out_buf,
                                   size_t out_cap);

uint64_t tp_jellyfin_cache_usage_bytes(void);
void     tp_jellyfin_clear_cache(uint8_t keep_pinned);

#ifdef __cplusplus
}
#endif

#endif /* UIPOD_RS_JELLYFIN_H */
