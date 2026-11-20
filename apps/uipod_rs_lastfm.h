/***************************************************************************
 * UIPod: C ABI header for the pure-Rust Last.fm crate (`uipod-lastfm`).
 *
 * Zero Rockbox dependencies cross this boundary.
 ****************************************************************************/
#ifndef UIPOD_RS_LASTFM_H
#define UIPOD_RS_LASTFM_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TpLfmHttpVtable {
    int32_t (*http_request)(
        uint8_t is_https,
        const char *host,
        uint16_t port,
        const char *method,
        const char *path_and_query,
        const char *extra_headers,
        const uint8_t *req_body,
        size_t req_body_len,
        int32_t timeout_ms,
        int32_t *out_status,
        uint8_t **out_data,
        size_t *out_len,
        char *err_buf,
        size_t err_cap
    );
    void (*http_free)(uint8_t *ptr);
} TpLfmHttpVtable;

typedef struct TpLfmConfigC {
    uint8_t enabled;
    uint8_t scrobble_streaming;
    uint8_t _pad[2];
    char    api_key[96];
    char    api_secret[96];
    char    username[96];
    char    password[96];
    char    session_key[96];
} TpLfmConfigC;

bool     tp_lastfm_init(const char *queue_file_path, const TpLfmHttpVtable *vt);
bool     tp_lastfm_load_config_text(const char *ini_text);
size_t   tp_lastfm_format_config_text(char *out_buf, size_t out_cap);
bool     tp_lastfm_get_config(TpLfmConfigC *out_cfg);
bool     tp_lastfm_set_config(const TpLfmConfigC *in_cfg);

bool     tp_lastfm_is_authenticated(void);
bool     tp_lastfm_has_api_credentials(void);
uint32_t tp_lastfm_cached_count(void);
bool     tp_lastfm_has_active_track(void);
void     tp_lastfm_clear_cache(void);
void     tp_lastfm_logout(void);

int32_t  tp_lastfm_request_auth_token(char *out_token, size_t token_cap,
                                      char *out_url, size_t url_cap,
                                      char *err_buf, size_t err_cap);
int32_t  tp_lastfm_poll_auth_session(const char *token,
                                     char *err_buf, size_t err_cap);
int32_t  tp_lastfm_login_password(const char *username,
                                  const char *password,
                                  char *err_buf, size_t err_cap);
size_t   tp_lastfm_encode_qr(const char *text,
                             uint8_t *out_modules, size_t modules_cap);

bool     tp_lastfm_on_track_start(const char *path,
                                  const char *artist,
                                  const char *track,
                                  const char *album,
                                  const char *album_artist,
                                  uint32_t track_number,
                                  uint64_t duration_ms,
                                  bool is_streaming,
                                  uint64_t now_unix,
                                  uint64_t now_mono_ms);
bool     tp_lastfm_on_playback_tick(uint64_t now_mono_ms, bool is_playing);
bool     tp_lastfm_on_track_finish(uint64_t now_mono_ms);

int32_t  tp_lastfm_sync_now(uint64_t now_unix,
                            bool *out_cfg_updated,
                            char *err_buf, size_t err_cap);

#ifdef __cplusplus
}
#endif

#endif /* UIPOD_RS_LASTFM_H */
