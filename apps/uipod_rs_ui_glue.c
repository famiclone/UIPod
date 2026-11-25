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
#include "backlight.h"
#include "button.h"
#include "file.h"
#include "gui/list.h"
#include "gui/statusbar-skinned.h"
#include "gui/viewport.h"
#include "gui/wps.h"
#include "kernel.h"
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
static volatile bool s_hud_was_animating = false;
static uint32_t      s_transient_progress_handle = 0;
static uint64_t      s_last_progress_present_ms = 0;
static volatile int  s_active_confirm_depth = 0;

/* Declarative LayoutSpec for GPU text pages (`Hold`, `Shutdown`, `MessagePage`)
 * and the hardware video player's bottom HUD / `.srt` subtitle strip. */
static const struct
{
    float   margin_x;
    float   font_primary;
    float   font_secondary;
    float   min_font;
    float   line_gap;
    uint8_t weight_primary;
    uint8_t weight_secondary;
} kTextBlockLayoutSpec = {
    .margin_x         = 48.0f,
    .font_primary     = 48.0f,
    .font_secondary   = 40.0f,
    .min_font         = 24.0f,
    .line_gap         = 26.0f,
    .weight_primary   = 2, /* Geologica SemiBold */
    .weight_secondary = 1, /* Geologica Medium */
};

static const struct
{
    float    strip_top;
    uint32_t strip_rows;
    float    bar_x;
    float    bar_y;
    float    bar_w;
    float    bar_h;
    float    text_center_y;
    float    hud_font_size;
    float    sub_strip_top;
    float    sub_strip_h;
    uint32_t sub_strip_rows;
    float    sub_margin_x;
    float    sub_font_size;
    float    sub_min_font;
    float    sub_line_gap;
    float    divider_stroke;
    uint8_t  font_weight;
} kVideoOverlayLayoutSpec = {
    .strip_top      = 576.0f,
    .strip_rows     = 192,
    .bar_x          = 64.0f,
    .bar_y          = 616.0f,
    .bar_w          = 896.0f,
    .bar_h          = 32.0f,
    .text_center_y  = 708.0f,
    .hud_font_size  = 42.0f,
    .sub_strip_top  = 656.0f,
    .sub_strip_h    = 112.0f,
    .sub_strip_rows = 112,
    .sub_margin_x   = 40.0f,
    .sub_font_size  = 38.0f,
    .sub_min_font   = 22.0f,
    .sub_line_gap   = 10.0f,
    .divider_stroke = 4.0f,
    .font_weight    = 2, /* Geologica SemiBold */
};

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

    int vid_rows = 0;
    if (sdl_gl_get_video_through(&vid_rows))
    {
        if (uipod_rs_ui_present_video_overlay_gpu((uint32_t)win_w,
                                                  (uint32_t)win_h,
                                                  (uint32_t)(vid_rows > 0 ? vid_rows : 0)))
        {
            sdl_gl_swap_window();
        }
        return;
    }

    uipod_rs_hud_set_clock((uint64_t)sdl_get_ms());
    if (uipod_rs_ui_plugin_present_gpu((uint32_t)win_w, (uint32_t)win_h))
        sdl_gl_swap_window();
}

static void hud_present_if_live(void)
{
    if (!uipod_viz_active && sdl_gl_get_context() &&
        (sdl_gl_is_gpu_active() || uipod_rs_ui_is_ready()) &&
        !lcd_panel_is_dark() && !s_capturing_transition)
    {
        uipod_gpu_present_current_fbo();
    }
}

void uipod_hud_volume(void)
{
    int steps = get_normalized_volume_steps();
    int notch = get_normalized_volume();
    float norm = (steps > 0) ? ((float)notch / (float)steps) : 0.0f;
    if (norm < 0.0f)
        norm = 0.0f;
    else if (norm > 1.0f)
        norm = 1.0f;

    uint64_t now_ms = (uint64_t)sdl_get_ms();
    uipod_rs_hud_volume(norm, now_ms);
    s_hud_was_animating = true;

    if (!uipod_viz_active && sdl_gl_is_gpu_active() &&
        !lcd_panel_is_dark() && !s_capturing_transition)
    {
        uipod_gpu_present_current_fbo();
    }
}

