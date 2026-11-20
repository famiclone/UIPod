/***************************************************************************
 * Trimpod: runs TrimUI's stock hardware video player (tplayerslave) as a child.
 *
 * The player decodes to a display layer below the app window and takes commands
 * through a FIFO.  Plugins from trimpod/video extend it: libpassthrua.so (AC-3,
 * found on LD_LIBRARY_PATH), and libdivx_shim.so (DivX) and the sound's route
 * to Trimpod (video-audio.c), both preloaded.
 ****************************************************************************/
#ifdef __ANDROID__
#include <jni.h>
#include <stdbool.h>
#include <string.h>
#include <SDL.h>
#include "config.h"
#include "window-sdl.h"
#include "video-target.h"

static bool s_video_running = false;

static jclass find_activity_cls(JNIEnv *env)
{
    if (!env)
        return NULL;
    jclass cls = (*env)->FindClass(env, "dev/famiclone/uipod/UIPodActivity");
    if (!cls)
        (*env)->ExceptionClear(env);
    return cls;
}

bool retrohh_video_probe(const char *path, struct retrohh_video_info *info)
{
    if (!info)
        return false;
    memset(info, 0, sizeof(*info));
    info->width  = 1280;
    info->height = 720;
    (void)path;
    return path != NULL && path[0] != '\0';
}

bool retrohh_video_start(const char *path, int start_ms)
{
    if (!path || !path[0])
        return false;
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    jclass cls = find_activity_cls(env);
    if (!cls)
        return false;
    jmethodID mid = (*env)->GetStaticMethodID(env, cls, "videoStart", "(Ljava/lang/String;I)Z");
    jboolean ok = JNI_FALSE;
    if (mid)
    {
        jstring jpath = (*env)->NewStringUTF(env, path);
        if (jpath)
        {
            ok = (*env)->CallStaticBooleanMethod(env, cls, mid, jpath, (jint)start_ms);
            (*env)->DeleteLocalRef(env, jpath);
        }
    }
    else
    {
        (*env)->ExceptionClear(env);
    }
    (*env)->DeleteLocalRef(env, cls);
    s_video_running = (ok == JNI_TRUE);
    return s_video_running;
}

void retrohh_video_stop(void)
{
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    jclass cls = find_activity_cls(env);
    if (cls)
    {
        jmethodID mid = (*env)->GetStaticMethodID(env, cls, "videoStop", "()V");
        if (mid)
            (*env)->CallStaticVoidMethod(env, cls, mid);
        else
            (*env)->ExceptionClear(env);
        (*env)->DeleteLocalRef(env, cls);
    }
    s_video_running = false;
    sdl_gl_set_video_through(false, 0);
}

bool retrohh_video_running(void)
{
    if (!s_video_running)
        return false;
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    jclass cls = find_activity_cls(env);
    if (!cls)
        return false;
    jmethodID mid = (*env)->GetStaticMethodID(env, cls, "videoIsRunning", "()Z");
    jboolean r = JNI_FALSE;
    if (mid)
        r = (*env)->CallStaticBooleanMethod(env, cls, mid);
    else
        (*env)->ExceptionClear(env);
    (*env)->DeleteLocalRef(env, cls);
    s_video_running = (r == JNI_TRUE);
    return s_video_running;
}

bool retrohh_video_status(int *position_ms, int *duration_ms, bool *playing)
{
    if (!s_video_running)
        return false;
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    jclass cls = find_activity_cls(env);
    if (!cls)
        return false;
    jmethodID mid = (*env)->GetStaticMethodID(env, cls, "videoGetState", "()[I");
    bool ready = false;
    if (mid)
    {
        jintArray arr = (jintArray)(*env)->CallStaticObjectMethod(env, cls, mid);
        if (arr && (*env)->GetArrayLength(env, arr) >= 4)
        {
            jint vals[4] = {0, 0, 0, 0};
            (*env)->GetIntArrayRegion(env, arr, 0, 4, vals);
            if (vals[0] != 0)
            {
                if (position_ms) *position_ms = (int)vals[1];
                if (duration_ms) *duration_ms = (int)vals[2];
                if (playing)     *playing     = (vals[3] != 0);
                ready = true;
            }
        }
        if (arr)
            (*env)->DeleteLocalRef(env, arr);
    }
    else
    {
        (*env)->ExceptionClear(env);
    }
    (*env)->DeleteLocalRef(env, cls);
    return ready;
}

