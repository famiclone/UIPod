/***************************************************************************
 * UIPod: full-screen video playback.
 *
 * The stock hardware player draws on a display layer below the app window (see
 * video-target.c).  This page clears the window so the video shows through, and
 * draws the Now Playing panels in a strip at the bottom when there's something
 * to show: progress after any control, volume after the rocker.
 *
 *   A            = play / pause (pausing shows the overlay)
 *   D-pad <- ->  = back / ahead 10 s (hold to keep going)
 *   L2 / R2      = previous / next chapter, as on Now Playing
 *   X            = aspect ratio: Fit, Stretch
 *   Volume +/-   = volume
 *   B            = with the overlay up, back to the list; else as A
 *   Hold B       = main menu
 ****************************************************************************/
#include "config.h"
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>   /* realpath */
#include <string.h>
#include "string-extra.h"
#include "action.h"
#include "audio.h"
#include "backlight.h"
#include "button.h"
#include "file.h"
#include "font.h"
#include "kernel.h"
#include "lang.h"
#include "misc.h"
#include "powermgmt.h"
#include "screen_access.h"
#include "settings.h"
#include "sound.h"
#include "splash.h"
#include "viewport.h"
#include "gui/scrollbar.h"
#include "gui/list.h"
#include "icon.h"
#include "recorder/bmp.h"
#include "video-target.h"
#include "uipod_page.h"      /* uipod_home_pending */
#include "uipod_transition.h"
#include "uipod_rs_ui.h"
#include "uipod_video.h"

extern void retrohh_cpu_hold_dynamic(void);     /* power-target.c owns CPU policy */
extern void retrohh_cpu_release_dynamic(void);

/* The bar and text of the Now Playing panels (1ST_GEN_REMIX.wps), on a taller strip. */
#define STRIP_TOP    576
#define STRIP_ROWS   (LCD_HEIGHT - STRIP_TOP)
#define BAR_X        64
#define BAR_Y        616
#define BAR_W        896
#define BAR_H        42
#define TEXT_Y       672
#define TEXT_H       80
#define BAR_BMP_DIR  ROCKBOX_DIR "/wps/1ST_GEN_REMIX"
#define BAR_BMP_BYTES ((BAR_W + 16) * (BAR_H + 4) * 4 + 256)

#define SEEK_STEP_MS        10000
#define SEEK_END_MARGIN_MS  1000
/* Joins D-pad repeats into one seek. */
#define SEEK_SETTLE_TICKS   (HZ / 3)
/* How long the player takes to report where a seek landed. */
#define SEEK_REPORT_TICKS   HZ
/* Past this far into a chapter, "previous" restarts it. */
#define CHAPTER_RESTART_MS  3000
#define OVERLAY_TICKS       (3 * HZ)
#define VOLUME_TICKS        (2 * HZ)
#define ASPECT_LABEL_TICKS  (2 * HZ)
#define POLL_TICKS          (HZ / 2)
#define ACTION_WAIT_TICKS   (HZ / 10)
/* Polls without movement near the end before the file counts as finished. */
#define END_STALL_POLLS     6
#define END_MARGIN_MS       2000

/* Per-video resume points: "<ms> <path>", oldest first.  Written to a temporary
 * file and renamed, so a power loss mid-write leaves the old file intact. */
#define RESUME_FILE         ROCKBOX_DIR "/.video_positions"
#define RESUME_TMP          RESUME_FILE ".tmp"
#define RESUME_MAX          256
/* Stopping earlier than this starts over next time. */
#define RESUME_MIN_MS       5000
/* Saved while watching too, in case the battery dies. */
#define RESUME_SAVE_TICKS   (60 * HZ)
/* The picture stays hidden until the resume seek lands, for at most this long.
 * It lands on a keyframe before the target, so half way counts. */
#define REVEAL_WAIT_TICKS        (4 * HZ)
#define STREAM_REVEAL_WAIT_TICKS (8 * HZ)

enum overlay_panel { PANEL_NONE, PANEL_PROGRESS, PANEL_VOLUME };

#define SUB_STRIP_ROWS 112
#define SUB_STRIP_TOP  (LCD_HEIGHT - SUB_STRIP_ROWS)
#define MAX_SRT_CUES   2048

struct srt_cue
{
    int  start_ms;
    int  end_ms;
    char line1[96];
    char line2[96];
};

static struct srt_cue *s_srt_cues;
static int s_srt_cue_count;
static int s_active_audio_idx;
static int s_active_sub_idx; /* -1 = Off, 0..sub_count-1 = active subtitle track */
static char s_active_play_path[MAX_PATH];

static struct retrohh_video_info info;
static enum retrohh_video_aspect aspect;   /* kept between videos */
static const struct uipod_video_stream_opts *s_stream_opts;
static int s_stream_base_ms;

