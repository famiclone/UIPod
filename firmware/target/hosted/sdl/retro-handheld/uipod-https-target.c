/***************************************************************************
 * UIPod: Non-blocking OpenSSL HTTPS 1.1 helper for TrimUI Brick (tg5040).
 *
 * Designed specifically for Allwinner Tina Linux / OpenWrt (NextUI on tg5040):
 *  - Pre-flight /proc/net/route check + one-shot `udhcpc` recovery if wlan0 is
 *    associated without a DHCP lease (NextUI's `wifi_init.sh` `udhcpc -b` race).
 *  - Self-contained RFC 1035 UDP DNS A-record client (zero dependency on glibc
 *    `libnss_dns.so.2` or `/etc/nsswitch.conf`), reading `/tmp/resolv.conf.auto`,
 *    `/etc/resolv.conf`, and public DNS (`1.1.1.1` / `8.8.8.8`), rejecting ISP
 *    sinkhole/private IPs, with automatic RFC 8484 DNS-over-HTTPS (`1.1.1.1:443`)
 *    fallback when port 53 is blocked or hijacked.
 *  - Strict `O_NONBLOCK` TCP + OpenSSL (`SSL_connect`/`SSL_write`/`SSL_read`)
 *    driven by `select()` with hard monotonic-millisecond deadlines so dropped
 *    packets or stalled connections never hang `Downloading...`.
 *  - Early completion on `Content-Length` / final chunked trailer without
 *    waiting for lingering CDN TCP `FIN`.
 *  - Mozilla CA bundle + SAN hostname verification tolerating handheld RTC skew.
 ****************************************************************************/
#include <sys/types.h>
#include <sys/socket.h>
#include <sys/select.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <fcntl.h>
#include <signal.h>
#include <errno.h>
#include <unistd.h>
#include <stdint.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <time.h>

#include <openssl/ssl.h>
#include <openssl/err.h>
#include <openssl/x509v3.h>

#include "uipod-https-target.h"

#define DNS_UDP_TIMEOUT_MS       1800
#define DNS_DOH_TIMEOUT_MS       2500
#define DNS_TCP_TIMEOUT_MS       1200
#define TCP_CONNECT_TIMEOUT_MS   2800
#define TLS_HANDSHAKE_TIMEOUT_MS 3500
#define HTTP_SMALL_TIMEOUT_MS    5000
#define HTTP_LARGE_TIMEOUT_MS    12000
#define HTTPS_MAX_REDIRECTS      4
#define MAX_RESOLVED_IPS         4
#define MAX_DNS_SERVERS          8

static char s_last_err[64] = "Unknown error";
static bool s_net_down     = false;

const char *retrohh_https_last_error(void)
{
    return s_last_err;
}

bool retrohh_https_network_down(void)
{
    return s_net_down;
}

static void set_err(const char *fmt, ...)
    __attribute__((format(printf, 1, 2)));

static void set_err(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(s_last_err, sizeof(s_last_err), fmt, ap);
    va_end(ap);
}

static int64_t mono_ms(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) == 0)
        return (int64_t)ts.tv_sec * 1000 + (int64_t)(ts.tv_nsec / 1000000);
    return (int64_t)time(NULL) * 1000;
}

/* Wait on non-blocking `fd` until `deadline_ms`.
 * Returns 1 when ready, 0 on timeout, -1 on unrecoverable error. */
static int wait_fd_until(int fd, bool for_write, int64_t deadline_ms)
{
    if (fd < 0 || fd >= FD_SETSIZE)
        return -1;

    for (;;)
    {
        int64_t rem = deadline_ms - mono_ms();
        if (rem <= 0)
            return 0;

        fd_set fds;
        FD_ZERO(&fds);
        FD_SET(fd, &fds);

        struct timeval tv;
        tv.tv_sec  = (time_t)(rem / 1000);
        tv.tv_usec = (suseconds_t)((rem % 1000) * 1000);

        int rc = select(fd + 1,
                        for_write ? NULL : &fds,
                        for_write ? &fds : NULL,
                        NULL, &tv);
        if (rc > 0)
            return 1;
        if (rc == 0)
        {
            if (mono_ms() >= deadline_ms)
                return 0;
            continue;
        }
        if (errno == EINTR)
            continue;
        return -1;
    }
}

static int socket_set_nonblock_cloexec(int fd)
{
    int fl = fcntl(fd, F_GETFL, 0);
    if (fl < 0 || fcntl(fd, F_SETFL, fl | O_NONBLOCK) < 0)
        return -1;
    int fdfl = fcntl(fd, F_GETFD, 0);
    if (fdfl >= 0)
        fcntl(fd, F_SETFD, fdfl | FD_CLOEXEC);
    return 0;
}

/* ---- Stage 1: IPv4 default route check + NextUI udhcpc recovery --------- */

/* Check /proc/net/route for an active IPv4 default route (Dest == 00000000,
 * RTF_UP == 0x1).  If /proc/net/route does not exist (non-Linux test env),
 * assume online. */
static bool proc_has_ipv4_default_route(void)
{
    FILE *f = fopen("/proc/net/route", "re");
    if (!f)
        return (errno == ENOENT);

    char line[256];
    bool found = false;
    if (fgets(line, sizeof(line), f)) /* skip header */
    {
        while (fgets(line, sizeof(line), f))
        {
            char iface[32];
            unsigned long dest = 1, gw = 0;
            unsigned int flags = 0;
            if (sscanf(line, "%31s %lx %lx %x", iface, &dest, &gw, &flags) >= 4)
            {
                if (strcmp(iface, "lo") != 0 && dest == 0 && (flags & 0x1))
                {
                    found = true;
                    break;
                }
            }
        }
    }
    fclose(f);
    return found;
}

static bool wlan0_carrier_up(void)
{
    FILE *f = fopen("/sys/class/net/wlan0/operstate", "re");
    if (!f)
        return false;
    char st[32] = {0};
    bool up = false;
    if (fgets(st, sizeof(st), f))
        up = (strncmp(st, "up", 2) == 0 || strncmp(st, "dormant", 7) == 0);
    fclose(f);
    return up;
}

