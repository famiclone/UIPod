/***************************************************************************
 * UIPod: Native NSF/NSFe 1024x768 UI screens & helpers (see uipod_nsf_ui.h).
 *
 * Implements:
 *   1. Now Playing -> `Channels ›` live hardware APU channel toggle submenu
 *      with real-time `ApuFrameTelemetry` activity meters, `[ON]`/`[OFF]`
 *      toggle pills, `Hold A` Solo channel, and `X` / `Enable All Channels`.
 *   2. File Browser virtual `.nsf` / `.nsfe` container drill-down:
 *      - `Shuffle Game Music`
 *      - Indexed `TrackKind::Music` subtracks (`01. Title Theme   1:42`)
 *      - Collapsed `Sound Effects (N) ›` virtual subfolder at bottom
 *      - Per-subtrack context menu (`Play`, `Play Next`, `Add to Play Queue`,
 *        `Add to Playlist...` persisting `/path/Game.nsf#idx`, `Track Info`)
 ****************************************************************************/
#include "config.h"

#include <ctype.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

#include "action.h"
#include "audio.h"
#include "button.h"
#include "file.h"
#include "filetypes.h"
#include "gui/list.h"
#include "gui/viewport.h"
#include "icon.h"
#include "kernel.h"
#include "lang.h"
#include "lcd.h"
#include "misc.h"
#include "playlist.h"
#include "root_menu.h"
#include "screens.h"
#include "settings.h"
#include "string-extra.h"
#include "system.h"
#include "uipod_library.h"
#include "uipod_nsf_ui.h"
#include "uipod_page.h"
#include "uipod_playlists.h"
#include "uipod_plugins.h"
#include "uipod_rs_codecs.h"
#include "uipod_rs_ui.h"
#include "uipod_svg.h"
#include "uipod_transition.h"
#include "uipod_ui.h"
#include "window-sdl.h"

#define NSF_MAX_SUBTRACKS     256
#define NSF_HOLD_SOLO_MS      420UL
#define NSF_HOLD_HOME_MS      650UL

static bool nsf_is_ukrainian(void)
{
    return strcmp((const char *)global_settings.lang_file, "ukrainian") == 0;
}

bool uipod_nsf_is_container_file(const char *path)
{
    if (!path || !path[0])
        return false;

    /* Canonical subtrack URIs (`Game.nsf#3`) are virtual tracks, not bare containers */
    char scratch[MAX_PATH];
    if (uipod_rs_nsf_parse_uri(path, scratch, sizeof(scratch)) >= 0)
        return false;

    const char *slash = strrchr(path, '/');
    const char *base  = slash ? slash + 1 : path;
    const char *dot   = strrchr(base, '.');
    if (!dot)
        return false;
    return strcasecmp(dot, ".nsf") == 0 || strcasecmp(dot, ".nsfe") == 0;
}

const char *uipod_nsf_chip_badge_str(uint8_t chip_flags)
{
    if (chip_flags & 0x20) return "5B";
    if (chip_flags & 0x10) return "N163";
    if (chip_flags & 0x02) return "VRC7";
    if (chip_flags & 0x04) return "FDS";
    if (chip_flags & 0x08) return "MMC5";
    if (chip_flags & 0x01) return "VRC6";
    return "2A03";
}

char *uipod_nsf_channels_menu_label(int selected_item,
                                    void *data,
                                    char *buffer,
                                    size_t buffer_len)
{
    (void)selected_item;
    (void)data;

    bool uk = nsf_is_ukrainian();
    const char *title = uk ? "Канали" : "Channels";
    int flags = uipod_rs_nsf_get_active_chip_flags();
    if (flags < 0)
    {
        strlcpy(buffer, title, buffer_len);
        return buffer;
    }

    struct uipod_rs_nsf_channel_desc descs[8];
    size_t n = uipod_rs_nsf_get_channel_descriptors((uint8_t)flags, descs, 8);
    if (n == 0)
        n = 5;

    uint16_t mask = uipod_rs_nsf_get_channel_mute_mask();
    size_t enabled = 0;
    for (size_t i = 0; i < n; i++)
    {
        if ((mask & (uint16_t)(1u << descs[i].bit_index)) == 0)
            enabled++;
    }

    if (enabled == n)
    {
        snprintf(buffer, buffer_len, "%s\x1e%s",
                 title, uk ? "Усі УВІМК" : "All ON");
    }
    else
    {
        snprintf(buffer, buffer_len, "%s\x1e%zu/%zu %s",
                 title, enabled, n, uk ? "УВІМК" : "ON");
    }
    return buffer;
}

/* =========================================================================
 * 1. Now Playing -> `Channels ›` Live Toggle Submenu (`uipod_nsf_channels_screen`)
 * ========================================================================= */

struct nsf_channels_page
{
    struct uipod_page                base;
    int                              chip_flags;
    struct uipod_rs_nsf_channel_desc descs[8];
    int                              channel_count;
    int                              selected_row;
    int                              start_row;

    /* Repeat & hold-gesture tracking */
    long                             prev_held;
    unsigned long                    ud_next_ms;
    unsigned long                    a_down_ms;
    bool                             a_was_down;
    bool                             a_solo_fired;
    unsigned long                    b_down_ms;
    bool                             b_was_down;
    bool                             b_home_fired;
};