static struct
{
    int  position_ms, duration_ms;
    int  seek_target_ms;        /* -1: no seek waiting */
    long seek_send_tick;
    long ignore_report_until;
    bool paused;
    bool reported;              /* the player has told us anything yet */
    bool revealed;              /* the window has stopped hiding the video */
    long reveal_by;
    int  stalled_polls;
    int  last_reported_ms;
    long progress_until, volume_until, aspect_label_until;
    int  last_volume;
    enum overlay_panel shown;
    int  shown_seconds, shown_volume;
    int  shown_cue_idx;
    bool shown_aspect_label;
    enum retrohh_video_aspect shown_aspect;
    bool drawn_once;
} v;

static unsigned char bar_fill_data[BAR_BMP_BYTES], bar_track_data[BAR_BMP_BYTES];
static struct bitmap bar_fill = { .data = bar_fill_data };
static struct bitmap bar_track = { .data = bar_track_data };
static bool have_bar_bitmaps;

/* ---- resume points ---------------------------------------------------- */

static struct resume_entry
{
    int  position_ms;
    char path[MAX_PATH];
} resume_entries[RESUME_MAX];
static int  resume_count;
static bool resume_loaded;
static char resume_key[MAX_PATH];  /* the video playing now, symlinks resolved */
static int  resume_start_ms;       /* where it should start */
static int  resume_saved_ms;       /* what the store holds for it */
static long resume_next_save;

static void resume_load(void)
{
    char line[MAX_PATH + 16];
    int fd;

    if (resume_loaded)
        return;
    resume_loaded = true;

    fd = open(RESUME_FILE, O_RDONLY);
    if (fd < 0)
        return;
    while (resume_count < RESUME_MAX && read_line(fd, line, sizeof line) > 0)
    {
        char *space = strchr(line, ' ');

        if (!space)
            continue;
        resume_entries[resume_count].position_ms = atoi(line);
        strlcpy(resume_entries[resume_count].path, space + 1, MAX_PATH);
        resume_count++;
    }
    close(fd);
}

static void resume_write(void)
{
    int fd = open(RESUME_TMP, O_WRONLY | O_CREAT | O_TRUNC, 0666);

    if (fd < 0)
        return;
    for (int i = 0; i < resume_count; i++)
        fdprintf(fd, "%d %s\n", resume_entries[i].position_ms,
                 resume_entries[i].path);
    close(fd);
    rename(RESUME_TMP, RESUME_FILE);
}

static int resume_find(const char *key)
{
    for (int i = 0; i < resume_count; i++)
        if (!strcmp(resume_entries[i].path, key))
            return i;
    return -1;
}

static void resume_remove_at(int index)
{
    memmove(&resume_entries[index], &resume_entries[index + 1],
            (resume_count - index - 1) * sizeof resume_entries[0]);
    resume_count--;
}

/* realpath: the virtual-folder farm and the real path name the same video. */
static void resume_open_ex(const char *path, const char *explicit_key,
                           int server_resume_ms, bool force_from_start)
{
    int index;

    resume_load();
    if (explicit_key && explicit_key[0])
        strlcpy(resume_key, explicit_key, sizeof resume_key);
    else if (!realpath(path, resume_key))
        strlcpy(resume_key, path, sizeof resume_key);
    index = resume_find(resume_key);
    resume_saved_ms = index >= 0 ? resume_entries[index].position_ms : 0;
    if (force_from_start)
        resume_start_ms = 0;
    else if (resume_saved_ms > 0)
        resume_start_ms = resume_saved_ms;
    else if (server_resume_ms >= RESUME_MIN_MS)
        resume_start_ms = server_resume_ms;
    else
        resume_start_ms = 0;
    resume_next_save = current_tick + RESUME_SAVE_TICKS;
}

/* Barely started or played out: the next play starts from the top. */
static void resume_save(int position_ms, int duration_ms)
{
    int index = resume_find(resume_key);

    if (position_ms < RESUME_MIN_MS || position_ms >= duration_ms - END_MARGIN_MS)
        position_ms = 0;
    if (position_ms == resume_saved_ms)
        return;
    resume_saved_ms = position_ms;

    if (index >= 0)
        resume_remove_at(index);        /* re-added last: most recently watched */
    if (position_ms)
    {
        if (resume_count == RESUME_MAX)
            resume_remove_at(0);
        resume_entries[resume_count].position_ms = position_ms;
        strlcpy(resume_entries[resume_count].path, resume_key, MAX_PATH);
        resume_count++;
    }
    resume_write();
}

/* ---- overlay ---------------------------------------------------------- */

/* As the theme loads them, so they draw in its colors.  pb_back.bmp is the fill
 * and pb.bmp the track: %pb(...,PBB,backdrop,PB). */
static void load_bar_bitmaps(void)
{
    if (have_bar_bitmaps)
        return;
    have_bar_bitmaps =
        read_bmp_file(BAR_BMP_DIR "/pb_back.bmp", &bar_fill, sizeof bar_fill_data,
                      FORMAT_ANY, NULL) > 0 &&
        read_bmp_file(BAR_BMP_DIR "/pb.bmp", &bar_track, sizeof bar_track_data,
                      FORMAT_ANY, NULL) > 0;
}

