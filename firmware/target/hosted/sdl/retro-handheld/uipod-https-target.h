/* UIPod: fast non-blocking OpenSSL HTTPS GET helper for TrimUI Brick (tg5040). */
#ifndef _UIPOD_HTTPS_TARGET_H_
#define _UIPOD_HTTPS_TARGET_H_

#include <stdbool.h>
#include <stddef.h>

/* Fetch https://<host><path> into `out_buf` (null-terminated on success;
 * `out_cap` includes the trailing '\0').  Follows up to 4 HTTPS redirects and
 * decodes chunked transfer-encoding.  `ca_file` is an optional PEM bundle path.
 * If `allow_curl_fallback` is true, tries system `curl` once if OpenSSL fails.
 * Returns true on HTTP 200 with a non-empty body that fits in `out_buf`. */
bool retrohh_https_get_ex(const char *host, const char *path,
                          const char *ca_file, bool allow_curl_fallback,
                          char *out_buf, size_t out_cap, size_t *out_len);

bool retrohh_https_get(const char *host, const char *path,
                       const char *ca_file,
                       char *out_buf, size_t out_cap, size_t *out_len);

/* Download https://<host><path> atomically to `dest_path` (up to `max_bytes`).
 * Returns true on HTTP 200 with a non-empty body written to `dest_path`. */
bool retrohh_https_download_file_ex(const char *host, const char *path,
                                    const char *ca_file, bool allow_curl_fallback,
                                    const char *dest_path, size_t max_bytes);

bool retrohh_https_download_file(const char *host, const char *path,
                                 const char *ca_file,
                                 const char *dest_path, size_t max_bytes);

/* Concise stage-specific reason for the most recent failure (e.g.
 * "No Wi-Fi IP", "DNS timeout", "TCP timeout", "TLS error", "HTTP 404"). */
const char *retrohh_https_last_error(void);

/* True when the most recent failure was due to no active IPv4 default route,
 * allowing callers to skip mirror retries immediately while offline. */
bool retrohh_https_network_down(void);

#endif /* _UIPOD_HTTPS_TARGET_H_ */