void retrohh_video_pause(bool paused)
{
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    jclass cls = find_activity_cls(env);
    if (!cls)
        return;
    jmethodID mid = (*env)->GetStaticMethodID(env, cls, "videoPause", "(Z)V");
    if (mid)
        (*env)->CallStaticVoidMethod(env, cls, mid, paused ? JNI_TRUE : JNI_FALSE);
    else
        (*env)->ExceptionClear(env);
    (*env)->DeleteLocalRef(env, cls);
}

void retrohh_video_seek(int position_ms)
{
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    jclass cls = find_activity_cls(env);
    if (!cls)
        return;
    jmethodID mid = (*env)->GetStaticMethodID(env, cls, "videoSeek", "(I)V");
    if (mid)
        (*env)->CallStaticVoidMethod(env, cls, mid, (jint)position_ms);
    else
        (*env)->ExceptionClear(env);
    (*env)->DeleteLocalRef(env, cls);
}

void retrohh_video_set_aspect(const struct retrohh_video_info *info,
                              enum retrohh_video_aspect aspect)
{
    (void)info;
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    jclass cls = find_activity_cls(env);
    if (!cls)
        return;
    jmethodID mid = (*env)->GetStaticMethodID(env, cls, "videoSetStretch", "(Z)V");
    if (mid)
        (*env)->CallStaticVoidMethod(env, cls, mid,
                                     (aspect == RETROHH_VIDEO_STRETCH) ? JNI_TRUE : JNI_FALSE);
    else
        (*env)->ExceptionClear(env);
    (*env)->DeleteLocalRef(env, cls);
}

void retrohh_video_set_overlay(bool video_visible, int overlay_rows)
{
    sdl_gl_set_video_through(video_visible, overlay_rows);
}

bool retrohh_video_set_audio_track(int track_ordinal)
{
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    jclass cls = find_activity_cls(env);
    if (!cls)
        return false;
    jmethodID mid = (*env)->GetStaticMethodID(env, cls, "videoSetAudioTrack", "(I)Z");
    jboolean ok = JNI_FALSE;
    if (mid)
        ok = (*env)->CallStaticBooleanMethod(env, cls, mid, (jint)track_ordinal);
    else
        (*env)->ExceptionClear(env);
    (*env)->DeleteLocalRef(env, cls);
    return ok == JNI_TRUE;
}

bool retrohh_video_extract_subtitle_srt(const char *video_path,
                                        const struct retrohh_video_track *sub,
                                        char *out_srt_path,
                                        size_t out_cap)
{
    (void)video_path;
    if (!sub || !out_srt_path || out_cap == 0)
        return false;
    if (sub->external && sub->ext_path[0])
    {
        strncpy(out_srt_path, sub->ext_path, out_cap - 1);
        out_srt_path[out_cap - 1] = '\0';
        return true;
    }
    return false;
}

#else /* !__ANDROID__ */

#include <sys/ioctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>
#include <SDL.h>

#include "config.h"
#include "window-sdl.h"
#include "video-audio.h"
#include "video-target.h"

#define PLAYER_BIN  "/usr/bin/tplayerslave"
#define PROBE_BIN   "/usr/bin/ffprobe"
#define FFMPEG_BIN  "/usr/bin/ffmpeg"
#define SUB_TMP_SRT "/tmp/uipod_sub.srt"
#define PLAYER_DIR  "/tmp/trimui_player"
#define CMD_FIFO    PLAYER_DIR "/cmd"
#define STATUS_FILE PLAYER_DIR "/status"
#define PLUGIN_DIR  ROCKBOX_DIR "/video"
#define ENV_MAX     128
#define PROBE_BYTES 16384

/* Wait up to two seconds for "exit" to work. */
#define STOP_POLL_US 20000
#define STOP_POLLS   100

/* Wait up to a second for the player's FIFO. */
#define OPEN_POLL_US 10000
#define OPEN_POLLS   100

/* The picture is display-engine channel 0, layer 0, and only the player's own
 * exit takes it down.  Left up after any other death, it reads freed buffers and
 * within seconds the engine stops producing frames until a reboot.  Switched off
 * at once, the engine recovers.  An all-zero layer config is that layer, off. */
#define DISP_DEV              "/dev/disp"
#define DISP_LAYER_SET_CONFIG 0x47
#define DISP_CONFIG_BYTES     512       /* more than struct disp_layer_config */
#define DISP_LATCH_US         50000     /* a config applies at a vsync */

