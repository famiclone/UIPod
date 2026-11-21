/***************************************************************************
 * UIPod: Native Plugin Host Bridge (`UIPodPluginApi`), J2ME-style System Menu
 * host (`host_show_menu`), `Extras -> Plugins` category browser, and Viewers.
 ****************************************************************************/
#include "config.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#include "action.h"
#include "audio.h"
#include "backlight.h"
#include "button.h"
#include "dir.h"
#include "file.h"
#include "gui/list.h"
#include "gui/viewport.h"
#include "gui/wps.h"
#include "icon.h"
#include "kernel.h"
#include "lang.h"
#include "lcd.h"
#include "menu.h"
#include "misc.h"
#include "pcmbuf.h"
#include "playlist.h"
#include "rbpaths.h"
#include "screen_access.h"
#include "settings.h"
#include "splash.h"
#include "string-extra.h"
#include "system.h"
#include "thread.h"

#include "uipod_keyboard.h"
#include "uipod_library.h"
#include "uipod_page.h"
#include "uipod_playlists.h"
#include "uipod_plugins.h"
#include "uipod_rs_codecs.h"
#include "uipod_rs_plugins.h"
#include "uipod_rs_ui.h"
#include "uipod_svg.h"
#include "uipod_transition.h"
#include "uipod_ui.h"
#include "window-sdl.h"
#include "tape_deck_fx.h"

extern void retrohh_cpu_hold_dynamic(void);
extern void retrohh_cpu_release_dynamic(void);
extern unsigned pcm_viz_latest(int16_t *out, unsigned max_frames);
extern unsigned pcm_viz_read_synced(int16_t *out, unsigned max_frames,
                                    unsigned *sample_rate_out);
extern unsigned int pcm_alsa_output_delay_us(void);

#define BUNDLED_PLUGINS_DIR   ROCKBOX_DIR "/plugins"
#define SDCARD_PLUGINS_DIR    UIPOD_SDCARD_ROOT "/Plugins/UIPod"
#define PLUGIN_HOLD_B_TICKS   (65 * HZ / 100)
#define MAX_CAT_PLUGINS       128
#define MAX_PLUGIN_MENU_ITEMS 32

static long s_b_press_tick        = 0;
static bool s_b_hold_fired        = false;
static bool s_active_menu_driven  = false;
static bool s_menu_on_screen      = false;
static bool s_prev_menu_cancelled = false;
static bool s_gpu_fbo_unsynced    = false;
static bool s_has_presented_frame = false;
static bool s_plugin_has_header   = false;
static bool s_trans_snapshotted   = false;
static char s_last_menu_title[128];
#ifdef HAVE_PRIORITY_SCHEDULING
static int  s_saved_ui_prio       = PRIORITY_USER_INTERFACE;
#endif

static bool is_lang_ukrainian(void)
{
    return strcmp((const char *)global_settings.lang_file, "ukrainian") == 0;
}

/* Rockbox on SDL (`HAVE_SIGALTSTACK_THREADS`) runs `main`, `audio`, `codec`,
 * and `buffering` as cooperative user-space coroutines on a single OS pthread.
 * A 60 FPS plugin paced by `SDL_GL_SwapWindow` vsync (~16.67 ms) never hits an
 * `elapsed < 16` sleep check, which would starve `codec_thread` until `pcmbuf`
 * drains and playback stops. While a plugin runs at `PRIORITY_BACKGROUND`,
 * pumping `yield()` lets `audio`/`buffering`/`codec` (priorities 5..16) refill
 * `pcmbuf` to 100% in <1 ms right at the start of each frame without dropping
 * below 60 FPS. */
static void pump_background_threads(void)
{
    yield();
    yield();
    yield();
}

static void sync_gpu_fbo_to_rockbox_fb_if_needed(void)
{
    if (!s_gpu_fbo_unsynced)
        return;
    s_gpu_fbo_unsynced = false;
    if (uipod_gpu_is_active() || !uipod_gpu_ui_ensure_ready())
        return;
    struct viewport *prev_vp = lcd_set_viewport(NULL);
    fb_data *fb = FBADDR(0, 0);
    (void)uipod_rs_ui_plugin_sync_fb((uint8_t *)fb);
    lcd_set_viewport(prev_vp);
}

/* ---- Host API Callback Implementations (`UIPodPluginApi`) ------------- */

static uint64_t host_get_ticks_ms(void)
{
    return (uint64_t)sdl_get_ms();
}

static void host_sleep_ms(uint32_t ms)
{
    if (ms == 0)
    {
        yield();
        return;
    }
    long ticks = (long)((ms * (uint32_t)HZ + 999u) / 1000u);
    if (ticks < 1)
        ticks = 1;
    sleep(ticks);
}

static uint32_t host_get_language(void)
{
    return is_lang_ukrainian() ? 1u : 0u;
}

static size_t host_get_data_dir(char *out, size_t cap)
{
    if (!out || cap == 0)
        return 0;

    char dir[512];
    if (dir_exists(UIPOD_SDCARD_ROOT))
    {
        mkdir(UIPOD_SDCARD_ROOT "/Plugins");
        mkdir(SDCARD_PLUGINS_DIR);
        snprintf(dir, sizeof(dir), "%s/.data", SDCARD_PLUGINS_DIR);
        mkdir(dir);
    }
    else
    {
        const char *home = getenv("HOME");
        if (home && home[0])
        {
            char d1[512], d2[512];
            snprintf(d1, sizeof(d1), "%s/.config", home);
            mkdir(d1);
            snprintf(d2, sizeof(d2), "%s/.config/rockbox.org", home);
            mkdir(d2);
            snprintf(dir, sizeof(dir), "%s/.config/rockbox.org/plugins", home);
            mkdir(dir);
        }
        else
        {
            strlcpy(dir, UIPOD_TMP_ROOT "/uipod_plugin_data", sizeof(dir));
            mkdir(dir);
        }
    }

    strlcpy(out, dir, cap);
    return strlen(out);
}