/* Each part of a panel has its own viewport, as in the theme: it clips the bar. */
static void panel_viewport(struct screen *display, int x, int y, int w, int h)
{
    static struct viewport vp;

    viewport_set_defaults(&vp, SCREEN_MAIN);
    vp.x = x;
    vp.y = y;
    vp.width = w;
    vp.height = h;
    vp.font = FONT_UI;
    vp.fg_pattern = global_settings.fg_color;
    vp.bg_pattern = global_settings.bg_color;
    display->set_viewport(&vp);
}

static void draw_bar(struct screen *display, int value, int max)
{
    if (max <= 0)
        max = 1;
    panel_viewport(display, BAR_X, BAR_Y, BAR_W, BAR_H);
    if (have_bar_bitmaps)
    {
        display->bmp_part(&bar_track, 0, 0, 0, 0, BAR_W, BAR_H);
        gui_bitmap_scrollbar_draw(display, &bar_fill, 0, 0, BAR_W, BAR_H,
                                  max, 0, value, HORIZONTAL | DONT_CLEAR_EXCESS);
    }
    else
        gui_scrollbar_draw(display, 0, 0, BAR_W, BAR_H, max, 0, value, HORIZONTAL);
    panel_viewport(display, BAR_X, TEXT_Y, BAR_W, TEXT_H);
}

static int display_position(void)
{
    return v.seek_target_ms >= 0 ? v.seek_target_ms : v.position_ms;
}

static const char *aspect_label(void)
{
    static const int labels[RETROHH_VIDEO_ASPECT_COUNT] = {
        LANG_UIPOD_ASPECT_FIT, LANG_UIPOD_ASPECT_STRETCH,
    };
    return str(labels[aspect]);
}

static void draw_progress(struct screen *display)
{
    char elapsed[16], remaining[16];
    int position = display_position(), w, h;

    draw_bar(display, position, v.duration_ms);

    format_time(elapsed, sizeof elapsed, position);
    format_time(remaining + 1, sizeof remaining - 1, v.duration_ms - position);
    remaining[0] = '-';

    display->putsxy(0, 0, elapsed);
    display->getstringsize(remaining, &w, &h);
    display->putsxy(BAR_W - w, 0, remaining);
    if (TIME_BEFORE(current_tick, v.aspect_label_until))
    {
        const char *label = aspect_label();

        display->getstringsize(label, &w, &h);
        display->putsxy((BAR_W - w) / 2, 0, label);
    }
}

static void draw_volume(struct screen *display)
{
    char label[32];
    int minvol = sound_min(SOUND_VOLUME), maxvol = sound_max(SOUND_VOLUME);
    int w, h;

    draw_bar(display,
             to_normalized_volume(global_status.volume, minvol, maxvol, 1000),
             1000);
    snprintf(label, sizeof label, "%s %d/%d", str(LANG_VOLUME),
             get_normalized_volume(), get_normalized_volume_steps());
    display->getstringsize(label, &w, &h);
    display->putsxy((BAR_W - w) / 2, 0, label);
}

static void srt_free(void)
{
    free(s_srt_cues);
    s_srt_cues = NULL;
    s_srt_cue_count = 0;
}

static int parse_srt_timestamp_ms(const char *s)
{
    int hh = 0, mm = 0, ss = 0, ms = 0;
    if (sscanf(s, "%d:%d:%d%*[,.:]%d", &hh, &mm, &ss, &ms) != 4)
        return -1;
    return ((hh * 3600 + mm * 60 + ss) * 1000) + ms;
}

static void strip_srt_tags(char *s)
{
    char *dst = s;
    bool in_tag = false;
    for (const char *p = s; *p; p++)
    {
        if (*p == '<' || *p == '{')
        {
            in_tag = true;
            continue;
        }
        if (*p == '>' || *p == '}')
        {
            in_tag = false;
            continue;
        }
        if (!in_tag && *p != '\r')
            *dst++ = *p;
    }
    *dst = '\0';
}