/* SIGSTOP lands as each thread leaves the kernel. */
#define FREEZE_POLL_US  5000
#define FREEZE_POLLS    20

extern char **environ;

static pid_t player_pid = -1;

static bool player_command(const char *fmt, ...);

/* Runs in the forked child, so only async-signal-safe calls. */
static void child_exec(const char *bin, char *const argv[], char *const envp[],
                       int out_fd, long maxfd)
{
    dup2(out_fd, STDOUT_FILENO);
    dup2(out_fd, STDERR_FILENO);
    for (long fd = 3; fd < maxfd; fd++)
        close(fd);
    execve(bin, argv, envp);
    _exit(127);
}

static void format_stream_label(char *out, size_t cap, int num,
                                const char *lang, const char *title,
                                const char *codec)
{
    char lang_up[12] = "";
    if (lang && lang[0] && strcasecmp(lang, "und") != 0)
    {
        size_t i = 0;
        for (; lang[i] && i < sizeof(lang_up) - 1; i++)
            lang_up[i] = (char)toupper((unsigned char)lang[i]);
        lang_up[i] = '\0';
    }
    if (title && title[0] && lang_up[0])
        snprintf(out, cap, "%s (%s)", title, lang_up);
    else if (title && title[0])
        snprintf(out, cap, "%s", title);
    else if (lang_up[0] && codec && codec[0])
        snprintf(out, cap, "Track %d (%s, %s)", num, lang_up, codec);
    else if (lang_up[0])
        snprintf(out, cap, "Track %d (%s)", num, lang_up);
    else if (codec && codec[0])
        snprintf(out, cap, "Track %d (%s)", num, codec);
    else
        snprintf(out, cap, "Track %d", num);
}

static void probe_sidecar_srts(const char *video_path, struct retrohh_video_info *info)
{
    if (!video_path || video_path[0] != '/')
        return;
    const char *slash = strrchr(video_path, '/');
    if (!slash)
        return;

    char dir_path[256];
    size_t dlen = (size_t)(slash - video_path);
    if (dlen == 0)
    {
        strcpy(dir_path, "/");
    }
    else
    {
        if (dlen >= sizeof(dir_path))
            return;
        memcpy(dir_path, video_path, dlen);
        dir_path[dlen] = '\0';
    }

    const char *base = slash + 1;
    const char *dot = strrchr(base, '.');
    size_t stem_len = dot ? (size_t)(dot - base) : strlen(base);
    if (stem_len == 0)
        return;

    DIR *d = opendir(dir_path);
    if (!d)
        return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL && info->sub_count < RETROHH_VIDEO_MAX_TRACKS)
    {
        size_t nl = strlen(e->d_name);
        if (nl <= 4 || strcasecmp(e->d_name + nl - 4, ".srt") != 0)
            continue;
        if (nl < stem_len + 4 || strncasecmp(e->d_name, base, stem_len) != 0)
            continue;
        char sep = e->d_name[stem_len];
        if (sep != '.' && sep != '_' && sep != '-')
            continue;

        struct retrohh_video_track *st = &info->sub_tracks[info->sub_count];
        memset(st, 0, sizeof(*st));
        st->stream_index = -1;
        st->external = true;
        snprintf(st->ext_path, sizeof(st->ext_path), "%s/%s",
                 strcmp(dir_path, "/") == 0 ? "" : dir_path, e->d_name);

        if (nl == stem_len + 4)
        {
            snprintf(st->label, sizeof(st->label), "External SRT");
        }
        else
        {
            char tag[32];
            size_t tlen = nl - 4 - (stem_len + 1);
            if (tlen >= sizeof(tag))
                tlen = sizeof(tag) - 1;
            memcpy(tag, e->d_name + stem_len + 1, tlen);
            tag[tlen] = '\0';
            snprintf(st->label, sizeof(st->label), "SRT (%s)", tag);
        }
        info->sub_count++;
    }
    closedir(d);
}

static bool is_text_subtitle_codec(const char *codec)
{
    if (!codec || !codec[0])
        return false;
    return !strcasecmp(codec, "subrip") ||
           !strcasecmp(codec, "srt") ||
           !strcasecmp(codec, "ass") ||
           !strcasecmp(codec, "ssa") ||
           !strcasecmp(codec, "mov_text") ||
           !strcasecmp(codec, "webvtt") ||
           !strcasecmp(codec, "text");
}