static void try_udhcpc_once(void)
{
    static int64_t s_last_try_ms = -60000;
    int64_t now = mono_ms();
    if (now - s_last_try_ms < 20000)
        return;
    s_last_try_ms = now;

    static const char *const bins[] = { "/sbin/udhcpc", "/bin/udhcpc" };
    const char *bin = NULL;
    for (size_t i = 0; i < sizeof(bins) / sizeof(bins[0]); i++)
    {
        if (access(bins[i], X_OK) == 0)
        {
            bin = bins[i];
            break;
        }
    }
    if (!bin)
        return;

    fprintf(stderr, "[https] wlan0 up without default route; running %s\n", bin);
    pid_t pid = fork();
    if (pid == 0)
    {
        int dn = open("/dev/null", O_RDWR);
        if (dn >= 0)
        {
            dup2(dn, STDIN_FILENO);
            dup2(dn, STDOUT_FILENO);
            dup2(dn, STDERR_FILENO);
            if (dn > 2)
                close(dn);
        }
        char *const argv[] = {
            (char *)bin, "-i", "wlan0", "-n", "-q", "-t", "2", "-T", "1", NULL
        };
        execv(bin, argv);
        _exit(127);
    }
    if (pid > 0)
    {
        int64_t deadline = mono_ms() + 2500;
        int status = 0;
        for (;;)
        {
            pid_t w = waitpid(pid, &status, WNOHANG);
            if (w == pid || (w < 0 && errno != EINTR))
                break;
            if (mono_ms() >= deadline)
            {
                kill(pid, SIGKILL);
                waitpid(pid, &status, 0);
                break;
            }
            struct timeval tv = { .tv_sec = 0, .tv_usec = 40000 };
            select(0, NULL, NULL, NULL, &tv);
        }
    }
}

static bool ensure_ipv4_route(void)
{
    if (proc_has_ipv4_default_route())
    {
        s_net_down = false;
        return true;
    }

    if (wlan0_carrier_up())
    {
        try_udhcpc_once();
        if (proc_has_ipv4_default_route())
        {
            s_net_down = false;
            return true;
        }
    }

    s_net_down = true;
    set_err("No Wi-Fi IP");
    fprintf(stderr, "[https] abort: no IPv4 default route in /proc/net/route\n");
    return false;
}

/* ---- Stage 2: Pure-socket RFC 1035 + RFC 8484 DoH IPv4 DNS resolver ----- */

static bool is_bogus_or_private_ipv4(const uint8_t b[4])
{
    if (b[0] == 0 || b[0] == 10 || b[0] == 127 || b[0] >= 224)
        return true;
    if (b[0] == 169 && b[1] == 254)
        return true;
    if (b[0] == 172 && (b[1] >= 16 && b[1] <= 31))
        return true;
    if (b[0] == 192 && b[1] == 168)
        return true;
    if (b[0] == 100 && (b[1] >= 64 && b[1] <= 127)) /* CGNAT ISP block page */
        return true;
    if (b[0] == 198 && (b[1] == 18 || b[1] == 19))
        return true;
    return false;
}

static void add_unique_ip(struct in_addr *list, int *count, int max_count,
                          struct in_addr ip)
{
    if (*count >= max_count)
        return;
    for (int i = 0; i < *count; i++)
    {
        if (list[i].s_addr == ip.s_addr)
            return;
    }
    list[(*count)++] = ip;
}

/* Collect IPv4 DNS servers from OpenWrt/Tina Linux + standard resolv.conf
 * paths, placing non-loopback local servers first, followed by Cloudflare/Google
 * public resolvers, and loopback (127.0.0.1) only once if configured. */
static int collect_dns_servers(struct in_addr out[MAX_DNS_SERVERS])
{
    static const char *const files[] = {
        "/tmp/resolv.conf.auto",
        "/tmp/resolv.conf.d/resolv.conf.auto",
        "/etc/resolv.conf",
        "/tmp/resolv.conf",
        "/var/run/resolv.conf",
    };

    int n = 0;
    struct in_addr loopback_ip = { .s_addr = 0 };
    bool have_loopback = false;

    const char *env_dns = getenv("UIPOD_DNS_SERVERS");
    if (env_dns && *env_dns)
    {
        char copy[256];
        snprintf(copy, sizeof(copy), "%s", env_dns);
        char *save = NULL;
        for (char *tok = strtok_r(copy, ", ", &save); tok;
             tok = strtok_r(NULL, ", ", &save))
        {
            struct in_addr ip;
            if (inet_pton(AF_INET, tok, &ip) == 1)
                add_unique_ip(out, &n, MAX_DNS_SERVERS, ip);
        }
        if (n > 0)
            return n;
    }

    for (size_t fi = 0; fi < sizeof(files) / sizeof(files[0]); fi++)
    {
        FILE *f = fopen(files[fi], "re");
        if (!f)
            continue;
        char line[256];
        while (fgets(line, sizeof(line), f))
        {
            char *p = line;
            while (*p == ' ' || *p == '\t')
                p++;
            if (strncmp(p, "nameserver", 10) != 0 ||
                (p[10] != ' ' && p[10] != '\t'))
                continue;
            p += 10;
            while (*p == ' ' || *p == '\t')
                p++;
            char tok[64];
            size_t ti = 0;
            while (*p && *p != ' ' && *p != '\t' && *p != '\r' &&
                   *p != '\n' && *p != '#' && *p != ';' && ti + 1 < sizeof(tok))
                tok[ti++] = *p++;
            tok[ti] = '\0';

            struct in_addr ip;
            if (inet_pton(AF_INET, tok, &ip) == 1)
            {
                uint8_t b0 = ((const uint8_t *)&ip.s_addr)[0];
                if (b0 == 0)
                    continue;
                if (b0 == 127)
                {
                    if (!have_loopback)
                    {
                        loopback_ip = ip;
                        have_loopback = true;
                    }
                }
                else
                {
                    add_unique_ip(out, &n, MAX_DNS_SERVERS, ip);
                }
            }
        }
        fclose(f);
    }

    /* If local dnsmasq on 127.0.0.1 is configured and no upstream was found in
     * resolv.conf.auto, include 127.0.0.1 alongside 1.1.1.1 so both race. */
    if (have_loopback && n == 0)
        add_unique_ip(out, &n, MAX_DNS_SERVERS, loopback_ip);

    static const char *const pub_dns[] = { "1.1.1.1", "8.8.8.8", "9.9.9.9" };
    for (size_t i = 0; i < sizeof(pub_dns) / sizeof(pub_dns[0]); i++)
    {
        struct in_addr ip;
        if (inet_pton(AF_INET, pub_dns[i], &ip) == 1)
            add_unique_ip(out, &n, MAX_DNS_SERVERS, ip);
    }

    if (have_loopback)
        add_unique_ip(out, &n, MAX_DNS_SERVERS, loopback_ip);

    return n;
}

static size_t build_dns_a_query(const char *host, uint16_t txid,
                                uint8_t *buf, size_t cap)
{
    size_t hlen = strlen(host);
    while (hlen > 0 && host[hlen - 1] == '.')
        hlen--;
    if (hlen == 0 || hlen > 253 || cap < 12 + hlen + 2 + 4)
        return 0;

    memset(buf, 0, 12);
    buf[0] = (uint8_t)(txid >> 8);
    buf[1] = (uint8_t)(txid & 0xFF);
    buf[2] = 0x01; /* RD = 1 */
    buf[3] = 0x00;
    buf[4] = 0x00;
    buf[5] = 0x01; /* QDCOUNT = 1 */

    size_t w = 12;
    size_t i = 0;
    while (i < hlen)
    {
        size_t dot = i;
        while (dot < hlen && host[dot] != '.')
            dot++;
        size_t lab_len = dot - i;
        if (lab_len == 0 || lab_len > 63 || w + 1 + lab_len + 5 > cap)
            return 0;
        buf[w++] = (uint8_t)lab_len;
        memcpy(buf + w, host + i, lab_len);
        w += lab_len;
        i = dot + 1;
    }
    buf[w++] = 0x00;
    buf[w++] = 0x00; buf[w++] = 0x01; /* QTYPE = A */
    buf[w++] = 0x00; buf[w++] = 0x01; /* QCLASS = IN */
    return w;
}