static void nsf_channels_sync_descriptors(struct nsf_channels_page *p)
{
    int flags = uipod_rs_nsf_get_active_chip_flags();
    if (flags < 0)
        flags = (p->chip_flags >= 0) ? p->chip_flags : 0;
    if (flags != p->chip_flags || p->channel_count <= 0)
    {
        p->chip_flags = flags;
        size_t n = uipod_rs_nsf_get_channel_descriptors((uint8_t)flags, p->descs, 8);
        p->channel_count = (int)((n > 0 && n <= 8) ? n : 5);
    }

    int total_rows = p->channel_count + 1; /* +1 for `Enable All Channels` */
    if (p->selected_row < 0)
        p->selected_row = 0;
    if (p->selected_row >= total_rows)
        p->selected_row = total_rows - 1;

    int vis = uipod_gpu_visible_list_rows();
    if (vis <= 0)
        vis = UIPOD_RS_LIST_VISIBLE_ROWS;
    if (total_rows <= vis)
    {
        p->start_row = 0;
    }
    else
    {
        if (p->selected_row < p->start_row)
            p->start_row = p->selected_row;
        if (p->selected_row >= p->start_row + vis)
            p->start_row = p->selected_row - vis + 1;
        if (p->start_row > total_rows - vis)
            p->start_row = total_rows - vis;
        if (p->start_row < 0)
            p->start_row = 0;
    }
}

static uint16_t nsf_channels_valid_mask(const struct nsf_channels_page *p)
{
    uint16_t m = 0;
    for (int i = 0; i < p->channel_count; i++)
        m |= (uint16_t)(1u << p->descs[i].bit_index);
    return m;
}

static void nsf_channels_toggle_row(struct nsf_channels_page *p, int row)
{
    if (row == p->channel_count)
    {
        uipod_rs_nsf_set_channel_mute_mask(0);
        return;
    }
    if (row >= 0 && row < p->channel_count)
    {
        uint16_t bit  = (uint16_t)(1u << p->descs[row].bit_index);
        uint16_t mask = uipod_rs_nsf_get_channel_mute_mask();
        uipod_rs_nsf_set_channel_mute_mask((uint16_t)(mask ^ bit));
    }
}

static void nsf_channels_solo_row(struct nsf_channels_page *p, int row)
{
    if (row < 0 || row >= p->channel_count)
        return;
    uint16_t bit        = (uint16_t)(1u << p->descs[row].bit_index);
    uint16_t valid_mask = nsf_channels_valid_mask(p);
    uint16_t solo_mask  = (uint16_t)(valid_mask & ~bit);
    uint16_t cur_mask   = (uint16_t)(uipod_rs_nsf_get_channel_mute_mask() & valid_mask);

    if (cur_mask == solo_mask)
        uipod_rs_nsf_set_channel_mute_mask(0);
    else
        uipod_rs_nsf_set_channel_mute_mask(solo_mask);
}

static void nsf_channels_draw_sw_fallback(const struct nsf_channels_page *p,
                                          uint16_t mute_mask)
{
    bool uk = nsf_is_ukrainian();
    struct screen *s = &screens[SCREEN_MAIN];
    struct viewport vp = {0};
    viewport_set_defaults(&vp, SCREEN_MAIN);
    s->set_viewport(&vp);
    s->set_background(global_settings.bg_color);
    s->clear_viewport();

    int total_rows = p->channel_count + 1;
    int vis = UIPOD_RS_MAX_VISIBLE_ROWS;
    int row_h = vp.height / vis;
    for (int r = 0; r < vis && (p->start_row + r) < total_rows; r++)
    {
        int idx = p->start_row + r;
        int y = r * row_h + 12;
        bool sel = (idx == p->selected_row);
        char line[96];
        if (idx < p->channel_count)
        {
            bool en = (mute_mask & (uint16_t)(1u << p->descs[idx].bit_index)) == 0;
            snprintf(line, sizeof(line), "%s %s   [%s]",
                     sel ? ">" : " ",
                     p->descs[idx].display_name,
                     en ? "ON" : "OFF");
        }
        else
        {
            snprintf(line, sizeof(line), "%s %s",
                     sel ? ">" : " ",
                     uk ? "Увімкнути всі канали" : "Enable All Channels");
        }
        s->set_foreground(sel ? global_settings.fg_color : global_settings.fg2_color);
        s->putsxy(24, y, (const unsigned char *)line);
    }

    s->update_viewport();
    s->set_viewport(NULL);
}