bool retrohh_video_probe(const char *path, struct retrohh_video_info *info)
{
    static char output[PROBE_BYTES];
    char *const argv[] = {
        "ffprobe", "-v", "error", "-show_entries",
        "stream=index,codec_type,codec_name,width,height,display_aspect_ratio:"
        "stream_tags=language,title:chapter=start_time",
        "-of", "default=nw=1", (char *)path, NULL
    };
    long maxfd = sysconf(_SC_OPEN_MAX);
    int fds[2], used = 0;
    ssize_t got;
    pid_t pid;

    memset(info, 0, sizeof *info);
    if (pipe(fds) < 0)
        return false;
    pid = fork();
    if (pid < 0)
    {
        close(fds[0]);
        close(fds[1]);
        return false;
    }
    if (pid == 0)
        child_exec(PROBE_BIN, argv, environ, fds[1], maxfd);

    close(fds[1]);
    while (used < PROBE_BYTES - 1 &&
           (got = read(fds[0], output + used, PROBE_BYTES - 1 - used)) > 0)
        used += got;
    close(fds[0]);
    waitpid(pid, NULL, 0);
    output[used] = '\0';
    if (!used)
        return false;

    bool have_video = false;
    int cur_index = -1;
    char cur_type[16] = "";
    char cur_codec[24] = "";
    char cur_lang[16] = "";
    char cur_title[40] = "";

    /* Flush previous stream block when a new `index=` or chapter arrives */
    for (char *line = strtok(output, "\n"); ; line = strtok(NULL, "\n"))
    {
        bool flush = (!line || !strncmp(line, "index=", 6) || !strncmp(line, "start_time=", 11));
        if (flush && cur_index >= 0)
        {
            if (!strcmp(cur_type, "audio") && info->audio_count < RETROHH_VIDEO_MAX_TRACKS)
            {
                struct retrohh_video_track *at = &info->audio_tracks[info->audio_count];
                at->stream_index = cur_index;
                at->external = false;
                format_stream_label(at->label, sizeof(at->label),
                                    info->audio_count + 1, cur_lang, cur_title, cur_codec);
                info->audio_count++;
            }
            else if (!strcmp(cur_type, "subtitle") &&
                     is_text_subtitle_codec(cur_codec) &&
                     info->sub_count < RETROHH_VIDEO_MAX_TRACKS)
            {
                struct retrohh_video_track *st = &info->sub_tracks[info->sub_count];
                st->stream_index = cur_index;
                st->external = false;
                format_stream_label(st->label, sizeof(st->label),
                                    info->sub_count + 1, cur_lang, cur_title, NULL);
                info->sub_count++;
            }
            cur_index = -1;
            cur_type[0] = cur_codec[0] = cur_lang[0] = cur_title[0] = '\0';
        }
        if (!line)
            break;

        if (!strncmp(line, "index=", 6))
            cur_index = atoi(line + 6);
        else if (!strncmp(line, "codec_type=", 11))
        {
            strncpy(cur_type, line + 11, sizeof(cur_type) - 1);
            cur_type[sizeof(cur_type) - 1] = '\0';
        }
        else if (!strncmp(line, "codec_name=", 11))
        {
            strncpy(cur_codec, line + 11, sizeof(cur_codec) - 1);
            cur_codec[sizeof(cur_codec) - 1] = '\0';
            if (!have_video && (!cur_type[0] || !strcmp(cur_type, "video")))
                info->hevc = !strcmp(cur_codec, "hevc");
        }
        else if (!strncmp(line, "width=", 6) && !have_video)
            info->width = atoi(line + 6);
        else if (!strncmp(line, "height=", 7) && !have_video)
        {
            info->height = atoi(line + 7);
            have_video = true;
        }
        else if (!strncmp(line, "display_aspect_ratio=", 21) && info->ratio_num == 0)
            sscanf(line + 21, "%d:%d", &info->ratio_num, &info->ratio_den);
        else if (!strncmp(line, "TAG:language=", 13))
        {
            strncpy(cur_lang, line + 13, sizeof(cur_lang) - 1);
            cur_lang[sizeof(cur_lang) - 1] = '\0';
        }
        else if (!strncmp(line, "TAG:title=", 10))
        {
            strncpy(cur_title, line + 10, sizeof(cur_title) - 1);
            cur_title[sizeof(cur_title) - 1] = '\0';
        }
        else if (!strncmp(line, "start_time=", 11) &&
                 info->chapters < RETROHH_VIDEO_MAX_CHAPTERS)
            info->chapter_ms[info->chapters++] = (int)(atof(line + 11) * 1000);
    }

    probe_sidecar_srts(path, info);
    return true;
}

