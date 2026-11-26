/***************************************************************************
 * UIPod: NextUI "Minimize to Background" Daemon & Unix IPC (`/tmp/uipod.sock`).
 ****************************************************************************/
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "config.h"

#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
#include <dirent.h>
#include <fcntl.h>
#include <linux/input.h>
#include <poll.h>
#include <signal.h>
#include <sys/inotify.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#endif

#include "action.h"
#include "appevents.h"
#include "audio.h"
#include "button.h"
#include "gui/wps.h"
#include "kernel.h"
#include "misc.h"
#include "pcm_sw_volume.h"
#include "playlist.h"
#include "power.h"
#include "settings.h"
#include "status.h"
#include "system.h"
#include "tree.h"
#include "uipod_daemon.h"
#include "uipod_downloads.h"
#include "uipod_lastfm.h"
#include "uipod_library.h"
#include "uipod_listenbrainz.h"
#include "uipod_rs_ui.h"
#include "uipod_transition.h"

#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
#include "backlight.h"
#include "backlight-target.h"
#include "button-sdl.h"
#include "button-target.h"
#include "power-target.h"
#include "system-sdl.h"
#include "trimpod_alsa.h"
#include "window-sdl.h"

extern void audiohw_close(void);

/* Ensure Rockbox VFS macros from any transitive header never redirect real
 * Linux host `/tmp`, `/proc`, or `/dev/input` syscalls! */
#undef open
#undef close
#undef read
#undef write
#undef unlink

#define UIPOD_SOCK_PATH          "/tmp/uipod.sock"
#define UIPOD_PID_PATH           "/tmp/uipod.pid"
#define UIPOD_BIND_MOUNT_PATH    "/tmp/uipod"
#define UIPOD_IDLE_EXIT_MS       (15ULL * 60ULL * 1000ULL) /* 15 minutes */
#define UIPOD_RELEASE_WAIT_MS    250U
#define UIPOD_REATTACH_GUARD_MS  200U
#define UIPOD_BG_CHORD_SETTLE_MS 400ULL
#define UIPOD_BG_VOL_REPEAT_MS   150ULL
#define UIPOD_MAX_EVDEV_FDS      10

static bool                  s_is_daemon = false;
static int                   s_listen_fd = -1;
static int                   s_client_fd = -1;
static volatile sig_atomic_t s_sig_quit  = 0;
static volatile bool         s_min_req   = false;
static bool                  s_in_bg     = false;

static void daemon_sig_handler(int sig)
{
    (void)sig;
    s_sig_quit = 1;
}

static uint64_t monotonic_ms(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) == 0)
        return (uint64_t)ts.tv_sec * 1000ULL + (uint64_t)(ts.tv_nsec / 1000000L);
    return (uint64_t)SDL_GetTicks();
}

static void sleep_short_ms(unsigned int ms)
{
    struct timespec req;
    req.tv_sec  = (time_t)(ms / 1000U);
    req.tv_nsec = (long)((ms % 1000U) * 1000000UL);
    nanosleep(&req, NULL);
}

static bool write_all_str(int fd, const char *s)
{
    if (fd < 0 || !s)
        return false;
    size_t rem = strlen(s);
    const char *p = s;
    while (rem > 0)
    {
        ssize_t n = send(fd, p, rem, MSG_NOSIGNAL);
        if (n < 0)
        {
            if (errno == EINTR)
                continue;
            return false;
        }
        p   += (size_t)n;
        rem -= (size_t)n;
    }
    return true;
}

static bool is_pid_live_uipod(pid_t pid)
{
    if (pid <= 1 || kill(pid, 0) != 0)
        return false;
    char path[64], comm[64];
    snprintf(path, sizeof(path), "/proc/%d/comm", (int)pid);
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return false;
    ssize_t n = read(fd, comm, sizeof(comm) - 1);
    close(fd);
    if (n <= 0)
        return false;
    comm[n] = '\0';
    return strstr(comm, "uipod") != NULL;
}

static pid_t read_daemon_pid(void)
{
    int fd = open(UIPOD_PID_PATH, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    char buf[32];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0)
        return -1;
    buf[n] = '\0';
    long v = strtol(buf, NULL, 10);
    return (v > 1) ? (pid_t)v : -1;
}