static void nsf_channels_page_draw(struct uipod_page *self)
{
    struct nsf_channels_page *p = (struct nsf_channels_page *)self;
    nsf_channels_sync_descriptors(p);

    bool uk = nsf_is_ukrainian();
    const char *title = uk ? "Канали" : "Channels";
    const char *enable_all_lbl = uk ? "Увімкнути всі канали" : "Enable All Channels";
    const char *footer_hint = uk
        ? "A: Перемкнути  ·  Утрим. A: Соло  ·  X: Усі УВІМК"
        : "A: Toggle  ·  Hold A: Solo  ·  X: Enable All";

    uint16_t mute_mask  = uipod_rs_nsf_get_channel_mute_mask();
    uint16_t valid_mask = nsf_channels_valid_mask(p);
    struct uipod_rs_apu_telemetry tel;
    memset(&tel, 0, sizeof(tel));
    bool have_tel = (uipod_rs_nsf_get_telemetry(&tel) != 0);
    bool paused   = (audio_status() & AUDIO_STATUS_PAUSE) != 0;

    struct UIPodRsNsfChannelRow ch_rows[8];
    memset(ch_rows, 0, sizeof(ch_rows));
    for (int i = 0; i < p->channel_count && i < 8; i++)
    {
        uint16_t bit = (uint16_t)(1u << p->descs[i].bit_index);
        bool enabled = (mute_mask & bit) == 0;
        bool soloed  = (p->channel_count > 1) &&
                       ((mute_mask & valid_mask) == (uint16_t)(valid_mask & ~bit));
        float vol = 0.0f;
        if (enabled && !paused && have_tel && i < (int)tel.channel_count)
        {
            vol = tel.channels[i].volume;
            if (vol < 0.0f) vol = 0.0f;
            if (vol > 1.0f) vol = 1.0f;
        }
        ch_rows[i].short_code   = p->descs[i].short_code;
        ch_rows[i].display_name = p->descs[i].display_name;
        ch_rows[i].enabled      = enabled;
        ch_rows[i].soloed       = soloed;
        ch_rows[i].active       = enabled && (vol > 0.02f);
        ch_rows[i].volume       = vol;
        ch_rows[i].selected     = (i == p->selected_row);
    }

    int vis = uipod_gpu_visible_list_rows();
    if (vis <= 0)
        vis = UIPOD_RS_LIST_VISIBLE_ROWS;

    if (uipod_gpu_render_nsf_channels(title,
                                      ch_rows,
                                      (size_t)p->channel_count,
                                      enable_all_lbl,
                                      p->selected_row == p->channel_count,
                                      footer_hint,
                                      (size_t)p->start_row,
                                      (size_t)vis))
    {
        return;
    }
    nsf_channels_draw_sw_fallback(p, mute_mask);
}