static struct UIPodPluginTheme host_get_theme(void)
{
    struct UIPodPluginTheme t;
    t.bg  = (uint32_t)(global_settings.bg_color  & 0xFFFFFF);
    t.fg  = (uint32_t)(global_settings.fg_color  & 0xFFFFFF);
    t.fg1 = (uint32_t)(global_settings.fg1_color & 0xFFFFFF);
    t.fg2 = (uint32_t)(global_settings.fg2_color & 0xFFFFFF);
    return t;
}

static void compute_logical_size_for_window(int win_w, int win_h,
                                            float *out_w, float *out_h)
{
    uint32_t pw = (win_w > 0) ? (uint32_t)win_w : (uint32_t)LCD_WIDTH;
    uint32_t ph = (win_h > 0) ? (uint32_t)win_h : (uint32_t)LCD_HEIGHT;
    float aspect = (float)pw / (float)ph;
    float diff43 = aspect - (4.0f / 3.0f);
    if (diff43 < 0.0f)
        diff43 = -diff43;

    float sw, sh;
    if (diff43 < 0.01f)
    {
        sw = 1024.0f;
        sh = 768.0f;
    }
    else if (aspect >= 1.2f)
    {
        sh = 768.0f;
        sw = (float)(int)(768.0f * aspect + 0.5f);
        if (sw < 920.0f)
            sw = 920.0f;
    }
    else
    {
        float clamped = (aspect > 0.35f) ? aspect : 0.35f;
        sw = 920.0f;
        sh = (float)(int)(920.0f / clamped + 0.5f);
        if (sh < 768.0f)
            sh = 768.0f;
    }

    if (out_w)
        *out_w = sw;
    if (out_h)
        *out_h = sh;
}

static void host_get_responsive_size(float *out_w, float *out_h)
{
    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);
    compute_logical_size_for_window(win_w, win_h, out_w, out_h);
}

static void host_canvas_begin_internal(uint32_t bg_rgb,
                                       bool show_header,
                                       const char *title,
                                       bool responsive,
                                       float *out_w,
                                       float *out_h)
{
    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);

    if (!uipod_gpu_ui_ensure_ready())
    {
        if (responsive)
            compute_logical_size_for_window(win_w, win_h, out_w, out_h);
        else
        {
            if (out_w) *out_w = 1024.0f;
            if (out_h) *out_h = 768.0f;
        }
        return;
    }

    if ((s_menu_on_screen || !s_has_presented_frame) &&
        !s_trans_snapshotted &&
        uipod_gpu_is_active())
    {
        (void)uipod_rs_ui_snapshot_transition();
        s_trans_snapshotted = true;
    }

    s_plugin_has_header = show_header;

    struct UIPodRsHeader hdr;
    const struct UIPodRsHeader *hdr_ptr = NULL;
    if (show_header)
    {
        uipod_gpu_fill_header(&hdr, (title && title[0]) ? title : "Plugin");
        hdr.legend = ""; /* Always show play icon + plugin title + battery */
        hdr_ptr = &hdr;
    }

    uipod_rs_ui_plugin_begin_ex((uint32_t)global_settings.bg_color,
                                (uint32_t)global_settings.fg_color,
                                (uint32_t)global_settings.fg1_color,
                                (uint32_t)global_settings.fg2_color,
                                bg_rgb,
                                hdr_ptr,
                                (uint32_t)win_w,
                                (uint32_t)win_h,
                                responsive,
                                out_w,
                                out_h);
}

static void host_canvas_begin(uint32_t bg_rgb, bool show_header, const char *title)
{
    host_canvas_begin_internal(bg_rgb, show_header, title, false, NULL, NULL);
}

static void host_canvas_begin_responsive(uint32_t bg_rgb,
                                         bool show_header,
                                         const char *title,
                                         float *out_w,
                                         float *out_h)
{
    host_canvas_begin_internal(bg_rgb, show_header, title, true, out_w, out_h);
}

static void host_canvas_fill_rect(float x, float y, float w, float h,
                                  float radius, uint32_t rgb)
{
    uipod_rs_ui_plugin_fill_rect(x, y, w, h, radius, rgb);
}

static void host_canvas_stroke_rect(float x, float y, float w, float h,
                                    float radius, float stroke_w, uint32_t rgb)
{
    uipod_rs_ui_plugin_stroke_rect(x, y, w, h, radius, stroke_w, rgb);
}

static void host_canvas_fill_circle(float cx, float cy, float r, uint32_t rgb)
{
    uipod_rs_ui_plugin_fill_circle(cx, cy, r, rgb);
}

static void host_canvas_draw_line(float x0, float y0, float x1, float y1,
                                  float stroke_w, uint32_t rgb)
{
    uipod_rs_ui_plugin_draw_line(x0, y0, x1, y1, stroke_w, rgb);
}

static float host_canvas_draw_text(float x, float center_y, const char *text,
                                   float font_size, uint8_t weight,
                                   uint8_t align, uint32_t rgb)
{
    return uipod_rs_ui_plugin_draw_text(x, center_y, text, font_size,
                                        weight, align, rgb);
}

static float host_canvas_measure_text(const char *text, float font_size,
                                      uint8_t weight)
{
    if (!uipod_gpu_ui_ensure_ready())
        return 0.0f;
    return uipod_rs_ui_plugin_measure_text(text, font_size, weight);
}

static void host_canvas_draw_progress_bar(float x, float y, float w, float h,
                                          float progress)
{
    uipod_rs_ui_plugin_draw_progress_bar((uint32_t)global_settings.bg_color,
                                         (uint32_t)global_settings.fg_color,
                                         (uint32_t)global_settings.fg1_color,
                                         (uint32_t)global_settings.fg2_color,
                                         x, y, w, h, progress);
}

static void host_canvas_draw_scrollbar(float x, float y, float w, float h,
                                       size_t total, size_t start, size_t visible)
{
    uipod_rs_ui_plugin_draw_scrollbar((uint32_t)global_settings.bg_color,
                                      (uint32_t)global_settings.fg_color,
                                      (uint32_t)global_settings.fg1_color,
                                      (uint32_t)global_settings.fg2_color,
                                      x, y, w, h, total, start, visible);
}