static void write_daemon_pid(void)
{
    int fd = open(UIPOD_PID_PATH, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
    if (fd < 0)
        return;
    char buf[32];
    int len = snprintf(buf, sizeof(buf), "%d\n", (int)getpid());
    if (len > 0)
    {
        ssize_t wr = write(fd, buf, (size_t)len);
        (void)wr;
    }
    close(fd);
}

static void cleanup_daemon_files(void)
{
    if (s_listen_fd >= 0)
    {
        close(s_listen_fd);
        s_listen_fd = -1;
    }
    unlink(UIPOD_SOCK_PATH);
    unlink(UIPOD_PID_PATH);
}

static int connect_daemon_sock(void)
{
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0)
        return -1;
    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, UIPOD_SOCK_PATH, sizeof(addr.sun_path) - 1);
    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0)
    {
        close(fd);
        return -1;
    }
    return fd;
}

static void close_inherited_fds_from(int min_fd)
{
    DIR *d = opendir("/proc/self/fd");
    if (d)
    {
        int dir_fd = dirfd(d);
        struct dirent *de;
        int fds[256];
        int count = 0;
        while ((de = readdir(d)) != NULL)
        {
            if (de->d_name[0] < '0' || de->d_name[0] > '9')
                continue;
            int fd = atoi(de->d_name);
            if (fd >= min_fd && fd != dir_fd && count < 256)
                fds[count++] = fd;
        }
        closedir(d);
        for (int i = 0; i < count; i++)
            close(fds[i]);
        return;
    }

    long max_fd = sysconf(_SC_OPEN_MAX);
    if (max_fd < 64 || max_fd > 4096)
        max_fd = 1024;
    for (int fd = min_fd; fd < (int)max_fd; fd++)
        close(fd);
}

static bool spawn_detached_daemon(int argc, char *argv[])
{
    pid_t pid = fork();
    if (pid < 0)
        return false;

    if (pid == 0)
    {
        /* Intermediate child: detach session and double-fork so the daemon is
         * reparented to PID 1 (`init`) and can never become a zombie. */
        (void)setsid();
        signal(SIGHUP, SIG_IGN);

        pid_t gpid = fork();
        if (gpid < 0)
            _exit(1);
        if (gpid > 0)
            _exit(0);

        int devnull = open("/dev/null", O_RDONLY);
        if (devnull >= 0)
        {
            (void)dup2(devnull, STDIN_FILENO);
            if (devnull > STDERR_FILENO)
                close(devnull);
        }

        char log_path[512];
        const char *pak_dir = getenv("PAK_DIR");
        if (pak_dir && pak_dir[0])
            snprintf(log_path, sizeof(log_path), "%s/uipod.log", pak_dir);
        else
            snprintf(log_path, sizeof(log_path), "/tmp/uipod.log");

        int log_fd = open(log_path, O_WRONLY | O_CREAT | O_APPEND, 0644);
        if (log_fd < 0)
            log_fd = open("/tmp/uipod.log", O_WRONLY | O_CREAT | O_APPEND, 0644);
        if (log_fd >= 0)
        {
            (void)dup2(log_fd, STDOUT_FILENO);
            (void)dup2(log_fd, STDERR_FILENO);
            if (log_fd > STDERR_FILENO)
                close(log_fd);
        }

        /* Close ALL inherited descriptors (`fd >= 3`) so NextUI's `system()`
         * launcher never blocks waiting on an open stdout/stderr/event pipe! */
        close_inherited_fds_from(3);

        char **new_argv = calloc((size_t)argc + 2U, sizeof(char *));
        if (!new_argv)
            _exit(1);
        int dst = 0;
        new_argv[dst++] = argv[0];
        new_argv[dst++] = "--daemon";
        for (int i = 1; i < argc; i++)
        {
            if (strcmp(argv[i], "--client") == 0 || strcmp(argv[i], "--daemon") == 0)
                continue;
            new_argv[dst++] = argv[i];
        }
        new_argv[dst] = NULL;

        execv(argv[0], new_argv);
        _exit(127);
    }

    int status = 0;
    (void)waitpid(pid, &status, 0);
    return true;
}

static volatile sig_atomic_t s_client_term_sig = 0;

static void client_sig_handler(int sig)
{
    s_client_term_sig = sig;
}