static int nsf_channels_page_poll(struct uipod_page *self, int timeout)
{
    (void)timeout;
    struct nsf_channels_page *p = (struct nsf_channels_page *)self;

    long ev = button_get_w_tmo(HZ / 30);
    (void)uipod_gpu_tick_volume_popup();

    if (ev != BUTTON_NONE && (ev & SYS_EVENT) != 0)
        return (int)ev;

    /* If playback stopped or moved to a non-NSF track, leave cleanly */
    if (!(audio_status() & AUDIO_STATUS_PLAY) ||
        uipod_rs_nsf_get_active_chip_flags() < 0)
    {
        return ACTION_STD_CANCEL;
    }

    long held = button_status();
    unsigned long now_ms = sdl_get_ms();
    (void)uipod_gpu_check_window_resized();

#if defined(BUTTON_VOL_UP) && defined(BUTTON_VOL_DOWN)
    if (ev != BUTTON_NONE && !(ev & BUTTON_REL))
    {
        long base_ev = ev & ~(BUTTON_REPEAT | BUTTON_REL);
        if ((base_ev & (BUTTON_VOL_UP | BUTTON_VOL_DOWN)) != 0)
        {
#if defined(BUTTON_SELECT) && defined(HAVE_BACKLIGHT_BRIGHTNESS)
            if ((held & BUTTON_SELECT) != 0)
                adjust_brightness((base_ev & BUTTON_VOL_UP) ? 1 : -1);
            else
#endif
                adjust_volume((base_ev & BUTTON_VOL_UP) ? 1 : -1);
        }
    }
#endif

#ifdef BUTTON_START
    if (ev == (BUTTON_START | BUTTON_REL))
    {
        int st = audio_status();
        if (st & AUDIO_STATUS_PLAY)
        {
            if (st & AUDIO_STATUS_PAUSE)
                audio_resume();
            else
                audio_pause();
        }
    }
#endif

    /* X button shortcut: Enable All Channels */
    if (ev == (BUTTON_X | BUTTON_REL))
    {
        uipod_rs_nsf_set_channel_mute_mask(0);
        uipod_menu_click();
    }

#ifdef BUTTON_MENU
    if (ev == (BUTTON_MENU | BUTTON_REL) && p->selected_row < p->channel_count)
    {
        nsf_channels_solo_row(p, p->selected_row);
        uipod_menu_click();
    }
#endif

    /* B tap -> back; Hold-B >= 650 ms -> Home */
    bool b_down = (held & BUTTON_B) != 0;
    if (b_down && !p->b_was_down)
    {
        p->b_was_down   = true;
        p->b_down_ms    = now_ms;
        p->b_home_fired = false;
    }
    else if (b_down && p->b_was_down && !p->b_home_fired)
    {
        if (now_ms - p->b_down_ms >= NSF_HOLD_HOME_MS)
        {
            p->b_home_fired = true;
            uipod_menu_click();
            return ACTION_TP_HOME;
        }
    }
    else if (!b_down && p->b_was_down)
    {
        bool was_home = p->b_home_fired;
        p->b_was_down   = false;
        p->b_home_fired = false;
        if (!was_home)
        {
            uipod_menu_click();
            return ACTION_STD_CANCEL;
        }
    }

    /* A tap -> toggle row / Enable All; Hold-A >= 420 ms -> Solo channel */
    bool a_down = (held & BUTTON_A) != 0;
    if (a_down && !p->a_was_down)
    {
        p->a_was_down   = true;
        p->a_down_ms    = now_ms;
        p->a_solo_fired = false;
    }
    else if (a_down && p->a_was_down && !p->a_solo_fired)
    {
        if (now_ms - p->a_down_ms >= NSF_HOLD_SOLO_MS &&
            p->selected_row < p->channel_count)
        {
            p->a_solo_fired = true;
            nsf_channels_solo_row(p, p->selected_row);
            uipod_menu_click();
        }
    }
    else if (!a_down && p->a_was_down)
    {
        bool was_solo = p->a_solo_fired;
        p->a_was_down   = false;
        p->a_solo_fired = false;
        if (!was_solo)
        {
            nsf_channels_toggle_row(p, p->selected_row);
            uipod_menu_click();
        }
    }

    /* LEFT / RIGHT tap: toggle selected channel live */
    bool lr_press =
        (((held & (BUTTON_LEFT | BUTTON_RIGHT)) != 0) &&
         ((p->prev_held & (BUTTON_LEFT | BUTTON_RIGHT)) == 0));
    if (lr_press && p->selected_row < p->channel_count)
    {
        nsf_channels_toggle_row(p, p->selected_row);
        uipod_menu_click();
    }

    /* UP / DOWN navigation with repeat */
    int total_rows = p->channel_count + 1;
    int ud_dir  = ((held & BUTTON_DOWN) != 0) ? 1 : (((held & BUTTON_UP) != 0) ? -1 : 0);
    int prev_ud = ((p->prev_held & BUTTON_DOWN) != 0) ? 1 : (((p->prev_held & BUTTON_UP) != 0) ? -1 : 0);
    if (ud_dir != 0 && total_rows > 0)
    {
        bool step = false;
        if (ud_dir != prev_ud)
        {
            step = true;
            p->ud_next_ms = now_ms + 260UL;
        }
        else if (now_ms >= p->ud_next_ms)
        {
            step = true;
            p->ud_next_ms = now_ms + 95UL;
        }
        if (step)
        {
            p->selected_row = (p->selected_row + ud_dir + total_rows) % total_rows;
            nsf_channels_sync_descriptors(p);
            uipod_menu_click();
        }
    }

    p->prev_held = held;

    /* Live 30 FPS redraw so APU activity meters track music in real time */
    nsf_channels_page_draw(self);
    return ACTION_NONE;
}