static size_t dns_skip_name(const uint8_t *pkt, size_t pkt_len, size_t off)
{
    for (int steps = 0; steps < 64 && off < pkt_len; steps++)
    {
        uint8_t c = pkt[off];
        if (c == 0)
            return off + 1;
        if ((c & 0xC0) == 0xC0)
            return (off + 1 < pkt_len) ? (off + 2) : 0;
        if ((c & 0xC0) != 0)
            return 0;
        off += 1 + (size_t)c;
    }
    return 0;
}

static int parse_dns_a_response(const uint8_t *pkt, size_t pkt_len,
                                uint16_t expected_txid, bool check_txid,
                                struct in_addr *out_ips, int max_ips)
{
    if (pkt_len < 12)
        return 0;

    uint16_t rx_id   = ((uint16_t)pkt[0] << 8) | pkt[1];
    uint16_t flags   = ((uint16_t)pkt[2] << 8) | pkt[3];
    uint16_t qdcount = ((uint16_t)pkt[4] << 8) | pkt[5];
    uint16_t ancount = ((uint16_t)pkt[6] << 8) | pkt[7];

    if (check_txid && rx_id != expected_txid)
        return 0;
    if ((flags & 0x8000) == 0) /* must be response (QR=1) */
        return 0;
    if ((flags & 0x000F) != 0) /* RCODE must be NOERROR */
        return 0;

    size_t off = 12;
    for (uint16_t q = 0; q < qdcount; q++)
    {
        off = dns_skip_name(pkt, pkt_len, off);
        if (off == 0 || off + 4 > pkt_len)
            return 0;
        off += 4;
    }

    int found = 0;
    for (uint16_t a = 0; a < ancount; a++)
    {
        off = dns_skip_name(pkt, pkt_len, off);
        if (off == 0 || off + 10 > pkt_len)
            break;

        uint16_t rtype  = ((uint16_t)pkt[off] << 8)     | pkt[off + 1];
        uint16_t rclass = ((uint16_t)pkt[off + 2] << 8) | pkt[off + 3];
        uint16_t rdlen  = ((uint16_t)pkt[off + 8] << 8) | pkt[off + 9];
        off += 10;
        if (off + rdlen > pkt_len)
            break;

        if (rtype == 1 && rclass == 1 && rdlen == 4)
        {
            if (!is_bogus_or_private_ipv4(pkt + off))
            {
                struct in_addr ip;
                memcpy(&ip.s_addr, pkt + off, 4);
                add_unique_ip(out_ips, &found, max_ips, ip);
            }
        }
        off += rdlen;
    }
    return found;
}

static int dns_query_udp_parallel(const uint8_t *qbuf, size_t qlen,
                                  uint16_t txid,
                                  const struct in_addr *servers, int nservers,
                                  struct in_addr *out_ips, int max_ips)
{
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0)
        return 0;
    if (socket_set_nonblock_cloexec(fd) < 0)
    {
        close(fd);
        return 0;
    }

    int64_t t0       = mono_ms();
    int64_t deadline = t0 + DNS_UDP_TIMEOUT_MS;
    int sent_idx     = 0;
    bool fanned_out  = false;
    bool retried     = false;

    /* Immediately send to first 2 servers (local DHCP DNS + first public). */
    int initial_burst = (nservers < 2) ? nservers : 2;
    for (; sent_idx < initial_burst; sent_idx++)
    {
        struct sockaddr_in sa;
        memset(&sa, 0, sizeof(sa));
        sa.sin_family = AF_INET;
        sa.sin_port   = htons(53);
        sa.sin_addr   = servers[sent_idx];
        sendto(fd, qbuf, qlen, 0, (struct sockaddr *)&sa, sizeof(sa));
    }

    int found = 0;
    while (mono_ms() < deadline && found == 0)
    {
        int64_t elapsed = mono_ms() - t0;
        if (!fanned_out && elapsed >= 180)
        {
            fanned_out = true;
            for (; sent_idx < nservers; sent_idx++)
            {
                struct sockaddr_in sa;
                memset(&sa, 0, sizeof(sa));
                sa.sin_family = AF_INET;
                sa.sin_port   = htons(53);
                sa.sin_addr   = servers[sent_idx];
                sendto(fd, qbuf, qlen, 0, (struct sockaddr *)&sa, sizeof(sa));
            }
        }
        if (!retried && elapsed >= 800)
        {
            retried = true;
            for (int i = 0; i < nservers; i++)
            {
                struct sockaddr_in sa;
                memset(&sa, 0, sizeof(sa));
                sa.sin_family = AF_INET;
                sa.sin_port   = htons(53);
                sa.sin_addr   = servers[i];
                sendto(fd, qbuf, qlen, 0, (struct sockaddr *)&sa, sizeof(sa));
            }
        }

        int64_t slice_end = mono_ms() + 180;
        if (slice_end > deadline)
            slice_end = deadline;

        if (wait_fd_until(fd, false, slice_end) > 0)
        {
            uint8_t rbuf[1500];
            for (;;)
            {
                ssize_t r = recvfrom(fd, rbuf, sizeof(rbuf), 0, NULL, NULL);
                if (r < 0)
                {
                    if (errno == EINTR)
                        continue;
                    break;
                }
                int n = parse_dns_a_response(rbuf, (size_t)r, txid, true,
                                             out_ips, max_ips);
                if (n > 0)
                {
                    found = n;
                    break;
                }
            }
        }
    }

    close(fd);
    return found;
}

/* ---- Non-blocking TCP + OpenSSL primitives ------------------------------ */

static int connect_ipv4_nonblock(struct in_addr ip, uint16_t port, int timeout_ms)
{
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0)
        return -1;

    if (socket_set_nonblock_cloexec(fd) < 0)
    {
        close(fd);
        return -1;
    }

    int one = 1;
    setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));

    struct sockaddr_in sa;
    memset(&sa, 0, sizeof(sa));
    sa.sin_family = AF_INET;
    sa.sin_port   = htons(port);
    sa.sin_addr   = ip;

    int rc;
    do {
        rc = connect(fd, (struct sockaddr *)&sa, sizeof(sa));
    } while (rc < 0 && errno == EINTR);

    if (rc == 0)
        return fd;

    if (errno == EINPROGRESS)
    {
        if (wait_fd_until(fd, true, mono_ms() + timeout_ms) > 0)
        {
            int so_err = 0;
            socklen_t slen = sizeof(so_err);
            if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &so_err, &slen) == 0 &&
                so_err == 0)
            {
                return fd; /* remains O_NONBLOCK */
            }
        }
    }

    close(fd);
    return -1;
}