static void run_thin_client_and_exit(int argc, char *argv[])
{
    signal(SIGPIPE, SIG_IGN);
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = client_sig_handler;
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT,  &sa, NULL);
    sigaction(SIGHUP,  &sa, NULL);

    int fd = connect_daemon_sock();
    if (fd < 0)
    {
        pid_t old_pid = read_daemon_pid();
        if (!is_pid_live_uipod(old_pid))
        {
            unlink(UIPOD_SOCK_PATH);
            unlink(UIPOD_PID_PATH);
        }

        if (!spawn_detached_daemon(argc, argv))
            _exit(1);

        /* Poll up to 2500 ms until `/tmp/uipod.sock` accepts connections */
        for (int attempt = 0; attempt < 166; attempt++)
        {
            sleep_short_ms(15);
            fd = connect_daemon_sock();
            if (fd >= 0)
                break;
        }
        if (fd < 0)
            _exit(1);
    }

    if (!write_all_str(fd, "ATTACH_UI\n"))
    {
        close(fd);
        _exit(1);
    }

    /* Block until daemon replies `DETACHED\n` (minimize) or `EXITED\n` / EOF */
    char buf[128];
    size_t used = 0;
    for (;;)
    {
        if (s_client_term_sig != 0)
        {
            (void)write_all_str(fd, "QUIT\n");
            break;
        }
        struct pollfd pfd = { .fd = fd, .events = POLLIN };
        int pr = poll(&pfd, 1, 200);
        if (pr < 0)
        {
            if (errno == EINTR)
                continue;
            break;
        }
        if (pr == 0)
            continue;

        ssize_t n = read(fd, buf + used, sizeof(buf) - 1 - used);
        if (n <= 0)
        {
            if (n < 0 && errno == EINTR)
                continue;
            break;
        }
        used += (size_t)n;
        buf[used] = '\0';
        if (strstr(buf, "DETACHED") != NULL)
            break;
        if (strstr(buf, "EXITED") != NULL)
        {
            /* Wait up to 200 ms for daemon PID to finish unlinking files */
            for (int w = 0; w < 20; w++)
            {
                pid_t dp = read_daemon_pid();
                if (dp <= 1 || kill(dp, 0) != 0)
                    break;
                sleep_short_ms(10);
            }
            break;
        }
        if (used >= sizeof(buf) - 2)
            used = 0;
    }

    close(fd);
    _exit(0);
}

static bool setup_daemon_listener(void)
{
    pid_t existing = read_daemon_pid();
    if (existing > 1 && existing != getpid() && is_pid_live_uipod(existing))
    {
        /* Another daemon instance is already live: exit cleanly */
        _exit(0);
    }

    unlink(UIPOD_SOCK_PATH);
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    if (fd < 0)
        return false;

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, UIPOD_SOCK_PATH, sizeof(addr.sun_path) - 1);

    if (bind(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0 ||
        listen(fd, 4) != 0)
    {
        close(fd);
        return false;
    }

    s_listen_fd = fd;
    write_daemon_pid();

    /* Accept the initial cold-launch `--client` connection within up to 500 ms */
    for (int i = 0; i < 50; i++)
    {
        struct pollfd pfd = { .fd = s_listen_fd, .events = POLLIN };
        if (poll(&pfd, 1, 10) > 0)
        {
            int cfd = accept(s_listen_fd, NULL, NULL);
            if (cfd >= 0)
            {
                fcntl(cfd, F_SETFD, FD_CLOEXEC);
                fcntl(cfd, F_SETFL, O_NONBLOCK);
                char cmd[64];
                sleep_short_ms(5);
                ssize_t r = read(cfd, cmd, sizeof(cmd) - 1);
                (void)r;
                s_client_fd = cfd;
                break;
            }
        }
    }
    return true;
}

/* ---- Non-Exclusive Background `evdev` Hotkey Watcher -------------------- */

struct bg_evdev_watcher
{
    int      fds[UIPOD_MAX_EVDEV_FDS];
    int      count;
    int      inotify_fd;
    bool     menu_held;
    bool     start_held;
    bool     l2_held;
    bool     r2_held;
    bool     up_held;
    bool     down_held;
    bool     chord_used;
    uint64_t enter_ms;
    uint64_t next_vol_repeat_ms;
};

static void bg_evdev_add_node(struct bg_evdev_watcher *w, const char *name)
{
    if (strncmp(name, "event", 5) != 0 || w->count >= UIPOD_MAX_EVDEV_FDS)
        return;
    char path[64];
    snprintf(path, sizeof(path), "/dev/input/%s", name);
    /* CRITICAL: O_RDONLY | O_NONBLOCK | O_CLOEXEC ONLY — NEVER EVIOCGRAB! */
    int fd = open(path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0)
        return;
    w->fds[w->count++] = fd;
}