/* The child's environment: ours, plus the plugins. */
static char **player_environment(void)
{
    static char *envp[ENV_MAX];
    static char lib_path[1024];
    const char *old = getenv("LD_LIBRARY_PATH");
    int n = 0;

    snprintf(lib_path, sizeof lib_path,
             "LD_LIBRARY_PATH=" PLUGIN_DIR ":/usr/trimui/lib:/usr/lib%s%s",
             old ? ":" : "", old ? old : "");
    envp[n++] = lib_path;
    envp[n++] = "LD_PRELOAD=" PLUGIN_DIR "/libdivx_shim.so:"
                PLUGIN_DIR "/libasound_module_pcm_uipod.so";
    for (char **e = environ; *e && n < ENV_MAX - 1; e++)
        if (strncmp(*e, "LD_LIBRARY_PATH=", 16) && strncmp(*e, "LD_PRELOAD=", 11))
            envp[n++] = *e;
    envp[n] = NULL;
    return envp;
}

bool retrohh_video_start(const char *path, int start_ms)
{
    char *const argv[] = { "tplayerslave", (char *)path, NULL };
    char **envp = player_environment();
    long maxfd = sysconf(_SC_OPEN_MAX);
    int null_fd;
    pid_t pid;

    /* A stale status would read as this video already playing. */
    remove(STATUS_FILE);

    null_fd = open("/dev/null", O_WRONLY);      /* the player's log is very chatty */
    if (null_fd < 0)
        return false;
    if (!video_audio_open())
        fprintf(stderr, "video: no route for the player's sound\n");
    pid = fork();
    if (pid == 0)
        child_exec(PLAYER_BIN, argv, envp, null_fd, maxfd);
    close(null_fd);
    if (pid < 0)
    {
        video_audio_close();
        return false;
    }
    player_pid = pid;

    /* No start-position option, but a seek queued before the file loads runs as
     * soon as playback starts. */
    if (start_ms > 0)
        for (int i = 0; i < OPEN_POLLS && !player_command("seek %d", start_ms); i++)
            usleep(OPEN_POLL_US);
    return true;
}

/* Stops every thread of the player, so none can put its layer back. */
static void freeze_player(void)
{
    int st;

    kill(player_pid, SIGSTOP);
    for (int i = 0; i < FREEZE_POLLS; i++)
    {
        if (waitpid(player_pid, &st, WNOHANG | WUNTRACED) == player_pid)
        {
            if (!WIFSTOPPED(st))
                player_pid = -1;        /* it exited */
            return;
        }
        usleep(FREEZE_POLL_US);
    }
}

static void hide_player_layer(void)
{
    static const unsigned char off[DISP_CONFIG_BYTES];
    unsigned long args[4] = { 0, (unsigned long)off, 1, 0 };
    int fd = open(DISP_DEV, O_RDWR);

    if (fd < 0)
        return;
    if (ioctl(fd, DISP_LAYER_SET_CONFIG, args) < 0)
        fprintf(stderr, "video: can't hide the player's layer: %s\n", strerror(errno));
    close(fd);
    usleep(DISP_LATCH_US);
}

bool retrohh_video_running(void)
{
    if (player_pid < 0)
        return false;
    if (waitpid(player_pid, NULL, WNOHANG) == player_pid)
    {
        player_pid = -1;
        hide_player_layer();            /* however it ended */
        return false;
    }
    return true;
}

/* False until the player has opened the FIFO. */
static bool player_command(const char *fmt, ...)
{
    char cmd[64];
    va_list ap;
    int fd, len;

    va_start(ap, fmt);
    len = vsnprintf(cmd, sizeof cmd - 1, fmt, ap);
    va_end(ap);
    if (len < 0 || len >= (int)sizeof cmd - 1)
        return false;
    cmd[len++] = '\n';

    fd = open(CMD_FIFO, O_WRONLY | O_NONBLOCK);
    if (fd < 0)
        return false;
    len = write(fd, cmd, len);
    close(fd);
    return len > 0;
}

void retrohh_video_stop(void)
{
    if (retrohh_video_running())
    {
        player_command("exit");
        for (int i = 0; i < STOP_POLLS && retrohh_video_running(); i++)
            usleep(STOP_POLL_US);
    }
    if (player_pid > 0)                 /* it ignored the request */
    {
        fprintf(stderr, "video: the player ignored exit; killing it\n");
        freeze_player();                /* hidden before its buffers go: no glitch */
        hide_player_layer();
        if (player_pid > 0)
        {
            kill(player_pid, SIGKILL);
            waitpid(player_pid, NULL, 0);
            player_pid = -1;
        }
    }
    remove(STATUS_FILE);
    video_audio_close();
}

