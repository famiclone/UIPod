/***************************************************************************
 * Trimpod: Thread-safe HTTP + HTTPS 1.1 streaming transport for Navidrome
 * on TrimUI Brick (tg5040 / NextUI).
 ****************************************************************************/
#include <sys/types.h>
#include <sys/socket.h>
#include <sys/select.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <netdb.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <errno.h>
#include <unistd.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <ctype.h>
#include <time.h>

#include <openssl/ssl.h>
#include <openssl/err.h>
#include <openssl/x509v3.h>

#include "navidrome-http-target.h"

#define NAV_DNS_TIMEOUT_MS        1600
#define NAV_TCP_CONNECT_MS        3200
#define NAV_TLS_HANDSHAKE_MS      4000
#define NAV_POLL_SLICE_MS         80
#define NAV_MAX_REDIRECTS         4
#define NAV_MAX_IPS               6
#define NAV_MAX_DNS_SERVERS       8
#define NAV_MAX_MEM_BYTES         (32 * 1024 * 1024)
#define NAV_SD_WRITE_BUF_SZ       (256 * 1024)

static pthread_once_t s_ssl_once = PTHREAD_ONCE_INIT;
static pthread_mutex_t s_udhcpc_mtx = PTHREAD_MUTEX_INITIALIZER;

static void init_ssl_once(void)
{
    signal(SIGPIPE, SIG_IGN);
    OPENSSL_init_ssl(0, NULL);
}

static void put_err(char *buf, size_t cap, const char *fmt, ...)
    __attribute__((format(printf, 3, 4)));

static void put_err(char *buf, size_t cap, const char *fmt, ...)
{
    if (!buf || cap == 0)
        return;
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, cap, fmt, ap);
    va_end(ap);
}

static int64_t mono_ms(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) == 0)
        return (int64_t)ts.tv_sec * 1000 + (int64_t)(ts.tv_nsec / 1000000);
    return (int64_t)time(NULL) * 1000;
}

static bool is_cancelled(uint8_t (*cancel_cb)(void *), void *cancel_ctx)
{
    return cancel_cb && cancel_cb(cancel_ctx) != 0;
}

/* Wait on non-blocking `fd` in 80 ms slices so `cancel_cb` responds immediately.
 * Returns 1 = ready, 0 = timeout, -1 = error, -2 = cancelled. */
static int wait_fd_cancellable(int fd, bool for_write, int64_t deadline_ms,
                               uint8_t (*cancel_cb)(void *), void *cancel_ctx)
{
    if (fd < 0 || fd >= FD_SETSIZE)
        return -1;

    for (;;)
    {
        if (is_cancelled(cancel_cb, cancel_ctx))
            return -2;

        int64_t rem = deadline_ms - mono_ms();
        if (rem <= 0)
            return 0;
        if (rem > NAV_POLL_SLICE_MS)
            rem = NAV_POLL_SLICE_MS;

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
            continue;
        if (errno == EINTR)
            continue;
        return -1;
    }
}

static int set_nonblock_cloexec(int fd)
{
    int fl = fcntl(fd, F_GETFL, 0);
    if (fl < 0 || fcntl(fd, F_SETFL, fl | O_NONBLOCK) < 0)
        return -1;
    int fdfl = fcntl(fd, F_GETFD, 0);
    if (fdfl >= 0)
        fcntl(fd, F_SETFD, fdfl | FD_CLOEXEC);
    return 0;
}

/* ---- Network interface check (supports LAN-only Wi-Fi, Tailscale, USB) --- */