static bool srt_load_file(const char *srt_path)
{
    srt_free();
    if (!srt_path || !srt_path[0])
        return false;

    int fd = open(srt_path, O_RDONLY);
    if (fd < 0)
        return false;

    struct srt_cue *arr = calloc(MAX_SRT_CUES, sizeof(*arr));
    if (!arr)
    {
        close(fd);
        return false;
    }

    char line[256];
    int count = 0;
    while (count < MAX_SRT_CUES && read_line(fd, line, sizeof(line)) >= 0)
    {
        char *arrow = strstr(line, "-->");
        if (!arrow)
        {
            if (line[0] == '\0')
            {
                /* Check EOF */
                char ch;
                if (read(fd, &ch, 1) <= 0)
                    break;
                lseek(fd, -1, SEEK_CUR);
            }
            continue;
        }
        int st = parse_srt_timestamp_ms(line);
        int en = parse_srt_timestamp_ms(arrow + 3);
        if (st < 0 || en <= st)
            continue;

        struct srt_cue *c = &arr[count];
        c->start_ms = st;
        c->end_ms = en;
        c->line1[0] = '\0';
        c->line2[0] = '\0';

        int lno = 0;
        while (read_line(fd, line, sizeof(line)) > 0)
        {
            strip_srt_tags(line);
            if (!line[0])
                continue;
            if (lno == 0)
                strlcpy(c->line1, line, sizeof(c->line1));
            else if (lno == 1)
                strlcpy(c->line2, line, sizeof(c->line2));
            lno++;
        }
        if (c->line1[0])
            count++;
    }
    close(fd);

    if (count == 0)
    {
        free(arr);
        return false;
    }
    s_srt_cues = arr;
    s_srt_cue_count = count;
    return true;
}

static int srt_find_cue(int pos_ms)
{
    if (!s_srt_cues || s_srt_cue_count <= 0)
        return -1;
    int lo = 0, hi = s_srt_cue_count - 1;
    while (lo <= hi)
    {
        int mid = (lo + hi) / 2;
        if (pos_ms < s_srt_cues[mid].start_ms)
            hi = mid - 1;
        else if (pos_ms > s_srt_cues[mid].end_ms)
            lo = mid + 1;
        else
            return mid;
    }
    return -1;
}

static void draw_subtitle_cue(struct screen *display, const struct srt_cue *c)
{
    int w1 = 0, h1 = 0, w2 = 0, h2 = 0;
    panel_viewport(display, 0, SUB_STRIP_TOP, LCD_WIDTH, SUB_STRIP_ROWS);
    display->clear_viewport();

    display->getstringsize((const unsigned char *)c->line1, &w1, &h1);
    if (c->line2[0])
    {
        display->getstringsize((const unsigned char *)c->line2, &w2, &h2);
        int tot = h1 + 6 + h2;
        int y0 = (SUB_STRIP_ROWS - tot) / 2;
        if (y0 < 4) y0 = 4;
        display->putsxy((LCD_WIDTH - w1) / 2, y0, (const unsigned char *)c->line1);
        display->putsxy((LCD_WIDTH - w2) / 2, y0 + h1 + 6, (const unsigned char *)c->line2);
    }
    else
    {
        int y0 = (SUB_STRIP_ROWS - h1) / 2;
        if (y0 < 4) y0 = 4;
        display->putsxy((LCD_WIDTH - w1) / 2, y0, (const unsigned char *)c->line1);
    }
    display->set_viewport(NULL);
}

/* Redraws only on a visible change: every present costs power. */
static void overlay_update(void)
{
    enum overlay_panel panel = PANEL_NONE;
    int pos_ms = display_position();
    int seconds = pos_ms / 1000;
    bool aspect_label_on = TIME_BEFORE(current_tick, v.aspect_label_until);
    int cue_idx = (s_active_sub_idx >= 0) ? srt_find_cue(pos_ms) : -1;

    if (TIME_BEFORE(current_tick, v.volume_until))
        panel = PANEL_VOLUME;
    else if (v.paused || TIME_BEFORE(current_tick, v.progress_until))
        panel = PANEL_PROGRESS;

    if (v.drawn_once && panel == v.shown &&
        (panel != PANEL_NONE || cue_idx == v.shown_cue_idx) &&
        (panel != PANEL_PROGRESS || (seconds == v.shown_seconds &&
                                     aspect == v.shown_aspect &&
                                     aspect_label_on == v.shown_aspect_label)) &&
        (panel != PANEL_VOLUME || global_status.volume == v.shown_volume))
        return;

    v.shown = panel;
    v.shown_seconds = seconds;
    v.shown_aspect = aspect;
    v.shown_aspect_label = aspect_label_on;
    v.shown_volume = global_status.volume;
    v.shown_cue_idx = cue_idx;
    v.drawn_once = true;

    int rows = (panel != PANEL_NONE) ? STRIP_ROWS
             : (cue_idx >= 0 ? SUB_STRIP_ROWS : 0);
    retrohh_video_set_overlay(true, rows);
    if (panel != PANEL_NONE)
    {
        struct screen *display = &screens[SCREEN_MAIN];

        panel_viewport(display, 0, STRIP_TOP, LCD_WIDTH, STRIP_ROWS);
        display->clear_viewport();
        if (panel == PANEL_VOLUME)
            draw_volume(display);
        else
            draw_progress(display);
        display->set_viewport(NULL);
    }
    else if (cue_idx >= 0)
    {
        draw_subtitle_cue(&screens[SCREEN_MAIN], &s_srt_cues[cue_idx]);
    }
    screens[SCREEN_MAIN].update();
}

static void show_progress(void)
{
    v.progress_until = current_tick + OVERLAY_TICKS;
    v.volume_until = current_tick;      /* a control press swaps straight back */
}

