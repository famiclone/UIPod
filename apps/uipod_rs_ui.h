/***************************************************************************
 * UIPod: Thin C ABI header & glue helpers for the pure-Rust GPU UI engine
 * (`crates/uipod-ui`, `taffy` + `femtovg` on OpenGL ES 3.0).
 ****************************************************************************/
#ifndef UIPOD_RS_UI_H
#define UIPOD_RS_UI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define UIPOD_RS_LIST_VISIBLE_ROWS 8
#define UIPOD_RS_MAX_VISIBLE_ROWS  24
#define UIPOD_RS_HEADER_BAND_ROWS  120

/* Prefix for `callback_get_item_indicator` to render plain right-aligned
 * value text (`Active5`, no `< >` arrows) instead of cycleable `< Value >`. */
#define UIPOD_INDIC_PLAIN         "\x1f"
/* Prefix for `callback_get_item_indicator` to render right-aligned value text
 * followed by submenu chevron `>` (e.g. `"3/5 ON ›"` or `"Sound Effects (8) ›"`). */
#define UIPOD_INDIC_VALUE_CHEVRON "\x1e"

enum uipod_rs_source_icon
{
    UIPOD_SRC_ICON_NONE      = 0,
    UIPOD_SRC_ICON_MICROSD   = 1,
    UIPOD_SRC_ICON_NAVIDROME = 2,
    UIPOD_SRC_ICON_JELLYFIN  = 3,
};

typedef const void *(*uipod_rs_gl_get_proc_fn)(const char *name);

struct UIPodRsHeader
{
    const char *title;
    const char *legend;
    uint32_t    play_mode;       /* 0=Stop, 1=Play, 2=Pause, 3=FF, 4=Rew */
    uint8_t     battery_percent; /* 0..100 */
    bool        is_charging;
    bool        is_hold;
    bool        is_downloading;
    uint8_t     source_icon;     /* bits 0..6: enum uipod_rs_source_icon; 0x80: is_fx */
    uint32_t    download_done;
    uint32_t    download_total;
};

struct UIPodRsListRow
{
    const char *label;
    const char *right_value;
    const char *file_tag;
    bool        selected;
    uint8_t     right_kind; /* 0=None, 1=Chevron, 2=ToggleOff, 3=ToggleOn, 4=CycleValue, 5=Value */
};

struct UIPodRsListScreen
{
    uint32_t                    bg_rgb;
    uint32_t                    fg_rgb;
    uint32_t                    fg1_rgb;
    uint32_t                    fg2_rgb;
    struct UIPodRsHeader        header;
    const struct UIPodRsListRow *rows;
    size_t                      row_count;
    size_t                      total_items;
    size_t                      start_item;
    size_t                      visible_items;
};

struct UIPodRsNsfApuPill
{
    const char *short_code;
    bool        active;
    bool        muted;
    float       volume;
};

struct UIPodRsNsfChannelRow
{
    const char *short_code;
    const char *display_name;
    bool        enabled;
    bool        soloed;
    bool        active;
    float       volume;
    bool        selected;
};

struct UIPodRsNsfChannels
{
    uint32_t                          bg_rgb;
    uint32_t                          fg_rgb;
    uint32_t                          fg1_rgb;
    uint32_t                          fg2_rgb;
    struct UIPodRsHeader              header;
    const struct UIPodRsNsfChannelRow *channels;
    size_t                            channel_count;
    const char                       *enable_all_label;
    bool                              enable_all_selected;
    const char                       *footer_hint;
    size_t                            start_item;
    size_t                            visible_items;
};

struct UIPodRsNowPlaying
{
    uint32_t                       bg_rgb;
    uint32_t                       fg_rgb;
    uint32_t                       fg1_rgb;
    uint32_t                       fg2_rgb;
    struct UIPodRsHeader           header;
    const char                    *track_pos_label;
    bool                           eq_enabled;
    const char                    *title;
    const char                    *artist;
    const char                    *album;
    const char                    *codec_label;
    const char                    *bitdepth_label;
    const char                    *samplerate_label;
    float                          progress;
    const char                    *elapsed_label;
    const char                    *remaining_label;
    const char                    *center_status_label;
    const uint8_t                 *art_rgb24;
    bool                           mono_album_art;
    bool                           alt_layout;
    bool                           is_nsf;
    bool                           nsf_endless_loop;
    const char                    *nsf_chip_badge;
    float                          nsf_loop_progress; /* < 0.0 when None */
    const struct UIPodRsNsfApuPill *nsf_channels;
    size_t                         nsf_channel_count;
    const char                    *dj_status_label;
    float                          dj_progress;
    bool                           dj_bass_swap_flash;
};

struct UIPodRsKeyboardKey
{
    const char *label;
    bool        selected;
    bool        is_modifier;
    bool        is_active_toggle;
};