static bool proc_has_any_ipv4_route(void)
{
    FILE *f = fopen("/proc/net/route", "re");
    if (!f)
        return (errno == ENOENT);

    char line[256];
    bool found = false;
    if (fgets(line, sizeof(line), f))
    {
        while (fgets(line, sizeof(line), f))
        {
            char iface[32];
            unsigned long dest = 0, gw = 0;
            unsigned int flags = 0;
            if (sscanf(line, "%31s %lx %lx %x", iface, &dest, &gw, &flags) >= 4)
            {
                if (strcmp(iface, "lo") != 0 && (flags & 0x1))
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

static bool ensure_network_ready(char *err_buf, size_t err_cap)
{
    if (proc_has_any_ipv4_route())
        return true;

    pthread_mutex_lock(&s_udhcpc_mtx);
    static int64_t s_last_try = -60000;
    int64_t now = mono_ms();
    if (!proc_has_any_ipv4_route() && wlan0_carrier_up() && (now - s_last_try >= 15000))
    {
        s_last_try = now;
        const char *bin = access("/sbin/udhcpc", X_OK) == 0 ? "/sbin/udhcpc"
                        : access("/bin/udhcpc", X_OK) == 0  ? "/bin/udhcpc" : NULL;
        if (bin)
        {
            pid_t pid = fork();
            if (pid == 0)
            {
                int dn = open("/dev/null", O_RDWR);
                if (dn >= 0)
                {
                    dup2(dn, STDIN_FILENO);
                    dup2(dn, STDOUT_FILENO);
                    dup2(dn, STDERR_FILENO);
                    if (dn > 2) close(dn);
                }
                char *const argv[] = {
                    (char *)bin, "-i", "wlan0", "-n", "-q", "-t", "2", "-T", "1", NULL
                };
                execv(bin, argv);
                _exit(127);
            }
            if (pid > 0)
            {
                int64_t dl = mono_ms() + 2500;
                int st = 0;
                while (mono_ms() < dl)
                {
                    pid_t w = waitpid(pid, &st, WNOHANG);
                    if (w == pid || (w < 0 && errno != EINTR))
                        break;
                    struct timeval tv = { .tv_sec = 0, .tv_usec = 40000 };
                    select(0, NULL, NULL, NULL, &tv);
                }
                kill(pid, SIGKILL);
                waitpid(pid, &st, WNOHANG);
            }
        }
    }
    pthread_mutex_unlock(&s_udhcpc_mtx);

    if (proc_has_any_ipv4_route())
        return true;

    put_err(err_buf, err_cap, "No Wi-Fi connection");
    return false;
}

/* ---- Pure-socket RFC 1035 DNS resolver (LAN + Tailscale friendly!) ------- */

static void add_unique_ip(struct in_addr *list, int *count, int max_count,
                          struct in_addr ip)
{
    if (*count >= max_count || ip.s_addr == 0)
        return;
    for (int i = 0; i < *count; i++)
        if (list[i].s_addr == ip.s_addr)
            return;
    list[(*count)++] = ip;
}

static bool lookup_etc_hosts(const char *host, struct in_addr *out_ips, int *count, int max_ips)
{
    if (strcasecmp(host, "localhost") == 0)
    {
        struct in_addr lo;
        if (inet_pton(AF_INET, "127.0.0.1", &lo) == 1)
            add_unique_ip(out_ips, count, max_ips, lo);
        return *count > 0;
    }

    FILE *f = fopen("/etc/hosts", "re");
    if (!f)
        return false;
    char line[256];
    while (fgets(line, sizeof(line), f))
    {
        char *hash = strchr(line, '#');
        if (hash) *hash = '\0';
        char ip_s[64];
        int off = 0;
        if (sscanf(line, "%63s%n", ip_s, &off) == 1)
        {
            struct in_addr ip;
            if (inet_pton(AF_INET, ip_s, &ip) == 1)
            {
                char *p = line + off;
                char alias[128];
                int aoff = 0;
                while (sscanf(p, "%127s%n", alias, &aoff) == 1)
                {
                    if (strcasecmp(alias, host) == 0)
                        add_unique_ip(out_ips, count, max_ips, ip);
                    p += aoff;
                }
            }
        }
    }
    fclose(f);
    return *count > 0;
}

static void collect_dns_servers(struct in_addr *servers, int *count)
{
    static const char *const files[] = {
        "/tmp/resolv.conf.auto",
        "/tmp/resolv.conf.d/resolv.conf.auto",
        "/tmp/resolv.conf",
        "/etc/resolv.conf",
    };

    for (size_t i = 0; i < sizeof(files) / sizeof(files[0]); i++)
    {
        FILE *f = fopen(files[i], "re");
        if (!f)
            continue;
        char line[256];
        while (fgets(line, sizeof(line), f))
        {
            char *p = line;
            while (*p == ' ' || *p == '\t')
                p++;
            if (strncmp(p, "nameserver", 10) == 0 &&
                (p[10] == ' ' || p[10] == '\t'))
            {
                char ip_s[64] = {0};
                if (sscanf(p + 10, "%63s", ip_s) == 1)
                {
                    struct in_addr ip;
                    if (inet_pton(AF_INET, ip_s, &ip) == 1)
                    {
                        const uint8_t *b = (const uint8_t *)&ip.s_addr;
                        if (b[0] != 127 && b[0] != 0)
                            add_unique_ip(servers, count, NAV_MAX_DNS_SERVERS, ip);
                    }
                }
            }
        }
        fclose(f);
    }

    /* Tailscale MagicDNS (`100.100.100.100`) if Tailscale interface exists */
    if (access("/sys/class/net/tailscale0", F_OK) == 0)
    {
        struct in_addr ts_dns;
        if (inet_pton(AF_INET, "100.100.100.100", &ts_dns) == 1)
            add_unique_ip(servers, count, NAV_MAX_DNS_SERVERS, ts_dns);
    }

    static const char *const pub_dns[] = { "1.1.1.1", "8.8.8.8" };
    for (size_t i = 0; i < 2; i++)
    {
        struct in_addr ip;
        if (inet_pton(AF_INET, pub_dns[i], &ip) == 1)
            add_unique_ip(servers, count, NAV_MAX_DNS_SERVERS, ip);
    }
}

static size_t build_dns_query(const char *host, uint16_t txid, uint8_t *buf, size_t cap)
{
    if (!host || !host[0] || cap < 64)
        return 0;
    buf[0] = (uint8_t)(txid >> 8);
    buf[1] = (uint8_t)(txid & 0xff);
    buf[2] = 0x01; buf[3] = 0x00; /* RD=1 */
    buf[4] = 0x00; buf[5] = 0x01; /* QDCOUNT=1 */
    memset(buf + 6, 0, 6);

    size_t pos = 12;
    const char *p = host;
    while (*p)
    {
        const char *dot = strchr(p, '.');
        size_t llen = dot ? (size_t)(dot - p) : strlen(p);
        if (llen == 0)
        {
            if (!dot) break;
            return 0;
        }
        if (llen > 63 || pos + 1 + llen + 5 >= cap)
            return 0;
        buf[pos++] = (uint8_t)llen;
        memcpy(buf + pos, p, llen);
        pos += llen;
        if (!dot) break;
        p = dot + 1;
    }
    buf[pos++] = 0x00;
    buf[pos++] = 0x00; buf[pos++] = 0x01; /* QTYPE A */
    buf[pos++] = 0x00; buf[pos++] = 0x01; /* QCLASS IN */
    return pos;
}

static size_t skip_dns_name(const uint8_t *msg, size_t len, size_t pos)
{
    int jumps = 0;
    while (pos < len && jumps < 16)
    {
        uint8_t b = msg[pos];
        if (b == 0) return pos + 1;
        if ((b & 0xc0) == 0xc0)
            return (pos + 2 <= len) ? (pos + 2) : 0;
        if (b > 63) return 0;
        pos += 1 + (size_t)b;
        jumps++;
    }
    return 0;
}

static int parse_dns_response(const uint8_t *msg, size_t len, uint16_t txid,
                              struct in_addr *out_ips, int max_ips)
{
    if (len < 12) return 0;
    uint16_t rxid = ((uint16_t)msg[0] << 8) | msg[1];
    if (rxid != txid || (msg[2] & 0x80) == 0 || (msg[3] & 0x0f) != 0)
        return 0;

    uint16_t qdcount = ((uint16_t)msg[4] << 8) | msg[5];
    uint16_t ancount = ((uint16_t)msg[6] << 8) | msg[7];
    size_t pos = 12;
    for (uint16_t i = 0; i < qdcount; i++)
    {
        pos = skip_dns_name(msg, len, pos);
        if (pos == 0 || pos + 4 > len) return 0;
        pos += 4;
    }

    int count = 0;
    for (uint16_t i = 0; i < ancount && pos < len; i++)
    {
        pos = skip_dns_name(msg, len, pos);
        if (pos == 0 || pos + 10 > len) break;
        uint16_t rtype  = ((uint16_t)msg[pos] << 8) | msg[pos + 1];
        uint16_t rclass = ((uint16_t)msg[pos + 2] << 8) | msg[pos + 3];
        uint16_t rdlen  = ((uint16_t)msg[pos + 8] << 8) | msg[pos + 9];
        pos += 10;
        if (pos + rdlen > len) break;
        if (rtype == 1 && rclass == 1 && rdlen == 4)
        {
            const uint8_t *b = msg + pos;
            /* Allow LAN (10.*, 192.168.*, 172.16-31.*) & Tailscale (100.64-127.*)!
             * Only reject 0.0.0.0 and multicast/broadcast. */
            if (b[0] != 0 && b[0] < 224)
            {
                struct in_addr ip;
                memcpy(&ip.s_addr, b, 4);
                add_unique_ip(out_ips, &count, max_ips, ip);
            }
        }
        pos += rdlen;
    }
    return count;
}

static int resolve_host_ipv4(const char *host, struct in_addr *out_ips, int max_ips,
                             uint8_t (*cancel_cb)(void *), void *cancel_ctx)
{
    int count = 0;
    struct in_addr lit;
    if (inet_pton(AF_INET, host, &lit) == 1)
    {
        add_unique_ip(out_ips, &count, max_ips, lit);
        return count;
    }

    if (lookup_etc_hosts(host, out_ips, &count, max_ips))
        return count;

    struct in_addr servers[NAV_MAX_DNS_SERVERS];
    int n_servers = 0;
    collect_dns_servers(servers, &n_servers);

    uint16_t txid = (uint16_t)((mono_ms() ^ (intptr_t)out_ips) & 0xffff);
    if (txid == 0) txid = 0x5a3c;
    uint8_t qbuf[512];
    size_t qlen = build_dns_query(host, txid, qbuf, sizeof(qbuf));

    if (qlen > 0 && n_servers > 0)
    {
        int ufd = socket(AF_INET, SOCK_DGRAM, 0);
        if (ufd >= 0)
        {
            set_nonblock_cloexec(ufd);
            for (int i = 0; i < n_servers; i++)
            {
                struct sockaddr_in dst = {0};
                dst.sin_family = AF_INET;
                dst.sin_port = htons(53);
                dst.sin_addr = servers[i];
                sendto(ufd, qbuf, qlen, 0, (struct sockaddr *)&dst, sizeof(dst));
            }

            int64_t dl = mono_ms() + NAV_DNS_TIMEOUT_MS;
            while (count == 0)
            {
                int w = wait_fd_cancellable(ufd, false, dl, cancel_cb, cancel_ctx);
                if (w <= 0) break;
                uint8_t rbuf[1500];
                for (;;)
                {
                    ssize_t r = recvfrom(ufd, rbuf, sizeof(rbuf), 0, NULL, NULL);
                    if (r < 0)
                    {
                        if (errno == EINTR) continue;
                        break;
                    }
                    int n = parse_dns_response(rbuf, (size_t)r, txid, out_ips, max_ips);
                    if (n > 0) { count = n; break; }
                }
            }
            close(ufd);
        }
    }

    return count;
}

/* ---- Non-blocking TCP + Optional OpenSSL TLS Stream --------------------- */

struct nav_conn {
    int      sock;
    SSL_CTX *ssl_ctx;
    SSL     *ssl;
};

static int ssl_verify_skew_tolerant(int preverify_ok, X509_STORE_CTX *x509_ctx)
{
    if (preverify_ok)
        return 1;
    int err = X509_STORE_CTX_get_error(x509_ctx);
    if (err == X509_V_ERR_CERT_NOT_YET_VALID || err == X509_V_ERR_CERT_HAS_EXPIRED)
        return 1;
    return 0;
}

static void conn_close(struct nav_conn *c)
{
    if (c->ssl)
    {
        SSL_set_shutdown(c->ssl, SSL_SENT_SHUTDOWN | SSL_RECEIVED_SHUTDOWN);
        SSL_free(c->ssl);
        c->ssl = NULL;
    }
    if (c->ssl_ctx)
    {
        SSL_CTX_free(c->ssl_ctx);
        c->ssl_ctx = NULL;
    }
    if (c->sock >= 0)
    {
        close(c->sock);
        c->sock = -1;
    }
}

static int conn_open(struct nav_conn *c, bool is_https, const char *host, uint16_t port,
                     bool allow_self_signed,
                     uint8_t (*cancel_cb)(void *), void *cancel_ctx,
                     char *err_buf, size_t err_cap)
{
    c->sock = -1;
    c->ssl_ctx = NULL;
    c->ssl = NULL;

    if (!ensure_network_ready(err_buf, err_cap))
        return -1;

    struct in_addr ips[NAV_MAX_IPS];
    int n_ips = resolve_host_ipv4(host, ips, NAV_MAX_IPS, cancel_cb, cancel_ctx);
    if (is_cancelled(cancel_cb, cancel_ctx))
    {
        put_err(err_buf, err_cap, "Cancelled");
        return -2;
    }
    if (n_ips <= 0)
    {
        put_err(err_buf, err_cap, "DNS lookup failed: %s", host);
        return -1;
    }

    int sock = -1;
    for (int i = 0; i < n_ips; i++)
    {
        if (is_cancelled(cancel_cb, cancel_ctx))
        {
            put_err(err_buf, err_cap, "Cancelled");
            return -2;
        }
        int fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd < 0) continue;
        if (set_nonblock_cloexec(fd) < 0) { close(fd); continue; }

        int one = 1;
        setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
        int rcvbuf = 262144;
        setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &rcvbuf, sizeof(rcvbuf));

        struct sockaddr_in sa = {0};
        sa.sin_family = AF_INET;
        sa.sin_port = htons(port);
        sa.sin_addr = ips[i];

        int rc;
        do {
            rc = connect(fd, (struct sockaddr *)&sa, sizeof(sa));
        } while (rc < 0 && errno == EINTR);

        if (rc == 0)
        {
            sock = fd;
            break;
        }
        if (errno == EINPROGRESS)
        {
            int w = wait_fd_cancellable(fd, true, mono_ms() + NAV_TCP_CONNECT_MS,
                                        cancel_cb, cancel_ctx);
            if (w == -2)
            {
                close(fd);
                put_err(err_buf, err_cap, "Cancelled");
                return -2;
            }
            if (w > 0)
            {
                int so_err = 0;
                socklen_t slen = sizeof(so_err);
                if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &so_err, &slen) == 0 && so_err == 0)
                {
                    sock = fd;
                    break;
                }
            }
        }
        close(fd);
    }

    if (sock < 0)
    {
        put_err(err_buf, err_cap, "Cannot connect to %s:%u", host, (unsigned)port);
        return -1;
    }

    c->sock = sock;
    if (!is_https)
        return 0;

    pthread_once(&s_ssl_once, init_ssl_once);
    c->ssl_ctx = SSL_CTX_new(TLS_client_method());
    if (!c->ssl_ctx)
    {
        conn_close(c);
        put_err(err_buf, err_cap, "SSL_CTX_new failed");
        return -1;
    }

    if (allow_self_signed)
    {
        SSL_CTX_set_verify(c->ssl_ctx, SSL_VERIFY_NONE, NULL);
    }
    else
    {
        bool have_ca = false;
        static const char *const ca_paths[] = {
            "/tmp/trimpod/autoeq/ca-bundle.crt",
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/ssl/cert.pem",
        };
        for (size_t i = 0; i < sizeof(ca_paths) / sizeof(ca_paths[0]); i++)
        {
            if (SSL_CTX_load_verify_locations(c->ssl_ctx, ca_paths[i], NULL) == 1)
            {
                have_ca = true;
                break;
            }
        }
        if (!have_ca && SSL_CTX_set_default_verify_paths(c->ssl_ctx) == 1)
            have_ca = true;

        if (have_ca)
        {
            SSL_CTX_set_verify(c->ssl_ctx, SSL_VERIFY_PEER, ssl_verify_skew_tolerant);
            X509_VERIFY_PARAM *param = SSL_CTX_get0_param(c->ssl_ctx);
            if (param)
            {
                X509_VERIFY_PARAM_set_flags(param, X509_V_FLAG_TRUSTED_FIRST);
                struct in_addr tmp_ip;
                if (inet_pton(AF_INET, host, &tmp_ip) == 1)
                    X509_VERIFY_PARAM_set1_ip(param, (const unsigned char *)&tmp_ip.s_addr, 4);
                else
                    X509_VERIFY_PARAM_set1_host(param, host, 0);
            }
        }
    }

    c->ssl = SSL_new(c->ssl_ctx);
    if (!c->ssl)
    {
        conn_close(c);
        put_err(err_buf, err_cap, "SSL_new failed");
        return -1;
    }
    struct in_addr dummy_ip;
    if (inet_pton(AF_INET, host, &dummy_ip) != 1)
        SSL_set_tlsext_host_name(c->ssl, host);
    SSL_set_fd(c->ssl, sock);

    int64_t hs_dl = mono_ms() + NAV_TLS_HANDSHAKE_MS;
    ERR_clear_error();
    for (;;)
    {
        if (is_cancelled(cancel_cb, cancel_ctx))
        {
            conn_close(c);
            put_err(err_buf, err_cap, "Cancelled");
            return -2;
        }
        errno = 0;
        int rc = SSL_connect(c->ssl);
        if (rc == 1)
            break;
        int err = SSL_get_error(c->ssl, rc);
        bool want_wr = (err == SSL_ERROR_WANT_WRITE);
        if (err == SSL_ERROR_WANT_READ || want_wr)
        {
            int w = wait_fd_cancellable(sock, want_wr, hs_dl, cancel_cb, cancel_ctx);
            if (w == -2)
            {
                conn_close(c);
                put_err(err_buf, err_cap, "Cancelled");
                return -2;
            }
            if (w > 0)
                continue;
        }
        conn_close(c);
        put_err(err_buf, err_cap, "TLS handshake failed");
        return -1;
    }

    return 0;
}