static int dns_query_tcp_one(const uint8_t *qbuf, size_t qlen, uint16_t txid,
                             struct in_addr server,
                             struct in_addr *out_ips, int max_ips)
{
    if (qlen > 512)
        return 0;
    int64_t deadline = mono_ms() + DNS_TCP_TIMEOUT_MS;
    int fd = connect_ipv4_nonblock(server, 53, DNS_TCP_TIMEOUT_MS);
    if (fd < 0)
        return 0;

    uint8_t frame[514];
    frame[0] = (uint8_t)(qlen >> 8);
    frame[1] = (uint8_t)(qlen & 0xFF);
    memcpy(frame + 2, qbuf, qlen);

    size_t total = qlen + 2, sent = 0;
    while (sent < total && mono_ms() < deadline)
    {
        ssize_t w = send(fd, frame + sent, total - sent, 0);
        if (w > 0) { sent += (size_t)w; continue; }
        if (w < 0 && errno == EINTR) continue;
        if (w < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
        {
            if (wait_fd_until(fd, true, deadline) > 0)
                continue;
        }
        close(fd);
        return 0;
    }

    uint8_t rbuf[2048];
    size_t got = 0, need = 2;
    while (got < need && mono_ms() < deadline)
    {
        ssize_t r = recv(fd, rbuf + got, sizeof(rbuf) - got, 0);
        if (r > 0)
        {
            got += (size_t)r;
            if (got >= 2 && need == 2)
            {
                uint16_t mlen = ((uint16_t)rbuf[0] << 8) | rbuf[1];
                need = 2 + (size_t)mlen;
                if (need > sizeof(rbuf))
                    break;
            }
            continue;
        }
        if (r == 0) break;
        if (errno == EINTR) continue;
        if (errno == EAGAIN || errno == EWOULDBLOCK)
        {
            if (wait_fd_until(fd, false, deadline) > 0)
                continue;
        }
        break;
    }
    close(fd);
    if (got < need || need <= 2)
        return 0;
    return parse_dns_a_response(rbuf + 2, need - 2, txid, true, out_ips, max_ips);
}

/* Verify callback: enforce cryptographic CA chain + SAN hostname match while
 * tolerating unsynced handheld clocks (1970/2024 vs 2026 leaf certificates). */
static int ssl_verify_cb(int preverify_ok, X509_STORE_CTX *x509_ctx)
{
    if (preverify_ok)
        return 1;
    int err = X509_STORE_CTX_get_error(x509_ctx);
    if (err == X509_V_ERR_CERT_NOT_YET_VALID ||
        err == X509_V_ERR_CERT_HAS_EXPIRED)
        return 1;
    return 0;
}

static void ensure_openssl_init(void)
{
    static bool inited = false;
    if (!inited)
    {
        signal(SIGPIPE, SIG_IGN);
        OPENSSL_init_ssl(0, NULL);
        inited = true;
    }
}

static bool ssl_wait_io(SSL *ssl, int sock, int ret, int64_t deadline_ms)
{
    if (mono_ms() >= deadline_ms)
        return false;
    int err = SSL_get_error(ssl, ret);
    if (err == SSL_ERROR_WANT_READ)
        return wait_fd_until(sock, false, deadline_ms) > 0;
    if (err == SSL_ERROR_WANT_WRITE)
        return wait_fd_until(sock, true, deadline_ms) > 0;
    if (err == SSL_ERROR_SYSCALL && errno == EINTR)
        return mono_ms() < deadline_ms;
    return false;
}

static SSL_CTX *create_verified_ssl_ctx(const char *ca_file,
                                        const char *verify_host)
{
    SSL_CTX *ctx = SSL_CTX_new(TLS_client_method());
    if (!ctx)
        return NULL;

    bool have_ca = false;
    if (ca_file && ca_file[0] &&
        SSL_CTX_load_verify_locations(ctx, ca_file, NULL) == 1)
        have_ca = true;
    if (!have_ca &&
        SSL_CTX_load_verify_locations(ctx, "/etc/ssl/certs/ca-certificates.crt", NULL) == 1)
        have_ca = true;
    if (!have_ca &&
        SSL_CTX_load_verify_locations(ctx, "/etc/ssl/cert.pem", NULL) == 1)
        have_ca = true;
    if (!have_ca && SSL_CTX_set_default_verify_paths(ctx) == 1)
        have_ca = true;

    if (have_ca && verify_host && verify_host[0])
    {
        SSL_CTX_set_verify(ctx, SSL_VERIFY_PEER, ssl_verify_cb);
        X509_VERIFY_PARAM *param = SSL_CTX_get0_param(ctx);
        if (param)
        {
            X509_VERIFY_PARAM_set_flags(param, X509_V_FLAG_TRUSTED_FIRST);
            X509_VERIFY_PARAM_set1_host(param, verify_host, 0);
        }
    }
    return ctx;
}

static const char *find_header_ci(const char *headers, size_t hdr_len,
                                  const char *name)
{
    size_t nlen = strlen(name);
    const char *p = headers;
    const char *end = headers + hdr_len;
    while (p < end)
    {
        const char *eol = memchr(p, '\n', (size_t)(end - p));
        size_t l = eol ? (size_t)(eol - p) : (size_t)(end - p);
        if (l > nlen && strncasecmp(p, name, nlen) == 0 && p[nlen] == ':')
        {
            const char *v = p + nlen + 1;
            while (v < p + l && (*v == ' ' || *v == '\t'))
                v++;
            return v;
        }
        if (!eol)
            break;
        p = eol + 1;
    }
    return NULL;
}

/* RFC 8484 DNS-over-HTTPS fallback to literal IP 1.1.1.1 / 8.8.8.8 when port 53
 * is blocked or hijacked by the ISP/router.  Sends the exact RFC 1035 binary
 * query via POST /dns-query (application/dns-message). */
static int dns_query_doh(const uint8_t *qbuf, size_t qlen, uint16_t txid,
                         const char *ca_file,
                         struct in_addr *out_ips, int max_ips)
{
    static const struct {
        const char *ip_str;
        const char *sni_host;
    } doh_endpoints[] = {
        { "1.1.1.1", "cloudflare-dns.com" },
        { "8.8.8.8", "dns.google" },
    };

    ensure_openssl_init();