struct UIPodRsKeyboard
{
    uint32_t                         bg_rgb;
    uint32_t                         fg_rgb;
    uint32_t                         fg1_rgb;
    uint32_t                         fg2_rgb;
    const char                      *text;
    size_t                           row_counts[5];
    const struct UIPodRsKeyboardKey *keys;
};

struct UIPodRsGraphicEq
{
    uint32_t             bg_rgb;
    uint32_t             fg_rgb;
    uint32_t             fg1_rgb;
    uint32_t             fg2_rgb;
    struct UIPodRsHeader header;
    int16_t              preamp_tenths_db;
    int16_t              band_gains_tenths_db[10];
    size_t               selected_col;
    bool                 adjusting;
    bool                 bypassed;
};

/* Raw Rust staticlib (`libuipod_ffi.a`) entry points */
bool uipod_rs_ui_init(uipod_rs_gl_get_proc_fn get_proc,
                      const char *cjk_font_path);
bool uipod_rs_ui_is_ready(void);
void uipod_rs_ui_shutdown(void);

bool uipod_rs_ui_render_header(uint32_t bg_rgb,
                               uint32_t fg_rgb,
                               uint32_t fg1_rgb,
                               uint32_t fg2_rgb,
                               const struct UIPodRsHeader *header,
                               uint8_t *fb_bgr);
bool uipod_rs_ui_render_list(const struct UIPodRsListScreen *screen,
                             uint8_t *fb_bgr);
bool uipod_rs_ui_render_now_playing(const struct UIPodRsNowPlaying *np,
                                    uint8_t *fb_bgr);
bool uipod_rs_ui_render_keyboard(const struct UIPodRsKeyboard *kbd,
                                 uint8_t *fb_bgr);

/* Plugin GPU Canvas entry points (`crates/uipod-ui/src/ffi.rs`) */
bool  uipod_rs_ui_plugin_begin(uint32_t bg_rgb,
                               uint32_t fg_rgb,
                               uint32_t fg1_rgb,
                               uint32_t fg2_rgb,
                               uint32_t clear_rgb,
                               const struct UIPodRsHeader *header);
bool  uipod_rs_ui_plugin_begin_ex(uint32_t bg_rgb,
                                  uint32_t fg_rgb,
                                  uint32_t fg1_rgb,
                                  uint32_t fg2_rgb,
                                  uint32_t clear_rgb,
                                  const struct UIPodRsHeader *header,
                                  uint32_t win_w,
                                  uint32_t win_h,
                                  bool responsive,
                                  float *out_logical_w,
                                  float *out_logical_h);

size_t uipod_rs_ui_visible_rows_for_window(uint32_t win_w, uint32_t win_h);
bool   uipod_rs_ui_render_header_gpu(uint32_t bg_rgb,
                                     uint32_t fg_rgb,
                                     uint32_t fg1_rgb,
                                     uint32_t fg2_rgb,
                                     const struct UIPodRsHeader *header,
                                     uint32_t win_w,
                                     uint32_t win_h);
bool   uipod_rs_ui_render_list_gpu(const struct UIPodRsListScreen *screen,
                                   uint32_t win_w,
                                   uint32_t win_h);
bool   uipod_rs_ui_render_now_playing_gpu(const struct UIPodRsNowPlaying *np,
                                          uint32_t win_w,
                                          uint32_t win_h);
bool   uipod_rs_ui_render_keyboard_gpu(const struct UIPodRsKeyboard *kbd,
                                       uint32_t win_w,
                                       uint32_t win_h);
bool   uipod_rs_ui_render_graphic_eq(const struct UIPodRsGraphicEq *eq,
                                     uint8_t *fb_bgr);
bool   uipod_rs_ui_render_graphic_eq_gpu(const struct UIPodRsGraphicEq *eq,
                                         uint32_t win_w,
                                         uint32_t win_h);
bool   uipod_rs_ui_render_nsf_channels(const struct UIPodRsNsfChannels *ch_sc,
                                       uint8_t *fb_bgr);
bool   uipod_rs_ui_render_nsf_channels_gpu(const struct UIPodRsNsfChannels *ch_sc,
                                           uint32_t win_w,
                                           uint32_t win_h);
bool   uipod_rs_ui_snapshot_transition(void);
bool   uipod_rs_ui_present_transition_gpu(uint32_t win_w,
                                          uint32_t win_h,
                                          uint8_t kind,
                                          float progress,
                                          int8_t dir,
                                          bool pin_header);
bool   uipod_rs_ui_present_fade_rgb_gpu(uint32_t win_w,
                                        uint32_t win_h,
                                        float fade,
                                        uint32_t target_rgb);
/* Unified Stacked HUD System Rust ABI (`crates/uipod-ui/src/ffi.rs`) */
void     uipod_rs_hud_volume(float level_norm, uint64_t now_ms);
void     uipod_rs_hud_toast(const char *key,
                            const char *message,
                            uint64_t hold_ms,
                            uint64_t now_ms);