static void queue_seek(int position_ms, long delay)
{
    int last = v.duration_ms - SEEK_END_MARGIN_MS;

    if (position_ms > last)
        position_ms = last;
    if (position_ms < 0)
        position_ms = 0;
    v.seek_target_ms = position_ms;
    v.seek_send_tick = current_tick + delay;
    show_progress();
}

static void send_queued_seek(void)
{
    if (v.seek_target_ms < 0 || TIME_BEFORE(current_tick, v.seek_send_tick))
        return;

    int target = v.seek_target_ms;
    v.seek_target_ms = -1;
    v.stalled_polls = 0;

    if (s_stream_opts && s_stream_opts->is_transcoded)
    {
        /* Live MPEG-TS transcode has no byte-range index: restart transcode at StartTimeTicks. */
        retrohh_video_stop();
        s_stream_base_ms = target;
        if (s_stream_opts->on_reopen_at)
            s_stream_opts->on_reopen_at(s_stream_opts->cb_ctx, target);
        v.reported = false;
        retrohh_video_start(s_stream_opts->url, 0);
        if (v.paused)
            retrohh_video_pause(true);
        v.position_ms = target;
        v.ignore_report_until = current_tick + 2 * HZ;
        return;
    }

    retrohh_video_seek(target);
    v.position_ms = target;
    v.ignore_report_until = current_tick + SEEK_REPORT_TICKS;
}

static void skip_chapter(bool forward)
{
    int position = display_position();

    if (!info.chapters)
    {
        show_progress();
        return;
    }
    if (forward)
    {
        for (int i = 0; i < info.chapters; i++)
            if (info.chapter_ms[i] > position)
            {
                queue_seek(info.chapter_ms[i], 0);
                return;
            }
        show_progress();                /* already in the last chapter */
        return;
    }
    for (int i = info.chapters - 1; i >= 0; i--)
        if (info.chapter_ms[i] + CHAPTER_RESTART_MS < position)
        {
            queue_seek(info.chapter_ms[i], 0);
            return;
        }
    queue_seek(0, 0);
}

static void set_paused(bool paused)
{
    v.paused = paused;
    retrohh_video_pause(paused);
    show_progress();
    if (s_stream_opts && s_stream_opts->on_progress && v.reported)
        s_stream_opts->on_progress(s_stream_opts->cb_ctx, display_position(), paused, false);
}

static void pause_and_save(void)
{
    set_paused(true);
    if (v.reported)
        resume_save(display_position(), v.duration_ms);
}

/* Opaque black, overlay included: the video layer below can't show until the
 * window goes clear. */
static void hide_video(void)
{
    struct screen *display = &screens[SCREEN_MAIN];

    uipod_gpu_prepare_cpu_fallback();
    retrohh_video_set_overlay(false, 0);
    display->set_background(LCD_BLACK);
    display->clear_display();
    display->set_background(global_settings.bg_color);
    display->update();
}

/* Behind an opaque window, which clears once the picture is where it should
 * be. */
static bool start_player(const char *path, int start_ms)
{
    long wait_ticks = s_stream_opts ? STREAM_REVEAL_WAIT_TICKS : REVEAL_WAIT_TICKS;
    int player_start = (s_stream_opts && s_stream_opts->is_transcoded) ? 0 : start_ms;

    hide_video();
    if (!retrohh_video_start(path, player_start))
        return false;
    v.position_ms = start_ms;
    if (s_stream_opts && s_stream_opts->duration_ms > 0)
        v.duration_ms = s_stream_opts->duration_ms;
    v.seek_target_ms = -1;
    v.reveal_by = current_tick + wait_ticks;
    v.ignore_report_until = player_start ? v.reveal_by : current_tick;
    return true;
}

/* False once the video is over. */
static bool poll_player(void)
{
    int position, duration;
    bool playing;

    reset_poweroff_timer();             /* watching isn't idling */

    if (!retrohh_video_running())
        return false;
    if (!retrohh_video_status(&position, &duration, &playing))
        return true;

    int eff_dur = duration;
    if (eff_dur <= 0 && s_stream_opts && s_stream_opts->duration_ms > 0)
        eff_dur = s_stream_opts->duration_ms;

    int eff_pos = (s_stream_opts && s_stream_opts->is_transcoded)
                      ? (s_stream_base_ms + position)
                      : position;
    if (eff_dur > 0 && eff_pos > eff_dur)
        eff_pos = eff_dur;

    v.duration_ms = eff_dur;
    if (!v.reported)
    {
        v.reported = true;
        retrohh_video_set_aspect(&info, aspect);
        if (s_stream_opts && s_stream_opts->on_progress)
            s_stream_opts->on_progress(s_stream_opts->cb_ctx, eff_pos, v.paused, false);
    }

    int target_reveal = (s_stream_opts && s_stream_opts->is_transcoded) ? 0 : (resume_start_ms / 2);
    if (position >= target_reveal || !TIME_BEFORE(current_tick, v.reveal_by))
        v.revealed = true;
    if (TIME_BEFORE(current_tick, v.ignore_report_until))
        return true;
    v.position_ms = eff_pos;

    if (v.paused)
        return true;
    if (!playing)
        return false;
    /* The player can sit on the last frame without saying it finished. */
    if (eff_dur > 0 && eff_pos >= eff_dur - END_MARGIN_MS &&
        eff_pos == v.last_reported_ms)
    {
        if (++v.stalled_polls >= END_STALL_POLLS)
            return false;
    }
    else
        v.stalled_polls = 0;
    v.last_reported_ms = eff_pos;
    return true;
}