static bool json_int(const char *json, const char *key, int *value)
{
    const char *found = strstr(json, key);

    if (!found)
        return false;
    *value = atoi(found + strlen(key));
    return true;
}

bool retrohh_video_status(int *position_ms, int *duration_ms, bool *playing)
{
    char json[192];
    int fd = open(STATUS_FILE, O_RDONLY);
    ssize_t got;

    if (fd < 0)
        return false;
    got = read(fd, json, sizeof json - 1);
    close(fd);
    if (got <= 0)                       /* it's rewritten in place: can be empty */
        return false;
    json[got] = '\0';
    if (!json_int(json, "\"position\":", position_ms) ||
        !json_int(json, "\"duration\":", duration_ms))
        return false;
    *playing = strstr(json, "\"status\":\"playing\"") != NULL;
    return true;
}

void retrohh_video_pause(bool paused)
{
    player_command(paused ? "pause" : "unpause");
}

void retrohh_video_seek(int position_ms)
{
    player_command("seek %d", position_ms);
}

void retrohh_video_set_aspect(const struct retrohh_video_info *info,
                              enum retrohh_video_aspect aspect)
{
    long ratio_num = info->ratio_num ? info->ratio_num : info->width;
    long ratio_den = info->ratio_den ? info->ratio_den : info->height;
    int screen_w, screen_h;
    long w, h;

    if (aspect == RETROHH_VIDEO_STRETCH || ratio_num <= 0 || ratio_den <= 0)
    {
        player_command("full");
        return;
    }
    SDL_GL_GetDrawableSize(sdlWindow, &screen_w, &screen_h);
    /* The limiting edge matches the screen.  The window must stay on it: the
     * display shows nothing of a layer that runs past an edge. */
    if (ratio_num * screen_h > ratio_den * screen_w)
    {
        w = screen_w;
        h = w * ratio_den / ratio_num;
    }
    else
    {
        h = screen_h;
        w = h * ratio_num / ratio_den;
    }
    player_command("win %ld %ld %ld %ld", (screen_w - w) / 2, (screen_h - h) / 2,
                   w, h);
}

void retrohh_video_set_overlay(bool video_visible, int overlay_rows)
{
    sdl_gl_set_video_through(video_visible, overlay_rows);
}

bool retrohh_video_set_audio_track(int track_ordinal)
{
    if (track_ordinal < 0)
        return false;
    return player_command("audio %d", track_ordinal);
}

bool retrohh_video_extract_subtitle_srt(const char *video_path,
                                        const struct retrohh_video_track *sub,
                                        char *out_srt_path,
                                        size_t out_cap)
{
    if (!sub || !out_srt_path || out_cap == 0)
        return false;
    if (sub->external && sub->ext_path[0])
    {
        strncpy(out_srt_path, sub->ext_path, out_cap - 1);
        out_srt_path[out_cap - 1] = '\0';
        return true;
    }
    if (!video_path || !video_path[0] || sub->stream_index < 0)
        return false;

    char map_spec[24];
    snprintf(map_spec, sizeof(map_spec), "0:%d", sub->stream_index);
    remove(SUB_TMP_SRT);

    char *const argv[] = {
        "ffmpeg", "-y", "-v", "error", "-i", (char *)video_path,
        "-map", map_spec, "-f", "srt", SUB_TMP_SRT, NULL
    };
    long maxfd = sysconf(_SC_OPEN_MAX);
    int null_fd = open("/dev/null", O_WRONLY);
    if (null_fd < 0)
        return false;

    pid_t pid = fork();
    if (pid == 0)
        child_exec(FFMPEG_BIN, argv, environ, null_fd, maxfd);
    close(null_fd);
    if (pid < 0)
        return false;

    int st = 0;
    waitpid(pid, &st, 0);
    if (!WIFEXITED(st) || WEXITSTATUS(st) != 0 || access(SUB_TMP_SRT, R_OK) != 0)
        return false;

    strncpy(out_srt_path, SUB_TMP_SRT, out_cap - 1);
    out_srt_path[out_cap - 1] = '\0';
    return true;
}

#endif /* !__ANDROID__ */
