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

#define UIPOD_PLUGIN_API_VERSION 11u

enum uipod_dj_deck_id
{
    UIPOD_DJ_DECK_A = 0,
    UIPOD_DJ_DECK_B = 1,
};

enum uipod_dj_cue_action
{
    UIPOD_DJ_CUE_RETURN   = 0,
    UIPOD_DJ_CUE_STUTTER  = 1,
    UIPOD_DJ_CUE_SET_HERE = 2,
};

enum uipod_dj_tempo_mode
{
    UIPOD_DJ_TEMPO_MASTER = 0,
    UIPOD_DJ_TEMPO_VINYL  = 1,
};

enum uipod_dj_loop_beats
{
    UIPOD_DJ_LOOP_OFF  = 0,
    UIPOD_DJ_LOOP_4    = 1,
    UIPOD_DJ_LOOP_2    = 2,
    UIPOD_DJ_LOOP_1    = 3,
    UIPOD_DJ_LOOP_HALF = 4,
};

enum uipod_dj_track_source
{
    UIPOD_DJ_SRC_QUEUE      = 0,
    UIPOD_DJ_SRC_FAVOURITES = 1,
    UIPOD_DJ_SRC_LIBRARY    = 2,
};

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

struct UIPodPluginChannelState
{
    uint8_t active;
    uint8_t muted;
    uint8_t onset_trigger;
    uint8_t duty_or_mode;
    float   volume;
    float   freq_hz;
};

struct UIPodPluginApuTelemetry
{
    uint8_t                        has_apu_telemetry;
    uint8_t                        channel_count;
    uint8_t                        chip_flags;
    uint8_t                        reserved;
    uint16_t                       mute_mask;
    uint16_t                       pad;
    struct UIPodPluginChannelState channels[8];
};

struct UIPodPluginTrackAnalysis
{
    uint64_t key_hash;
    uint32_t record_crc32;
    uint16_t subtrack_index;
    uint8_t  channels;
    uint8_t  analysis_flags;
    uint32_t duration_ms;
    uint32_t sample_rate;
    float    bpm;
    float    bpm_confidence;
    uint32_t first_beat_offset_ms;
    float    tag_tbpm;
    float    tonal_balance[5];
    float    avg_rms;
    float    crest_factor;
    float    kick_baseline;
    float    snare_baseline;
    uint8_t  title[64];
    uint8_t  artist[56];
    uint8_t  album[40];
    uint8_t  genre[20];
    uint8_t  energy_contour[256];
};

struct UIPodPluginLiveTelemetry
{
    uint64_t                       track_key_hash;
    uint64_t                       beat_index;
    uint32_t                       seq_generation;
    uint8_t                        status;
    uint8_t                        has_analysis;
    uint8_t                        bar_beat;
    uint8_t                        beat_flags;
    uint32_t                       playhead_ms;
    uint32_t                       duration_ms;
    uint32_t                       sample_rate;
    float                          output_latency_ms;
    float                          bpm;
    float                          bpm_confidence;
    float                          beat_phase;
    float                          bar_phase;
    float                          kick_impulse;
    float                          snare_impulse;
    float                          structural_energy;
    float                          vu_l;
    float                          vu_r;
    float                          vu_peak_l;
    float                          vu_peak_r;
    float                          bands_5[5];
    uint8_t                        spectrum_64[64];
    uint8_t                        spectrum_peak_64[64];
    int8_t                         waveform_256[256];
    uint8_t                        beat_substeps_16[16];
    struct UIPodPluginApuTelemetry apu;
};

struct UIPodDjDeckTelemetry
{
    uint8_t  loaded;
    uint8_t  playing;
    uint8_t  loop_beats;
    uint8_t  sync_locked;
    uint32_t elapsed_ms;
    uint32_t remaining_ms;
    uint32_t duration_ms;
    uint32_t cue_ms;
    float    native_bpm;
    float    effective_bpm;
    float    beat_phase;
    float    bar_phase;
    float    filter_val;
    uint8_t  bars_peak_24[24];
    uint8_t  bars_kick_24[24];
    uint8_t  title[96];
    uint8_t  artist[80];
};

struct UIPodDjTelemetry
{
    uint32_t                    seq_generation;
    uint8_t                     session_active;
    uint8_t                     bass_swap_enabled;
    uint8_t                     tempo_mode;
    uint8_t                     reserved;
    float                       crossfader;
    int16_t                     analog_lx;
    int16_t                     analog_ly;
    int16_t                     analog_rx;
    int16_t                     analog_ry;
    struct UIPodDjDeckTelemetry decks[2];
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

    /* Responsive Canvas Extension (v6+) */
    void     (*canvas_begin_responsive)(uint32_t bg_rgb,
                                        bool show_header,
                                        const char *title,
                                        float *out_w,
                                        float *out_h);
    void     (*get_responsive_size)(float *out_w, float *out_h);

    /* Hardware APU Channel Telemetry Extension (v7+) */
    bool     (*audio_get_apu_telemetry)(struct UIPodPluginApuTelemetry *out);

    /* Unified Track Intelligence Extension (v8+) */
    bool     (*audio_get_track_analysis)(struct UIPodPluginTrackAnalysis *out);
    bool     (*audio_get_live_telemetry)(struct UIPodPluginLiveTelemetry *out);

    /* GPU Nearest-Neighbor RGBA Image Blitter Extension (v9+) */
    void     (*canvas_draw_rgba_image)(uint64_t tex_key,
                                       uint32_t src_w, uint32_t src_h,
                                       const uint8_t *rgba_ptr,
                                       size_t rgba_len,
                                       float dst_x, float dst_y,
                                       float dst_w, float dst_h,
                                       float radius);

    /* Manual Dual-Deck DJ Extension (v11+) */
    bool     (*dj_session_acquire)(void);
    void     (*dj_session_release)(void);
    bool     (*dj_deck_load)(uint8_t deck, const char *path, bool start_paused);
    void     (*dj_deck_set_playing)(uint8_t deck, bool playing, bool quantize);
    void     (*dj_deck_cue)(uint8_t deck, uint8_t cue_action);
    void     (*dj_deck_seek_beats)(uint8_t deck, int32_t delta_beats);
    void     (*dj_set_crossfader)(float position, bool bass_swap_enabled);
    void     (*dj_deck_set_tempo)(uint8_t deck, float target_bpm_ratio,
                                  uint8_t tempo_mode, float momentary_bend);
    void     (*dj_deck_set_sync)(uint8_t deck, bool enabled);
    void     (*dj_deck_set_loop)(uint8_t deck, uint8_t loop_beats);
    void     (*dj_deck_set_filter)(uint8_t deck, float filter_value);
    bool     (*dj_get_telemetry)(struct UIPodDjTelemetry *out);
    size_t   (*dj_source_track_count)(uint8_t source_kind);
    bool     (*dj_source_track_at)(uint8_t source_kind, size_t index,
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