static int conn_write_all(struct nav_conn *c, const uint8_t *buf, size_t len,
                          int64_t deadline_ms,
                          uint8_t (*cancel_cb)(void *), void *cancel_ctx)
{
    size_t sent = 0;
    while (sent < len)
    {
        if (is_cancelled(cancel_cb, cancel_ctx))
            return -2;
        if (mono_ms() >= deadline_ms)
            return -1;

        if (c->ssl)
        {
            errno = 0;
            int w = SSL_write(c->ssl, buf + sent, (int)(len - sent));
            if (w > 0) { sent += (size_t)w; continue; }
            int err = SSL_get_error(c->ssl, w);
            if (err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE)
            {
                int wr = wait_fd_cancellable(c->sock, err == SSL_ERROR_WANT_WRITE,
                                             deadline_ms, cancel_cb, cancel_ctx);
                if (wr > 0) continue;
                return (wr == -2) ? -2 : -1;
            }
            return -1;
        }
        else
        {
            ssize_t w = send(c->sock, buf + sent, len - sent, 0);
            if (w > 0) { sent += (size_t)w; continue; }
            if (w < 0 && errno == EINTR) continue;
            if (w < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
            {
                int wr = wait_fd_cancellable(c->sock, true, deadline_ms, cancel_cb, cancel_ctx);
                if (wr > 0) continue;
                return (wr == -2) ? -2 : -1;
            }
            return -1;
        }
    }
    return 0;
}

/* Read >0 bytes, 0 on clean EOF, -1 on error/timeout, -2 on cancel. */
static ssize_t conn_read_some(struct nav_conn *c, uint8_t *buf, size_t cap,
                              int idle_timeout_ms,
                              uint8_t (*cancel_cb)(void *), void *cancel_ctx)
{
    int64_t dl = mono_ms() + idle_timeout_ms;
    for (;;)
    {
        if (is_cancelled(cancel_cb, cancel_ctx))
            return -2;
        if (mono_ms() >= dl)
            return -1;

        if (c->ssl)
        {
            errno = 0;
            int r = SSL_read(c->ssl, buf, (int)cap);
            if (r > 0) return (ssize_t)r;
            int err = SSL_get_error(c->ssl, r);
            if (err == SSL_ERROR_ZERO_RETURN) return 0;
            if (err == SSL_ERROR_SYSCALL && r == 0 && errno == 0) return 0;
            if (err == SSL_ERROR_WANT_READ || err == SSL_ERROR_WANT_WRITE)
            {
                int wr = wait_fd_cancellable(c->sock, err == SSL_ERROR_WANT_WRITE,
                                             dl, cancel_cb, cancel_ctx);
                if (wr > 0) continue;
                return (wr == -2) ? -2 : -1;
            }
            return -1;
        }
        else
        {
            ssize_t r = recv(c->sock, buf, cap, 0);
            if (r > 0) return r;
            if (r == 0) return 0;
            if (errno == EINTR) continue;
            if (errno == EAGAIN || errno == EWOULDBLOCK)
            {
                int wr = wait_fd_cancellable(c->sock, false, dl, cancel_cb, cancel_ctx);
                if (wr > 0) continue;
                return (wr == -2) ? -2 : -1;
            }
            return -1;
        }
    }
}

/* ---- HTTP/1.1 Header + Chunked/Content-Length Body Sink ----------------- */

static const char *find_hdr_ci(const char *hdrs, size_t hlen, const char *name)
{
    size_t nlen = strlen(name);
    const char *p = hdrs;
    const char *end = hdrs + hlen;
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
        if (!eol) break;
        p = eol + 1;
    }
    return NULL;
}

