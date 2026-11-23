/***************************************************************************
 * UIPod: Thin Strangler-Fig C glue bridging Rockbox runtime state to the
 * pure-Rust native-HiDPI GPU UI engine (`crates/uipod-ui`).
 ****************************************************************************/
#include "config.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include "action.h"
#include "audio.h"
#include "button.h"
#include "file.h"
#include "gui/list.h"
#include "gui/statusbar-skinned.h"
#include "gui/viewport.h"
#include "gui/wps.h"
#include "lang.h"
#include "lcd.h"
#include "lcd-sdl.h"
#include "misc.h"
#include "power.h"
#include "powermgmt.h"
#include "rbpaths.h"
#include "screen_access.h"
#include "settings.h"
#include "status.h"
#include "string-extra.h"
#include "system.h"
#include "uipod_downloads.h"
#include "uipod_jellyfin.h"
#include "uipod_navidrome.h"
#include "uipod_rs_jellyfin.h"
#include "uipod_rs_navidrome.h"
#include "uipod_rs_ui.h"
#include "uipod_svg.h"
#include "uipod_ui.h"
#include "window-sdl.h"

static uint8_t s_header_source_override = UIPOD_SRC_ICON_NONE;
static bool    s_capturing_transition = false;
static bool    s_captured_transition_frame = false;
static bool    s_prev_had_header = false;
static bool    s_curr_had_header = false;
static volatile bool s_vol_popup_was_animating = false;

void uipod_gpu_set_header_source(uint8_t src_icon)
{
    s_header_source_override = src_icon;
}

uint8_t uipod_gpu_get_header_source(void)
{
    return s_header_source_override;
}

bool uipod_gpu_is_active(void)
{
    return sdl_gl_is_gpu_active();
}

void uipod_gpu_set_active(bool active)
{
    sdl_gl_set_gpu_active(active);
}

int uipod_gpu_visible_list_rows(void)
{
    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);
    size_t rows = uipod_rs_ui_visible_rows_for_window((uint32_t)win_w,
                                                      (uint32_t)win_h);
    if (rows < 5)
        rows = 5;
    if (rows > UIPOD_RS_MAX_VISIBLE_ROWS)
        rows = UIPOD_RS_MAX_VISIBLE_ROWS;
    return (int)rows;
}

void uipod_gpu_begin_transition_capture(void)
{
    s_prev_had_header = sdl_gl_is_gpu_active() && s_curr_had_header;
    s_capturing_transition = true;
    s_captured_transition_frame = false;
}

bool uipod_gpu_did_capture_transition_frame(bool *out_pin_header)
{
    bool got = s_captured_transition_frame;
    s_capturing_transition = false;
    s_captured_transition_frame = false;
    if (out_pin_header)
        *out_pin_header = got && s_prev_had_header && s_curr_had_header;
    return got;
}

void uipod_gpu_present_current_fbo(void)
{
    if (uipod_viz_active || !sdl_gl_get_context())
        return;

    sdl_gl_set_gpu_active(true);
    if (lcd_panel_is_dark())
        return;

    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_make_current();
    sdl_gl_get_drawable_size(&win_w, &win_h);
    uipod_rs_ui_set_volume_popup_clock((uint64_t)sdl_get_ms());
    if (uipod_rs_ui_plugin_present_gpu((uint32_t)win_w, (uint32_t)win_h))
        sdl_gl_swap_window();
}

void uipod_gpu_notify_volume_changed(void)
{
    int steps = get_normalized_volume_steps();
    int notch = get_normalized_volume();
    float norm = (steps > 0) ? ((float)notch / (float)steps) : 0.0f;
    if (norm < 0.0f)
        norm = 0.0f;
    else if (norm > 1.0f)
        norm = 1.0f;

    uint64_t now_ms = (uint64_t)sdl_get_ms();
    uipod_rs_ui_trigger_volume_popup(norm, now_ms);
    s_vol_popup_was_animating = true;

    if (!uipod_viz_active && sdl_gl_is_gpu_active() &&
        !lcd_panel_is_dark() && !s_capturing_transition)
    {
        uipod_gpu_present_current_fbo();
    }
}

void uipod_gpu_notify_hud_toast(const char *message)
{
    if (!message || !message[0])
        return;

    uint64_t now_ms = (uint64_t)sdl_get_ms();
    uipod_rs_ui_trigger_hud_toast(message, now_ms);
    s_vol_popup_was_animating = true;

    if (!uipod_viz_active && sdl_gl_is_gpu_active() &&
        !lcd_panel_is_dark() && !s_capturing_transition)
    {
        uipod_gpu_present_current_fbo();
    }
}