void uipod_hud_toast_ex(const char *key, const char *message, uint32_t hold_ms)
{
    if (!message || !message[0])
        return;

    if (!uipod_rs_ui_is_ready())
        (void)uipod_gpu_ui_ensure_ready();

    /* If a legacy splash(0, ...) transient progress was left open before a summary toast,
     * end it cleanly after pushing the toast so it fades out beneath the toast. */
    uint32_t stale_ph = s_transient_progress_handle;
    s_transient_progress_handle = 0;

    uint64_t now_ms = (uint64_t)sdl_get_ms();
    uipod_rs_hud_toast(key, message, (uint64_t)hold_ms, now_ms);
    if (stale_ph != 0)
        (void)uipod_rs_hud_progress_end(stale_ph, now_ms);
    s_hud_was_animating = true;

    if (!uipod_viz_active && sdl_gl_is_gpu_active() &&
        !lcd_panel_is_dark() && !s_capturing_transition)
    {
        uipod_gpu_present_current_fbo();
    }
}

void uipod_hud_toast(const char *message)
{
    uipod_hud_toast_ex(NULL, message, 0);
}

uint32_t uipod_hud_progress_begin(const char *title, const char *detail, float progress)
{
    if (!title || !title[0])
        return 0;
    if (!uipod_rs_ui_is_ready())
        (void)uipod_gpu_ui_ensure_ready();

    uint64_t now_ms = (uint64_t)sdl_get_ms();
    uint32_t h = uipod_rs_hud_progress_begin(title, detail, progress, now_ms);
    if (h != 0)
    {
        s_hud_was_animating = true;
        s_last_progress_present_ms = now_ms;
        hud_present_if_live();
    }
    return h;
}

uint32_t uipod_hud_progress_begin_blocking(const char *title, const char *detail)
{
    uint32_t h = uipod_hud_progress_begin(title, detail, -1.0f);
    if (h == 0 || uipod_viz_active || lcd_panel_is_dark() || s_capturing_transition)
        return h;

    /* Pump the ~160 ms top slide-in via cooperative `sleep(1)` (never touching `button_queue`
     * or SDL's event thread) so the card visibly settles before a blocking network/FS call. */
    uint64_t start_ms = (uint64_t)sdl_get_ms();
    while ((uint64_t)sdl_get_ms() - start_ms < 165u)
    {
        sleep(1);
        hud_present_if_live();
    }
    return h;
}

void uipod_hud_progress_update(uint32_t handle,
                               const char *title,
                               const char *detail,
                               float progress)
{
    if (handle == 0)
        return;
    uint64_t now_ms = (uint64_t)sdl_get_ms();
    if (uipod_rs_hud_progress_update(handle, title, detail, progress, now_ms))
    {
        s_hud_was_animating = true;
        /* Throttle synchronous loop presents to ~60 FPS (`>= 15 ms`) unless hitting 100%. */
        if (progress >= 0.999f || now_ms - s_last_progress_present_ms >= 15u)
        {
            s_last_progress_present_ms = now_ms;
            hud_present_if_live();
        }
    }
}

void uipod_hud_progress_end(uint32_t handle)
{
    if (handle == 0)
        return;
    uint64_t now_ms = (uint64_t)sdl_get_ms();
    if (uipod_rs_hud_progress_end(handle, now_ms))
    {
        s_hud_was_animating = true;
        hud_present_if_live();
    }
}

void uipod_hud_progress_set_transient(const char *title, const char *detail, float progress)
{
    if (!title || !title[0])
        return;
    if (s_transient_progress_handle == 0)
        s_transient_progress_handle = uipod_hud_progress_begin(title, detail, progress);
    else
        uipod_hud_progress_update(s_transient_progress_handle, title, detail, progress);
}

void uipod_hud_progress_clear_transient(void)
{
    if (s_transient_progress_handle != 0)
    {
        uint32_t h = s_transient_progress_handle;
        s_transient_progress_handle = 0;
        uipod_hud_progress_end(h);
    }
}