static bool parse_redirect_url(const char *loc, size_t loc_len,
                               bool *io_https, char *host_buf, size_t host_cap,
                               uint16_t *io_port, char *pq_buf, size_t pq_cap)
{
    while (loc_len > 0 && (loc[loc_len - 1] == '\r' || loc[loc_len - 1] == '\n' ||
                           loc[loc_len - 1] == ' '  || loc[loc_len - 1] == '\t'))
        loc_len--;
    if (loc_len == 0 || loc_len >= pq_cap)
        return false;

    if (loc[0] == '/')
    {
        memcpy(pq_buf, loc, loc_len);
        pq_buf[loc_len] = '\0';
        return true;
    }

    bool https = false;
    const char *rest = NULL;
    size_t rlen = 0;
    if (loc_len > 8 && strncasecmp(loc, "https://", 8) == 0)
    {
        https = true;
        rest = loc + 8;
        rlen = loc_len - 8;
    }
    else if (loc_len > 7 && strncasecmp(loc, "http://", 7) == 0)
    {
        https = false;
        rest = loc + 7;
        rlen = loc_len - 7;
    }
    else
        return false;

    const char *slash = memchr(rest, '/', rlen);
    size_t alen = slash ? (size_t)(slash - rest) : rlen;
    if (alen == 0 || alen >= host_cap)
        return false;

    const char *colon = memchr(rest, ':', alen);
    uint16_t port = https ? 443 : 80;
    size_t hlen = alen;
    if (colon)
    {
        hlen = (size_t)(colon - rest);
        char pbuf[16] = {0};
        size_t plen = alen - hlen - 1;
        if (plen == 0 || plen >= sizeof(pbuf)) return false;
        memcpy(pbuf, colon + 1, plen);
        long pv = strtol(pbuf, NULL, 10);
        if (pv <= 0 || pv > 65535) return false;
        port = (uint16_t)pv;
    }

    memcpy(host_buf, rest, hlen);
    host_buf[hlen] = '\0';
    *io_https = https;
    *io_port = port;

    if (slash)
    {
        size_t plen = rlen - (size_t)(slash - rest);
        if (plen >= pq_cap) return false;
        memcpy(pq_buf, slash, plen);
        pq_buf[plen] = '\0';
    }
    else
    {
        strcpy(pq_buf, "/");
    }
    return true;
}

typedef bool (*body_sink_fn)(void *sink_ctx, const uint8_t *data, size_t len);
typedef bool (*resp_hdr_fn)(void *sink_ctx, int status_code, const char *hdrs,
                            size_t hdr_len, bool is_chunked, int64_t content_length);

/* Stream HTTP/1.1 response body (either Content-Length, chunked, or until EOF)
 * into `sink_fn`, with optional method, custom headers, request body, and
 * response-header callback. */