static void present_canvas_to_fb_cb(void *ctx)
{
    (void)ctx;
    struct viewport *prev_vp = lcd_set_viewport(NULL);
    fb_data *fb = FBADDR(0, 0);
    (void)uipod_rs_ui_plugin_present((uint8_t *)fb);
    lcd_set_viewport(prev_vp);
}

static void host_canvas_present(void)
{
    if (s_menu_on_screen || !s_has_presented_frame)
    {
        enum uipod_transition_dir dir = (s_menu_on_screen && s_has_presented_frame)
                                            ? UIPOD_TRANS_BACK
                                            : UIPOD_TRANS_FORWARD;
        bool had_gpu_snap     = s_trans_snapshotted;
        s_trans_snapshotted   = false;
        s_menu_on_screen      = false;
        s_prev_menu_cancelled = false;
        s_gpu_fbo_unsynced    = false;
        s_has_presented_frame = true;
        s_last_menu_title[0]  = '\0';
        uipod_transition_take_back();

        (void)uipod_rs_ui_plugin_flush_fbo();

        if (!lcd_panel_is_dark())
        {
            if (had_gpu_snap)
            {
                int style = global_settings.transition_style;
                if (style != UIPOD_TRANS_STYLE_OFF)
                {
                    uint8_t gpu_kind = (style >= 0 && style <= 4)
                                           ? (uint8_t)(style + 1)
                                           : 1U;
                    int8_t dir_sign = (dir == UIPOD_TRANS_FORWARD) ? 1 : -1;
                    unsigned long dur_ms = (style == UIPOD_TRANS_STYLE_SLIDE)
                                               ? UIPOD_TRANSITION_MS
                                               : 135UL;
                    unsigned long start = sdl_get_ms();
                    for (;;)
                    {
                        unsigned long el = sdl_get_ms() - start;
                        if (el >= dur_ms || lcd_panel_is_dark())
                            break;
                        float prog = (float)el / (float)dur_ms;
                        int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
                        sdl_gl_make_current();
                        sdl_gl_get_drawable_size(&win_w, &win_h);
                        uipod_rs_ui_set_volume_popup_clock((uint64_t)sdl_get_ms());
                        if (uipod_rs_ui_present_transition_gpu(
                                (uint32_t)win_w,
                                (uint32_t)win_h,
                                gpu_kind,
                                prog,
                                dir_sign,
                                s_plugin_has_header))
                        {
                            sdl_gl_swap_window();
                        }
                    }
                }
                uipod_gpu_note_rendered_screen(s_plugin_has_header);
                s_gpu_fbo_unsynced = true;
                pump_background_threads();
                return;
            }

            FOR_NB_SCREENS(i)
                viewportmanager_theme_undo(i, false);
            uipod_transition_animate(dir, present_canvas_to_fb_cb, NULL);
            FOR_NB_SCREENS(i)
                viewportmanager_theme_enable(i, false, NULL);
            uipod_gpu_note_rendered_screen(s_plugin_has_header);
            s_gpu_fbo_unsynced = true;
            pump_background_threads();
            return;
        }
    }

    s_trans_snapshotted   = false;
    s_has_presented_frame = true;

    if (lcd_panel_is_dark())
    {
        (void)uipod_rs_ui_plugin_flush_fbo();
        uipod_gpu_set_active(true);
        s_gpu_fbo_unsynced = true;
        sleep(HZ / 20);
        return;
    }

    int win_w = LCD_WIDTH, win_h = LCD_HEIGHT;
    sdl_gl_get_drawable_size(&win_w, &win_h);
    uipod_rs_ui_set_volume_popup_clock((uint64_t)sdl_get_ms());
    if (uipod_rs_ui_plugin_present_gpu((uint32_t)win_w, (uint32_t)win_h))
    {
        uipod_gpu_set_active(true);
        sdl_gl_swap_window();
        s_gpu_fbo_unsynced = true;
    }
    pump_background_threads();
}

static uint8_t *host_fb_ptr(void)
{
    s_trans_snapshotted = false;
    s_gpu_fbo_unsynced  = false;
    uipod_gpu_prepare_cpu_fallback();
    lcd_set_viewport(NULL);
    return (uint8_t *)FBADDR(0, 0);
}

static void host_fb_present(void)
{
    s_gpu_fbo_unsynced = false;
    if (s_menu_on_screen)
    {
        s_menu_on_screen      = false;
        s_prev_menu_cancelled = false;
        s_last_menu_title[0]  = '\0';
        uipod_transition_take_back();
    }
    if (lcd_panel_is_dark())
    {
        sleep(HZ / 20);
        return;
    }
    lcd_update();
    pump_background_threads();
}

/* ---- Native J2ME-Style Plugin System Menu (`host_show_menu`) ---------- */

struct plugin_sys_menu_row
{
    char label[128];
    char indic[80];
    bool has_indic;
    bool has_chevron;
};

struct plugin_sys_menu_page
{
    struct uipod_page          base;
    struct gui_synclist        lists;
    char                       title_buf[128];
    struct plugin_sys_menu_row rows[MAX_PLUGIN_MENU_ITEMS];
    int                        count;
    int32_t                    chosen; /* >= 0 selected, -1 cancelled, -2 force-exit */
};

static const char *sys_menu_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    struct plugin_sys_menu_page *mp = (struct plugin_sys_menu_page *)data;
    (void)buf; (void)buf_len;
    if (sel < 0 || sel >= mp->count)
        return "";
    return mp->rows[sel].label;
}

static const char *sys_menu_get_indic(int sel, void *data)
{
    struct plugin_sys_menu_page *mp = (struct plugin_sys_menu_page *)data;
    if (sel < 0 || sel >= mp->count || !mp->rows[sel].has_indic)
        return NULL;
    return mp->rows[sel].indic;
}

static bool sys_menu_get_chevron(int sel, void *data)
{
    struct plugin_sys_menu_page *mp = (struct plugin_sys_menu_page *)data;
    if (sel < 0 || sel >= mp->count)
        return false;
    return mp->rows[sel].has_chevron;
}

static void sys_menu_draw(struct uipod_page *self)
{
    struct plugin_sys_menu_page *mp = (struct plugin_sys_menu_page *)self;
    gui_synclist_draw(&mp->lists);
}