    for (size_t i = 0; i < sizeof(doh_endpoints) / sizeof(doh_endpoints[0]); i++)
    {
        struct in_addr srv_ip;
        if (inet_pton(AF_INET, doh_endpoints[i].ip_str, &srv_ip) != 1)
            continue;

        int64_t deadline = mono_ms() + DNS_DOH_TIMEOUT_MS;
        int sock = connect_ipv4_nonblock(srv_ip, 443, 1500);
        if (sock < 0)
            continue;

        SSL_CTX *ctx = create_verified_ssl_ctx(ca_file, doh_endpoints[i].sni_host);
        if (!ctx)
        {
            close(sock);
            continue;
        }
        SSL *ssl = SSL_new(ctx);
        if (!ssl)
        {
            SSL_CTX_free(ctx);
            close(sock);
            continue;
        }
        SSL_set_tlsext_host_name(ssl, doh_endpoints[i].sni_host);
        SSL_set_fd(ssl, sock);

        bool hs_ok = false;
        ERR_clear_error();
        for (;;)
        {
            errno = 0;
            int rc = SSL_connect(ssl);
            if (rc == 1) { hs_ok = true; break; }
            if (!ssl_wait_io(ssl, sock, rc, deadline))
                break;
        }

        uint8_t req[384];
        int hdr_n = snprintf((char *)req, sizeof(req),
            "POST /dns-query HTTP/1.1\r\n"
            "Host: %s\r\n"
            "Content-Type: application/dns-message\r\n"
            "Accept: application/dns-message\r\n"
            "Content-Length: %zu\r\n"
            "Connection: close\r\n\r\n",
            doh_endpoints[i].sni_host, qlen);

        bool io_ok = hs_ok && hdr_n > 0 && (size_t)hdr_n + qlen <= sizeof(req);
        if (io_ok)
            memcpy(req + hdr_n, qbuf, qlen);

        size_t req_tot = io_ok ? ((size_t)hdr_n + qlen) : 0;
        size_t sent = 0;
        while (io_ok && sent < req_tot)
        {
            errno = 0;
            int w = SSL_write(ssl, req + sent, (int)(req_tot - sent));
            if (w > 0) { sent += (size_t)w; continue; }
            if (!ssl_wait_io(ssl, sock, w, deadline))
                io_ok = false;
        }

        uint8_t rbuf[2048];
        size_t rlen = 0;
        size_t hdr_end_off = 0;
        long cl = -1;

        while (io_ok && rlen + 1 < sizeof(rbuf))
        {
            errno = 0;
            int r = SSL_read(ssl, rbuf + rlen, (int)(sizeof(rbuf) - 1 - rlen));
            if (r > 0)
            {
                rlen += (size_t)r;
                rbuf[rlen] = 0;
                if (hdr_end_off == 0)
                {
                    char *he = strstr((char *)rbuf, "\r\n\r\n");
                    if (he)
                    {
                        size_t hlen = (size_t)(he - (char *)rbuf);
                        hdr_end_off = hlen + 4;
                        const char *cl_hdr = find_header_ci((char *)rbuf, hlen,
                                                            "Content-Length");
                        if (cl_hdr)
                            cl = strtol(cl_hdr, NULL, 10);
                    }
                }
                if (hdr_end_off > 0 && cl >= 0 &&
                    rlen - hdr_end_off >= (size_t)cl)
                    break;
                continue;
            }
            if (SSL_get_error(ssl, r) == SSL_ERROR_ZERO_RETURN)
                break;
            if (!ssl_wait_io(ssl, sock, r, deadline))
                break;
        }

        SSL_set_shutdown(ssl, SSL_SENT_SHUTDOWN | SSL_RECEIVED_SHUTDOWN);
        SSL_free(ssl);
        SSL_CTX_free(ctx);
        close(sock);

        if (hdr_end_off > 0 && rlen > hdr_end_off &&
            strncmp((char *)rbuf, "HTTP/1.1 200", 12) == 0)
        {
            size_t blen = rlen - hdr_end_off;
            if (cl >= 0 && (size_t)cl < blen)
                blen = (size_t)cl;
            int found = parse_dns_a_response(rbuf + hdr_end_off, blen,
                                             txid, false, out_ips, max_ips);
            if (found > 0)
            {
                fprintf(stderr, "[https] DoH (%s) resolved %d IP(s)\n",
                        doh_endpoints[i].ip_str, found);
                return found;
            }
        }
    }
    return 0;
}

/* Simple 120s in-memory DNS cache to avoid redundant lookups on redirects or
 * consecutive preset downloads. */
static struct {
    char           host[128];
    struct in_addr ips[MAX_RESOLVED_IPS];
    int            count;
    int64_t        ts_ms;
} s_dns_cache;

static void invalidate_dns_cache(const char *host)
{
    if (strcasecmp(s_dns_cache.host, host) == 0)
        s_dns_cache.count = 0;
}

static int resolve_ipv4_fast(const char *host, const char *ca_file,
                             struct in_addr *out_ips, int max_ips)
{
    struct in_addr lit;
    if (inet_pton(AF_INET, host, &lit) == 1)
    {
        out_ips[0] = lit;
        return 1;
    }

    int64_t now = mono_ms();
    if (s_dns_cache.count > 0 &&
        now - s_dns_cache.ts_ms < 120000 &&
        strcasecmp(s_dns_cache.host, host) == 0)
    {
        int n = (s_dns_cache.count < max_ips) ? s_dns_cache.count : max_ips;
        memcpy(out_ips, s_dns_cache.ips, (size_t)n * sizeof(struct in_addr));
        return n;
    }

    uint8_t qbuf[300];
    uint16_t txid = (uint16_t)(((unsigned)getpid() ^ (unsigned)now) & 0xFFFF);
    if (txid == 0)
        txid = 0x5A3C;
    size_t qlen = build_dns_a_query(host, txid, qbuf, sizeof(qbuf));
    if (qlen == 0)
        return 0;

    struct in_addr servers[MAX_DNS_SERVERS];
    int nservers = collect_dns_servers(servers);

    int found = dns_query_udp_parallel(qbuf, qlen, txid, servers, nservers,
                                       out_ips, max_ips);
    if (found == 0)
        found = dns_query_doh(qbuf, qlen, txid, ca_file, out_ips, max_ips);

    if (found == 0 && nservers > 0)
        found = dns_query_tcp_one(qbuf, qlen, txid, servers[0],
                                  out_ips, max_ips);