static int perform_http_stream_ex(
    const char *method,
    bool init_https,
    const char *init_host,
    uint16_t init_port,
    const char *init_pq,
    const char *extra_headers,
    const uint8_t *req_body,
    size_t req_body_len,
    bool allow_self_signed,
    int idle_timeout_ms,
    int32_t *out_status,
    resp_hdr_fn hdr_fn,
    body_sink_fn sink_fn,
    void *sink_ctx,
    uint8_t (*cancel_cb)(void *),
    void *cancel_ctx,
    char *err_buf,
    size_t err_cap)
{
    if (out_status)
        *out_status = 0;
    const char *verb = (method && method[0]) ? method : "GET";
    bool is_head = (strcasecmp(verb, "HEAD") == 0);
    bool is_post_like = (strcasecmp(verb, "POST") == 0 || strcasecmp(verb, "PUT") == 0);

    bool cur_https = init_https;
    char cur_host[256];
    uint16_t cur_port = init_port;
    char *cur_pq = malloc(4096);
    uint8_t *rx = malloc(65536);
    if (!cur_pq || !rx)
    {
        free(cur_pq);
        free(rx);
        put_err(err_buf, err_cap, "Out of memory");
        return -1;
    }

    snprintf(cur_host, sizeof(cur_host), "%s", init_host);
    snprintf(cur_pq, 4096, "%s", init_pq);

    int idle_ms = (idle_timeout_ms > 2000) ? idle_timeout_ms : 8000;
    if (idle_ms > 30000)
        idle_ms = 30000; /* per-read stall deadline (up to 30s for NAS video transcode spin-up) */

    int final_rc = -1;

    for (int redir = 0; redir <= NAV_MAX_REDIRECTS; redir++)
    {
        struct nav_conn conn;
        int orc = conn_open(&conn, cur_https, cur_host, cur_port, allow_self_signed,
                            cancel_cb, cancel_ctx, err_buf, err_cap);
        if (orc != 0)
        {
            final_rc = orc;
            break;
        }

        char cl_hdr[64] = "";
        if ((req_body && req_body_len > 0) || is_post_like)
            snprintf(cl_hdr, sizeof(cl_hdr), "Content-Length: %zu\r\n", req_body_len);

        const char *xhdr = extra_headers ? extra_headers : "";
        char req_hdr[8192];
        bool default_port = (cur_https && cur_port == 443) || (!cur_https && cur_port == 80);
        int req_len = default_port
            ? snprintf(req_hdr, sizeof(req_hdr),
                       "%s %s HTTP/1.1\r\n"
                       "Host: %s\r\n"
                       "User-Agent: UIPod/1.0\r\n"
                       "Accept: */*\r\n"
                       "Accept-Encoding: identity\r\n"
                       "%s%s"
                       "Connection: close\r\n\r\n",
                       verb, cur_pq, cur_host, cl_hdr, xhdr)
            : snprintf(req_hdr, sizeof(req_hdr),
                       "%s %s HTTP/1.1\r\n"
                       "Host: %s:%u\r\n"
                       "User-Agent: UIPod/1.0\r\n"
                       "Accept: */*\r\n"
                       "Accept-Encoding: identity\r\n"
                       "%s%s"
                       "Connection: close\r\n\r\n",
                       verb, cur_pq, cur_host, (unsigned)cur_port, cl_hdr, xhdr);

        if (req_len <= 0 || (size_t)req_len >= sizeof(req_hdr))
        {
            conn_close(&conn);
            put_err(err_buf, err_cap, "Request URL too long");
            final_rc = -1;
            break;
        }

        int wrc = conn_write_all(&conn, (const uint8_t *)req_hdr, (size_t)req_len,
                                 mono_ms() + 8000, cancel_cb, cancel_ctx);
        if (wrc == 0 && req_body && req_body_len > 0)
        {
            wrc = conn_write_all(&conn, req_body, req_body_len,
                                 mono_ms() + 12000, cancel_cb, cancel_ctx);
        }
        if (wrc != 0)
        {
            conn_close(&conn);
            put_err(err_buf, err_cap, (wrc == -2) ? "Cancelled" : "Send failed");
            final_rc = wrc;
            break;
        }

        /* Read response headers (up to 16 KB) */
        char hbuf[16384];
        size_t hgot = 0;
        size_t body_off = 0;
        int rrc = 0;
        while (hgot + 1 < sizeof(hbuf))
        {
            ssize_t n = conn_read_some(&conn, (uint8_t *)hbuf + hgot,
                                       sizeof(hbuf) - 1 - hgot, idle_ms,
                                       cancel_cb, cancel_ctx);
            if (n < 0) { rrc = (int)n; break; }
            if (n == 0) { rrc = -1; break; }
            hgot += (size_t)n;
            hbuf[hgot] = '\0';
            char *sep = strstr(hbuf, "\r\n\r\n");
            if (sep)
            {
                body_off = (size_t)(sep - hbuf) + 4;
                break;
            }
        }

        if (body_off == 0)
        {
            conn_close(&conn);
            put_err(err_buf, err_cap, (rrc == -2) ? "Cancelled" : "Invalid HTTP response");
            final_rc = (rrc == -2) ? -2 : -1;
            break;
        }

        int status_code = 0;
        if (strncmp(hbuf, "HTTP/1.", 7) == 0)
            status_code = atoi(hbuf + 9);
        if (out_status)
            *out_status = status_code;

        size_t hdr_len = body_off - 4;
        if (status_code == 301 || status_code == 302 ||
            status_code == 307 || status_code == 308)
        {
            const char *loc = find_hdr_ci(hbuf, hdr_len, "Location");
            conn_close(&conn);
            if (!loc || redir == NAV_MAX_REDIRECTS)
            {
                put_err(err_buf, err_cap, "Redirect failed");
                final_rc = -1;
                break;
            }
            if (status_code == 301 || status_code == 302)
            {
                verb = "GET";
                is_post_like = false;
                req_body = NULL;
                req_body_len = 0;
            }
            const char *leol = memchr(loc, '\n', (size_t)((hbuf + hdr_len) - loc));
            size_t llen = leol ? (size_t)(leol - loc) : strlen(loc);
            if (!parse_redirect_url(loc, llen, &cur_https, cur_host, sizeof(cur_host),
                                    &cur_port, cur_pq, 4096))
            {
                put_err(err_buf, err_cap, "Bad redirect URL");
                final_rc = -1;
                break;
            }
            continue;
        }

        if (status_code < 200 || status_code >= 300)
        {
            if (hdr_fn)
                hdr_fn(sink_ctx, status_code, hbuf, hdr_len, false, 0);
            conn_close(&conn);
            put_err(err_buf, err_cap, "HTTP %d", status_code);
            final_rc = -1;
            break;
        }

        bool is_chunked = false;
        const char *te = find_hdr_ci(hbuf, hdr_len, "Transfer-Encoding");
        if (te)
        {
            const char *te_end = memchr(te, '\n', (size_t)((hbuf + hdr_len) - te));
            size_t tlen = te_end ? (size_t)(te_end - te) : strlen(te);
            for (size_t k = 0; k + 7 <= tlen; k++)
            {
                if (strncasecmp(te + k, "chunked", 7) == 0)
                {
                    is_chunked = true;
                    break;
                }
            }
        }

        int64_t content_length = -1;
        if (!is_chunked)
        {
            const char *cl = find_hdr_ci(hbuf, hdr_len, "Content-Length");
            if (cl)
                content_length = strtoll(cl, NULL, 10);
        }

        if (hdr_fn && !hdr_fn(sink_ctx, status_code, hbuf, hdr_len, is_chunked, content_length))
        {
            conn_close(&conn);
            final_rc = 0;
            break;
        }

        if (is_head || status_code == 204 || content_length == 0)
        {
            conn_close(&conn);
            final_rc = 0;
            break;
        }

        /* Seed `rx` with any body bytes already read after `\r\n\r\n` */
        size_t rx_len = hgot - body_off;
        if (rx_len > 0)
            memcpy(rx, hbuf + body_off, rx_len);

        if (!is_chunked)
        {
            int64_t received = 0;
            bool ok = true;
            if (rx_len > 0)
            {
                size_t emit = rx_len;
                if (content_length >= 0 && (int64_t)emit > content_length)
                    emit = (size_t)content_length;
                if (!sink_fn(sink_ctx, rx, emit))
                    ok = false;
                received += (int64_t)emit;
            }

            while (ok && (content_length < 0 || received < content_length))
            {
                ssize_t n = conn_read_some(&conn, rx, 65536, idle_ms,
                                           cancel_cb, cancel_ctx);
                if (n == -2)
                {
                    conn_close(&conn);
                    put_err(err_buf, err_cap, "Cancelled");
                    free(cur_pq);
                    free(rx);
                    return -2;
                }
                if (n < 0)
                {
                    ok = false;
                    put_err(err_buf, err_cap, "Network timeout");
                    break;
                }
                if (n == 0)
                {
                    if (content_length >= 0 && received < content_length)
                    {
                        ok = false;
                        put_err(err_buf, err_cap, "Incomplete stream");
                    }
                    break;
                }
                size_t emit = (size_t)n;
                if (content_length >= 0 && received + (int64_t)emit > content_length)
                    emit = (size_t)(content_length - received);
                if (!sink_fn(sink_ctx, rx, emit))
                {
                    ok = false;
                    put_err(err_buf, err_cap, "Disk write error");
                    break;
                }
                received += (int64_t)emit;
            }

            conn_close(&conn);
            final_rc = ok ? 0 : -1;
            break;
        }
        else
        {
            /* Streaming RFC 9112 chunked transfer decoder */
            enum { CH_SIZE, CH_DATA, CH_CRLF, CH_DONE } st = CH_SIZE;
            size_t chunk_rem = 0;
            size_t rx_pos = 0;
            bool ok = true;

            while (ok && st != CH_DONE)
            {
                if (st == CH_SIZE)
                {
                    /* Need a complete `\r\n` line in rx[rx_pos..rx_len] */
                    size_t avail = rx_len - rx_pos;
                    uint8_t *lf = avail > 0 ? memchr(rx + rx_pos, '\n', avail) : NULL;
                    if (!lf)
                    {
                        if (rx_pos > 0)
                        {
                            memmove(rx, rx + rx_pos, avail);
                            rx_len = avail;
                            rx_pos = 0;
                        }
                        if (rx_len >= 65536)
                        {
                            ok = false;
                            put_err(err_buf, err_cap, "Bad chunk header");
                            break;
                        }
                        ssize_t n = conn_read_some(&conn, rx + rx_len, 65536 - rx_len,
                                                   idle_ms, cancel_cb, cancel_ctx);
                        if (n == -2)
                        {
                            conn_close(&conn);
                            put_err(err_buf, err_cap, "Cancelled");
                            free(cur_pq);
                            free(rx);
                            return -2;
                        }
                        if (n <= 0)
                        {
                            ok = false;
                            put_err(err_buf, err_cap, "Truncated chunk stream");
                            break;
                        }
                        rx_len += (size_t)n;
                        continue;
                    }

                    char hexbuf[32] = {0};
                    size_t line_len = (size_t)(lf - (rx + rx_pos));
                    size_t hi = 0;
                    for (size_t k = 0; k < line_len && hi + 1 < sizeof(hexbuf); k++)
                    {
                        char ch = (char)rx[rx_pos + k];
                        if (ch == ';' || ch == '\r' || ch == ' ')
                            break;
                        if (!isxdigit((unsigned char)ch))
                        {
                            ok = false;
                            break;
                        }
                        hexbuf[hi++] = ch;
                    }
                    if (!ok || hi == 0)
                    {
                        ok = false;
                        put_err(err_buf, err_cap, "Malformed chunk size");
                        break;
                    }
                    chunk_rem = (size_t)strtoull(hexbuf, NULL, 16);
                    rx_pos = (size_t)((lf - rx) + 1);
                    st = (chunk_rem == 0) ? CH_DONE : CH_DATA;
                }
                else if (st == CH_DATA)
                {
                    size_t avail = rx_len - rx_pos;
                    if (avail == 0)
                    {
                        rx_pos = rx_len = 0;
                        ssize_t n = conn_read_some(&conn, rx, 65536, idle_ms,
                                                   cancel_cb, cancel_ctx);
                        if (n == -2)
                        {
                            conn_close(&conn);
                            put_err(err_buf, err_cap, "Cancelled");
                            free(cur_pq);
                            free(rx);
                            return -2;
                        }
                        if (n <= 0)
                        {
                            ok = false;
                            put_err(err_buf, err_cap, "Truncated chunk data");
                            break;
                        }
                        rx_len = (size_t)n;
                        avail = rx_len;
                    }

                    size_t take = (avail < chunk_rem) ? avail : chunk_rem;
                    if (!sink_fn(sink_ctx, rx + rx_pos, take))
                    {
                        ok = false;
                        put_err(err_buf, err_cap, "Disk write error");
                        break;
                    }
                    rx_pos += take;
                    chunk_rem -= take;
                    if (chunk_rem == 0)
                        st = CH_CRLF;
                }
                else if (st == CH_CRLF)
                {
                    while (rx_len - rx_pos < 2)
                    {
                        size_t avail = rx_len - rx_pos;
                        if (rx_pos > 0)
                        {
                            memmove(rx, rx + rx_pos, avail);
                            rx_len = avail;
                            rx_pos = 0;
                        }
                        ssize_t n = conn_read_some(&conn, rx + rx_len, 65536 - rx_len,
                                                   idle_ms, cancel_cb, cancel_ctx);
                        if (n == -2)
                        {
                            conn_close(&conn);
                            put_err(err_buf, err_cap, "Cancelled");
                            free(cur_pq);
                            free(rx);
                            return -2;
                        }
                        if (n <= 0)
                        {
                            ok = false;
                            break;
                        }
                        rx_len += (size_t)n;
                    }
                    if (!ok) break;
                    if (rx[rx_pos] == '\r' && rx[rx_pos + 1] == '\n')
                        rx_pos += 2;
                    else if (rx[rx_pos] == '\n')
                        rx_pos += 1;
                    st = CH_SIZE;
                }
            }

            conn_close(&conn);
            final_rc = ok ? 0 : -1;
            break;
        }
    }

    free(cur_pq);
    free(rx);
    return final_rc;
}

