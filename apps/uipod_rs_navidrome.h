/***************************************************************************
 * UIPod: C ABI header for the pure-Rust Navidrome crate (`libuipod_navidrome.a`).
 *
 * Zero Rockbox dependencies cross this boundary.
 ****************************************************************************/
#ifndef UIPOD_RS_NAVIDROME_H
#define UIPOD_RS_NAVIDROME_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum tp_nav_quality {
    TP_NAV_QUALITY_ORIGINAL = 0,
    TP_NAV_QUALITY_MP3_320  = 1,
    TP_NAV_QUALITY_MP3_192  = 2,
};

typedef uint8_t (*tp_nav_cancel_fn)(void *ctx);

typedef struct TpNavHttpVtable {
    int32_t (*http_get)(
        uint8_t is_https,
        const char *host,
        uint16_t port,
        const char *path_and_query,
        uint8_t allow_self_signed,
        int32_t timeout_ms,
        uint8_t **out_data,
        size_t *out_len,
        char *err_buf,
        size_t err_cap
    );
    void (*http_free)(uint8_t *ptr);
    int32_t (*http_download_file)(
        uint8_t is_https,
        const char *host,
        uint16_t port,
        const char *path_and_query,
        uint8_t allow_self_signed,
        const char *dest_path,
        int32_t timeout_ms,
        tp_nav_cancel_fn cancel_cb,
        void *cancel_ctx,
        char *err_buf,
        size_t err_cap
    );
} TpNavHttpVtable;

typedef struct TpNavConfigC {
    char     url[256];
    char     username[128];
    char     password[128];
    char     cache_location[256];
    uint8_t  quality;           /* enum tp_nav_quality */
    uint8_t  allow_self_signed; /* 0 or 1 */
    uint16_t _pad;
    uint32_t cache_limit_mb;    /* 0 = unlimited */
} TpNavConfigC;

typedef struct TpNavSongC {
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
} TpNavSongC;

typedef struct TpNavTreeEntryC {
    uint8_t is_dir;
    char    name[256];
    char    target_path[512];
} TpNavTreeEntryC;

typedef void (*tp_nav_on_song_fn)(void *ctx, const TpNavSongC *song);
typedef void (*tp_nav_on_playlist_fn)(void *ctx, const char *playlist_name,
                                      const char *m3u8_content);
typedef void (*tp_nav_on_progress_fn)(void *ctx, uint32_t synced_tracks);
typedef uint8_t (*tp_nav_path_iter_fn)(void *ctx, const char *path);

bool   tp_navidrome_init(const char *cache_root_utf8, const TpNavHttpVtable *vtable);
void   tp_navidrome_shutdown(void);

bool   tp_navidrome_load_config_text(const char *ini_utf8);
size_t tp_navidrome_format_config_text(char *out_buf, size_t out_cap);
bool   tp_navidrome_get_config(TpNavConfigC *out_cfg);
bool   tp_navidrome_set_config(const TpNavConfigC *in_cfg);
size_t tp_navidrome_get_cache_root(char *out_buf, size_t out_cap);
bool   tp_navidrome_is_busy(void);
bool   tp_navidrome_is_configured(void);

bool   tp_navidrome_ping(char *err_buf, size_t err_cap);
int32_t tp_navidrome_sync_catalog(tp_nav_on_song_fn on_song,
                                  tp_nav_on_playlist_fn on_playlist,
                                  tp_nav_on_progress_fn on_progress,
                                  tp_nav_cancel_fn cancel_cb,
                                  void *user_ctx,
                                  char *err_buf,
                                  size_t err_cap);
int32_t tp_navidrome_sync_starred(tp_nav_on_song_fn on_song,
                                  tp_nav_on_playlist_fn on_playlist,
                                  tp_nav_cancel_fn cancel_cb,
                                  void *user_ctx,
                                  char *err_buf,
                                  size_t err_cap);

size_t tp_navidrome_track_path_for(const char *server_path_utf8,
                                   const char *artist_utf8,
                                   const char *album_utf8,
                                   int32_t track_no,
                                   const char *title_utf8,
                                   const char *song_id_utf8,
                                   const char *suffix_utf8,
                                   uint8_t quality,
                                   char *out_buf,
                                   size_t out_cap);
size_t tp_navidrome_disambiguate_track_path(const char *base_path_utf8,
                                            const char *song_id_utf8,
                                            const char *suffix_utf8,
                                            uint8_t quality,
                                            char *out_buf,
                                            size_t out_cap);
size_t tp_navidrome_cover_path_for(const char *local_track_path_utf8,
                                   const char *cover_id_utf8,
                                   char *out_buf,
                                   size_t out_cap);
bool   tp_navidrome_is_track_path(const char *path_utf8);
bool   tp_navidrome_pin_track(const char *local_path_utf8);
bool   tp_navidrome_is_track_pinned(const char *local_path_utf8);

int32_t tp_navidrome_ensure_track_cached(const char *local_path_utf8,
                                         const char *song_id_utf8,
                                         const char *cover_id_utf8,
                                         tp_nav_cancel_fn cancel_cb,
                                         void *cancel_ctx,
                                         char *err_buf,
                                         size_t err_cap);
bool   tp_navidrome_ensure_cover_cached(const char *local_track_path_utf8,
                                        const char *cover_id_utf8,
                                        char *out_buf,
                                        size_t out_cap);
void   tp_navidrome_enqueue_prefetch(const char *path1_utf8,
                                     const char *song1_utf8,
                                     const char *cover1_utf8,
                                     const char *path2_utf8,
                                     const char *song2_utf8,
                                     const char *cover2_utf8);
void   tp_navidrome_enqueue_prefetch3(const char *path1_utf8,
                                      const char *song1_utf8,
                                      const char *cover1_utf8,
                                      const char *path2_utf8,
                                      const char *song2_utf8,
                                      const char *cover2_utf8,
                                      const char *path3_utf8,
                                      const char *song3_utf8,
                                      const char *cover3_utf8);
bool   tp_navidrome_take_prefetch_done(void);

void   tp_navidrome_notify_track_start(const char *song_id_utf8);
void   tp_navidrome_notify_track_played(const char *song_id_utf8,
                                        uint32_t played_secs,
                                        uint32_t duration_secs);

uint64_t tp_navidrome_cache_usage_bytes(void);
bool     tp_navidrome_clear_cache(void);

void   tp_navidrome_tree_clear(void);
void   tp_navidrome_tree_insert(const char *server_path_utf8,
                                const char *local_track_path_utf8,
                                const char *fallback_title_utf8);
void   tp_navidrome_tree_finalize(void);
size_t tp_navidrome_tree_list_count(const char *rel_dir_utf8);
bool   tp_navidrome_tree_list_get(const char *rel_dir_utf8,
                                  size_t index,
                                  TpNavTreeEntryC *out_entry);
void   tp_navidrome_tree_collect_tracks(const char *rel_dir_utf8,
                                        tp_nav_path_iter_fn cb,
                                        void *ctx);

#ifdef __cplusplus
}
#endif

#endif /* UIPOD_RS_NAVIDROME_H */