uint32_t uipod_rs_hud_progress_begin(const char *title,
                                     const char *detail,
                                     float progress,
                                     uint64_t now_ms);
bool     uipod_rs_hud_progress_update(uint32_t handle,
                                      const char *title,
                                      const char *detail,
                                      float progress,
                                      uint64_t now_ms);
bool     uipod_rs_hud_progress_end(uint32_t handle, uint64_t now_ms);
uint32_t uipod_rs_hud_confirm_push(const char *question,
                                   const char *detail,
                                   const char *confirm_label,
                                   const char *cancel_label,
                                   uint64_t now_ms);
bool     uipod_rs_hud_confirm_resolve(uint32_t id, uint64_t now_ms);
bool     uipod_rs_hud_has_confirm(void);
void     uipod_rs_hud_set_clock(uint64_t now_ms);
bool     uipod_rs_hud_needs_tick(uint64_t now_ms);
bool     uipod_rs_hud_is_visible(uint64_t now_ms);
bool     uipod_rs_hud_render_over_screen(uint32_t win_w,
                                         uint32_t win_h,
                                         uint32_t bg_rgb,
                                         uint32_t fg_rgb,
                                         uint32_t fg1_rgb,
                                         uint32_t fg2_rgb,
                                         uint64_t now_ms);

void   uipod_rs_ui_trigger_volume_popup(float level_norm, uint64_t now_ms);
void   uipod_rs_ui_trigger_hud_toast(const char *message, uint64_t now_ms);
void   uipod_rs_ui_set_volume_popup_clock(uint64_t now_ms);
bool   uipod_rs_ui_is_volume_popup_animating(uint64_t now_ms);
bool   uipod_rs_ui_render_volume_popup_over_screen(uint32_t win_w,
                                                   uint32_t win_h,
                                                   uint32_t bg_rgb,
                                                   uint32_t fg_rgb,
                                                   uint32_t fg1_rgb,
                                                   uint32_t fg2_rgb,
                                                   uint64_t now_ms);
void  uipod_rs_ui_plugin_fill_rect(float x, float y, float w, float h,
                                   float radius, uint32_t rgb);
void  uipod_rs_ui_plugin_stroke_rect(float x, float y, float w, float h,
                                     float radius, float stroke_w, uint32_t rgb);
void  uipod_rs_ui_plugin_fill_circle(float cx, float cy, float r, uint32_t rgb);
void  uipod_rs_ui_plugin_draw_line(float x0, float y0, float x1, float y1,
                                   float stroke_w, uint32_t rgb);
float uipod_rs_ui_plugin_draw_text(float x, float center_y, const char *text,
                                   float font_size, uint8_t weight,
                                   uint8_t align, uint32_t rgb);
float uipod_rs_ui_plugin_measure_text(const char *text, float font_size,
                                      uint8_t weight);
void  uipod_rs_ui_plugin_draw_progress_bar(uint32_t bg_rgb, uint32_t fg_rgb,
                                           uint32_t fg1_rgb, uint32_t fg2_rgb,
                                           float x, float y, float w, float h,
                                           float progress);
void  uipod_rs_ui_plugin_draw_scrollbar(uint32_t bg_rgb, uint32_t fg_rgb,
                                        uint32_t fg1_rgb, uint32_t fg2_rgb,
                                        float x, float y, float w, float h,
                                        size_t total, size_t start,
                                        size_t visible);
void  uipod_rs_ui_plugin_draw_rgba_image(uint64_t tex_key,
                                         uint32_t src_w, uint32_t src_h,
                                         const uint8_t *rgba_ptr,
                                         size_t rgba_len,
                                         float dst_x, float dst_y,
                                         float dst_w, float dst_h,
                                         float radius);
bool  uipod_rs_ui_plugin_present(uint8_t *fb_bgr);
bool  uipod_rs_ui_plugin_present_gpu(uint32_t win_w, uint32_t win_h);
bool  uipod_rs_ui_present_video_overlay_gpu(uint32_t win_w,
                                            uint32_t win_h,
                                            uint32_t bottom_rows_768);
bool  uipod_rs_ui_plugin_flush_fbo(void);
bool  uipod_rs_ui_plugin_sync_fb(uint8_t *fb_bgr);