/* ---- Video Settings (`Context Menu - Video Player` #11301) ------------ */

enum
{
    VCTX_ROW_ASPECT = 0,
    VCTX_ROW_AUDIO  = 1,
    VCTX_ROW_SUBS   = 2,
    VCTX_ROW_COUNT  = 3,
};

struct video_ctx_page
{
    struct uipod_page   base;
    struct gui_synclist lists;
    int                 audio_idx;
    int                 sub_idx; /* -1 = Off, 0..sub_count-1 */
    char                indic_buf[3][64];
};

static const char *vctx_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    (void)data; (void)buf; (void)buf_len;
    switch (sel)
    {
        case VCTX_ROW_ASPECT: return str(LANG_UIPOD_ASPECT_MODE);
        case VCTX_ROW_AUDIO:  return str(LANG_UIPOD_AUDIO_TRACK);
        case VCTX_ROW_SUBS:   return str(LANG_UIPOD_SUBTITLES);
        default:              return "";
    }
}

static const char *vctx_get_indic(int sel, void *data)
{
    struct video_ctx_page *p = (struct video_ctx_page *)data;
    if (!p || sel < 0 || sel >= VCTX_ROW_COUNT)
        return NULL;

    if (sel == VCTX_ROW_ASPECT)
    {
        snprintf(p->indic_buf[0], sizeof(p->indic_buf[0]), "%s", aspect_label());
        return p->indic_buf[0];
    }
    if (sel == VCTX_ROW_AUDIO)
    {
        if (info.audio_count > 0 && p->audio_idx >= 0 && p->audio_idx < info.audio_count)
            snprintf(p->indic_buf[1], sizeof(p->indic_buf[1]), "%s",
                     info.audio_tracks[p->audio_idx].label);
        else
            snprintf(p->indic_buf[1], sizeof(p->indic_buf[1]), "Track 1");
        return p->indic_buf[1];
    }
    if (p->sub_idx < 0 || p->sub_idx >= info.sub_count)
        snprintf(p->indic_buf[2], sizeof(p->indic_buf[2]), "%s", str(LANG_OFF));
    else
        snprintf(p->indic_buf[2], sizeof(p->indic_buf[2]), "%s",
                 info.sub_tracks[p->sub_idx].label);
    return p->indic_buf[2];
}

static void vctx_cycle_row(struct video_ctx_page *p, int sel, int dir)
{
    if (sel == VCTX_ROW_ASPECT)
    {
        int n = (int)RETROHH_VIDEO_ASPECT_COUNT;
        aspect = (enum retrohh_video_aspect)(((int)aspect + dir + n) % n);
    }
    else if (sel == VCTX_ROW_AUDIO && info.audio_count > 1)
    {
        int n = info.audio_count;
        p->audio_idx = (p->audio_idx + dir + n) % n;
    }
    else if (sel == VCTX_ROW_SUBS && info.sub_count > 0)
    {
        /* Cycle through -1 (Off), 0, 1, ..., sub_count - 1 */
        int total = info.sub_count + 1;
        int cur   = p->sub_idx + 1;
        cur = (cur + dir + total) % total;
        p->sub_idx = cur - 1;
    }
}

static void vctx_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct video_ctx_page *)self)->lists);
}

static int vctx_poll(struct uipod_page *self, int timeout)
{
    int action = get_action(self->context, timeout);
    if (list_do_action_timeout(&((struct video_ctx_page *)self)->lists, action))
        return ACTION_NONE;
    return action;
}

static enum uipod_page_result vctx_on_action(struct uipod_page *self, int action)
{
    struct video_ctx_page *p = (struct video_ctx_page *)self;
    int sel = gui_synclist_get_sel_pos(&p->lists);