static void bg_evdev_open(struct bg_evdev_watcher *w)
{
    memset(w, 0, sizeof(*w));
    for (int i = 0; i < UIPOD_MAX_EVDEV_FDS; i++)
        w->fds[i] = -1;
    w->inotify_fd = -1;
    w->enter_ms = monotonic_ms();

    DIR *d = opendir("/dev/input");
    if (d)
    {
        struct dirent *de;
        while ((de = readdir(d)) != NULL)
            bg_evdev_add_node(w, de->d_name);
        closedir(d);
    }

    int infd = inotify_init1(IN_NONBLOCK | IN_CLOEXEC);
    if (infd >= 0)
    {
        if (inotify_add_watch(infd, "/dev/input", IN_CREATE) < 0)
        {
            close(infd);
            infd = -1;
        }
    }
    w->inotify_fd = infd;
}

static void bg_evdev_close(struct bg_evdev_watcher *w)
{
    for (int i = 0; i < w->count; i++)
    {
        if (w->fds[i] >= 0)
        {
            close(w->fds[i]);
            w->fds[i] = -1;
        }
    }
    w->count = 0;
    if (w->inotify_fd >= 0)
    {
        close(w->inotify_fd);
        w->inotify_fd = -1;
    }
}

static bool is_screen_suspended(void)
{
    if (power_display_off())
        return true;
    int fd = open("/sys/class/backlight/backlight/bl_power", O_RDONLY | O_CLOEXEC);
    if (fd >= 0)
    {
        char c = '0';
        ssize_t n = read(fd, &c, 1);
        close(fd);
        if (n == 1 && c != '0')
            return true;
    }
    return false;
}

static void bg_trigger_track_skip(bool next)
{
    /* Smooth 5 ms software micro-fade before track skip to guarantee zero click */
    pcm_set_master_volume(-360, -360);
    sleep_short_ms(5);
    if (next)
        audio_next();
    else
        audio_prev();
    trimpod_alsa_adjust_bg_volume(0);
}

static void bg_toggle_play_pause(void)
{
    int st = audio_status();
    if (st & AUDIO_STATUS_PLAY)
    {
        if (st & AUDIO_STATUS_PAUSE)
            unpause_action(true);
        else
            pause_action(true);
    }
    else if (global_status.resume_index != -1)
    {
        if (playlist_resume() != -1)
            playlist_start(global_status.resume_index,
                           global_status.resume_elapsed,
                           global_status.resume_offset);
    }
}