/* ---- Public Entry Points (Memory Sink & 256 KB Buffered File Sink) ------ */

struct mem_sink {
    uint8_t *buf;
    size_t   len;
    size_t   cap;
};

static bool mem_sink_cb(void *ctx, const uint8_t *data, size_t len)
{
    struct mem_sink *m = ctx;
    if (len == 0) return true;
    if (m->len + len + 1 > NAV_MAX_MEM_BYTES)
        return false;
    if (m->len + len + 1 > m->cap)
    {
        size_t ncap = m->cap ? m->cap * 2 : 16384;
        while (ncap < m->len + len + 1)
            ncap *= 2;
        uint8_t *nb = realloc(m->buf, ncap);
        if (!nb) return false;
        m->buf = nb;
        m->cap = ncap;
    }
    memcpy(m->buf + m->len, data, len);
    m->len += len;
    m->buf[m->len] = 0;
    return true;
}

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
    size_t err_cap)
{
    if (!out_data || !out_len || !host || !path_and_query)
        return -1;
    *out_data = NULL;
    *out_len = 0;

    struct mem_sink ms = { NULL, 0, 0 };
    int rc = perform_http_stream_ex(
        method, is_https != 0, host, port, path_and_query, extra_headers,
        body, body_len, allow_self_signed != 0, timeout_ms, out_status,
        NULL, mem_sink_cb, &ms, NULL, NULL, err_buf, err_cap);
    if (rc != 0)
    {
        free(ms.buf);
        return rc;
    }
    if (!ms.buf)
    {
        ms.buf = calloc(1, 1);
        if (!ms.buf) return -1;
    }
    *out_data = ms.buf;
    *out_len = ms.len;
    return 0;
}

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
    size_t err_cap)
{
    return retrohh_http_request(
        "GET", is_https, host, port, path_and_query, NULL, NULL, 0,
        allow_self_signed, timeout_ms, NULL, out_data, out_len, err_buf, err_cap);
}

void retrohh_nav_http_free(uint8_t *ptr)
{
    free(ptr);
}

struct file_sink {
    int      fd;
    uint8_t *wbuf;
    size_t   wused;
};

static bool file_sink_flush(struct file_sink *fs)
{
    size_t off = 0;
    while (off < fs->wused)
    {
        ssize_t w = write(fs->fd, fs->wbuf + off, fs->wused - off);
        if (w > 0) { off += (size_t)w; continue; }
        if (w < 0 && errno == EINTR) continue;
        return false;
    }
    fs->wused = 0;
    return true;
}