bool uipod_gpu_is_volume_popup_active(void)
{
    if (!s_vol_popup_was_animating)
        return false;
    return uipod_rs_ui_is_volume_popup_animating((uint64_t)sdl_get_ms());
}

bool uipod_gpu_tick_volume_popup(void)
{
    if (!s_vol_popup_was_animating)
        return false;

    uint64_t now_ms = (uint64_t)sdl_get_ms();
    bool anim = uipod_rs_ui_is_volume_popup_animating(now_ms);

    if (!uipod_viz_active && sdl_gl_is_gpu_active() &&
        !lcd_panel_is_dark() && !s_capturing_transition)
    {
        uipod_gpu_present_current_fbo();
    }

    s_vol_popup_was_animating = anim;
    return anim;
}

static int s_last_rendered_w = 0;
static int s_last_rendered_h = 0;

bool uipod_gpu_check_window_resized(void)
{
    if (!sdl_gl_get_context())
        return false;
    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);
    if (s_last_rendered_w > 0 && s_last_rendered_h > 0 &&
        (win_w != s_last_rendered_w || win_h != s_last_rendered_h))
    {
        return true;
    }
    return false;
}

void uipod_gpu_note_rendered_screen(bool has_header)
{
    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);
    s_last_rendered_w = win_w;
    s_last_rendered_h = win_h;
    s_curr_had_header = has_header;
    if (s_capturing_transition)
    {
        s_captured_transition_frame = true;
        sdl_gl_set_gpu_active(true);
        return;
    }
    uipod_gpu_present_current_fbo();
}

void uipod_gpu_prepare_cpu_fallback(void)
{
    if (!sdl_gl_is_gpu_active())
        return;

    sdl_gl_set_gpu_active(false);
    struct viewport *prev_vp = lcd_set_viewport(NULL);
    fb_data *fb = FBADDR(0, 0);
    bool synced = false;
    if (!uipod_viz_active && sdl_gl_get_context() && uipod_rs_ui_is_ready())
    {
        sdl_gl_make_current();
        synced = uipod_rs_ui_plugin_sync_fb((uint8_t *)fb);
    }
    if (!synced)
    {
        unsigned int bg = (unsigned int)global_settings.bg_color;
        fb_data px;
        px.b = (unsigned char)(bg & 0xFF);
        px.g = (unsigned char)((bg >> 8) & 0xFF);
        px.r = (unsigned char)((bg >> 16) & 0xFF);
        for (int i = 0; i < LCD_FBWIDTH * LCD_FBHEIGHT; i++)
            fb[i] = px;
    }
    if (s_curr_had_header && !uipod_viz_active && uipod_rs_ui_is_ready())
    {
        struct UIPodRsHeader hdr;
        uipod_gpu_fill_header(&hdr, NULL);
        (void)uipod_rs_ui_render_header((uint32_t)global_settings.bg_color,
                                        (uint32_t)global_settings.fg_color,
                                        (uint32_t)global_settings.fg1_color,
                                        (uint32_t)global_settings.fg2_color,
                                        &hdr,
                                        (uint8_t *)fb);
    }
    if (sim_lcd_surface)
    {
        SDL_Surface *lcd = SDL_CreateRGBSurfaceFrom(fb, LCD_FBWIDTH, LCD_FBHEIGHT,
                                                    LCD_DEPTH,
                                                    LCD_FBWIDTH * LCD_DEPTH / 8,
                                                    0, 0, 0, 0);
        if (lcd)
        {
            SDL_BlitSurface(lcd, NULL, sim_lcd_surface, NULL);
            SDL_FreeSurface(lcd);
        }
    }
    lcd_set_viewport(prev_vp);
}

bool uipod_gpu_ui_ensure_ready(void)
{
    /* Never touch the SDL GLES context while the visualizer thread owns it */
    if (uipod_viz_active || !sdl_gl_get_context())
        return false;

    sdl_gl_set_gpu_repaint_fn(uipod_gpu_present_current_fbo);
    sdl_gl_make_current();

    if (uipod_rs_ui_is_ready())
        return true;

    return uipod_rs_ui_init(sdl_gl_get_proc_address,
                            ROCKBOX_DIR "/fonts/PixelMplus12-Regular.ttf");
}