/* 4-Shade Audio-Reactive Visualizer entry points (`crates/uipod-ui/src/ffi.rs`) */
size_t uipod_rs_viz_preset_count(void);
size_t uipod_rs_viz_preset_id(size_t idx, char *out, size_t cap);
size_t uipod_rs_viz_preset_name(size_t idx, bool is_ukrainian, char *out, size_t cap);
bool   uipod_rs_viz_init(uipod_rs_gl_get_proc_fn get_proc);
void   uipod_rs_viz_set_preset(size_t idx);
bool   uipod_rs_viz_toggle_grain(void);
bool   uipod_rs_viz_render_frame(const int16_t *pcm_interleaved,
                                 size_t frames,
                                 uint32_t sample_rate,
                                 float dt,
                                 int32_t win_w,
                                 int32_t win_h,
                                 uint32_t bg_rgb,
                                 uint32_t fg_rgb,
                                 uint32_t fg1_rgb,
                                 uint32_t fg2_rgb,
                                 float fade,
                                 float track_progress,
                                 float transport_speed);

/* Pure-Rust Album Art Decoder (`crates/uipod-ui/src/image_decode.rs`) */
uint8_t *uipod_rs_image_decode_rgb(const uint8_t *bytes,
                                   size_t len,
                                   int32_t *out_w,
                                   int32_t *out_h,
                                   size_t *out_byte_len);
void     uipod_rs_image_free_rgb(uint8_t *ptr, size_t byte_len);

/* High-level C glue helpers (`apps/uipod_rs_ui_glue.c`) */
struct gui_synclist;

void uipod_gpu_set_header_source(uint8_t src_icon);
uint8_t uipod_gpu_get_header_source(void);

bool uipod_gpu_ui_ensure_ready(void);
void uipod_gpu_fill_header(struct UIPodRsHeader *out,
                           const char *override_title);
int  uipod_gpu_visible_list_rows(void);
bool uipod_gpu_check_window_resized(void);
bool uipod_gpu_is_active(void);
void uipod_gpu_set_active(bool active);
void uipod_gpu_begin_transition_capture(void);
bool uipod_gpu_did_capture_transition_frame(bool *out_pin_header);
void uipod_gpu_present_current_fbo(void);
void uipod_gpu_note_rendered_screen(bool has_header);
void uipod_gpu_prepare_cpu_fallback(void);
bool uipod_gpu_render_header(void);
bool uipod_gpu_render_synclist(struct gui_synclist *list);
bool uipod_gpu_render_keyboard(const char *text,
                               const size_t row_counts[5],
                               const struct UIPodRsKeyboardKey *keys);
bool uipod_gpu_render_graphic_eq(const char *header_title,
                                 int16_t preamp_tenths_db,
                                 const int16_t band_gains_tenths_db[10],
                                 size_t selected_col,
                                 bool adjusting,
                                 bool bypassed);
bool uipod_gpu_render_nsf_channels(const char *header_title,
                                   const struct UIPodRsNsfChannelRow *channels,
                                   size_t channel_count,
                                   const char *enable_all_label,
                                   bool enable_all_selected,
                                   const char *footer_hint,
                                   size_t start_item,
                                   size_t visible_items);
bool uipod_gpu_render_text_block(const char *header_title,
                                 bool show_header,
                                 const char *const *lines,
                                 const uint8_t *line_roles,
                                 size_t line_count,
                                 bool left_align_block);
bool uipod_gpu_render_video_overlay(int panel_kind,
                                    float progress_norm,
                                    const char *left_text,
                                    const char *center_text,
                                    const char *right_text,
                                    const char *sub_line1,
                                    const char *sub_line2);
enum uipod_hud_confirm_res
{
    UIPOD_CONFIRM_NO  = 0,
    UIPOD_CONFIRM_YES = 1,
    UIPOD_CONFIRM_USB = 2,
};

/* Unified Stacked HUD System C API (`apps/uipod_rs_ui_glue.c`) */
void     uipod_hud_volume(void);
void     uipod_hud_toast(const char *message);
void     uipod_hud_toast_ex(const char *key, const char *message, uint32_t hold_ms);
uint32_t uipod_hud_progress_begin(const char *title, const char *detail, float progress);
uint32_t uipod_hud_progress_begin_blocking(const char *title, const char *detail);
void     uipod_hud_progress_update(uint32_t handle,
                                   const char *title,
                                   const char *detail,
                                   float progress);
void     uipod_hud_progress_end(uint32_t handle);
void     uipod_hud_progress_set_transient(const char *title, const char *detail, float progress);
void     uipod_hud_progress_clear_transient(void);
bool     uipod_hud_confirm(const char *question, const char *detail);
int      uipod_hud_confirm_ex(const char *question,
                              const char *detail,
                              int timeout_ticks,
                              int default_res);
bool     uipod_hud_has_confirm(void);
bool     uipod_hud_is_animating(void);
bool     uipod_hud_tick(void);

void uipod_gpu_notify_volume_changed(void);
void uipod_gpu_notify_hud_toast(const char *message);
bool uipod_gpu_is_volume_popup_active(void);
bool uipod_gpu_tick_volume_popup(void);

#ifdef __cplusplus
}
#endif

#endif /* UIPOD_RS_UI_H */