static bool file_sink_cb(void *ctx, const uint8_t *data, size_t len)
{
    struct file_sink *fs = ctx;
    size_t pos = 0;
    while (pos < len)
    {
        size_t space = NAV_SD_WRITE_BUF_SZ - fs->wused;
        size_t take = (len - pos < space) ? (len - pos) : space;
        memcpy(fs->wbuf + fs->wused, data + pos, take);
        fs->wused += take;
        pos += take;
        if (fs->wused == NAV_SD_WRITE_BUF_SZ)
        {
            if (!file_sink_flush(fs))
                return false;
        }
    }
    return true;
}

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
    size_t err_cap)
{
    if (!host || !path_and_query || !dest_path)
        return -1;

    int fd = open(dest_path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0666);
    if (fd < 0)
    {
        put_err(err_buf, err_cap, "Cannot create cache file");
        return -1;
    }

    uint8_t *wbuf = malloc(NAV_SD_WRITE_BUF_SZ);
    if (!wbuf)
    {
        close(fd);
        unlink(dest_path);
        put_err(err_buf, err_cap, "Out of memory");
        return -1;
    }

    struct file_sink fs = { fd, wbuf, 0 };
    int rc = perform_http_stream_ex(
        "GET", is_https != 0, host, port, path_and_query, extra_headers,
        NULL, 0, allow_self_signed != 0, timeout_ms, NULL, NULL,
        file_sink_cb, &fs, cancel_cb, cancel_ctx, err_buf, err_cap);

    if (rc == 0 && !file_sink_flush(&fs))
    {
        put_err(err_buf, err_cap, "Disk write error");
        rc = -1;
    }

    free(wbuf);
    close(fd);
    if (rc != 0)
        unlink(dest_path);
    return rc;
}

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
    size_t err_cap)
{
    return retrohh_http_download_ex(
        is_https, host, port, path_and_query, NULL, allow_self_signed,
        dest_path, timeout_ms, cancel_cb, cancel_ctx, err_buf, err_cap);
}

/* ---- Local Loopback Video Stream Proxy (`http://127.0.0.1:<port>/...`) --- */

static pthread_mutex_t s_proxy_mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t  s_proxy_cv  = PTHREAD_COND_INITIALIZER;
static bool            s_proxy_running = false;
static uint32_t        s_proxy_gen = 0;
static int             s_proxy_listen_fd = -1;
static pthread_t       s_proxy_accept_tid;
static int             s_proxy_active_clients = 0;

static struct {
    bool     is_https;
    char     host[256];
    uint16_t port;
    char     pq[3072];
    char     extra_headers[1024];
    bool     allow_self_signed;
} s_proxy_cfg;

struct proxy_client_ctx {
    int      client_fd;
    uint32_t gen;
    bool     is_head;
};

static uint8_t proxy_cancel_cb(void *vctx)
{
    struct proxy_client_ctx *pc = vctx;
    pthread_mutex_lock(&s_proxy_mtx);
    bool stop = (!s_proxy_running || pc->gen != s_proxy_gen);
    pthread_mutex_unlock(&s_proxy_mtx);
    return stop ? 1 : 0;
}

static bool proxy_write_client(struct proxy_client_ctx *pc, const uint8_t *data, size_t len)
{
    size_t sent = 0;
    while (sent < len)
    {
        if (proxy_cancel_cb(pc))
            return false;
#ifdef MSG_NOSIGNAL
        ssize_t w = send(pc->client_fd, data + sent, len - sent, MSG_NOSIGNAL);
#else
        ssize_t w = send(pc->client_fd, data + sent, len - sent, 0);
#endif
        if (w > 0)
        {
            sent += (size_t)w;
            continue;
        }
        if (w < 0 && errno == EINTR)
            continue;
        if (w < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
        {
            int wr = wait_fd_cancellable(pc->client_fd, true, mono_ms() + 10000,
                                         proxy_cancel_cb, pc);
            if (wr > 0)
                continue;
            return false;
        }
        return false;
    }
    return true;
}

static void copy_hdr_line_val(const char *hdrs, size_t hdr_len, const char *name,
                              char *out, size_t out_cap)
{
    out[0] = '\0';
    const char *v = find_hdr_ci(hdrs, hdr_len, name);
    if (!v)
        return;
    const char *eol = memchr(v, '\n', (size_t)((hdrs + hdr_len) - v));
    size_t len = eol ? (size_t)(eol - v) : strlen(v);
    while (len > 0 && (v[len - 1] == '\r' || v[len - 1] == '\n' || v[len - 1] == ' '))
        len--;
    if (len >= out_cap)
        len = out_cap - 1;
    memcpy(out, v, len);
    out[len] = '\0';
}

static bool proxy_resp_hdr_cb(void *vctx, int status_code, const char *hdrs,
                              size_t hdr_len, bool is_chunked, int64_t content_length)
{
    struct proxy_client_ctx *pc = vctx;
    char ctype[128], crange[128], aranges[64];
    copy_hdr_line_val(hdrs, hdr_len, "Content-Type", ctype, sizeof(ctype));
    copy_hdr_line_val(hdrs, hdr_len, "Content-Range", crange, sizeof(crange));
    copy_hdr_line_val(hdrs, hdr_len, "Accept-Ranges", aranges, sizeof(aranges));
    if (!ctype[0])
        snprintf(ctype, sizeof(ctype), "application/octet-stream");

    const char *reason = (status_code == 206) ? "Partial Content"
                       : (status_code >= 200 && status_code < 300) ? "OK"
                       : "Error";
    char out[1024];
    int n = snprintf(out, sizeof(out),
                     "HTTP/1.1 %d %s\r\n"
                     "Content-Type: %s\r\n",
                     status_code > 0 ? status_code : 502, reason, ctype);
    if (!is_chunked && content_length >= 0 && (size_t)n < sizeof(out))
        n += snprintf(out + n, sizeof(out) - (size_t)n,
                      "Content-Length: %lld\r\n", (long long)content_length);
    if (crange[0] && (size_t)n < sizeof(out))
        n += snprintf(out + n, sizeof(out) - (size_t)n,
                      "Content-Range: %s\r\n", crange);
    if ((aranges[0] || status_code == 206 || (!is_chunked && content_length > 0)) &&
        (size_t)n < sizeof(out))
        n += snprintf(out + n, sizeof(out) - (size_t)n, "Accept-Ranges: bytes\r\n");
    if ((size_t)n < sizeof(out))
        n += snprintf(out + n, sizeof(out) - (size_t)n, "Connection: close\r\n\r\n");

    if (n <= 0 || (size_t)n >= sizeof(out))
        return false;
    if (!proxy_write_client(pc, (const uint8_t *)out, (size_t)n))
        return false;
    return !pc->is_head && (status_code >= 200 && status_code < 300);
}

static bool proxy_body_sink_cb(void *vctx, const uint8_t *data, size_t len)
{
    struct proxy_client_ctx *pc = vctx;
    return proxy_write_client(pc, data, len);
}

static void *proxy_client_worker(void *arg)
{
    int client_fd = (int)(intptr_t)arg;
    set_nonblock_cloexec(client_fd);
    int one = 1;
    setsockopt(client_fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));

    pthread_mutex_lock(&s_proxy_mtx);
    uint32_t gen = s_proxy_gen;
    bool is_https = s_proxy_cfg.is_https;
    bool allow_self_signed = s_proxy_cfg.allow_self_signed;
    uint16_t port = s_proxy_cfg.port;
    char host[256], pq[3072], base_hdrs[1024];
    snprintf(host, sizeof(host), "%s", s_proxy_cfg.host);
    snprintf(pq, sizeof(pq), "%s", s_proxy_cfg.pq);
    snprintf(base_hdrs, sizeof(base_hdrs), "%s", s_proxy_cfg.extra_headers);
    pthread_mutex_unlock(&s_proxy_mtx);

    struct proxy_client_ctx pc = {
        .client_fd = client_fd,
        .gen       = gen,
        .is_head   = false,
    };

    /* Read client HTTP request headers from tplayerslave / ffprobe */
    char req[4096];
    size_t rgot = 0;
    int64_t rdl = mono_ms() + 4000;
    while (rgot + 1 < sizeof(req))
    {
        ssize_t r = recv(client_fd, req + rgot, sizeof(req) - 1 - rgot, 0);
        if (r > 0)
        {
            rgot += (size_t)r;
            req[rgot] = '\0';
            if (strstr(req, "\r\n\r\n") || strstr(req, "\n\n"))
                break;
            continue;
        }
        if (r == 0) break;
        if (errno == EINTR) continue;
        if (errno == EAGAIN || errno == EWOULDBLOCK)
        {
            if (wait_fd_cancellable(client_fd, false, rdl, proxy_cancel_cb, &pc) > 0)
                continue;
        }
        break;
    }

    if (rgot > 4)
    {
        pc.is_head = (strncasecmp(req, "HEAD ", 5) == 0);
        char range_val[128];
        copy_hdr_line_val(req, rgot, "Range", range_val, sizeof(range_val));

        char merged_hdrs[1280];
        if (range_val[0])
            snprintf(merged_hdrs, sizeof(merged_hdrs), "%sRange: %s\r\n", base_hdrs, range_val);
        else
            snprintf(merged_hdrs, sizeof(merged_hdrs), "%s", base_hdrs);

        char err_unused[64];
        perform_http_stream_ex(
            pc.is_head ? "HEAD" : "GET",
            is_https, host, port, pq, merged_hdrs, NULL, 0,
            allow_self_signed, 25000, NULL,
            proxy_resp_hdr_cb, proxy_body_sink_cb, &pc,
            proxy_cancel_cb, &pc, err_unused, sizeof(err_unused));
    }

    shutdown(client_fd, SHUT_RDWR);
    close(client_fd);

    pthread_mutex_lock(&s_proxy_mtx);
    if (--s_proxy_active_clients <= 0)
        pthread_cond_broadcast(&s_proxy_cv);
    pthread_mutex_unlock(&s_proxy_mtx);
    return NULL;
}

