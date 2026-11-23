/***************************************************************************
 * UIPod: C ABI header for the pure-Rust ListenBrainz crate (`uipod-listenbrainz`).
 *
 * Zero Rockbox dependencies cross this boundary.
 ****************************************************************************/
#ifndef UIPOD_RS_LISTENBRAINZ_H
#define UIPOD_RS_LISTENBRAINZ_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TpLbHttpVtable {
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
} TpLbHttpVtable;

typedef struct TpLbConfigC {
    uint8_t enabled;
    uint8_t now_playing;
    uint8_t scrobble_streaming;
    uint8_t allow_self_signed;
    char    api_url[192];
    char    user_token[128];
    char    username[96];
} TpLbConfigC;

bool     tp_listenbrainz_init(const char *queue_file_path, const TpLbHttpVtable *vt);
bool     tp_listenbrainz_load_config_text(const char *ini_text);
size_t   tp_listenbrainz_format_config_text(char *out_buf, size_t out_cap);
bool     tp_listenbrainz_get_config(TpLbConfigC *out_cfg);
bool     tp_listenbrainz_set_config(const TpLbConfigC *in_cfg);

bool     tp_listenbrainz_is_authenticated(void);
bool     tp_listenbrainz_has_token(void);
bool     tp_listenbrainz_is_token_invalid(void);
uint32_t tp_listenbrainz_cached_count(void);
bool     tp_listenbrainz_has_active_track(void);
void     tp_listenbrainz_clear_cache(void);
void     tp_listenbrainz_logout(void);

int32_t  tp_listenbrainz_validate_token(char *err_buf, size_t err_cap);

bool     tp_listenbrainz_on_track_start(const char *path,
                                        const char *artist,
                                        const char *track,
                                        const char *album,
                                        const char *album_artist,
                                        uint32_t track_number,
                                        uint64_t duration_ms,
                                        bool is_streaming,
                                        uint64_t now_unix,
                                        uint64_t now_mono_ms);
bool     tp_listenbrainz_on_playback_tick(uint64_t now_mono_ms, bool is_playing);
bool     tp_listenbrainz_on_track_finish(uint64_t now_mono_ms);

int32_t  tp_listenbrainz_sync_now(uint64_t now_unix,
                                  bool *out_cfg_updated,
                                  char *err_buf, size_t err_cap);

#ifdef __cplusplus
}
#endif

#endif /* UIPOD_RS_LISTENBRAINZ_H */
