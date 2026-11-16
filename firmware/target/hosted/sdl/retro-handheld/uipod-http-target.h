/***************************************************************************
 * UIPod: Thread-safe HTTP + HTTPS 1.1 streaming transport for Navidrome
 * on TrimUI Brick (tg5040 / NextUI).
 *
 * Supports:
 *  - Both plain HTTP (`http://`) and OpenSSL TLS (`https://`) on any port
 *  - LAN (`192.168.*`, `10.*`, `172.16-31.*`), Tailscale (`100.64-127.*`),
 *    `.local` / router DNS, and public DNS
 *  - Verified CA bundle + SAN check OR self-signed homelab TLS (`allow_self_signed`)
 *  - Content-Length, Transfer-Encoding: chunked, and EOF-framed streaming
 *  - 256 KB write buffering for fast, stutter-free writes to `sync` vfat SD cards
 *  - Cooperative cancellation (`cancel_cb`) checked every 80 ms
 ****************************************************************************/
#ifndef UIPOD_HTTP_TARGET_H
#define UIPOD_HTTP_TARGET_H

#include <stddef.h>
#include <stdint.h>

int32_t retrohh_nav_http_get(
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

void retrohh_nav_http_free(uint8_t *ptr);

int32_t retrohh_nav_http_download_file(
    uint8_t is_https,
    const char *host,
    uint16_t port,
    const char *path_and_query,
    uint8_t allow_self_signed,
    const char *dest_path,
    int32_t timeout_ms,
    uint8_t (*cancel_cb)(void *ctx),
    void *cancel_ctx,
    char *err_buf,
    size_t err_cap
);

/* Extended HTTP/1.1 request supporting GET/POST/DELETE, custom headers, JSON body,
 * and HTTP status code reporting (used by Jellyfin auth, sync, and playback reporting). */
int32_t retrohh_http_request(
    const char *method,
    uint8_t is_https,
    const char *host,
    uint16_t port,
    const char *path_and_query,
    const char *extra_headers,
    const uint8_t *body,
    size_t body_len,
    uint8_t allow_self_signed,
    int32_t timeout_ms,
    int32_t *out_status,
    uint8_t **out_data,
    size_t *out_len,
    char *err_buf,
    size_t err_cap
);

int32_t retrohh_http_download_ex(
    uint8_t is_https,
    const char *host,
    uint16_t port,
    const char *path_and_query,
    const char *extra_headers,
    uint8_t allow_self_signed,
    const char *dest_path,
    int32_t timeout_ms,
    uint8_t (*cancel_cb)(void *ctx),
    void *cancel_ctx,
    char *err_buf,
    size_t err_cap
);

/* Local loopback HTTP/1.1 proxy (`http://127.0.0.1:<port>/...`) for `tplayerslave`:
 * terminates TLS/SNI/Tailscale DNS, injects auth headers, forwards `Range:` headers
 * for 206 Partial Content seeking, and unchunks chunked transcode streams into raw bytes. */
int32_t retrohh_http_proxy_start(
    uint8_t is_https,
    const char *host,
    uint16_t port,
    const char *path_and_query,
    const char *extra_headers,
    uint8_t allow_self_signed,
    uint16_t *out_local_port,
    char *err_buf,
    size_t err_cap
);

void retrohh_http_proxy_set_path(const char *path_and_query);
void retrohh_http_proxy_stop(void);

#endif /* UIPOD_HTTP_TARGET_H */