static const struct button_mapping s_hud_confirm_keymap[] =
{
    { ACTION_STD_OK,     BUTTON_A | BUTTON_REL, BUTTON_A },
    { ACTION_STD_OK,     BUTTON_A | BUTTON_REL, BUTTON_A | BUTTON_REPEAT },
    { ACTION_STD_CANCEL, BUTTON_B | BUTTON_REL, BUTTON_B },
    { ACTION_STD_CANCEL, BUTTON_B | BUTTON_REL, BUTTON_B | BUTTON_REPEAT },
    LAST_ITEM_IN_LIST
};

static const struct button_mapping *hud_confirm_get_context_map(int context)
{
    (void)context;
    return s_hud_confirm_keymap;
}

bool uipod_hud_has_confirm(void)
{
    if (s_active_confirm_depth <= 0)
        return false;
    return uipod_rs_hud_has_confirm();
}

int uipod_hud_confirm_ex(const char *question,
                         const char *detail,
                         int timeout_ticks,
                         int default_res)
{
    if (!question || !question[0])
        return UIPOD_CONFIRM_NO;

    if (!uipod_rs_ui_is_ready() && !uipod_gpu_ui_ensure_ready())
        return UIPOD_CONFIRM_NO;

    uipod_hud_progress_clear_transient();

    bool uk = strcmp((const char *)global_settings.lang_file, "ukrainian") == 0;
    const char *confirm_lbl = uk ? "Підтвердити" : "Confirm";
    const char *cancel_lbl  = uk ? "Скасувати" : "Cancel";

    uint64_t open_ms = (uint64_t)sdl_get_ms();
    uint32_t cid = uipod_rs_hud_confirm_push(question,
                                             detail ? detail : "",
                                             confirm_lbl,
                                             cancel_lbl,
                                             open_ms);
    if (cid == 0)
        return UIPOD_CONFIRM_NO;

    s_active_confirm_depth++;
    s_hud_was_animating = true;
    hud_present_if_live();

    /* Drain the button press that opened this prompt while animating at 60 FPS
     * and still passing hardware VOL +/- (and SELECT + VOL +/- brightness) through! */
    long non_vol_mask = ~(long)(BUTTON_VOL_UP | BUTTON_VOL_DOWN | BUTTON_SELECT);
    while ((button_status() & non_vol_mask) != 0)
    {
        long ev = button_get_w_tmo(1);
#if defined(BUTTON_VOL_UP) && defined(BUTTON_VOL_DOWN)
        if (ev != BUTTON_NONE && (ev & SYS_EVENT) == 0 && !(ev & BUTTON_REL))
        {
            long base_ev = ev & ~(BUTTON_REPEAT | BUTTON_REL);
            if ((base_ev & (BUTTON_VOL_UP | BUTTON_VOL_DOWN)) != 0)
            {
#if defined(BUTTON_SELECT) && defined(HAVE_BACKLIGHT_BRIGHTNESS)
                if ((button_status() & BUTTON_SELECT) != 0)
                    adjust_brightness((base_ev & BUTTON_VOL_UP) ? 1 : -1);
                else
#endif
                    adjust_volume((base_ev & BUTTON_VOL_UP) ? 1 : -1);
            }
        }
#endif
        (void)uipod_hud_tick();
    }

    bool use_tmo = (timeout_ticks >= HZ &&
                    (default_res == UIPOD_CONFIRM_YES || default_res == UIPOD_CONFIRM_NO));
    long end_tick = current_tick + timeout_ticks;
    int result = -1;

    while (result < 0)
    {
        bool bl_was_on = is_backlight_on(false);
        int wait_ticks = uipod_hud_is_animating() ? 1 : (HZ / 10);
        if (wait_ticks < 1)
            wait_ticks = 1;

        int action = get_custom_action(CONTEXT_STD, wait_ticks, hud_confirm_get_context_map);

        if (uipod_gpu_check_window_resized())
            hud_present_if_live();

        if (action == ACTION_NONE)
        {
            if (use_tmo && TIME_AFTER(current_tick, end_tick))
            {
                result = default_res;
                break;
            }
            continue;
        }

        if (action & SYS_EVENT)
        {
            long handled = default_event_handler(action);
            if (handled == SYS_USB_CONNECTED)
            {
                result = UIPOD_CONFIRM_USB;
                break;
            }
            if (handled == SYS_POWEROFF || handled == SYS_REBOOT)
            {
                result = UIPOD_CONFIRM_NO;
                break;
            }
            continue;
        }

        /* Never confirm/cancel on the tap that merely wakes a sleeping backlight */
        if (!bl_was_on)
            continue;

        if (action == ACTION_STD_OK)
        {
            uipod_menu_click();
            result = UIPOD_CONFIRM_YES;
            break;
        }
        if (action == ACTION_STD_CANCEL)
        {
            uipod_menu_click();
            result = UIPOD_CONFIRM_NO;
            break;
        }
    }

    if (s_active_confirm_depth > 0)
        s_active_confirm_depth--;

    (void)uipod_rs_hud_confirm_resolve(cid, (uint64_t)sdl_get_ms());
    s_hud_was_animating = true;
    hud_present_if_live();

    return result;
}