static void bg_evdev_poll(struct bg_evdev_watcher *w)
{
    uint64_t now = monotonic_ms();
    bool locked = retrohh_hold_switch() || is_screen_suspended();

    if (w->inotify_fd >= 0)
    {
        char ibuf[256] __attribute__((aligned(__alignof__(struct inotify_event))));
        ssize_t got;
        while ((got = read(w->inotify_fd, ibuf, sizeof(ibuf))) > 0)
        {
            for (char *p = ibuf; p < ibuf + got;
                 p += sizeof(struct inotify_event) + ((struct inotify_event *)p)->len)
            {
                const struct inotify_event *ev = (const struct inotify_event *)p;
                if (ev->len > 0)
                    bg_evdev_add_node(w, ev->name);
            }
        }
    }

    for (int i = 0; i < w->count; i++)
    {
        int fd = w->fds[i];
        if (fd < 0)
            continue;

        struct input_event evs[16];
        ssize_t got;
        while ((got = read(fd, evs, sizeof(evs))) > 0)
        {
            int n_ev = (int)(got / (ssize_t)sizeof(evs[0]));
            for (int e = 0; e < n_ev; e++)
            {
                const struct input_event *ev = &evs[e];
                if (ev->type == EV_KEY && ev->value <= 1)
                {
                    bool down = (ev->value == 1);
                    switch (ev->code)
                    {
                        case BTN_MODE:
                        case KEY_MENU:
                            w->menu_held = down;
                            if (!down)
                                w->chord_used = false;
                            break;
                        case BTN_START:
                            if (down && !w->start_held && w->menu_held && !locked &&
                                (now - w->enter_ms) >= UIPOD_BG_CHORD_SETTLE_MS)
                            {
                                w->chord_used = true;
                                bg_toggle_play_pause();
                            }
                            w->start_held = down;
                            break;
                        case BTN_TL2:
                            if (down && !w->l2_held && w->menu_held && !locked)
                            {
                                w->chord_used = true;
                                bg_trigger_track_skip(false);
                            }
                            w->l2_held = down;
                            break;
                        case BTN_TR2:
                            if (down && !w->r2_held && w->menu_held && !locked)
                            {
                                w->chord_used = true;
                                bg_trigger_track_skip(true);
                            }
                            w->r2_held = down;
                            break;
                        case BTN_DPAD_UP:
                        case KEY_UP:
                            if (down && !w->up_held && w->menu_held && !locked)
                            {
                                w->chord_used = true;
                                trimpod_alsa_adjust_bg_volume(1);
                                w->next_vol_repeat_ms = now + UIPOD_BG_VOL_REPEAT_MS;
                            }
                            w->up_held = down;
                            break;
                        case BTN_DPAD_DOWN:
                        case KEY_DOWN:
                            if (down && !w->down_held && w->menu_held && !locked)
                            {
                                w->chord_used = true;
                                trimpod_alsa_adjust_bg_volume(-1);
                                w->next_vol_repeat_ms = now + UIPOD_BG_VOL_REPEAT_MS;
                            }
                            w->down_held = down;
                            break;
                        case KEY_PLAYPAUSE:
                            if (down && !locked)
                                bg_toggle_play_pause();
                            break;
                        case KEY_NEXTSONG:
                            if (down && !locked)
                                bg_trigger_track_skip(true);
                            break;
                        case KEY_PREVIOUSSONG:
                            if (down && !locked)
                                bg_trigger_track_skip(false);
                            break;
                        default:
                            break;
                    }
                }
                else if (ev->type == EV_ABS)
                {
                    /* TrimUI Brick (`tg5040`): L2/R2 report on ABS_Z (2) / ABS_RZ (5),
                     * and D-Pad reports on ABS_HAT0Y (17). */
                    if (ev->code == ABS_Z)
                    {
                        bool down = (ev->value > 0);
                        if (down && !w->l2_held && w->menu_held && !locked)
                        {
                            w->chord_used = true;
                            bg_trigger_track_skip(false);
                        }
                        w->l2_held = down;
                    }
                    else if (ev->code == ABS_RZ)
                    {
                        bool down = (ev->value > 0);
                        if (down && !w->r2_held && w->menu_held && !locked)
                        {
                            w->chord_used = true;
                            bg_trigger_track_skip(true);
                        }
                        w->r2_held = down;
                    }
                    else if (ev->code == ABS_HAT0Y)
                    {
                        bool up_now   = (ev->value < 0);
                        bool down_now = (ev->value > 0);
                        if (up_now && !w->up_held && w->menu_held && !locked)
                        {
                            w->chord_used = true;
                            trimpod_alsa_adjust_bg_volume(1);
                            w->next_vol_repeat_ms = now + UIPOD_BG_VOL_REPEAT_MS;
                        }
                        if (down_now && !w->down_held && w->menu_held && !locked)
                        {
                            w->chord_used = true;
                            trimpod_alsa_adjust_bg_volume(-1);
                            w->next_vol_repeat_ms = now + UIPOD_BG_VOL_REPEAT_MS;
                        }
                        w->up_held   = up_now;
                        w->down_held = down_now;
                    }
                }
            }
        }
        if (got < 0 && errno != EAGAIN && errno != EWOULDBLOCK && errno != EINTR)
        {
            close(fd);
            w->fds[i] = -1;
        }
    }

    if (w->menu_held && !locked && (w->up_held || w->down_held) &&
        now >= w->next_vol_repeat_ms)
    {
        trimpod_alsa_adjust_bg_volume(w->up_held ? 1 : -1);
        w->next_vol_repeat_ms = now + UIPOD_BG_VOL_REPEAT_MS;
    }
}

/*Accepts any incoming `--client` IPC message while running in headless mode.
 * Returns: `1` = `ATTACH_UI`, `-1` = `QUIT`, `0` = stay in background. */
