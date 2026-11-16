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
#define UIPOD_RS_HEADER_BAND_ROWS  120

typedef const void *(*uipod_rs_gl_get_proc_fn)(const char *name);

struct UIPodRsHeader
{
    const char *title;
    const char *legend;
    uint32_t    play_mode;       /* 0=Stop, 1=Play, 2=Pause, 3=FF, 4=Rew */
    uint8_t     battery_percent; /* 0..100 */
    bool        is_charging;
    bool        is_hold;
};

struct UIPodRsListRow
{
    const char *label;
    const char *right_value;
    const char *file_tag;
    bool        selected;
    uint8_t     right_kind; /* 0=None, 1=Chevron, 2=ToggleOff, 3=ToggleOn, 4=Value */
};

struct UIPodRsListScreen
{
    uint32_t                      bg_rgb;
    uint32_t                      fg_rgb;
    struct UIPodRsHeader        header;
    const struct UIPodRsListRow *rows;
    size_t                        row_count;
    size_t                        total_items;
    size_t                        start_item;
    size_t                        visible_items;
};

struct UIPodRsNowPlaying
{
    uint32_t               bg_rgb;
    uint32_t               fg_rgb;
    struct UIPodRsHeader header;
    const char            *track_pos_label;
    bool                   eq_enabled;
    const char            *title;
    const char            *artist;
    const char            *album;
    const char            *codec_label;
    const char            *bitdepth_label;
    const char            *samplerate_label;
    float                  progress;
    const char            *elapsed_label;
    const char            *remaining_label;
    const char            *center_status_label;
    const uint8_t         *art_rgb24;
    bool                   mono_album_art;
};

/* Raw Rust staticlib (`libuipod_ui.a`) entry points */
bool uipod_rs_ui_init(uipod_rs_gl_get_proc_fn get_proc,
                        const char *cjk_font_path);
bool uipod_rs_ui_is_ready(void);
void uipod_rs_ui_shutdown(void);

bool uipod_rs_ui_render_header(uint32_t bg_rgb,
                                 uint32_t fg_rgb,
                                 const struct UIPodRsHeader *header,
                                 uint8_t *fb_bgr);
bool uipod_rs_ui_render_list(const struct UIPodRsListScreen *screen,
                               uint8_t *fb_bgr);
bool uipod_rs_ui_render_now_playing(const struct UIPodRsNowPlaying *np,
                                      uint8_t *fb_bgr);

/* High-level C glue helpers (`apps/uipod_rs_ui_glue.c`) */
struct gui_synclist;

bool uipod_gpu_ui_ensure_ready(void);
void uipod_gpu_fill_header(struct UIPodRsHeader *out,
                             const char *override_title);
bool uipod_gpu_render_header(void);
bool uipod_gpu_render_synclist(struct gui_synclist *list);

#ifdef __cplusplus
}
#endif

#endif /* UIPOD_RS_UI_H */