bool uipod_hud_confirm(const char *question, const char *detail)
{
    return uipod_hud_confirm_ex(question, detail, 0, -1) == UIPOD_CONFIRM_YES;
}

bool uipod_hud_is_animating(void)
{
    if (!s_hud_was_animating)
        return false;
    return uipod_rs_hud_needs_tick((uint64_t)sdl_get_ms());
}

bool uipod_hud_tick(void)
{
    if (!s_hud_was_animating)
        return false;

    uint64_t now_ms = (uint64_t)sdl_get_ms();
    bool anim = uipod_rs_hud_needs_tick(now_ms);

    if (!uipod_viz_active && sdl_gl_is_gpu_active() &&
        !lcd_panel_is_dark() && !s_capturing_transition)
    {
        uipod_gpu_present_current_fbo();
    }

    s_hud_was_animating = anim;
    return anim;
}

void uipod_gpu_notify_volume_changed(void)
{
    uipod_hud_volume();
}

void uipod_gpu_notify_hud_toast(const char *message)
{
    uipod_hud_toast(message);
}

bool uipod_gpu_is_volume_popup_active(void)
{
    return uipod_hud_is_animating();
}

bool uipod_gpu_tick_volume_popup(void)
{
    return uipod_hud_tick();
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
                            ROCKBOX_DIR "/fonts/NotoSansJP-Regular.ttf");
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
        if (ok && !s_capturing_transition && !lcd_is_update_suppressed())
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

static float fit_line_font_size(const char *text,
                                float base_size,
                                float min_size,
                                uint8_t weight,
                                float max_w)
{
    if (!text || !text[0] || max_w <= 1.0f)
        return base_size;
    float w = uipod_rs_ui_plugin_measure_text(text, base_size, weight);
    if (w > max_w && w > 1.0f)
    {
        float scaled = base_size * (max_w / w);
        return (scaled < min_size) ? min_size : scaled;
    }
    return base_size;
}