static int poll_ipc_socket_headless(void)
{
    if (s_listen_fd < 0)
        return 0;

    struct pollfd pfd = { .fd = s_listen_fd, .events = POLLIN };
    if (poll(&pfd, 1, 0) <= 0)
        return 0;

    int cfd = accept(s_listen_fd, NULL, NULL);
    if (cfd < 0)
        return 0;

    fcntl(cfd, F_SETFD, FD_CLOEXEC);
    fcntl(cfd, F_SETFL, O_NONBLOCK);

    char cmd[64];
    memset(cmd, 0, sizeof(cmd));
    for (int tries = 0; tries < 10; tries++)
    {
        ssize_t n = read(cfd, cmd, sizeof(cmd) - 1);
        if (n > 0)
        {
            cmd[n] = '\0';
            break;
        }
        sleep_short_ms(4);
    }

    if (strstr(cmd, "QUIT") != NULL)
    {
        if (s_client_fd >= 0)
            close(s_client_fd);
        s_client_fd = cfd;
        return -1;
    }

    if (s_client_fd >= 0)
        close(s_client_fd);
    s_client_fd = cfd;
    return 1;
}

static void flush_persistent_state(void)
{
    settings_save();
    status_save(true);
}

static bool enter_background_and_wait_for_reattach(void)
{
    if (s_in_bg)
        return false;
    s_in_bg   = true;
    s_min_req = false;

    /* 1. Wait up to 250 ms for physical release of `MENU` and `START` so
     *    NextUI never receives a leftover held button upon regaining focus! */
    uint64_t wait_start = monotonic_ms();
    while (sdl_button_menu_or_start_held() &&
           (monotonic_ms() - wait_start) < UIPOD_RELEASE_WAIT_MS)
    {
        sleep(1);
    }

    /* 2. Flush persistent config & playback resume state */
    flush_persistent_state();

    /* 3. Switch ALSA to shared `Playback` (`dmix`) + independent software volume */
    pcm_alsa_set_background_shared(true);
    trimpod_alsa_set_background_mode(true);

    /* 4. Tear down SDL input thread, Rust GLES3 resources, GL context & SDL window */
    sdl_event_thread_stop();
    uipod_gpu_teardown_gl();
    sdl_window_teardown_for_background();

    /* 5. Open non-exclusive `/dev/input/event*` watcher for background chords */
    struct bg_evdev_watcher evw;
    bg_evdev_open(&evw);

    /* 6. Notify `--client` (`launch.sh`) so NextUI reclaims the screen immediately */
    if (s_client_fd >= 0)
    {
        (void)write_all_str(s_client_fd, "DETACHED\n");
        close(s_client_fd);
        s_client_fd = -1;
    }

    /* 7. Headless Daemon Loop (`~30 ms` sleep ticks, `< 3%` CPU, 15-min idle guard) */
    uint64_t idle_since_ms = 0;
    while (s_in_bg)
    {
        if (s_sig_quit)
        {
            bg_evdev_close(&evw);
            s_in_bg = false;
            uipod_daemon_quit();
            return false;
        }

        int ipc = poll_ipc_socket_headless();
        if (ipc < 0)
        {
            bg_evdev_close(&evw);
            s_in_bg = false;
            uipod_daemon_quit();
            return false;
        }
        if (ipc > 0)
            break; /* `ATTACH_UI` received! */

        bg_evdev_poll(&evw);

        /* Forgotten-Daemon Battery Guard: auto-exit after 15 minutes paused/stopped */
        int astat = audio_status();
        bool actively_playing = (astat & AUDIO_STATUS_PLAY) != 0 &&
                                (astat & AUDIO_STATUS_PAUSE) == 0;
        if (actively_playing)
        {
            idle_since_ms = 0;
        }
        else
        {
            uint64_t now = monotonic_ms();
            if (idle_since_ms == 0)
                idle_since_ms = now;
            else if (now - idle_since_ms >= UIPOD_IDLE_EXIT_MS)
            {
                bg_evdev_close(&evw);
                s_in_bg = false;
                flush_persistent_state();
                cleanup_daemon_files();
                (void)umount2(UIPOD_BIND_MOUNT_PATH, MNT_DETACH);
                uipod_daemon_quit();
                return false;
            }
        }

        /* Sleep 3 Rockbox kernel ticks (`30 ms` at `HZ=100`), yielding cleanly
         * to `audio_thread`, `buffering_thread`, Smart DJ Crossfade, & scrobblers. */
        sleep(3);
    }

    /* 8. Re-Attach UI (`ATTACH_UI` received from NextUI `launch.sh`) */
    bg_evdev_close(&evw);
    trimpod_alsa_set_background_mode(false);
    if (!s_is_daemon)
        pcm_alsa_set_background_shared(false);

    if (!sdl_window_reattach_from_background())
    {
        uipod_daemon_quit();
        return false;
    }
    (void)sdl_event_thread_start();

    /* 200 ms bounce guard so the `A` / `START` press in NextUI never leaks in */
    sdl_button_arm_reattach_guard(UIPOD_REATTACH_GUARD_MS);
    button_clear_queue();
    action_wait_for_release();
    uipod_transition_suppress_next();
    retrohh_cpu_apply_saved();
    backlight_on();
    backlight_hw_brightness(global_settings.brightness);
    s_in_bg = false;
    send_event(GUI_EVENT_ACTIONUPDATE, (void *)1);
    return true;
}