static void *proxy_accept_loop(void *arg)
{
    (void)arg;
    for (;;)
    {
        pthread_mutex_lock(&s_proxy_mtx);
        bool running = s_proxy_running;
        int lfd = s_proxy_listen_fd;
        pthread_mutex_unlock(&s_proxy_mtx);
        if (!running || lfd < 0)
            break;

        fd_set fds;
        FD_ZERO(&fds);
        FD_SET(lfd, &fds);
        struct timeval tv = { .tv_sec = 0, .tv_usec = 80000 };
        int rc = select(lfd + 1, &fds, NULL, NULL, &tv);
        if (rc <= 0)
            continue;

        int cfd = accept(lfd, NULL, NULL);
        if (cfd < 0)
            continue;

        pthread_mutex_lock(&s_proxy_mtx);
        if (!s_proxy_running)
        {
            pthread_mutex_unlock(&s_proxy_mtx);
            close(cfd);
            break;
        }
        s_proxy_active_clients++;
        pthread_mutex_unlock(&s_proxy_mtx);

        pthread_t tid;
        pthread_attr_t attr;
        pthread_attr_init(&attr);
        pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
        if (pthread_create(&tid, &attr, proxy_client_worker, (void *)(intptr_t)cfd) != 0)
        {
            close(cfd);
            pthread_mutex_lock(&s_proxy_mtx);
            if (--s_proxy_active_clients <= 0)
                pthread_cond_broadcast(&s_proxy_cv);
            pthread_mutex_unlock(&s_proxy_mtx);
        }
        pthread_attr_destroy(&attr);
    }
    return NULL;
}

int32_t retrohh_http_proxy_start(
    uint8_t is_https,
    const char *host,
    uint16_t port,
    const char *path_and_query,
    const char *extra_headers,
    uint8_t allow_self_signed,
    uint16_t *out_local_port,
    char *err_buf,
    size_t err_cap)
{
    if (!host || !path_and_query || !out_local_port)
        return -1;
    init_ssl_once();
    if (!ensure_network_ready(err_buf, err_cap))
        return -1;

    retrohh_http_proxy_stop();

    int lfd = socket(AF_INET, SOCK_STREAM, 0);
    if (lfd < 0)
    {
        put_err(err_buf, err_cap, "Proxy socket failed");
        return -1;
    }
    set_nonblock_cloexec(lfd);
    int one = 1;
    setsockopt(lfd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));

    struct sockaddr_in sin;
    memset(&sin, 0, sizeof(sin));
    sin.sin_family = AF_INET;
    sin.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    sin.sin_port = htons(0);
    if (bind(lfd, (struct sockaddr *)&sin, sizeof(sin)) != 0 || listen(lfd, 8) != 0)
    {
        close(lfd);
        put_err(err_buf, err_cap, "Proxy bind failed");
        return -1;
    }

    socklen_t slen = sizeof(sin);
    if (getsockname(lfd, (struct sockaddr *)&sin, &slen) != 0)
    {
        close(lfd);
        put_err(err_buf, err_cap, "Proxy getsockname failed");
        return -1;
    }
    *out_local_port = ntohs(sin.sin_port);

    pthread_mutex_lock(&s_proxy_mtx);
    s_proxy_cfg.is_https = (is_https != 0);
    snprintf(s_proxy_cfg.host, sizeof(s_proxy_cfg.host), "%s", host);
    s_proxy_cfg.port = port;
    snprintf(s_proxy_cfg.pq, sizeof(s_proxy_cfg.pq), "%s", path_and_query);
    snprintf(s_proxy_cfg.extra_headers, sizeof(s_proxy_cfg.extra_headers), "%s",
             extra_headers ? extra_headers : "");
    s_proxy_cfg.allow_self_signed = (allow_self_signed != 0);
    s_proxy_listen_fd = lfd;
    s_proxy_running = true;
    s_proxy_gen++;
    pthread_mutex_unlock(&s_proxy_mtx);

    if (pthread_create(&s_proxy_accept_tid, NULL, proxy_accept_loop, NULL) != 0)
    {
        pthread_mutex_lock(&s_proxy_mtx);
        s_proxy_running = false;
        s_proxy_listen_fd = -1;
        pthread_mutex_unlock(&s_proxy_mtx);
        close(lfd);
        put_err(err_buf, err_cap, "Proxy thread failed");
        return -1;
    }
    return 0;
}

void retrohh_http_proxy_set_path(const char *path_and_query)
{
    if (!path_and_query)
        return;
    pthread_mutex_lock(&s_proxy_mtx);
    snprintf(s_proxy_cfg.pq, sizeof(s_proxy_cfg.pq), "%s", path_and_query);
    s_proxy_gen++; /* aborts any prior in-flight upstream transcode stream */
    pthread_mutex_unlock(&s_proxy_mtx);
}

void retrohh_http_proxy_stop(void)
{
    pthread_mutex_lock(&s_proxy_mtx);
    if (!s_proxy_running)
    {
        pthread_mutex_unlock(&s_proxy_mtx);
        return;
    }
    s_proxy_running = false;
    s_proxy_gen++;
    int lfd = s_proxy_listen_fd;
    s_proxy_listen_fd = -1;
    pthread_mutex_unlock(&s_proxy_mtx);

    if (lfd >= 0)
    {
        shutdown(lfd, SHUT_RDWR);
        close(lfd);
    }
    pthread_join(s_proxy_accept_tid, NULL);

    pthread_mutex_lock(&s_proxy_mtx);
    while (s_proxy_active_clients > 0)
        pthread_cond_wait(&s_proxy_cv, &s_proxy_mtx);
    pthread_mutex_unlock(&s_proxy_mtx);
}