static enum uipod_page_result nsf_channels_page_on_action(struct uipod_page *self,
                                                          int action)
{
    (void)self;
    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable nsf_channels_vtable = {
    .legend    = NULL,
    .draw      = nsf_channels_page_draw,
    .poll      = nsf_channels_page_poll,
    .on_action = nsf_channels_page_on_action,
};

static const int nsf_channels_allowed[] = { ACTION_STD_CANCEL, -1 };

void uipod_nsf_channels_screen(void)
{
    int flags = uipod_rs_nsf_get_active_chip_flags();
    if (flags < 0)
        return;

    struct nsf_channels_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt                = &nsf_channels_vtable;
    p.base.context           = CONTEXT_KEYBOARD;
    p.base.allowed           = nsf_channels_allowed;
    p.base.title             = nsf_is_ukrainian() ? "Канали" : "Channels";
    p.base.no_theme          = true;
    p.base.no_header_refresh = true;
    p.chip_flags             = -1;

    nsf_channels_sync_descriptors(&p);
    uipod_page_run(&p.base);
}

int uipod_nsf_channels_menu_fn(void)
{
    uipod_nsf_channels_screen();
    return 0;
}

/* =========================================================================
 * 2. File Browser: Virtual `.nsf` / `.nsfe` Container Drill-Down
 * ========================================================================= */

struct nsf_container_data
{
    char                                  nsf_path[MAX_PATH];
    char                                  info_path[MAX_PATH];
    char                                  game_title[256];
    char                                  artist[256];
    struct uipod_rs_nsf_container_header  hdr;
    struct uipod_rs_nsf_subtrack_info    *subs;
    uint16_t                              music_idx[NSF_MAX_SUBTRACKS];
    int                                   music_count;
    uint16_t                              sfx_idx[NSF_MAX_SUBTRACKS];
    int                                   sfx_count;
};

struct nsf_list_page
{
    struct uipod_page          base;
    struct gui_synclist        lists;
    struct nsf_container_data *data;
    bool                       sfx_view;
    int                        result;
    char                       title_buf[256];
    char                       indic_buf[32];
};

static void format_subtrack_mmss(uint32_t dur_ms, char *out, size_t cap)
{
    unsigned long sec = (dur_ms + 500U) / 1000U;
    unsigned long hrs = sec / 3600UL;
    unsigned long min = (sec % 3600UL) / 60UL;
    unsigned long rem = sec % 60UL;
    if (hrs > 0)
        snprintf(out, cap, "%lu:%02lu:%02lu", hrs, min, rem);
    else
        snprintf(out, cap, "%lu:%02lu", min, rem);
}

static bool title_has_leading_number(const char *s)
{
    if (!s || !isdigit((unsigned char)s[0]))
        return false;
    const char *p = s;
    while (isdigit((unsigned char)*p))
        p++;
    return (*p == '.' || *p == ')' || *p == '-') && p[1] == ' ';
}

static void format_music_row_title(const struct uipod_rs_nsf_subtrack_info *sub,
                                   int display_num,
                                   char *out,
                                   size_t cap)
{
    const char *t = (sub && sub->title[0]) ? sub->title : NULL;
    if (t && title_has_leading_number(t))
        snprintf(out, cap, "%s", t);
    else if (t)
        snprintf(out, cap, "%02d. %s", display_num, t);
    else
        snprintf(out, cap, "%02d. Track %02d", display_num, display_num);
}

static void format_sfx_row_title(const struct uipod_rs_nsf_subtrack_info *sub,
                                 int display_num,
                                 char *out,
                                 size_t cap)
{
    const char *t = (sub && sub->title[0]) ? sub->title : NULL;
    char def_en[32];
    snprintf(def_en, sizeof(def_en), "Track %02u",
             sub ? (unsigned)(sub->subtrack_index + 1) : (unsigned)display_num);
    if (t && strcasecmp(t, def_en) != 0)
        snprintf(out, cap, "SFX %02d - %s", display_num, t);
    else
        snprintf(out, cap, "SFX %02d", display_num);
}

static int nsf_page_row_count(const struct nsf_list_page *p)
{
    const struct nsf_container_data *d = p->data;
    if (p->sfx_view)
        return d->sfx_count;

    int rows = 0;
    if (d->music_count > 0)
        rows += 1 + d->music_count; /* Shuffle Game Music + Music subtracks */
    if (d->sfx_count > 0)
        rows += 1;                  /* Sound Effects (N) › */
    return rows;
}

static const char *nsf_list_get_name(int sel, void *vdata, char *buf, size_t buf_len)
{
    struct nsf_list_page *p = (struct nsf_list_page *)vdata;
    const struct nsf_container_data *d = p->data;
    bool uk = nsf_is_ukrainian();

    if (p->sfx_view)
    {
        if (sel < 0 || sel >= d->sfx_count)
            return "";
        const struct uipod_rs_nsf_subtrack_info *sub = &d->subs[d->sfx_idx[sel]];
        format_sfx_row_title(sub, sel + 1, buf, buf_len);
        return buf;
    }

    int cur = sel;
    if (d->music_count > 0)
    {
        if (cur == 0)
            return uk ? "Перемішати музику гри" : "Shuffle Game Music";
        cur -= 1;
        if (cur < d->music_count)
        {
            const struct uipod_rs_nsf_subtrack_info *sub = &d->subs[d->music_idx[cur]];
            format_music_row_title(sub, cur + 1, buf, buf_len);
            return buf;
        }
        cur -= d->music_count;
    }

    if (d->sfx_count > 0 && cur == 0)
    {
        snprintf(buf, buf_len,
                 uk ? "Звукові ефекти (%d)" : "Sound Effects (%d)",
                 d->sfx_count);
        return buf;
    }
    return "";
}

static const struct uipod_svg_icon *nsf_list_get_svg_icon(int sel, void *vdata)
{
    struct nsf_list_page *p = (struct nsf_list_page *)vdata;
    const struct nsf_container_data *d = p->data;

    if (p->sfx_view)
        return (sel >= 0 && sel < d->sfx_count) ? uipod_svg_tag("nsf") : NULL;

    int cur = sel;
    if (d->music_count > 0)
    {
        if (cur == 0)
            return NULL; /* Action item (like Shuffle All) has clean text */
        cur -= 1;
        if (cur < d->music_count)
            return uipod_svg_tag("nsf");
        cur -= d->music_count;
    }
    if (d->sfx_count > 0 && cur == 0)
        return uipod_svg_tag("folder");
    return NULL;
}

static bool nsf_list_get_chevron(int sel, void *vdata)
{
    struct nsf_list_page *p = (struct nsf_list_page *)vdata;
    const struct nsf_container_data *d = p->data;

    if (p->sfx_view)
        return false;

    int sfx_row = (d->music_count > 0) ? (1 + d->music_count) : 0;
    return (d->sfx_count > 0 && sel == sfx_row);
}

static const char *nsf_list_get_indicator(int sel, void *vdata)
{
    struct nsf_list_page *p = (struct nsf_list_page *)vdata;
    const struct nsf_container_data *d = p->data;

    const struct uipod_rs_nsf_subtrack_info *sub = NULL;
    if (p->sfx_view)
    {
        if (sel >= 0 && sel < d->sfx_count)
            sub = &d->subs[d->sfx_idx[sel]];
    }
    else if (d->music_count > 0)
    {
        int m = sel - 1;
        if (m >= 0 && m < d->music_count)
            sub = &d->subs[d->music_idx[m]];
    }

    if (!sub || sub->duration_ms == 0)
        return NULL;

    char mmss[24];
    format_subtrack_mmss(sub->duration_ms, mmss, sizeof(mmss));
    snprintf(p->indic_buf, sizeof(p->indic_buf), "\x1f%s", mmss);
    return p->indic_buf;
}

static bool nsf_play_music_list(const struct nsf_container_data *d,
                                int start_music_pos,
                                bool shuffle)
{
    if (d->music_count <= 0)
        return false;

    if (shuffle)
    {
        struct uipod_queue q;
        if (!uipod_queue_begin(&q, PLAYLIST_REPLACE))
            return false;
        for (int i = 0; i < d->music_count; i++)
        {
            char uri[MAX_PATH + 16];
            snprintf(uri, sizeof(uri), "%s#%u",
                     d->nsf_path,
                     (unsigned)d->subs[d->music_idx[i]].subtrack_index);
            if (!uipod_queue_add_path(uri, &q))
                break;
        }
        return uipod_queue_end(&q, true);
    }

    char **uris = calloc((size_t)d->music_count, sizeof(*uris));
    if (!uris)
        return false;

    int k = 0;
    bool ok = true;
    for (int i = 0; i < d->music_count; i++)
    {
        char uri[MAX_PATH + 16];
        snprintf(uri, sizeof(uri), "%s#%u",
                 d->nsf_path,
                 (unsigned)d->subs[d->music_idx[i]].subtrack_index);
        uris[k] = strdup(uri);
        if (!uris[k])
        {
            ok = false;
            break;
        }
        k++;
    }

    bool started = false;
    if (ok && k == d->music_count)
    {
        int pos = (start_music_pos >= 0 && start_music_pos < k) ? start_music_pos : 0;
        started = uipod_play_tracks_at(uris, k, pos);
    }

    for (int i = 0; i < k; i++)
        free(uris[i]);
    free(uris);
    return started;
}

static bool write_subtrack_info_report(const struct nsf_container_data *d,
                                       const struct uipod_rs_nsf_subtrack_info *sub,
                                       const char *row_label,
                                       const char *uri)
{
    if (!d->info_path[0])
        return false;
    int fd = open(d->info_path, O_WRONLY | O_CREAT | O_TRUNC, 0666);
    if (fd < 0)
        return false;

    bool uk = nsf_is_ukrainian();
    char dur_str[24];
    format_subtrack_mmss(sub->duration_ms, dur_str, sizeof(dur_str));

    fdprintf(fd, "# %s\n\n", row_label);
    fdprintf(fd, "## %s\n\n", uk ? "Метадані підтреку NSF" : "NSF Subtrack Metadata");
    fdprintf(fd, "• %s: %s\n", uk ? "Гра / Альбом" : "Game / Album", d->game_title);
    if (d->artist[0])
        fdprintf(fd, "• %s: %s\n", uk ? "Виконавець" : "Artist", d->artist);
    fdprintf(fd, "• %s: #%u (%s)\n",
             uk ? "Підтрек" : "Subtrack",
             (unsigned)(sub->subtrack_index + 1),
             sub->track_kind == 0
                 ? (uk ? "Музика" : "Music")
                 : (uk ? "Звуковий ефект" : "Sound Effect"));
    fdprintf(fd, "• %s: NSF (%s)\n",
             uk ? "Чип" : "Sound Chip",
             uipod_nsf_chip_badge_str(sub->chip_flags));
    fdprintf(fd, "• %s: %s\n", uk ? "Тривалість" : "Duration", dur_str);
    if (sub->loop_point_ms >= 0)
    {
        char lp_str[24];
        format_subtrack_mmss((uint32_t)sub->loop_point_ms, lp_str, sizeof(lp_str));
        fdprintf(fd, "• %s: %s\n", uk ? "Точка циклу" : "Loop Point", lp_str);
    }
    fdprintf(fd, "• URI: %s\n", uri);

    close(fd);
    return true;
}

static bool nsf_subtrack_context_menu(struct nsf_list_page *p,
                                      const struct uipod_rs_nsf_subtrack_info *sub,
                                      int music_pos_or_neg1,
                                      const char *row_label)
{
    char uri[MAX_PATH + 16];
    snprintf(uri, sizeof(uri), "%s#%u",
             p->data->nsf_path, (unsigned)sub->subtrack_index);

    const char *opts[5] = {
        ID2P(LANG_PLAY),
        ID2P(LANG_PLAY_NEXT),
        ID2P(LANG_UIPOD_ADD_TO_PLAY_QUEUE),
        ID2P(LANG_UIPOD_ADD_TO_PLAYLIST),
        ID2P(LANG_UIPOD_FILE_INFO),
    };
    int count = p->data->info_path[0] ? 5 : 4;
    int act = uipod_context_menu(row_label, opts, count);
    if (act < 0)
        return false;

    switch (act)
    {
        case 0: /* Play */
            if (music_pos_or_neg1 >= 0)
                return nsf_play_music_list(p->data, music_pos_or_neg1, false);
            return uipod_play_track(uri);

        case 1: /* Play Next */
            uipod_queue_add(uri, FILE_ATTR_AUDIO, PLAYLIST_INSERT, false);
            break;

        case 2: /* Add to Play Queue */
            uipod_queue_add(uri, FILE_ATTR_AUDIO, PLAYLIST_INSERT_LAST, false);
            break;

        case 3: /* Add to Playlist... (persists exact `/path/Game.nsf#idx`) */
            uipod_playlists_pick(uri, FILE_ATTR_AUDIO);
            break;

        case 4: /* Track Info */
            if (write_subtrack_info_report(p->data, sub, row_label, uri))
                uipod_plugins_open_file(p->data->info_path);
            break;
    }
    return false;
}

static void nsf_list_page_draw(struct uipod_page *self)
{
    gui_synclist_draw(&((struct nsf_list_page *)self)->lists);
}

static int nsf_list_page_poll(struct uipod_page *self, int timeout)
{
    int action;
    list_do_action(self->context, timeout,
                   &((struct nsf_list_page *)self)->lists, &action);
    return action;
}

static int nsf_run_sfx_subpage(struct nsf_container_data *data);

static enum uipod_page_result nsf_list_page_on_action(struct uipod_page *self,
                                                      int action)
{
    struct nsf_list_page *p = (struct nsf_list_page *)self;
    struct nsf_container_data *d = p->data;
    int sel = gui_synclist_get_sel_pos(&p->lists);
    int total = nsf_page_row_count(p);

    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;

    if (sel < 0 || sel >= total)
        return UIPOD_PAGE_STAY;

    if (p->sfx_view)
    {
        const struct uipod_rs_nsf_subtrack_info *sub = &d->subs[d->sfx_idx[sel]];
        if (action == ACTION_STD_OK || action == ACTION_TP_LIST_PLAY)
        {
            char uri[MAX_PATH + 16];
            snprintf(uri, sizeof(uri), "%s#%u",
                     d->nsf_path, (unsigned)sub->subtrack_index);
            if (uipod_play_track(uri))
            {
                p->result = GO_TO_WPS;
                return UIPOD_PAGE_DONE;
            }
            return UIPOD_PAGE_STAY;
        }
        if (action == ACTION_STD_CONTEXT)
        {
            char lbl[256];
            format_sfx_row_title(sub, sel + 1, lbl, sizeof(lbl));
            if (nsf_subtrack_context_menu(p, sub, -1, lbl))
            {
                p->result = GO_TO_WPS;
                return UIPOD_PAGE_DONE;
            }
            if (uipod_home_pending)
            {
                p->base.home = true;
                return UIPOD_PAGE_DONE;
            }
            gui_synclist_select_item(&p->lists, sel);
        }
        return UIPOD_PAGE_STAY;
    }

    /* Top-level `.nsf` container view */
    int cur = sel;
    if (d->music_count > 0)
    {
        if (cur == 0)
        {
            /* Row 0: `Shuffle Game Music` */
            if (action == ACTION_STD_OK || action == ACTION_TP_LIST_PLAY)
            {
                if (nsf_play_music_list(d, 0, true))
                {
                    p->result = GO_TO_WPS;
                    return UIPOD_PAGE_DONE;
                }
            }
            return UIPOD_PAGE_STAY;
        }
        cur -= 1;
        if (cur < d->music_count)
        {
            const struct uipod_rs_nsf_subtrack_info *sub = &d->subs[d->music_idx[cur]];
            if (action == ACTION_STD_OK || action == ACTION_TP_LIST_PLAY)
            {
                if (nsf_play_music_list(d, cur, false))
                {
                    p->result = GO_TO_WPS;
                    return UIPOD_PAGE_DONE;
                }
                return UIPOD_PAGE_STAY;
            }
            if (action == ACTION_STD_CONTEXT)
            {
                char lbl[256];
                format_music_row_title(sub, cur + 1, lbl, sizeof(lbl));
                if (nsf_subtrack_context_menu(p, sub, cur, lbl))
                {
                    p->result = GO_TO_WPS;
                    return UIPOD_PAGE_DONE;
                }
                if (uipod_home_pending)
                {
                    p->base.home = true;
                    return UIPOD_PAGE_DONE;
                }
                gui_synclist_select_item(&p->lists, sel);
            }
            return UIPOD_PAGE_STAY;
        }
        cur -= d->music_count;
    }

    if (d->sfx_count > 0 && cur == 0)
    {
        /* Bottom row: `Sound Effects (N) ›` */
        if (action == ACTION_STD_OK)
        {
            int r = nsf_run_sfx_subpage(d);
            if (r == GO_TO_WPS)
            {
                p->result = GO_TO_WPS;
                return UIPOD_PAGE_DONE;
            }
            if (uipod_home_pending)
            {
                p->base.home = true;
                return UIPOD_PAGE_DONE;
            }
            gui_synclist_select_item(&p->lists, sel);
        }
    }

    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable nsf_list_vtable = {
    .legend    = NULL,
    .draw      = nsf_list_page_draw,
    .poll      = nsf_list_page_poll,
    .on_action = nsf_list_page_on_action,
};

static const int nsf_list_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL, ACTION_STD_CONTEXT, ACTION_TP_LIST_PLAY, -1
};

static int nsf_run_sfx_subpage(struct nsf_container_data *data)
{
    bool uk = nsf_is_ukrainian();
    struct nsf_list_page sp;
    memset(&sp, 0, sizeof(sp));
    sp.base.vt      = &nsf_list_vtable;
    sp.base.context = CONTEXT_LIST;
    sp.base.allowed = nsf_list_allowed;
    sp.data         = data;
    sp.sfx_view     = true;
    sp.result       = 0;

    snprintf(sp.title_buf, sizeof(sp.title_buf),
             uk ? "Звукові ефекти (%d)" : "Sound Effects (%d)",
             data->sfx_count);
    sp.base.title = sp.title_buf;

    gui_synclist_init(&sp.lists, nsf_list_get_name, &sp, false, 1, NULL);
    gui_synclist_set_svg_icon_callback(&sp.lists, nsf_list_get_svg_icon);
    gui_synclist_set_chevron_callback(&sp.lists, nsf_list_get_chevron);
    gui_synclist_set_indicator_callback(&sp.lists, nsf_list_get_indicator);
    gui_synclist_set_title(&sp.lists, sp.title_buf, Icon_Audio);
    gui_synclist_set_nb_items(&sp.lists, data->sfx_count);
    if (data->sfx_count > 0)
        gui_synclist_select_item(&sp.lists, 0);

    uipod_page_run(&sp.base);
    if (sp.base.home)
        return GO_TO_ROOT;
    return sp.result;
}

static void stem_from_path(const char *path, char *out, size_t cap)
{
    const char *slash = strrchr(path, '/');
    const char *base  = slash ? slash + 1 : path;
    strlcpy(out, base, cap);
    char *dot = strrchr(out, '.');
    if (dot && dot != out)
        *dot = '\0';
}

int uipod_nsf_container_screen(const char *nsf_path,
                               const char *info_file_path)
{
    if (!nsf_path || !nsf_path[0])
        return 0;

    struct nsf_container_data *data = calloc(1, sizeof(*data));
    if (!data)
        return 0;
    data->subs = calloc(NSF_MAX_SUBTRACKS, sizeof(*data->subs));
    if (!data->subs)
    {
        free(data);
        return 0;
    }

    strlcpy(data->nsf_path, nsf_path, sizeof(data->nsf_path));
    if (info_file_path)
        strlcpy(data->info_path, info_file_path, sizeof(data->info_path));

    size_t n = uipod_library_load_nsf_container(nsf_path,
                                                &data->hdr,
                                                data->subs,
                                                NSF_MAX_SUBTRACKS);
    if (n == 0)
    {
        free(data->subs);
        free(data);
        return 0;
    }

    if (data->hdr.game_title[0])
        strlcpy(data->game_title, data->hdr.game_title, sizeof(data->game_title));
    else
        stem_from_path(nsf_path, data->game_title, sizeof(data->game_title));
    strlcpy(data->artist, data->hdr.artist, sizeof(data->artist));

    for (size_t i = 0; i < n; i++)
    {
        if (data->subs[i].track_kind == 0)
            data->music_idx[data->music_count++] = (uint16_t)i;
        else
            data->sfx_idx[data->sfx_count++] = (uint16_t)i;
    }

    struct nsf_list_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt      = &nsf_list_vtable;
    p.base.context = CONTEXT_LIST;
    p.base.allowed = nsf_list_allowed;
    p.data         = data;
    p.sfx_view     = false;
    p.result       = 0;
    strlcpy(p.title_buf, data->game_title, sizeof(p.title_buf));
    p.base.title   = p.title_buf;

    int total_rows = nsf_page_row_count(&p);
    gui_synclist_init(&p.lists, nsf_list_get_name, &p, false, 1, NULL);
    gui_synclist_set_svg_icon_callback(&p.lists, nsf_list_get_svg_icon);
    gui_synclist_set_chevron_callback(&p.lists, nsf_list_get_chevron);
    gui_synclist_set_indicator_callback(&p.lists, nsf_list_get_indicator);
    gui_synclist_set_title(&p.lists, p.title_buf, Icon_Audio);
    gui_synclist_set_nb_items(&p.lists, total_rows);
    if (total_rows > 0)
    {
        /* Highlight the first music track (or row 0 if SFX-only) or feel free to keep row 0:
         * row 0 is `Shuffle Game Music` per spec (`1. Shuffle Game Music action item at top`). */
        gui_synclist_select_item(&p.lists, 0);
    }

    uipod_page_run(&p.base);

    int res = p.base.home ? GO_TO_ROOT : p.result;
    if (res == GO_TO_WPS)
    {
        /* Going forward into Now Playing: consume the back-slide armed by uipod_page_run
         * so Now Playing slides in forward (`UIPOD_TRANS_FORWARD`), while returning
         * from Now Playing via B still lands back on the parent folder! */
        (void)uipod_transition_take_back();
    }

    free(data->subs);
    free(data);
    return res;
}