void uipod_gpu_fill_header(struct UIPodRsHeader *out,
                           const char *override_title)
{
    if (!out)
        return;

    memset(out, 0, sizeof(*out));

    const char *title = override_title;
    if (!title || !title[0])
        title = sb_get_title(SCREEN_MAIN);
    if (!title || !title[0])
        title = "UIPod";

    out->title  = title;
    out->legend = uipod_get_header_legend();

    int pm = current_playmode();
    struct wps_state *wstate = get_wps_state();
    if (pm == STATUS_PLAY &&
        (((audio_status() & AUDIO_STATUS_PAUSE) != 0) ||
         (wstate && wstate->paused)))
    {
        pm = STATUS_PAUSE;
    }

    switch (pm)
    {
        case STATUS_PLAY:         out->play_mode = 1; break;
        case STATUS_PAUSE:        out->play_mode = 2; break;
        case STATUS_FASTFORWARD:  out->play_mode = 3; break;
        case STATUS_FASTBACKWARD: out->play_mode = 4; break;
        default:                  out->play_mode = 0; break;
    }

    int pct = battery_level_display();
    if (pct < 0)
        pct = 0;
    else if (pct > 100)
        pct = 100;
    out->battery_percent = (uint8_t)pct;

    out->is_charging = charging_state();
#ifdef HAS_BUTTON_HOLD
    out->is_hold = button_hold();
#else
    out->is_hold = is_keys_locked();
#endif

    bool dl_active = false;
    uint32_t dl_done = 0, dl_total = 0;
    uipod_downloads_header_status(&dl_active, &dl_done, &dl_total);
    if (dl_active)
    {
        out->is_downloading = true;
        out->download_done  = dl_done;
        out->download_total = dl_total;
    }
    else
    {
        out->is_downloading = !uipod_offline_mode_get() &&
                              (tp_navidrome_is_busy() ||
                               tp_jellyfin_is_busy());
        out->download_done  = 0;
        out->download_total = 0;
    }

    uint8_t src_icon = s_header_source_override;
    if (src_icon == UIPOD_SRC_ICON_NONE &&
        (audio_status() & AUDIO_STATUS_PLAY) &&
        wstate && wstate->id3 && wstate->id3->path[0])
    {
        const char *p = wstate->id3->path;
        if (uipod_navidrome_is_track_path(p))
            src_icon = UIPOD_SRC_ICON_NAVIDROME;
        else if (uipod_jellyfin_is_track_path(p))
            src_icon = UIPOD_SRC_ICON_JELLYFIN;
        else
            src_icon = UIPOD_SRC_ICON_MICROSD;
    }
    if (uipod_fx_is_active())
        src_icon |= 0x80;
    out->source_icon = src_icon;
}

bool uipod_gpu_render_header(void)
{
    if (get_current_activity() == ACTIVITY_USBSCREEN)
        return false;

    if (!uipod_gpu_ui_ensure_ready())
        return false;

    struct UIPodRsHeader hdr;
    uipod_gpu_fill_header(&hdr, NULL);

    if (sdl_gl_is_gpu_active())
    {
        if (!s_curr_had_header)
            return true;
        int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
        sdl_gl_get_drawable_size(&win_w, &win_h);
        bool ok = uipod_rs_ui_render_header_gpu((uint32_t)global_settings.bg_color,
                                                (uint32_t)global_settings.fg_color,
                                                (uint32_t)global_settings.fg1_color,
                                                (uint32_t)global_settings.fg2_color,
                                                &hdr,
                                                (uint32_t)win_w,
                                                (uint32_t)win_h);
        if (ok && !s_capturing_transition)
            uipod_gpu_present_current_fbo();
        return ok;
    }

    struct viewport *prev_vp = lcd_set_viewport(NULL);
    fb_data *fb = FBADDR(0, 0);
    bool ok = uipod_rs_ui_render_header((uint32_t)global_settings.bg_color,
                                        (uint32_t)global_settings.fg_color,
                                        (uint32_t)global_settings.fg1_color,
                                        (uint32_t)global_settings.fg2_color,
                                        &hdr,
                                        (uint8_t *)fb);
    lcd_set_viewport(prev_vp);

    if (ok)
        lcd_update_rect(0, 0, LCD_WIDTH, UIPOD_RS_HEADER_BAND_ROWS);
    return ok;
}