static int sys_menu_poll(struct uipod_page *self, int timeout)
{
    struct plugin_sys_menu_page *mp = (struct plugin_sys_menu_page *)self;
    int action;
    list_do_action(self->context, timeout, &mp->lists, &action);
    return action;
}

static enum uipod_page_result sys_menu_on_action(struct uipod_page *self, int action)
{
    struct plugin_sys_menu_page *mp = (struct plugin_sys_menu_page *)self;
    if (action == ACTION_STD_CANCEL || action == ACTION_STD_CONTEXT)
    {
        mp->chosen = -1;
        mp->base.no_arm_back = false;
        return UIPOD_PAGE_DONE;
    }
    if (action == ACTION_STD_OK && mp->count > 0)
    {
        int sel = gui_synclist_get_sel_pos(&mp->lists);
        mp->chosen = (sel >= 0 && sel < mp->count) ? (int32_t)sel : 0;
        mp->base.no_arm_back = true;
        return UIPOD_PAGE_DONE;
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable sys_menu_vtable = {
    .legend    = NULL,
    .draw      = sys_menu_draw,
    .poll      = sys_menu_poll,
    .on_action = sys_menu_on_action,
};

static const int sys_menu_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL, ACTION_STD_CONTEXT, -1
};

static int32_t host_show_menu(const char *title,
                              const struct UIPodPluginMenuItem *items,
                              size_t count,
                              size_t initial_sel)
{
    if (!items || count == 0)
        return -1;

    struct plugin_sys_menu_page *mp = calloc(1, sizeof(*mp));
    if (!mp)
        return -1;

    const char *safe_title = (title && title[0])
                                 ? title
                                 : (const char *)str(LANG_UIPOD_PLUGINS);
    strlcpy(mp->title_buf, safe_title, sizeof(mp->title_buf));

    int n = (count > MAX_PLUGIN_MENU_ITEMS) ? MAX_PLUGIN_MENU_ITEMS : (int)count;
    mp->count  = n;
    mp->chosen = -2; /* Remains -2 if exited via Hold-B Home or USB/Poweroff */

    for (int i = 0; i < n; i++)
    {
        strlcpy(mp->rows[i].label,
                items[i].label ? items[i].label : "",
                sizeof(mp->rows[i].label));
        if (items[i].value && items[i].value[0])
        {
            snprintf(mp->rows[i].indic, sizeof(mp->rows[i].indic),
                     "%s%s", UIPOD_INDIC_PLAIN, items[i].value);
            mp->rows[i].has_indic = true;
        }
        mp->rows[i].has_chevron = items[i].has_chevron;
    }

    bool same_title_in_place = s_menu_on_screen &&
                               !s_prev_menu_cancelled &&
                               strcmp(s_last_menu_title, mp->title_buf) == 0;

    mp->base.vt               = &sys_menu_vtable;
    mp->base.context          = s_has_presented_frame
                                    ? (CONTEXT_PLUGIN | CONTEXT_LIST)
                                    : CONTEXT_LIST;
    mp->base.allowed          = sys_menu_allowed;
    mp->base.title            = mp->title_buf;
    mp->base.no_enter_anim    = same_title_in_place;
    mp->base.enter_honor_back = true;

    gui_synclist_init(&mp->lists, sys_menu_get_name, mp, false, 1, NULL);
    gui_synclist_set_title(&mp->lists, mp->title_buf, Icon_NOICON);
    gui_synclist_set_indicator_callback(&mp->lists, sys_menu_get_indic);
    gui_synclist_set_chevron_callback(&mp->lists, sys_menu_get_chevron);
    gui_synclist_set_nb_items(&mp->lists, n);

    int sel = ((int)initial_sel >= 0 && (int)initial_sel < n) ? (int)initial_sel : 0;
    gui_synclist_select_item(&mp->lists, sel);

    if (!s_menu_on_screen)
        sync_gpu_fbo_to_rockbox_fb_if_needed();

#ifdef HAVE_PRIORITY_SCHEDULING
    unsigned int self_tid = thread_self();
    thread_set_priority(self_tid, s_saved_ui_prio);
#endif
    FOR_NB_SCREENS(i)
        viewportmanager_theme_undo(i, false);

    uipod_page_run(&mp->base);

    FOR_NB_SCREENS(i)
        viewportmanager_theme_enable(i, false, NULL);
#ifdef HAVE_PRIORITY_SCHEDULING
    thread_set_priority(self_tid, PRIORITY_BACKGROUND);
#endif

    int32_t res = mp->chosen;
    if (uipod_home_pending || mp->base.home)
        res = -2;

    s_menu_on_screen      = true;
    s_prev_menu_cancelled = (res == -1);
    strlcpy(s_last_menu_title, mp->title_buf, sizeof(s_last_menu_title));

    action_wait_for_release();
    s_b_press_tick = 0;
    s_b_hold_fired = false;

    free(mp);
    return res;
}

/* ---- Input Polling (`Utility` vs `MenuDriven`) ------------------------ */

static void process_single_button_event(long ev, struct UIPodPluginInput *out)
{
    if (ev == BUTTON_NONE)
        return;

    if (ev & SYS_EVENT)
    {
        int handled = default_event_handler(ev);
        if (handled == SYS_USB_CONNECTED ||
            handled == SYS_POWEROFF ||
            handled == SYS_REBOOT)
        {
            out->should_exit = true;
        }
        return;
    }

    backlight_on();

    long base = ev & ~(BUTTON_REPEAT | BUTTON_REL);

#if defined(BUTTON_VOL_UP) && defined(BUTTON_VOL_DOWN)
    if ((base & (BUTTON_VOL_UP | BUTTON_VOL_DOWN)) != 0)
    {
        if (!(ev & BUTTON_REL))
        {
#if defined(BUTTON_SELECT) && defined(HAVE_BACKLIGHT_BRIGHTNESS)
            if ((button_status() & BUTTON_SELECT) != 0)
                adjust_brightness((base & BUTTON_VOL_UP) ? 1 : -1);
            else
#endif
                adjust_volume((base & BUTTON_VOL_UP) ? 1 : -1);
        }
        return;
    }
#endif

    if (!s_active_menu_driven && base == BUTTON_B)
    {
        /* Utility mode: short B tap exits on release; hold B >= 650 ms unwinds Home. */
        if (!(ev & (BUTTON_REPEAT | BUTTON_REL)))
        {
            s_b_press_tick = current_tick;
            s_b_hold_fired = false;
        }
        else if (ev & BUTTON_REPEAT)
        {
            if (!s_b_hold_fired && s_b_press_tick != 0 &&
                current_tick - s_b_press_tick >= PLUGIN_HOLD_B_TICKS)
            {
                s_b_hold_fired     = true;
                uipod_home_pending = true;
                out->should_exit   = true;
            }
        }
        else if (ev & BUTTON_REL)
        {
            bool was_hold  = s_b_hold_fired;
            s_b_press_tick = 0;
            s_b_hold_fired = false;
            if (!was_hold)
                out->pressed |= (uint32_t)BUTTON_B;
        }
        return;
    }

    /* MenuDriven mode: MENU or START requests the native J2ME-style system menu! */
    if (s_active_menu_driven &&
        (base & (BUTTON_MENU | BUTTON_START)) != 0 &&
        !(ev & (BUTTON_REPEAT | BUTTON_REL)))
    {
        out->menu_requested = true;
    }

    /* Action buttons fire immediately on initial press; D-pad also auto-repeats. */
    if (!(ev & BUTTON_REL))
    {
        bool is_dpad = (base & (BUTTON_UP | BUTTON_DOWN | BUTTON_LEFT | BUTTON_RIGHT)) != 0;
        if (!(ev & BUTTON_REPEAT) || is_dpad)
            out->pressed |= (uint32_t)(base & BUTTON_MAIN);
    }
}

static struct UIPodPluginInput host_poll_input(uint32_t timeout_ms)
{
    struct UIPodPluginInput out;
    memset(&out, 0, sizeof(out));