bool uipod_gpu_render_text_block(const char *header_title,
                                 bool show_header,
                                 const char *const *lines,
                                 const uint8_t *line_roles,
                                 size_t line_count,
                                 bool left_align_block)
{
    if (!lines || line_count == 0 || line_count > 16)
        return false;

    if (!uipod_gpu_ui_ensure_ready())
        return false;

    screens[SCREEN_MAIN].scroll_stop();

    const uint32_t bg  = (uint32_t)global_settings.bg_color;
    const uint32_t fg  = (uint32_t)global_settings.fg_color;
    const uint32_t fg1 = (uint32_t)global_settings.fg1_color;
    const uint32_t fg2 = (uint32_t)global_settings.fg2_color;

    struct UIPodRsHeader hdr;
    if (show_header)
        uipod_gpu_fill_header(&hdr, header_title);

    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);

    float lw = (float)LCD_WIDTH, lh = (float)LCD_HEIGHT;
    if (!uipod_rs_ui_plugin_begin_ex(bg, fg, fg1, fg2, bg,
                                     show_header ? &hdr : NULL,
                                     (uint32_t)win_w, (uint32_t)win_h,
                                     true, &lw, &lh))
    {
        return false;
    }

    const float content_top = show_header ? (float)UIPOD_RS_HEADER_BAND_ROWS : 0.0f;
    const float content_h   = lh - content_top;
    const float max_w       = lw - 2.0f * kTextBlockLayoutSpec.margin_x;

    float sizes[16];
    uint8_t weights[16];
    uint32_t colors[16];
    float widths[16];
    float max_block_w = 0.0f;
    float total_h = 0.0f;

    for (size_t i = 0; i < line_count; i++)
    {
        const char *s = lines[i] ? lines[i] : "";
        bool secondary = (line_roles && line_roles[i] != 0);
        uint8_t wt = secondary ? kTextBlockLayoutSpec.weight_secondary
                               : kTextBlockLayoutSpec.weight_primary;
        float base_sz = secondary ? kTextBlockLayoutSpec.font_secondary
                                  : kTextBlockLayoutSpec.font_primary;
        float fsz = fit_line_font_size(s, base_sz, kTextBlockLayoutSpec.min_font, wt, max_w);
        float w = uipod_rs_ui_plugin_measure_text(s, fsz, wt);
        if (w > max_block_w)
            max_block_w = w;
        sizes[i]   = fsz;
        weights[i] = wt;
        colors[i]  = secondary ? fg2 : fg;
        widths[i]  = w;
        total_h += fsz;
    }

    float gap = kTextBlockLayoutSpec.line_gap;
    if (line_count > 1)
    {
        if (total_h + gap * (float)(line_count - 1) > content_h - 32.0f)
        {
            gap = (content_h - 32.0f - total_h) / (float)(line_count - 1);
            if (gap < 8.0f)
                gap = 8.0f;
        }
        total_h += gap * (float)(line_count - 1);
    }

    float cur_y = content_top + (content_h - total_h) * 0.5f;
    if (cur_y < content_top + 12.0f)
        cur_y = content_top + 12.0f;

    float block_left_x = (lw - max_block_w) * 0.5f;
    if (block_left_x < kTextBlockLayoutSpec.margin_x)
        block_left_x = kTextBlockLayoutSpec.margin_x;

    for (size_t i = 0; i < line_count; i++)
    {
        const char *s = lines[i] ? lines[i] : "";
        float center_y = cur_y + sizes[i] * 0.5f;
        if (left_align_block)
        {
            (void)widths;
            (void)uipod_rs_ui_plugin_draw_text(block_left_x, center_y, s,
                                               sizes[i], weights[i], 0, colors[i]);
        }
        else
        {
            (void)uipod_rs_ui_plugin_draw_text(lw * 0.5f, center_y, s,
                                               sizes[i], weights[i], 1, colors[i]);
        }
        cur_y += sizes[i] + gap;
    }

    (void)uipod_rs_ui_plugin_flush_fbo();
    uipod_gpu_note_rendered_screen(show_header);
    return true;
}

