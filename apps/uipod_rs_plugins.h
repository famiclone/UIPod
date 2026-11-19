/***************************************************************************
 * UIPod: C ABI declarations matching `crates/uipod-plugin-sdk` and
 * `crates/uipod-plugins` (`libuipod_ffi.a`).
 ****************************************************************************/
#ifndef UIPOD_RS_PLUGINS_H
#define UIPOD_RS_PLUGINS_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define UIPOD_PLUGIN_API_VERSION 5u

enum uipod_plugin_category
{
    UIPOD_PLUGIN_CAT_GAME   = 0,
    UIPOD_PLUGIN_CAT_APP    = 1,
    UIPOD_PLUGIN_CAT_DEMO   = 2,
    UIPOD_PLUGIN_CAT_VIEWER = 3,
    UIPOD_PLUGIN_CAT_COUNT  = 4,
};

enum uipod_plugin_kind
{
    UIPOD_PLUGIN_KIND_UTILITY     = 0,
    UIPOD_PLUGIN_KIND_MENU_DRIVEN = 1,
};

struct UIPodPluginTheme
{
    uint32_t bg;
    uint32_t fg;
    uint32_t fg1;
    uint32_t fg2;
};

struct UIPodPluginInput
{
    uint32_t pressed;
    uint32_t held;
    bool     should_exit;
    bool     menu_requested;
};

struct UIPodPluginMenuItem
{
    const char *label;
    const char *value;
    bool        has_chevron;
};

struct UIPodPluginApi
{
    uint32_t api_version;

    /* Timing, Localization & Persistent Storage */
    uint64_t (*get_ticks_ms)(void);
    void     (*sleep_ms)(uint32_t ms);
    uint32_t (*get_language)(void);
    size_t   (*get_data_dir)(char *out, size_t cap);

    /* 4-Color Theme & GPU Vector 2D Canvas (1024x768) */
    struct UIPodPluginTheme (*get_theme)(void);
    void  (*canvas_begin)(uint32_t bg_rgb, bool show_header, const char *title);
    void  (*canvas_fill_rect)(float x, float y, float w, float h,
                              float radius, uint32_t rgb);
    void  (*canvas_stroke_rect)(float x, float y, float w, float h,
                                float radius, float stroke_w, uint32_t rgb);
    void  (*canvas_fill_circle)(float cx, float cy, float r, uint32_t rgb);
    void  (*canvas_draw_line)(float x0, float y0, float x1, float y1,
                              float stroke_w, uint32_t rgb);
    float (*canvas_draw_text)(float x, float center_y, const char *text,
                              float font_size, uint8_t weight,
                              uint8_t align, uint32_t rgb);
    float (*canvas_measure_text)(const char *text, float font_size,
                                 uint8_t weight);
    void  (*canvas_draw_progress_bar)(float x, float y, float w, float h,
                                      float progress);
    void  (*canvas_draw_scrollbar)(float x, float y, float w, float h,
                                   size_t total, size_t start, size_t visible);
    void  (*canvas_present)(void);

    /* Direct Raw 24-bit {B, G, R} Framebuffer (1024 * 768 * 3) */
    uint8_t *(*fb_ptr)(void);
    void     (*fb_present)(void);

    /* Input, Native J2ME-Style System Menu, On-Screen QWERTY Keyboard & Dialogs */
    struct UIPodPluginInput (*poll_input)(uint32_t timeout_ms);
    int32_t (*show_menu)(const char *title,
                         const struct UIPodPluginMenuItem *items,
                         size_t count,
                         size_t initial_sel);
    bool (*keyboard_input)(char *buf, size_t cap, bool allow_symbols);
    void (*show_splash)(const char *message, uint32_t duration_ms);

    /* Background Audio Playback & Metadata */
    uint32_t (*audio_status)(void);
    void     (*audio_toggle_play_pause)(void);
    void     (*audio_next)(void);
    void     (*audio_prev)(void);
    void     (*audio_seek_ms)(uint32_t pos_ms);
    bool     (*audio_current_track)(char *path_out, size_t path_cap,
                                    char *title_out, size_t title_cap,
                                    char *artist_out, size_t artist_cap,
                                    uint32_t *pos_ms_out, uint32_t *len_ms_out);
    size_t   (*audio_read_pcm)(uint32_t ahead_ms,
                               int16_t *out_stereo,
                               size_t max_frames);
    bool     (*audio_play_track)(const char *path, uint32_t start_ms);
    void     (*audio_stop)(void);

    /* Unified Music Library Access (`TP_CAT_MUSIC`) */
    size_t   (*library_track_count)(void);
    bool     (*library_track_at)(size_t index,
                                 char *path_out, size_t path_cap,
                                 char *artist_out, size_t artist_cap,
                                 char *title_out, size_t title_cap,
                                 uint32_t *duration_ms_out);
};

struct UIPodPluginInfoC
{
    char     id[64];
    char     title[128];
    char     author[64];
    char     version[32];
    uint32_t category;
    uint32_t kind;
    bool     is_sdcard;
};

size_t uipod_rs_plugins_init(const char *bundled_dir, const char *sdcard_dir);
size_t uipod_rs_plugins_rescan(void);
size_t uipod_rs_plugins_count_category(uint32_t category);
size_t uipod_rs_plugins_list_category(uint32_t category,
                                      bool ukrainian,
                                      struct UIPodPluginInfoC *out,
                                      size_t cap);
bool uipod_rs_plugins_is_menu_driven(const char *plugin_id,
                                     const char *open_path);
int32_t uipod_rs_plugins_run_by_id(const char *plugin_id,
                                   const struct UIPodPluginApi *api,
                                   const char *file_path,
                                   char *err_out,
                                   size_t err_cap);
bool uipod_rs_plugins_is_plugin_file(const char *path);
bool uipod_rs_plugins_has_viewer(const char *path);
int32_t uipod_rs_plugins_open_viewer(const char *path,
                                     const struct UIPodPluginApi *api,
                                     char *err_out,
                                     size_t err_cap);

#ifdef __cplusplus
}
#endif

#endif /* UIPOD_RS_PLUGINS_H */