    if (action == ACTION_STD_CANCEL || action == ACTION_STD_CONTEXT)
    {
        uipod_transition_arm_back();
        return UIPOD_PAGE_DONE;
    }
    if (action == ACTION_SETTINGS_INC || action == ACTION_STD_OK)
    {
        vctx_cycle_row(p, sel, +1);
        return UIPOD_PAGE_STAY;
    }
    if (action == ACTION_SETTINGS_DEC)
    {
        vctx_cycle_row(p, sel, -1);
        return UIPOD_PAGE_STAY;
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable vctx_vtable =
{
    .legend    = NULL,
    .draw      = vctx_draw,
    .poll      = vctx_poll,
    .on_action = vctx_on_action,
};

static const int vctx_allowed[] =
{
    ACTION_STD_OK, ACTION_STD_CANCEL, ACTION_STD_CONTEXT,
    ACTION_SETTINGS_INC, ACTION_SETTINGS_DEC, -1
};

static void open_video_settings_menu(void)
{
    bool was_paused = v.paused;
    if (!was_paused)
        retrohh_video_pause(true);

    hide_video();
    FOR_NB_SCREENS(i)
        viewportmanager_theme_undo(i, true);

    struct video_ctx_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt      = &vctx_vtable;
    p.base.context = CONTEXT_SETTINGS_RECTRIGGER;
    p.base.allowed = vctx_allowed;
    p.base.title   = str(LANG_UIPOD_VIDEO_SETTINGS);
    p.audio_idx    = s_active_audio_idx;
    p.sub_idx      = s_active_sub_idx;

    gui_synclist_init(&p.lists, vctx_get_name, &p, false, 1, NULL);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_UIPOD_VIDEO_SETTINGS), Icon_NOICON);
    gui_synclist_set_indicator_callback(&p.lists, vctx_get_indic);
    gui_synclist_set_nb_items(&p.lists, VCTX_ROW_COUNT);
    gui_synclist_select_item(&p.lists, 0);

    uipod_page_run(&p.base);

    /* Apply updated video aspect, audio stream, and subtitles */
    retrohh_video_set_aspect(&info, aspect);

    if (p.audio_idx != s_active_audio_idx)
    {
        s_active_audio_idx = p.audio_idx;
        retrohh_video_set_audio_track(s_active_audio_idx);
    }

    if (p.sub_idx != s_active_sub_idx)
    {
        s_active_sub_idx = p.sub_idx;
        if (s_active_sub_idx < 0 || s_active_sub_idx >= info.sub_count)
        {
            srt_free();
            s_active_sub_idx = -1;
        }
        else
        {
            char srt_path[MAX_PATH];
            if (!retrohh_video_extract_subtitle_srt(s_active_play_path,
                                                    &info.sub_tracks[s_active_sub_idx],
                                                    srt_path, sizeof(srt_path)) ||
                !srt_load_file(srt_path))
            {
                srt_free();
                s_active_sub_idx = -1;
            }
        }
    }

    FOR_NB_SCREENS(i)
        viewportmanager_theme_enable(i, false, NULL);

    hide_video();
    v.drawn_once = false;
    v.shown = PANEL_NONE;
    v.shown_cue_idx = -1;

    if (!p.base.home && !was_paused)
        retrohh_video_pause(false);

    if (p.base.home)
        uipod_home_pending = true;
}

static void run_session(void)
{
    long next_poll = current_tick;

    for (;;)
    {
        int action = get_action(CONTEXT_TP_VIDEO, ACTION_WAIT_TICKS);

        if (action == SYS_MEDIA_KEY)            /* a headset: as A, R2, L2 */
        {
            int command = button_get_data();

            action = command == MEDIA_KEY_NEXT ? ACTION_TP_CHAP_NEXT :
                     command == MEDIA_KEY_PREV ? ACTION_TP_CHAP_PREV :
                                                 ACTION_WPS_PLAY;
        }

        if (action & SYS_EVENT)
        {
            long handled = default_event_handler(action);

            if (handled == SYS_POWEROFF || handled == SYS_REBOOT)
                return;
            /* Headphones gone, from the jack, USB-C or Bluetooth: pause, as
             * music does.  The sound follows the output on its own. */
            if (action == SYS_PHONE_UNPLUGGED && !v.paused)
                pause_and_save();
        }
        else switch (action)
        {
            case ACTION_TP_HOME:
                uipod_home_pending = true;
                return;
            case ACTION_WPS_BROWSE:
                /* While still buffering (!revealed) or with overlay up, B leaves immediately. */
                if (!v.revealed || v.paused || v.shown != PANEL_NONE)
                    return;
                pause_and_save();
                break;
            case ACTION_WPS_PLAY:
                if (v.paused)
                    set_paused(false);
                else
                    pause_and_save();
                break;
            case ACTION_WPS_SEEKBACK:
                queue_seek(display_position() - SEEK_STEP_MS, SEEK_SETTLE_TICKS);
                break;
            case ACTION_WPS_SEEKFWD:
                queue_seek(display_position() + SEEK_STEP_MS, SEEK_SETTLE_TICKS);
                break;
            case ACTION_TP_CHAP_PREV:
                skip_chapter(false);
                break;
            case ACTION_TP_CHAP_NEXT:
                skip_chapter(true);
                break;
            case ACTION_TP_VIDEO_ASPECT:
                aspect = (aspect + 1) % RETROHH_VIDEO_ASPECT_COUNT;
                retrohh_video_set_aspect(&info, aspect);
                v.aspect_label_until = current_tick + ASPECT_LABEL_TICKS;
                v.volume_until = current_tick;
                if (TIME_BEFORE(v.progress_until, v.aspect_label_until))
                    v.progress_until = v.aspect_label_until;
                break;
            case ACTION_STD_CONTEXT:
                open_video_settings_menu();
                if (uipod_home_pending)
                    return;
                show_progress();
                break;
            default:
                break;
        }

        if (power_display_off() && !v.paused)
            set_paused(true);

        send_queued_seek();

        if (global_status.volume != v.last_volume)
        {
            v.last_volume = global_status.volume;
            v.volume_until = current_tick + VOLUME_TICKS;
        }

        if (!TIME_BEFORE(current_tick, next_poll))
        {
            if (!poll_player())
                return;
            next_poll = current_tick + (v.revealed ? POLL_TICKS : 0);
            if (v.reported && !TIME_BEFORE(current_tick, resume_next_save))
            {
                int pos_now = display_position();
                resume_next_save = current_tick + RESUME_SAVE_TICKS;
                resume_save(pos_now, v.duration_ms);
                if (s_stream_opts && s_stream_opts->on_progress)
                    s_stream_opts->on_progress(s_stream_opts->cb_ctx, pos_now, v.paused, false);
            }
        }
        if (v.revealed)
            overlay_update();
    }
}