    if (timeout_ms == 0)
    {
        pump_background_threads();
        for (;;)
        {
            long ev = button_get(false);
            if (ev == BUTTON_NONE)
                break;
            process_single_button_event(ev, &out);
            if (out.should_exit)
                break;
        }
    }
    else
    {
        if (timeout_ms > 16u && uipod_gpu_is_volume_popup_active())
            timeout_ms = 16u;
        long ticks = (long)((timeout_ms * (uint32_t)HZ + 999u) / 1000u);
        if (ticks < 1)
            ticks = 1;
        long ev = button_get_w_tmo(ticks);
        process_single_button_event(ev, &out);
        while (!out.should_exit)
        {
            long more = button_get(false);
            if (more == BUTTON_NONE)
                break;
            process_single_button_event(more, &out);
        }
    }

    long raw_held = button_status();
    if (!s_active_menu_driven &&
        (raw_held & BUTTON_B) != 0 && s_b_press_tick != 0 && !s_b_hold_fired &&
        current_tick - s_b_press_tick >= PLUGIN_HOLD_B_TICKS)
    {
        s_b_hold_fired     = true;
        uipod_home_pending = true;
        out.should_exit    = true;
    }

    out.held = (uint32_t)(raw_held & BUTTON_MAIN & ~(BUTTON_VOL_UP | BUTTON_VOL_DOWN));
    return out;
}

static bool host_keyboard_input(char *buf, size_t cap, bool allow_symbols)
{
    if (!buf || cap == 0)
        return false;
    sync_gpu_fbo_to_rockbox_fb_if_needed();
    int r = uipod_kbd_input_ex(buf, (int)cap, allow_symbols);
    action_wait_for_release();
    return r == 0;
}

static void host_show_splash(const char *message, uint32_t duration_ms)
{
    if (!message || !message[0])
        return;
    sync_gpu_fbo_to_rockbox_fb_if_needed();
    long ticks = (long)((duration_ms * (uint32_t)HZ + 999u) / 1000u);
    if (ticks < HZ / 4)
        ticks = HZ / 4;
    splash(ticks, message);
}

static uint32_t host_audio_status(void)
{
    int st = audio_status();
    if ((st & AUDIO_STATUS_PLAY) == 0)
        return 0u;
    if ((st & AUDIO_STATUS_PAUSE) != 0)
        return 2u;
    return 1u;
}

static void host_audio_toggle_play_pause(void)
{
    int st = audio_status();
    if (st & AUDIO_STATUS_PLAY)
    {
        if (st & AUDIO_STATUS_PAUSE)
            audio_resume();
        else
            audio_pause();
    }
    else
    {
        playlist_resume_saved();
    }
    pump_background_threads();
}

static void host_audio_next(void)
{
    if (audio_status() & AUDIO_STATUS_PLAY)
    {
        audio_next();
        pump_background_threads();
    }
}

static void host_audio_prev(void)
{
    if (audio_status() & AUDIO_STATUS_PLAY)
    {
        audio_prev();
        pump_background_threads();
    }
}

static void host_audio_seek_ms(uint32_t pos_ms)
{
    int st = audio_status();
    if (!(st & AUDIO_STATUS_PLAY))
    {
        playlist_resume_saved();
        pump_background_threads();
        st = audio_status();
    }
    if (st & AUDIO_STATUS_PLAY)
    {
        audio_pre_ff_rewind();
        audio_ff_rewind((long)pos_ms);
        if (st & AUDIO_STATUS_PAUSE)
            audio_resume();
        /* Let audio & codec coroutines commit the seek immediately */
        for (int i = 0; i < 24; i++)
            yield();
    }
}

static bool host_audio_current_track(char *path_out, size_t path_cap,
                                     char *title_out, size_t title_cap,
                                     char *artist_out, size_t artist_cap,
                                     uint32_t *pos_ms_out, uint32_t *len_ms_out)
{
    if (!(audio_status() & AUDIO_STATUS_PLAY))
        return false;
    struct mp3entry *id3 = audio_current_track();
    if (!id3)
        return false;

    if (path_out && path_cap > 0)
        strlcpy(path_out, id3->path[0] ? id3->path : "", path_cap);
    if (title_out && title_cap > 0)
    {
        const char *t = (id3->title && id3->title[0]) ? id3->title : id3->path;
        strlcpy(title_out, t ? t : "", title_cap);
    }
    if (artist_out && artist_cap > 0)
        strlcpy(artist_out, (id3->artist && id3->artist[0]) ? id3->artist : "", artist_cap);
    if (pos_ms_out)
    {
        uint32_t out_ms = (pcm_alsa_output_delay_us() + tape_deck_fx_delay_us()) / 1000u;
        uint32_t el = (uint32_t)id3->elapsed;
        *pos_ms_out = (el > out_ms) ? (el - out_ms) : 0u;
    }
    if (len_ms_out)
        *len_ms_out = (uint32_t)id3->length;
    return true;
}

static size_t host_audio_read_pcm(uint32_t ahead_ms,
                                  int16_t *out_stereo,
                                  size_t max_frames)
{
    if (!out_stereo || max_frames == 0)
        return 0;
    int st = audio_status();
    if (!(st & AUDIO_STATUS_PLAY) || (st & AUDIO_STATUS_PAUSE))
        return 0;

    if (ahead_ms == 0)
        return (size_t)pcm_viz_read_synced(out_stereo, (unsigned)max_frames, NULL);

    uint32_t hw_delay_ms = (pcm_alsa_output_delay_us() + tape_deck_fx_delay_us()) / 1000u;
    if (ahead_ms <= hw_delay_ms)
        return (size_t)pcm_viz_latest(out_stereo, (unsigned)max_frames);

    return pcmbuf_peek_ahead_s16(ahead_ms - hw_delay_ms, out_stereo, max_frames);
}

static bool host_audio_play_track(const char *path, uint32_t start_ms)
{
    if (!path || !path[0])
        return false;
    bool ok = uipod_play_single_at_ms(path, (unsigned long)start_ms);
    pump_background_threads();
    return ok;
}

static void host_audio_stop(void)
{
    if (audio_status() & AUDIO_STATUS_PLAY)
    {
        audio_stop();
        pump_background_threads();
    }
}

static size_t host_library_track_count(void)
{
    int n = uipod_library_music_count();
    return (n > 0) ? (size_t)n : 0u;
}

static bool host_library_track_at(size_t index,
                                  char *path_out, size_t path_cap,
                                  char *artist_out, size_t artist_cap,
                                  char *title_out, size_t title_cap,
                                  uint32_t *duration_ms_out)
{
    return uipod_library_music_at((int)index,
                                  path_out, path_cap,
                                  artist_out, artist_cap,
                                  title_out, title_cap,
                                  duration_ms_out);
}

static bool host_audio_get_apu_telemetry(struct UIPodPluginApuTelemetry *out)
{
    if (!out)
        return false;
    memset(out, 0, sizeof(*out));
    if (uipod_rs_nsf_get_active_chip_flags() < 0)
        return false;

    struct uipod_rs_apu_telemetry raw;
    memset(&raw, 0, sizeof(raw));
    if (!uipod_rs_nsf_get_telemetry(&raw))
        return false;

    out->has_apu_telemetry = 1u;
    out->channel_count     = raw.channel_count;
    out->chip_flags        = raw.chip_flags;
    out->mute_mask         = raw.mute_mask;
    for (int i = 0; i < 8; i++)
    {
        out->channels[i].active        = raw.channels[i].active;
        out->channels[i].muted         = raw.channels[i].muted;
        out->channels[i].onset_trigger = raw.channels[i].onset_trigger;
        out->channels[i].duty_or_mode  = raw.channels[i].duty_or_mode;
        out->channels[i].volume        = raw.channels[i].volume;
        out->channels[i].freq_hz       = raw.channels[i].freq_hz;
    }
    return true;
}

static const struct UIPodPluginApi s_host_api = {
    .api_version             = UIPOD_PLUGIN_API_VERSION,
    .get_ticks_ms            = host_get_ticks_ms,
    .sleep_ms                = host_sleep_ms,
    .get_language            = host_get_language,
    .get_data_dir            = host_get_data_dir,
    .get_theme               = host_get_theme,
    .canvas_begin            = host_canvas_begin,
    .canvas_fill_rect        = host_canvas_fill_rect,
    .canvas_stroke_rect      = host_canvas_stroke_rect,
    .canvas_fill_circle      = host_canvas_fill_circle,
    .canvas_draw_line        = host_canvas_draw_line,
    .canvas_draw_text        = host_canvas_draw_text,
    .canvas_measure_text     = host_canvas_measure_text,
    .canvas_draw_progress_bar= host_canvas_draw_progress_bar,
    .canvas_draw_scrollbar   = host_canvas_draw_scrollbar,
    .canvas_present          = host_canvas_present,
    .fb_ptr                  = host_fb_ptr,
    .fb_present              = host_fb_present,
    .poll_input              = host_poll_input,
    .show_menu               = host_show_menu,
    .keyboard_input          = host_keyboard_input,
    .show_splash             = host_show_splash,
    .audio_status            = host_audio_status,
    .audio_toggle_play_pause = host_audio_toggle_play_pause,
    .audio_next              = host_audio_next,
    .audio_prev              = host_audio_prev,
    .audio_seek_ms           = host_audio_seek_ms,
    .audio_current_track     = host_audio_current_track,
    .audio_read_pcm          = host_audio_read_pcm,
    .audio_play_track        = host_audio_play_track,
    .audio_stop              = host_audio_stop,
    .library_track_count     = host_library_track_count,
    .library_track_at        = host_library_track_at,
    .canvas_begin_responsive = host_canvas_begin_responsive,
    .get_responsive_size     = host_get_responsive_size,
    .audio_get_apu_telemetry = host_audio_get_apu_telemetry,
};

/* ---- Safe Execution Envelope ------------------------------------------ */

static bool invoke_plugin_guarded(const char *plugin_id, const char *open_path)
{
    s_b_press_tick        = 0;
    s_b_hold_fired        = false;
    s_active_menu_driven  = uipod_rs_plugins_is_menu_driven(plugin_id, open_path);
    s_menu_on_screen      = false;
    s_prev_menu_cancelled = false;
    s_gpu_fbo_unsynced    = false;
    s_has_presented_frame = false;
    s_plugin_has_header   = false;
    s_trans_snapshotted   = false;
    s_last_menu_title[0]  = '\0';
    action_wait_for_release();

    FOR_NB_SCREENS(i)
        viewportmanager_theme_enable(i, false, NULL);

    /* Hold dynamic cpufreq during active plugin sessions so a low user CPU cap
     * cannot starve 60 FPS GPU rasterization + lossless/stream audio decoding. */
    retrohh_cpu_hold_dynamic();

#ifdef HAVE_PRIORITY_SCHEDULING
    unsigned int self_tid = thread_self();
    int prev_prio = thread_get_priority(self_tid);
    s_saved_ui_prio = prev_prio;
    thread_set_priority(self_tid, PRIORITY_BACKGROUND);
#endif

    char err[160];
    err[0] = '\0';
    int32_t rc;
    if (plugin_id && plugin_id[0])
    {
        rc = uipod_rs_plugins_run_by_id(plugin_id, &s_host_api, open_path,
                                        err, sizeof(err));
    }
    else
    {
        rc = uipod_rs_plugins_open_viewer(open_path, &s_host_api,
                                          err, sizeof(err));
    }

#ifdef HAVE_PRIORITY_SCHEDULING
    thread_set_priority(self_tid, prev_prio);
#endif
    retrohh_cpu_release_dynamic();

    if (!s_menu_on_screen)
        sync_gpu_fbo_to_rockbox_fb_if_needed();
    else
        s_gpu_fbo_unsynced = false;

    s_active_menu_driven  = false;
    s_menu_on_screen      = false;
    s_prev_menu_cancelled = false;
    s_has_presented_frame = false;
    s_plugin_has_header   = false;
    s_trans_snapshotted   = false;
    s_last_menu_title[0]  = '\0';

    FOR_NB_SCREENS(i)
        viewportmanager_theme_undo(i, false);

    action_wait_for_release();
    button_touch_activity();

    if (rc == -2 && err[0])
    {
        splashf(HZ * 3 / 2, "%s: %s", str(LANG_UIPOD_PLUGIN_ERROR), err);
        uipod_transition_suppress_next();
    }
    else if (!uipod_home_pending)
    {
        uipod_transition_arm_back();
    }
    return rc != -1;
}

/* ---- Public Lifecycle & Viewer Hooks ---------------------------------- */

void uipod_plugins_init(void)
{
#ifdef __ANDROID__
    const char *nlibdir = getenv("UIPOD_NATIVE_LIB_DIR");
    uipod_rs_plugins_init((nlibdir && nlibdir[0]) ? nlibdir : BUNDLED_PLUGINS_DIR,
                          SDCARD_PLUGINS_DIR);
#else
    uipod_rs_plugins_init(BUNDLED_PLUGINS_DIR, SDCARD_PLUGINS_DIR);
#endif
}

bool uipod_plugins_menu_driven_active(void)
{
    return s_active_menu_driven;
}

bool uipod_plugins_can_open_file(const char *path)
{
    if (!path || !path[0])
        return false;
    return uipod_rs_plugins_is_plugin_file(path) ||
           uipod_rs_plugins_has_viewer(path);
}

bool uipod_plugins_open_file(const char *path)
{
    if (!uipod_plugins_can_open_file(path))
        return false;
    return invoke_plugin_guarded(NULL, path);
}

/* ---- Category Plugin List Screen (`Games` / `Apps` / `Demos` / `Viewers`) */

struct plugin_cat_page
{
    struct uipod_page       base;
    struct gui_synclist     lists;
    uint32_t                category;
    struct UIPodPluginInfoC items[MAX_CAT_PLUGINS];
    int                     count;
    char                    indics[MAX_CAT_PLUGINS][48];
};

static int cat_lang_id(uint32_t cat)
{
    switch (cat)
    {
        case UIPOD_PLUGIN_CAT_GAME:   return LANG_UIPOD_PLUGINS_GAMES;
        case UIPOD_PLUGIN_CAT_APP:    return LANG_UIPOD_PLUGINS_APPS;
        case UIPOD_PLUGIN_CAT_DEMO:   return LANG_UIPOD_PLUGINS_DEMOS;
        case UIPOD_PLUGIN_CAT_VIEWER:
        default:                      return LANG_UIPOD_PLUGINS_VIEWERS;
    }
}

static void cat_page_reload(struct plugin_cat_page *p)
{
    p->count = (int)uipod_rs_plugins_list_category(
        p->category, is_lang_ukrainian(), p->items, MAX_CAT_PLUGINS);
    for (int i = 0; i < p->count; i++)
    {
        if (p->items[i].is_sdcard)
            snprintf(p->indics[i], sizeof(p->indics[i]), "%sSD • v%s",
                     UIPOD_INDIC_PLAIN, p->items[i].version);
        else
            snprintf(p->indics[i], sizeof(p->indics[i]), "%sv%s",
                     UIPOD_INDIC_PLAIN, p->items[i].version);
    }
    int rows = p->count > 0 ? p->count : 1;
    gui_synclist_set_nb_items(&p->lists, rows);
}

static const char *cat_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    struct plugin_cat_page *p = (struct plugin_cat_page *)data;
    (void)buf; (void)buf_len;
    if (p->count == 0)
        return str(LANG_UIPOD_PLUGINS_EMPTY);
    if (sel < 0 || sel >= p->count)
        return "";
    return p->items[sel].title;
}