bool uipod_gpu_render_synclist(struct gui_synclist *list)
{
    if (!list || !list->callback_get_item_name || list->callback_draw_item != NULL)
        return false;

    struct viewport *parent = list->parent[SCREEN_MAIN];
    if (parent && (parent->width < 800 || parent->height < 500))
        return false;

    if (!uipod_gpu_ui_ensure_ready())
        return false;

    /* Stop any legacy bitmap scroll-engine marquees on SCREEN_MAIN */
    screens[SCREEN_MAIN].scroll_stop();

    /* Keep statusbar-skinned title state in sync for standalone header refreshes */
    const char *resolved_title = NULL;
    if (list->title)
    {
        if (P2ID((const unsigned char *)list->title) <= VOICEONLY_DELIMITER)
            resolved_title = (const char *)P2STR((const unsigned char *)list->title);
    }
    sb_set_title_text(resolved_title, list->title_icon, SCREEN_MAIN);

    int vis_rows = uipod_gpu_visible_list_rows();
    int total = (list->nb_items > 0) ? list->nb_items : 0;
    int start = list->start_item[SCREEN_MAIN];
    if (start < 0)
        start = 0;
    if (start > total)
        start = total;

    static char label_bufs[UIPOD_RS_MAX_VISIBLE_ROWS][MAX_PATH];
    static char rval_bufs[UIPOD_RS_MAX_VISIBLE_ROWS][128];
    static char ftag_bufs[UIPOD_RS_MAX_VISIBLE_ROWS][32];
    struct UIPodRsListRow rows[UIPOD_RS_MAX_VISIBLE_ROWS];
    size_t row_count = 0;

    bool cursor_visible = (list->cursor_style != SYNCLIST_CURSOR_NOSTYLE);
    int sel_begin = list->selected_item;
    int sel_end   = sel_begin + ((list->selected_size > 0) ? list->selected_size : 1);

    for (int r = 0; r < vis_rows && (start + r) < total; r++)
    {
        int idx = start + r;
        char scratch[MAX_PATH];
        const char *raw = list->callback_get_item_name(idx, list->data,
                                                       scratch, sizeof(scratch));
        const char *entry_str = "";
        if (raw && P2ID((const unsigned char *)raw) <= VOICEONLY_DELIMITER)
            entry_str = (const char *)P2STR((const unsigned char *)raw);
        while (*entry_str == '\t')
            entry_str++;

        strlcpy(label_bufs[r], entry_str, sizeof(label_bufs[r]));
        rval_bufs[r][0] = '\0';
        ftag_bufs[r][0] = '\0';

        char *embedded_vc = strchr(label_bufs[r], '\x1e');
        if (embedded_vc)
        {
            *embedded_vc = '\0';
            strlcpy(rval_bufs[r], embedded_vc + 1, sizeof(rval_bufs[r]));
        }

        const struct uipod_svg_icon *svg_icon =
            list->callback_get_item_svg_icon
                ? list->callback_get_item_svg_icon(idx, list->data)
                : NULL;
        if (svg_icon && svg_icon->name[0])
            strlcpy(ftag_bufs[r], svg_icon->name, sizeof(ftag_bufs[r]));

        const char *indic =
            list->callback_get_item_indicator
                ? list->callback_get_item_indicator(idx, list->data)
                : NULL;
        bool chev =
            list->callback_get_item_chevron
                ? list->callback_get_item_chevron(idx, list->data)
                : false;

        uint8_t right_kind = 0;
        if (indic && indic[0])
        {
            if (indic[0] == '[' && indic[1] && indic[2] == ']' && indic[3] == '\0')
            {
                right_kind = (indic[1] != ' ') ? 3 : 2;
            }
            else if (indic[0] == '>' && indic[1] == '\0')
            {
                right_kind = 1;
            }
            else if (indic[0] == '\x1e')
            {
                strlcpy(rval_bufs[r], indic + 1, sizeof(rval_bufs[r]));
                right_kind = 6;
            }
            else if (indic[0] == '\x1f')
            {
                strlcpy(rval_bufs[r], indic + 1, sizeof(rval_bufs[r]));
                right_kind = chev ? 6 : 5;
            }
            else
            {
                strlcpy(rval_bufs[r], indic, sizeof(rval_bufs[r]));
                right_kind = 4;
            }
        }
        else if (embedded_vc)
        {
            right_kind = chev ? 6 : 5;
        }
        else if (chev)
        {
            right_kind = 1;
        }

        rows[row_count].label       = label_bufs[r];
        rows[row_count].right_value = rval_bufs[r];
        rows[row_count].file_tag    = ftag_bufs[r];
        rows[row_count].selected    = cursor_visible &&
                                      (idx >= sel_begin && idx < sel_end);
        rows[row_count].right_kind  = right_kind;
        row_count++;
    }

    struct UIPodRsListScreen sc;
    memset(&sc, 0, sizeof(sc));
    sc.bg_rgb        = (uint32_t)global_settings.bg_color;
    sc.fg_rgb        = (uint32_t)global_settings.fg_color;
    sc.fg1_rgb       = (uint32_t)global_settings.fg1_color;
    sc.fg2_rgb       = (uint32_t)global_settings.fg2_color;
    uipod_gpu_fill_header(&sc.header, resolved_title);
    sc.rows          = rows;
    sc.row_count     = row_count;
    sc.total_items   = (size_t)total;
    sc.start_item    = (size_t)start;
    sc.visible_items = (size_t)vis_rows;

    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);
    bool ok = uipod_rs_ui_render_list_gpu(&sc, (uint32_t)win_w, (uint32_t)win_h);
    if (ok)
        uipod_gpu_note_rendered_screen(true);
    return ok;
}