static void run_video_common(const char *play_target)
{
    if (audio_status())
        audio_stop();
    load_bar_bitmaps();

    retrohh_cpu_hold_dynamic();
    backlight_set_timeout(0);           /* a video is active use, not idle */
    FOR_NB_SCREENS(i)
        viewportmanager_theme_enable(i, false, NULL);

    memset(&v, 0, sizeof v);
    v.last_volume = global_status.volume;
    v.shown_cue_idx = -1;
    s_active_audio_idx = 0;
    s_active_sub_idx = -1;
    strlcpy(s_active_play_path, play_target ? play_target : "", sizeof(s_active_play_path));
    srt_free();
    if (start_player(play_target, resume_start_ms))
    {
        button_clear_queue();
        run_session();
    }
    srt_free();

    int final_pos = display_position();
    int final_dur = v.duration_ms;
    bool was_reported = v.reported;

    /* Picture and overlay go in one frame; the player exits behind it. */
    hide_video();
    if (was_reported)
        resume_save(final_pos, final_dur);
    retrohh_video_stop();
    if (s_stream_opts && s_stream_opts->on_progress && was_reported)
        s_stream_opts->on_progress(s_stream_opts->cb_ctx, resume_saved_ms, false, true);

    FOR_NB_SCREENS(i)
        viewportmanager_theme_undo(i, true);
    if (power_display_off())
        backlight_set_timeout_quiet(global_settings.backlight_timeout);
    else
        backlight_set_timeout(global_settings.backlight_timeout);
    retrohh_cpu_release_dynamic();
    button_clear_queue();
    button_touch_activity();

    if (!was_reported)
        splash(2 * HZ, ID2P(LANG_UIPOD_VIDEO_FAILED));
}

void uipod_video_play(const char *path)
{
    s_stream_opts = NULL;
    s_stream_base_ms = 0;

    if (retrohh_video_probe(path, &info) && info.hevc)
    {
        splash(2 * HZ, ID2P(LANG_UIPOD_VIDEO_HEVC));
        return;
    }

    resume_open_ex(path, NULL, 0, false);
    run_video_common(path);
}

void uipod_video_play_stream(const struct uipod_video_stream_opts *opts)
{
    if (!opts || !opts->url || !opts->url[0])
        return;

    memset(&info, 0, sizeof(info));
    info.hevc      = false;
    info.width     = opts->width > 0 ? opts->width : 1024;
    info.height    = opts->height > 0 ? opts->height : 576;
    info.ratio_num = opts->ratio_num;
    info.ratio_den = opts->ratio_den;
    if (opts->chapter_ms && opts->chapters > 0)
    {
        int n = opts->chapters;
        if (n > RETROHH_VIDEO_MAX_CHAPTERS)
            n = RETROHH_VIDEO_MAX_CHAPTERS;
        memcpy(info.chapter_ms, opts->chapter_ms, (size_t)n * sizeof(int));
        info.chapters = n;
    }

    /* If playing a local downloaded file without pre-known dimensions, probe it cleanly */
    if (opts->url[0] == '/' && opts->width <= 0)
        retrohh_video_probe(opts->url, &info);

    resume_open_ex(opts->url, opts->resume_id, opts->server_resume_ms, opts->force_from_start);

    s_stream_opts = opts;
    s_stream_base_ms = opts->is_transcoded ? resume_start_ms : 0;
    if (opts->is_transcoded && opts->on_reopen_at)
        opts->on_reopen_at(opts->cb_ctx, s_stream_base_ms);

    run_video_common(opts->url);
    s_stream_opts = NULL;
    s_stream_base_ms = 0;
}