static const char *cat_get_indic(int sel, void *data)
{
    struct plugin_cat_page *p = (struct plugin_cat_page *)data;
    if (p->count == 0 || sel < 0 || sel >= p->count)
        return NULL;
    return p->indics[sel];
}

static const struct uipod_svg_icon *cat_get_svg(int sel, void *data)
{
    struct plugin_cat_page *p = (struct plugin_cat_page *)data;
    if (p->count == 0 || sel < 0 || sel >= p->count)
        return NULL;
    return uipod_svg_tag("uipod");
}

static void cat_draw(struct uipod_page *self)
{
    struct plugin_cat_page *p = (struct plugin_cat_page *)self;
    gui_synclist_draw(&p->lists);
}

static int cat_poll(struct uipod_page *self, int timeout)
{
    struct plugin_cat_page *p = (struct plugin_cat_page *)self;
    int action;
    list_do_action(self->context, timeout, &p->lists, &action);
    return action;
}

static enum uipod_page_result cat_on_action(struct uipod_page *self, int action)
{
    struct plugin_cat_page *p = (struct plugin_cat_page *)self;
    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;

    if (action == ACTION_STD_OK && p->count > 0)
    {
        int sel = gui_synclist_get_sel_pos(&p->lists);
        if (sel >= 0 && sel < p->count)
        {
            invoke_plugin_guarded(p->items[sel].id, NULL);
            if (uipod_home_pending)
            {
                p->base.home = true;
                return UIPOD_PAGE_DONE;
            }
        }
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable cat_vtable = {
    .legend    = NULL,
    .draw      = cat_draw,
    .poll      = cat_poll,
    .on_action = cat_on_action,
};

static const int cat_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL, -1
};

static void open_plugin_category(uint32_t category)
{
    struct plugin_cat_page *p = calloc(1, sizeof(*p));
    if (!p)
        return;
    const char *title = str(cat_lang_id(category));
    p->base.vt      = &cat_vtable;
    p->base.context = CONTEXT_LIST;
    p->base.allowed = cat_allowed;
    p->base.title   = title;
    p->category     = category;

    gui_synclist_init(&p->lists, cat_get_name, p, false, 1, NULL);
    gui_synclist_set_title(&p->lists, (char *)title, Icon_NOICON);
    gui_synclist_set_indicator_callback(&p->lists, cat_get_indic);
    gui_synclist_set_svg_icon_callback(&p->lists, cat_get_svg);
    cat_page_reload(p);
    gui_synclist_select_item(&p->lists, 0);

    uipod_page_run(&p->base);
    free(p);
}

/* ---- `Extras -> Plugins` Hub Screen ----------------------------------- */

enum
{
    HUB_ROW_GAMES   = 0,
    HUB_ROW_APPS    = 1,
    HUB_ROW_DEMOS   = 2,
    HUB_ROW_VIEWERS = 3,
    HUB_ROW_RESCAN  = 4,
    HUB_ROW_COUNT   = 5,
};

struct plugins_hub_page
{
    struct uipod_page   base;
    struct gui_synclist lists;
    char                indics[4][24];
};

static const char *hub_get_name(int sel, void *data, char *buf, size_t buf_len)
{
    (void)data; (void)buf; (void)buf_len;
    switch (sel)
    {
        case HUB_ROW_GAMES:   return str(LANG_UIPOD_PLUGINS_GAMES);
        case HUB_ROW_APPS:    return str(LANG_UIPOD_PLUGINS_APPS);
        case HUB_ROW_DEMOS:   return str(LANG_UIPOD_PLUGINS_DEMOS);
        case HUB_ROW_VIEWERS: return str(LANG_UIPOD_PLUGINS_VIEWERS);
        case HUB_ROW_RESCAN:  return str(LANG_UIPOD_PLUGINS_RESCAN);
        default:              return "";
    }
}

static const char *hub_get_indic(int sel, void *data)
{
    struct plugins_hub_page *p = (struct plugins_hub_page *)data;
    if (sel < 0 || sel >= 4)
        return NULL;
    size_t cnt = uipod_rs_plugins_count_category((uint32_t)sel);
    snprintf(p->indics[sel], sizeof(p->indics[sel]), "%s%zu",
             UIPOD_INDIC_PLAIN, cnt);
    return p->indics[sel];
}

static bool hub_get_chevron(int sel, void *data)
{
    (void)data;
    return sel >= 0 && sel < 4;
}

static void hub_draw(struct uipod_page *self)
{
    struct plugins_hub_page *p = (struct plugins_hub_page *)self;
    gui_synclist_draw(&p->lists);
}

static int hub_poll(struct uipod_page *self, int timeout)
{
    struct plugins_hub_page *p = (struct plugins_hub_page *)self;
    int action;
    list_do_action(self->context, timeout, &p->lists, &action);
    return action;
}

static enum uipod_page_result hub_on_action(struct uipod_page *self, int action)
{
    struct plugins_hub_page *p = (struct plugins_hub_page *)self;
    if (action == ACTION_STD_CANCEL)
        return UIPOD_PAGE_DONE;