bool uipod_gpu_render_keyboard(const char *text,
                               const size_t row_counts[5],
                               const struct UIPodRsKeyboardKey *keys)
{
    if (!row_counts || !keys)
        return false;

    if (!uipod_gpu_ui_ensure_ready())
        return false;

    screens[SCREEN_MAIN].scroll_stop();

    struct UIPodRsKeyboard kbd;
    memset(&kbd, 0, sizeof(kbd));
    kbd.bg_rgb  = (uint32_t)global_settings.bg_color;
    kbd.fg_rgb  = (uint32_t)global_settings.fg_color;
    kbd.fg1_rgb = (uint32_t)global_settings.fg1_color;
    kbd.fg2_rgb = (uint32_t)global_settings.fg2_color;
    kbd.text    = text ? text : "";
    for (int i = 0; i < 5; i++)
        kbd.row_counts[i] = row_counts[i];
    kbd.keys = keys;

    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);
    bool ok = uipod_rs_ui_render_keyboard_gpu(&kbd, (uint32_t)win_w, (uint32_t)win_h);
    if (ok)
        uipod_gpu_note_rendered_screen(false);
    return ok;
}

bool uipod_gpu_render_graphic_eq(const char *header_title,
                                 int16_t preamp_tenths_db,
                                 const int16_t band_gains_tenths_db[10],
                                 size_t selected_col,
                                 bool adjusting,
                                 bool bypassed)
{
    if (!band_gains_tenths_db)
        return false;

    if (!uipod_gpu_ui_ensure_ready())
        return false;

    screens[SCREEN_MAIN].scroll_stop();

    struct UIPodRsGraphicEq geq;
    memset(&geq, 0, sizeof(geq));
    geq.bg_rgb  = (uint32_t)global_settings.bg_color;
    geq.fg_rgb  = (uint32_t)global_settings.fg_color;
    geq.fg1_rgb = (uint32_t)global_settings.fg1_color;
    geq.fg2_rgb = (uint32_t)global_settings.fg2_color;
    uipod_gpu_fill_header(&geq.header, header_title);
    geq.preamp_tenths_db = preamp_tenths_db;
    for (int i = 0; i < 10; i++)
        geq.band_gains_tenths_db[i] = band_gains_tenths_db[i];
    geq.selected_col = selected_col;
    geq.adjusting    = adjusting;
    geq.bypassed     = bypassed;

    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);
    bool ok = uipod_rs_ui_render_graphic_eq_gpu(&geq, (uint32_t)win_w, (uint32_t)win_h);
    if (ok)
        uipod_gpu_note_rendered_screen(true);
    return ok;
}

bool uipod_gpu_render_nsf_channels(const char *header_title,
                                   const struct UIPodRsNsfChannelRow *channels,
                                   size_t channel_count,
                                   const char *enable_all_label,
                                   bool enable_all_selected,
                                   const char *footer_hint,
                                   size_t start_item,
                                   size_t visible_items)
{
    if (!channels || channel_count == 0)
        return false;

    if (!uipod_gpu_ui_ensure_ready())
        return false;

    screens[SCREEN_MAIN].scroll_stop();

    struct UIPodRsNsfChannels ch;
    memset(&ch, 0, sizeof(ch));
    ch.bg_rgb              = (uint32_t)global_settings.bg_color;
    ch.fg_rgb              = (uint32_t)global_settings.fg_color;
    ch.fg1_rgb             = (uint32_t)global_settings.fg1_color;
    ch.fg2_rgb             = (uint32_t)global_settings.fg2_color;
    uipod_gpu_fill_header(&ch.header, header_title);
    ch.channels            = channels;
    ch.channel_count       = channel_count;
    ch.enable_all_label    = enable_all_label ? enable_all_label : "";
    ch.enable_all_selected = enable_all_selected;
    ch.footer_hint         = footer_hint ? footer_hint : "";
    ch.start_item          = start_item;
    ch.visible_items       = visible_items;

    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);
    bool ok = uipod_rs_ui_render_nsf_channels_gpu(&ch, (uint32_t)win_w, (uint32_t)win_h);
    if (ok)
        uipod_gpu_note_rendered_screen(true);
    return ok;
}