    if (found > 0 && strlen(host) < sizeof(s_dns_cache.host))
    {
        snprintf(s_dns_cache.host, sizeof(s_dns_cache.host), "%s", host);
        int c = (found < MAX_RESOLVED_IPS) ? found : MAX_RESOLVED_IPS;
        memcpy(s_dns_cache.ips, out_ips, (size_t)c * sizeof(struct in_addr));
        s_dns_cache.count = c;
        s_dns_cache.ts_ms = mono_ms();
    }
    return found;
}

/* ---- HTTP response helpers ---------------------------------------------- */

static bool decode_chunked(const char *in, size_t in_len,
                           char *out, size_t *out_len)
{
    size_t pos = 0, w = 0;
    while (pos < in_len)
    {
        const char *line = in + pos;
        const char *crlf = memchr(line, '\n', in_len - pos);
        if (!crlf)
            break;

        char *endp = NULL;
        unsigned long chunk_sz = strtoul(line, &endp, 16);
        if (endp == line)
            return false;

        pos = (size_t)((crlf + 1) - in);
        if (chunk_sz == 0)
        {
            *out_len = w;
            return true;
        }
        if (chunk_sz > in_len - pos)
            return false;

        memmove(out + w, in + pos, chunk_sz);
        w += chunk_sz;
        pos += chunk_sz;

        if (pos < in_len && in[pos] == '\r')
            pos++;
        if (pos < in_len && in[pos] == '\n')
            pos++;
    }
    *out_len = w;
    return w > 0;
}

static bool chunked_stream_complete(const char *body, size_t blen)
{
    if (blen < 5)
        return false;
    return (memcmp(body + blen - 5, "0\r\n\r\n", 5) == 0);
}

static bool parse_https_url(const char *url,
                            char *host, size_t host_cap,
                            char *path, size_t path_cap)
{
    if (strncasecmp(url, "https://", 8) != 0)
        return false;
    const char *h = url + 8;
    const char *slash = strchr(h, '/');
    size_t hlen = slash ? (size_t)(slash - h) : strlen(h);
    if (hlen == 0 || hlen >= host_cap)
        return false;
    memcpy(host, h, hlen);
    host[hlen] = '\0';

    const char *p = slash ? slash : "/";
    size_t plen = strlen(p);
    while (plen > 0 && (p[plen - 1] == '\r' || p[plen - 1] == '\n' ||
                        p[plen - 1] == ' '  || p[plen - 1] == '\t'))
        plen--;
    if (plen == 0 || plen >= path_cap)
        return false;
    memcpy(path, p, plen);
    path[plen] = '\0';
    return true;
}

#define SCRATCH_HOST_CAP 256
#define SCRATCH_PATH_CAP 1536
#define SCRATCH_REQ_CAP  2048

struct https_scratch
{
    char cur_host[SCRATCH_HOST_CAP];
    char cur_path[SCRATCH_PATH_CAP];
    char loc_buf[SCRATCH_PATH_CAP];
    char req[SCRATCH_REQ_CAP];
};

/* Bounded single-attempt system `curl -4kfsSL` fallback, passing `--resolve`
 * when our socket DNS succeeded so `curl` never blocks on broken resolv.conf. */
static char *curl_fallback_alloc(const char *host, const char *path,
                                 const struct in_addr *opt_ip,
                                 size_t max_body, size_t *out_len)
{
    static const char *const curl_bins[] = {
        "/usr/bin/curl",
        "/mnt/SDCARD/.system/tg5040/bin/curl",
        "/bin/curl",
    };
    const char *bin = NULL;
    for (size_t i = 0; i < sizeof(curl_bins) / sizeof(curl_bins[0]); i++)
    {
        if (access(curl_bins[i], X_OK) == 0)
        {
            bin = curl_bins[i];
            break;
        }
    }
    if (!bin)
        return NULL;

    size_t ulen = 8 + strlen(host) + strlen(path) + 1;
    char *url = malloc(ulen);
    if (!url)
        return NULL;
    snprintf(url, ulen, "https://%s%s", host, path);

    char resolve_arg[320] = {0};
    if (opt_ip)
    {
        char ipbuf[INET_ADDRSTRLEN];
        if (inet_ntop(AF_INET, opt_ip, ipbuf, sizeof(ipbuf)))
            snprintf(resolve_arg, sizeof(resolve_arg), "%s:443:%s", host, ipbuf);
    }

    char tmp_out[128];
    snprintf(tmp_out, sizeof(tmp_out), "/tmp/uipod_https_%d.tmp", (int)getpid());
    unlink(tmp_out);

    bool is_gh_api = (strcasecmp(host, "api.github.com") == 0);
    pid_t pid = fork();
    if (pid == 0)
    {
        int dn = open("/dev/null", O_RDWR);
        if (dn >= 0)
        {
            dup2(dn, STDIN_FILENO);
            dup2(dn, STDOUT_FILENO);
            dup2(dn, STDERR_FILENO);
            if (dn > 2)
                close(dn);
        }
        const char *argv[18];
        int ac = 0;
        argv[ac++] = bin;
        argv[ac++] = "-4kfsSL";
        argv[ac++] = "--connect-timeout";
        argv[ac++] = "3";
        argv[ac++] = "--max-time";
        argv[ac++] = (max_body > 65536) ? "12" : "6";
        argv[ac++] = "-A";
        argv[ac++] = "UIPod/1.0";
        if (is_gh_api)
        {
            argv[ac++] = "-H";
            argv[ac++] = "Accept: application/vnd.github.raw";
        }
        if (resolve_arg[0])
        {
            argv[ac++] = "--resolve";
            argv[ac++] = resolve_arg;
        }
        argv[ac++] = "-o";
        argv[ac++] = tmp_out;
        argv[ac++] = url;
        argv[ac]   = NULL;
        execv(bin, (char *const *)argv);
        _exit(127);
    }

    free(url);
    bool ran_ok = false;
    if (pid > 0)
    {
        int64_t deadline = mono_ms() + ((max_body > 65536) ? 13000 : 7000);
        int status = 0;
        for (;;)
        {
            pid_t w = waitpid(pid, &status, WNOHANG);
            if (w == pid)
            {
                ran_ok = WIFEXITED(status) && (WEXITSTATUS(status) == 0);
                break;
            }
            if (w < 0 && errno != EINTR)
                break;
            if (mono_ms() >= deadline)
            {
                kill(pid, SIGKILL);
                waitpid(pid, &status, 0);
                break;
            }
            struct timeval tv = { .tv_sec = 0, .tv_usec = 40000 };
            select(0, NULL, NULL, NULL, &tv);
        }
    }

    if (!ran_ok)
    {
        unlink(tmp_out);
        return NULL;
    }

    int fd = open(tmp_out, O_RDONLY);
    if (fd < 0)
    {
        unlink(tmp_out);
        return NULL;
    }
    off_t sz = lseek(fd, 0, SEEK_END);
    lseek(fd, 0, SEEK_SET);
    if (sz <= 0 || (size_t)sz > max_body)
    {
        close(fd);
        unlink(tmp_out);
        return NULL;
    }
    char *buf = malloc((size_t)sz + 1);
    if (!buf)
    {
        close(fd);
        unlink(tmp_out);
        return NULL;
    }
    size_t got = 0;
    while (got < (size_t)sz)
    {
        ssize_t r = read(fd, buf + got, (size_t)sz - got);
        if (r < 0 && errno == EINTR)
            continue;
        if (r <= 0)
            break;
        got += (size_t)r;
    }
    close(fd);
    unlink(tmp_out);

    if (got == 0)
    {
        free(buf);
        return NULL;
    }
    buf[got] = '\0';
    if (out_len)
        *out_len = got;
    return buf;
}

/* Core non-blocking OpenSSL fetch returning a newly malloc'd null-terminated
 * body buffer of up to `max_body` bytes. */
static char *https_fetch_alloc(const char *init_host, const char *init_path,
                               const char *ca_file, bool allow_curl_fallback,
                               size_t max_body, size_t *out_len)
{
    int64_t t0 = mono_ms();
    ensure_openssl_init();

    if (!ensure_ipv4_route())
        return NULL;

    struct https_scratch *sc = malloc(sizeof(*sc));
    if (!sc)
    {
        set_err("Out of memory");
        return NULL;
    }

    snprintf(sc->cur_host, sizeof(sc->cur_host), "%s", init_host);
    snprintf(sc->cur_path, sizeof(sc->cur_path), "%s", init_path);

    struct in_addr first_resolved_ip = { .s_addr = 0 };
    bool have_first_ip = false;
    bool got_http_status_err = false;

    for (int hop = 0; hop <= HTTPS_MAX_REDIRECTS; hop++)
    {
        struct in_addr ips[MAX_RESOLVED_IPS];
        int n_ips = resolve_ipv4_fast(sc->cur_host, ca_file,
                                      ips, MAX_RESOLVED_IPS);
        if (n_ips <= 0)
        {
            set_err("DNS timeout");
            fprintf(stderr, "[https +%lldms] DNS failed for %s\n",
                    (long long)(mono_ms() - t0), sc->cur_host);
            break;
        }

        if (hop == 0)
        {
            first_resolved_ip = ips[0];
            have_first_ip = true;
        }

        int sock = -1;
        int try_ips = (n_ips < 2) ? n_ips : 2;
        for (int i = 0; i < try_ips; i++)
        {
            char ipstr[INET_ADDRSTRLEN] = "?";
            inet_ntop(AF_INET, &ips[i], ipstr, sizeof(ipstr));
            sock = connect_ipv4_nonblock(ips[i], 443, TCP_CONNECT_TIMEOUT_MS);
            if (sock >= 0)
            {
                fprintf(stderr, "[https +%lldms] TCP connected %s (%s:443)\n",
                        (long long)(mono_ms() - t0), sc->cur_host, ipstr);
                break;
            }
            fprintf(stderr, "[https +%lldms] TCP timeout %s (%s:443)\n",
                    (long long)(mono_ms() - t0), sc->cur_host, ipstr);
        }

        if (sock < 0)
        {
            invalidate_dns_cache(sc->cur_host);
            set_err("TCP timeout");
            break;
        }

        SSL_CTX *ctx = create_verified_ssl_ctx(ca_file, sc->cur_host);
        if (!ctx)
        {
            set_err("TLS init error");
            close(sock);
            break;
        }

        SSL *ssl = SSL_new(ctx);
        if (!ssl)
        {
            set_err("TLS init error");
            SSL_CTX_free(ctx);
            close(sock);
            break;
        }

        SSL_set_tlsext_host_name(ssl, sc->cur_host);
        SSL_set_fd(ssl, sock);

        int64_t hs_deadline = mono_ms() + TLS_HANDSHAKE_TIMEOUT_MS;
        bool hs_ok = false;
        ERR_clear_error();
        for (;;)
        {
            errno = 0;
            int rc = SSL_connect(ssl);
            if (rc == 1)
            {
                hs_ok = true;
                break;
            }
            if (!ssl_wait_io(ssl, sock, rc, hs_deadline))
                break;
        }

        if (!hs_ok)
        {
            long vr = SSL_get_verify_result(ssl);
            if (vr != X509_V_OK)
                set_err("TLS cert err (%ld)", vr);
            else if (mono_ms() >= hs_deadline)
                set_err("TLS timeout");
            else
                set_err("TLS handshake err");

            fprintf(stderr, "[https +%lldms] SSL_connect failed for %s: %s\n",
                    (long long)(mono_ms() - t0), sc->cur_host, s_last_err);
            ERR_print_errors_fp(stderr);
            SSL_free(ssl);
            SSL_CTX_free(ctx);
            close(sock);
            break;
        }

        const char *accept_hdr = (strcasecmp(sc->cur_host, "api.github.com") == 0)
                               ? "application/vnd.github.raw"
                               : "*/*";

        int req_len = snprintf(sc->req, sizeof(sc->req),
            "GET %s HTTP/1.1\r\n"
            "Host: %s\r\n"
            "User-Agent: UIPod/1.0\r\n"
            "Accept: %s\r\n"
            "Accept-Encoding: identity\r\n"
            "Connection: close\r\n\r\n",
            sc->cur_path, sc->cur_host, accept_hdr);

        int64_t io_deadline = mono_ms() +
            ((max_body > 65536) ? HTTP_LARGE_TIMEOUT_MS : HTTP_SMALL_TIMEOUT_MS);

        bool write_ok = (req_len > 0 && (size_t)req_len < sizeof(sc->req));
        int sent = 0;
        while (write_ok && sent < req_len)
        {
            errno = 0;
            int n = SSL_write(ssl, sc->req + sent, req_len - sent);
            if (n > 0)
            {
                sent += n;
                continue;
            }
            if (!ssl_wait_io(ssl, sock, n, io_deadline))
            {
                write_ok = false;
                break;
            }
        }

        size_t cap = 16384;
        size_t len = 0;
        size_t hard_cap = max_body + 16384;
        char *raw = write_ok ? malloc(cap) : NULL;

        bool headers_parsed = false;
        size_t hdr_len = 0, body_off = 0;
        long content_len = -1;
        bool is_chunked = false;

        while (raw)
        {
            if (len + 4096 + 1 > cap)
            {
                size_t ncap = cap * 2;
                if (ncap > hard_cap)
                    ncap = hard_cap;
                if (len + 2 > ncap)
                {
                    free(raw);
                    raw = NULL;
                    set_err("Response too large");
                    break;
                }
                char *nr = realloc(raw, ncap);
                if (!nr)
                {
                    free(raw);
                    raw = NULL;
                    set_err("Out of memory");
                    break;
                }
                raw = nr;
                cap = ncap;
            }

            errno = 0;
            int n = SSL_read(ssl, raw + len, (int)(cap - len - 1));
            if (n > 0)
            {
                len += (size_t)n;
                raw[len] = '\0';

                if (!headers_parsed)
                {
                    char *he = strstr(raw, "\r\n\r\n");
                    size_t sep = 4;
                    if (!he)
                    {
                        he = strstr(raw, "\n\n");
                        sep = 2;
                    }
                    if (he)
                    {
                        headers_parsed = true;
                        hdr_len  = (size_t)(he - raw);
                        body_off = hdr_len + sep;

                        const char *cl = find_header_ci(raw, hdr_len,
                                                        "Content-Length");
                        if (cl)
                            content_len = strtol(cl, NULL, 10);

                        const char *te = find_header_ci(raw, hdr_len,
                                                        "Transfer-Encoding");
                        if (te)
                        {
                            const char *eol = memchr(te, '\n',
                                                     hdr_len - (size_t)(te - raw));
                            size_t tlen = eol ? (size_t)(eol - te) : strlen(te);
                            if (tlen >= 7 && strncasecmp(te, "chunked", 7) == 0)
                                is_chunked = true;
                        }
                    }
                }

                if (headers_parsed)
                {
                    if (content_len >= 0 &&
                        len - body_off >= (size_t)content_len)
                    {
                        len = body_off + (size_t)content_len;
                        raw[len] = '\0';
                        break;
                    }
                    if (is_chunked &&
                        chunked_stream_complete(raw + body_off, len - body_off))
                        break;
                }
                continue;
            }

            if (SSL_get_error(ssl, n) == SSL_ERROR_ZERO_RETURN)
                break;
            if (!ssl_wait_io(ssl, sock, n, io_deadline))
                break;
        }

        SSL_set_shutdown(ssl, SSL_SENT_SHUTDOWN | SSL_RECEIVED_SHUTDOWN);
        SSL_free(ssl);
        SSL_CTX_free(ctx);
        close(sock);

        if (!raw || !headers_parsed || len < 12)
        {
            if (raw)
                set_err("Read timeout");
            fprintf(stderr, "[https +%lldms] incomplete response (%zu bytes)\n",
                    (long long)(mono_ms() - t0), len);
            free(raw);
            break;
        }

        int status = 0;
        if (strncmp(raw, "HTTP/1.", 7) == 0 && raw[7] && raw[8] == ' ')
            status = atoi(raw + 9);

        if (status == 301 || status == 302 || status == 307 || status == 308)
        {
            const char *loc = find_header_ci(raw, hdr_len, "Location");
            if (loc)
            {
                const char *eol = memchr(loc, '\n', hdr_len - (size_t)(loc - raw));
                size_t llen = eol ? (size_t)(eol - loc) : strlen(loc);
                if (llen < sizeof(sc->loc_buf))
                {
                    memcpy(sc->loc_buf, loc, llen);
                    sc->loc_buf[llen] = '\0';
                    while (llen > 0 && (sc->loc_buf[llen - 1] == '\r' ||
                                        sc->loc_buf[llen - 1] == ' '))
                        sc->loc_buf[--llen] = '\0';

                    if (sc->loc_buf[0] == '/')
                    {
                        snprintf(sc->cur_path, sizeof(sc->cur_path), "%s",
                                 sc->loc_buf);
                        free(raw);
                        continue;
                    }
                    if (parse_https_url(sc->loc_buf,
                                        sc->cur_host, sizeof(sc->cur_host),
                                        sc->cur_path, sizeof(sc->cur_path)))
                    {
                        free(raw);
                        continue;
                    }
                }
            }
            set_err("Bad redirect");
            free(raw);
            break;
        }

        if (status != 200)
        {
            got_http_status_err = true;
            set_err("HTTP %d", status);
            fprintf(stderr, "[https +%lldms] HTTP %d for https://%s%s\n",
                    (long long)(mono_ms() - t0), status, sc->cur_host, sc->cur_path);
            free(raw);
            break;
        }

        size_t body_len = len - body_off;
        if (is_chunked)
        {
            size_t dec_len = 0;
            if (!decode_chunked(raw + body_off, body_len, raw, &dec_len) ||
                dec_len == 0 || dec_len > max_body)
            {
                set_err("Chunk decode err");
                free(raw);
                break;
            }
            raw[dec_len] = '\0';
            free(sc);
            if (out_len)
                *out_len = dec_len;
            fprintf(stderr, "[https +%lldms] OK %zu bytes from %s\n",
                    (long long)(mono_ms() - t0), dec_len, init_host);
            return raw;
        }

        if (body_len == 0 || body_len > max_body)
        {
            set_err(body_len == 0 ? "Empty response" : "Response too large");
            free(raw);
            break;
        }

        memmove(raw, raw + body_off, body_len);
        raw[body_len] = '\0';
        free(sc);
        if (out_len)
            *out_len = body_len;
        fprintf(stderr, "[https +%lldms] OK %zu bytes from %s\n",
                (long long)(mono_ms() - t0), body_len, init_host);
        return raw;
    }

    free(sc);

    if (allow_curl_fallback && !s_net_down && !got_http_status_err)
    {
        fprintf(stderr, "[https +%lldms] trying curl fallback for https://%s%s\n",
                (long long)(mono_ms() - t0), init_host, init_path);
        char *cbuf = curl_fallback_alloc(init_host, init_path,
                                         have_first_ip ? &first_resolved_ip : NULL,
                                         max_body, out_len);
        if (cbuf)
            return cbuf;
    }
    return NULL;
}

bool retrohh_https_get_ex(const char *host, const char *path,
                          const char *ca_file, bool allow_curl_fallback,
                          char *out_buf, size_t out_cap, size_t *out_len)
{
    if (!out_buf || out_cap < 2)
        return false;
    size_t n = 0;
    char *body = https_fetch_alloc(host, path, ca_file, allow_curl_fallback,
                                   out_cap - 1, &n);
    if (!body)
        return false;
    memcpy(out_buf, body, n + 1);
    free(body);
    if (out_len)
        *out_len = n;
    return true;
}

bool retrohh_https_get(const char *host, const char *path,
                       const char *ca_file,
                       char *out_buf, size_t out_cap, size_t *out_len)
{
    return retrohh_https_get_ex(host, path, ca_file, true,
                                out_buf, out_cap, out_len);
}

bool retrohh_https_download_file_ex(const char *host, const char *path,
                                    const char *ca_file, bool allow_curl_fallback,
                                    const char *dest_path, size_t max_bytes)
{
    if (!dest_path || !dest_path[0] || max_bytes == 0)
        return false;
    size_t n = 0;
    char *body = https_fetch_alloc(host, path, ca_file, allow_curl_fallback,
                                   max_bytes, &n);
    if (!body)
        return false;

    char tmp_path[512];
    snprintf(tmp_path, sizeof(tmp_path), "%s.tmp", dest_path);

    int fd = open(tmp_path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0)
    {
        set_err("Disk write error");
        free(body);
        return false;
    }

    size_t written = 0;
    bool ok = true;
    while (written < n)
    {
        ssize_t w = write(fd, body + written, n - written);
        if (w < 0 && errno == EINTR)
            continue;
        if (w <= 0)
        {
            ok = false;
            break;
        }
        written += (size_t)w;
    }
    close(fd);
    free(body);

    if (!ok || rename(tmp_path, dest_path) != 0)
    {
        set_err("Disk rename error");
        unlink(tmp_path);
        return false;
    }
    return true;
}

bool retrohh_https_download_file(const char *host, const char *path,
                                 const char *ca_file,
                                 const char *dest_path, size_t max_bytes)
{
    return retrohh_https_download_file_ex(host, path, ca_file, true,
                                          dest_path, max_bytes);
}
