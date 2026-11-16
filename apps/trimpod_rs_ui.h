/***************************************************************************
 * Trimpod: Thin C ABI header & glue helpers for the pure-Rust GPU UI engine
 * (`crates/trimpod-ui`, `taffy` + `femtovg` on OpenGL ES 3.0).
 ****************************************************************************/
#ifndef TRIMPOD_RS_UI_H
#define TRIMPOD_RS_UI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define TRIMPOD_RS_LIST_VISIBLE_ROWS 8
#define TRIMPOD_RS_HEADER_BAND_ROWS  120

typedef const void *(*trimpod_rs_gl_get_proc_fn)(const char *name);

struct TrimpodRsHeader
{
    const char *title;
    const char *legend;
    uint32_t    play_mode;       /* 0=Stop, 1=Play, 2=Pause, 3=FF, 4=Rew */
    uint8_t     battery_percent; /* 0..100 */
    bool        is_charging;
    bool        is_hold;
};

struct TrimpodRsListRow
{
    const char *label;
    const char *right_value;
    const char *file_tag;
    bool        selected;
    uint8_t     right_kind; /* 0=None, 1=Chevron, 2=ToggleOff, 3=ToggleOn, 4=Value */
};

struct TrimpodRsListScreen
{
    uint32_t                      bg_rgb;
    uint32_t                      fg_rgb;
    struct TrimpodRsHeader        header;
    const struct TrimpodRsListRow *rows;
    size_t                        row_count;
    size_t                        total_items;
    size_t                        start_item;
    size_t                        visible_items;
};

struct TrimpodRsNowPlaying
{
    uint32_t               bg_rgb;
    uint32_t               fg_rgb;
    struct TrimpodRsHeader header;
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

/* Raw Rust staticlib (`libtrimpod_ui.a`) entry points */
bool trimpod_rs_ui_init(trimpod_rs_gl_get_proc_fn get_proc,
                        const char *cjk_font_path);
bool trimpod_rs_ui_is_ready(void);
void trimpod_rs_ui_shutdown(void);

bool trimpod_rs_ui_render_header(uint32_t bg_rgb,
                                 uint32_t fg_rgb,
                                 const struct TrimpodRsHeader *header,
                                 uint8_t *fb_bgr);
bool trimpod_rs_ui_render_list(const struct TrimpodRsListScreen *screen,
                               uint8_t *fb_bgr);
bool trimpod_rs_ui_render_now_playing(const struct TrimpodRsNowPlaying *np,
                                      uint8_t *fb_bgr);

/* High-level C glue helpers (`apps/trimpod_rs_ui_glue.c`) */
struct gui_synclist;

bool trimpod_gpu_ui_ensure_ready(void);
void trimpod_gpu_fill_header(struct TrimpodRsHeader *out,
                             const char *override_title);
bool trimpod_gpu_render_header(void);
bool trimpod_gpu_render_synclist(struct gui_synclist *list);

#ifdef __cplusplus
}
#endif

#endif /* TRIMPOD_RS_UI_H */