    if (action == ACTION_STD_OK)
    {
        int sel = gui_synclist_get_sel_pos(&p->lists);
        if (sel >= 0 && sel < 4)
        {
            open_plugin_category((uint32_t)sel);
            if (uipod_home_pending)
            {
                p->base.home = true;
                return UIPOD_PAGE_DONE;
            }
        }
        else if (sel == HUB_ROW_RESCAN)
        {
            size_t total = uipod_rs_plugins_rescan();
            splashf(HZ, str(LANG_UIPOD_PLUGINS_FOUND_FMT), (int)total);
            hub_draw(self);
        }
    }
    return UIPOD_PAGE_STAY;
}

static const struct uipod_page_vtable hub_vtable = {
    .legend    = NULL,
    .draw      = hub_draw,
    .poll      = hub_poll,
    .on_action = hub_on_action,
};

static const int hub_allowed[] = {
    ACTION_STD_OK, ACTION_STD_CANCEL, -1
};

int uipod_plugins_page(void)
{
    struct plugins_hub_page p;
    memset(&p, 0, sizeof(p));
    p.base.vt      = &hub_vtable;
    p.base.context = CONTEXT_LIST;
    p.base.allowed = hub_allowed;
    p.base.title   = str(LANG_UIPOD_PLUGINS);

    gui_synclist_init(&p.lists, hub_get_name, &p, false, 1, NULL);
    gui_synclist_set_title(&p.lists, (char *)str(LANG_UIPOD_PLUGINS), Icon_NOICON);
    gui_synclist_set_indicator_callback(&p.lists, hub_get_indic);
    gui_synclist_set_chevron_callback(&p.lists, hub_get_chevron);
    gui_synclist_set_nb_items(&p.lists, HUB_ROW_COUNT);
    gui_synclist_select_item(&p.lists, 0);

    uipod_page_run(&p.base);
    return 0;
}