#endif /* RETRO_HANDHELD && !__ANDROID__ */

void uipod_daemon_early_main(int argc, char *argv[])
{
#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
    bool want_client = false;
    bool want_daemon = false;
    for (int i = 1; i < argc; i++)
    {
        if (strcmp(argv[i], "--client") == 0)
            want_client = true;
        else if (strcmp(argv[i], "--daemon") == 0)
            want_daemon = true;
    }

    if (want_client)
        run_thin_client_and_exit(argc, argv);

    signal(SIGPIPE, SIG_IGN);
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = daemon_sig_handler;
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT,  &sa, NULL);
    sigaction(SIGHUP,  &sa, NULL);

    if (want_daemon)
    {
        s_is_daemon = true;
        (void)setup_daemon_listener();
    }
#else
    (void)argc;
    (void)argv;
#endif
}

void uipod_daemon_post_audio_init(void)
{
#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
    if (s_is_daemon)
    {
        /* Keep shared ALSA `Playback` (`dmix`) active throughout daemon sessions
         * so minimizing and re-attaching have 0.000 ms audio interruption and
         * `minarch.elf` can always mix game audio simultaneously. */
        pcm_alsa_set_background_shared(true);
    }
#endif
}

bool uipod_daemon_supported(void)
{
#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
    return true;
#else
    return false;
#endif
}

void uipod_daemon_request_minimize(void)
{
#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
    s_min_req = true;
#endif
}

bool uipod_daemon_minimize_pending(void)
{
#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
    if (sdl_button_take_minimize_chord())
        s_min_req = true;
    return s_min_req;
#else
    return false;
#endif
}

bool uipod_daemon_poll(bool allow_minimize)
{
#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
    if (s_sig_quit)
    {
        uipod_daemon_quit();
        return false;
    }
    if (sdl_button_take_minimize_chord())
    {
        if (allow_minimize)
            s_min_req = true;
    }
    if (allow_minimize && s_min_req)
        return enter_background_and_wait_for_reattach();
#else
    (void)allow_minimize;
#endif
    return false;
}

void uipod_daemon_notify_exit(void)
{
#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
    if (s_client_fd >= 0)
    {
        (void)write_all_str(s_client_fd, "EXITED\n");
        close(s_client_fd);
        s_client_fd = -1;
    }
    cleanup_daemon_files();
#endif
}

void uipod_daemon_quit(void)
{
#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
    bool headless = s_in_bg || sdl_window_is_headless();
    s_sig_quit = 0;
    s_min_req  = false;
    s_in_bg    = false;
    audio_stop();
    flush_persistent_state();
#endif
    uipod_lastfm_shutdown();
    uipod_listenbrainz_shutdown();
    uipod_downloads_shutdown();
    uipod_daemon_notify_exit();
#if defined(RETRO_HANDHELD) && !defined(__ANDROID__)
    if (headless)
    {
        playlist_shutdown();
        uipod_library_close();
        tree_flush();
        audiohw_close();
        power_off();
        return;
    }
#endif
    sys_poweroff();
}

char *uipod_daemon_minimize_label(int selected_item, void *data,
                                  char *buffer, size_t buffer_len)
{
    (void)selected_item;
    (void)data;
    bool uk = strcmp((const char *)global_settings.lang_file, "ukrainian") == 0;
    snprintf(buffer, buffer_len, "%s",
             uk ? "Згорнути у фон" : "Minimize to Background");
    return buffer;
}

char *uipod_daemon_quit_label(int selected_item, void *data,
                              char *buffer, size_t buffer_len)
{
    (void)selected_item;
    (void)data;
    bool uk = strcmp((const char *)global_settings.lang_file, "ukrainian") == 0;
    snprintf(buffer, buffer_len, "%s",
             uk ? "Вийти з UIPod" : "Quit UIPod");
    return buffer;
}