bool uipod_gpu_render_video_overlay(int panel_kind,
                                    float progress_norm,
                                    const char *left_text,
                                    const char *center_text,
                                    const char *right_text,
                                    const char *sub_line1,
                                    const char *sub_line2)
{
    if (panel_kind < 0 || panel_kind > 3)
        return false;

    if (!uipod_gpu_ui_ensure_ready())
        return false;

    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);

    if (panel_kind == 0)
    {
        s_curr_had_header = false;
        sdl_gl_set_gpu_active(true);
        if (!lcd_panel_is_dark() &&
            uipod_rs_ui_present_video_overlay_gpu((uint32_t)win_w, (uint32_t)win_h, 0))
        {
            sdl_gl_swap_window();
        }
        return true;
    }

    const uint32_t bg  = (uint32_t)global_settings.bg_color;
    const uint32_t fg  = (uint32_t)global_settings.fg_color;
    const uint32_t fg1 = (uint32_t)global_settings.fg1_color;
    const uint32_t fg2 = (uint32_t)global_settings.fg2_color;

    if (!uipod_rs_ui_plugin_begin(bg, fg, fg1, fg2, bg, NULL))
        return false;

    const float W = (float)LCD_WIDTH;
    uint32_t overlay_rows = 0;

    if (panel_kind == 1 || panel_kind == 2)
    {
        overlay_rows = kVideoOverlayLayoutSpec.strip_rows;
        uipod_rs_ui_plugin_fill_rect(0.0f,
                                     kVideoOverlayLayoutSpec.strip_top,
                                     W,
                                     kVideoOverlayLayoutSpec.divider_stroke,
                                     0.0f,
                                     fg1);
        uipod_rs_ui_plugin_draw_progress_bar(bg, fg, fg1, fg2,
                                             kVideoOverlayLayoutSpec.bar_x,
                                             kVideoOverlayLayoutSpec.bar_y,
                                             kVideoOverlayLayoutSpec.bar_w,
                                             kVideoOverlayLayoutSpec.bar_h,
                                             progress_norm);
        const float ty   = kVideoOverlayLayoutSpec.text_center_y;
        const float fsz  = kVideoOverlayLayoutSpec.hud_font_size;
        const uint8_t wt = kVideoOverlayLayoutSpec.font_weight;

        if (left_text && left_text[0])
            (void)uipod_rs_ui_plugin_draw_text(kVideoOverlayLayoutSpec.bar_x, ty,
                                               left_text, fsz, wt, 0, fg);
        if (center_text && center_text[0])
            (void)uipod_rs_ui_plugin_draw_text(W * 0.5f, ty,
                                               center_text, fsz, wt, 1, fg);
        if (right_text && right_text[0])
            (void)uipod_rs_ui_plugin_draw_text(kVideoOverlayLayoutSpec.bar_x + kVideoOverlayLayoutSpec.bar_w,
                                               ty, right_text, fsz, wt, 2, fg);
    }
    else if (panel_kind == 3 && sub_line1 && sub_line1[0])
    {
        overlay_rows = kVideoOverlayLayoutSpec.sub_strip_rows;
        uipod_rs_ui_plugin_fill_rect(0.0f,
                                     kVideoOverlayLayoutSpec.sub_strip_top,
                                     W,
                                     kVideoOverlayLayoutSpec.divider_stroke,
                                     0.0f,
                                     fg1);
        const float max_w = W - 2.0f * kVideoOverlayLayoutSpec.sub_margin_x;
        const uint8_t wt  = kVideoOverlayLayoutSpec.font_weight;
        float fsz1 = fit_line_font_size(sub_line1,
                                        kVideoOverlayLayoutSpec.sub_font_size,
                                        kVideoOverlayLayoutSpec.sub_min_font,
                                        wt, max_w);
        if (sub_line2 && sub_line2[0])
        {
            float fsz2 = fit_line_font_size(sub_line2,
                                            kVideoOverlayLayoutSpec.sub_font_size,
                                            kVideoOverlayLayoutSpec.sub_min_font,
                                            wt, max_w);
            float tot = fsz1 + kVideoOverlayLayoutSpec.sub_line_gap + fsz2;
            float y0  = kVideoOverlayLayoutSpec.sub_strip_top +
                        (kVideoOverlayLayoutSpec.sub_strip_h - tot) * 0.5f;
            (void)uipod_rs_ui_plugin_draw_text(W * 0.5f, y0 + fsz1 * 0.5f,
                                               sub_line1, fsz1, wt, 1, fg);
            (void)uipod_rs_ui_plugin_draw_text(W * 0.5f,
                                               y0 + fsz1 + kVideoOverlayLayoutSpec.sub_line_gap + fsz2 * 0.5f,
                                               sub_line2, fsz2, wt, 1, fg);
        }
        else
        {
            float cy = kVideoOverlayLayoutSpec.sub_strip_top +
                       kVideoOverlayLayoutSpec.sub_strip_h * 0.5f;
            (void)uipod_rs_ui_plugin_draw_text(W * 0.5f, cy,
                                               sub_line1, fsz1, wt, 1, fg);
        }
    }

    s_curr_had_header = false;
    sdl_gl_set_gpu_active(true);
    if (lcd_panel_is_dark())
    {
        (void)uipod_rs_ui_plugin_flush_fbo();
        return true;
    }

    if (!uipod_rs_ui_present_video_overlay_gpu((uint32_t)win_w, (uint32_t)win_h, overlay_rows))
        return false;
    sdl_gl_swap_window();
    return true;
}
