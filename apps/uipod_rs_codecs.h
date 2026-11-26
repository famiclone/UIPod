/* UIPod: C FFI header for pure-Rust audio codecs (`crates/uipod-codecs` & `crates/uipod-nsf`). */
#ifndef UIPOD_RS_CODECS_H
#define UIPOD_RS_CODECS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

struct uipod_rs_track_meta
{
    char     title[256];
    char     artist[256];
    char     album[256];
    char     genre[128];
    char     composer[256];
    char     comment[256];
    int32_t  year;
    int32_t  track_num;
    uint32_t duration_ms;
    uint32_t sample_rate;
    uint32_t bitrate_kbps;
};

struct uipod_rs_nsf_subtrack_info
{
    uint16_t subtrack_index;
    uint8_t  track_kind;    /* 0 = TrackKind::Music, 1 = TrackKind::Sfx */
    uint8_t  chip_flags;    /* ChipFlags bitmask */
    uint32_t duration_ms;   /* play_ms + fade_ms */
    uint32_t fade_ms;
    int32_t  loop_point_ms; /* -1 if None, >= 0 if Some */
    char     title[256];
};

struct uipod_rs_nsf_container_header
{
    char     game_title[256];
    char     artist[256];
    char     copyright[256];
    uint16_t total_subtracks;
    uint16_t starting_subtrack;
    uint8_t  chip_flags;
    uint8_t  is_pal;
    uint32_t payload_crc32;
};

struct uipod_rs_nsf_channel_desc
{
    uint8_t  bit_index;
    char     short_code[8];
    char     display_name[32];
};

struct uipod_rs_channel_state
{
    uint8_t  active;
    uint8_t  muted;
    uint8_t  onset_trigger;
    uint8_t  duty_or_mode;
    float    volume;
    float    freq_hz;
};

struct uipod_rs_apu_telemetry
{
    uint8_t                       channel_count;
    uint8_t                       chip_flags;
    uint16_t                      mute_mask;
    struct uipod_rs_channel_state channels[8];
};

int      uipod_rs_codec_is_builtin(const char *codec_name);
int      uipod_rs_codec_open(const char *codec_name,
                             const char *track_path,
                             const uint8_t *data,
                             size_t data_len);
uint32_t uipod_rs_codec_sample_rate(void);
uint32_t uipod_rs_codec_duration_ms(void);
uint32_t uipod_rs_codec_elapsed_ms(void);
int      uipod_rs_codec_seek_ms(uint32_t target_ms);
int      uipod_rs_codec_seek_step_ms(uint32_t target_ms, uint32_t max_step_ms);
int      uipod_rs_codec_render_s16(int16_t *out_stereo_s16,
                                   size_t max_frames,
                                   int loop_forever);
void     uipod_rs_codec_close(void);

int      uipod_rs_gsf_parse_metadata(const uint8_t *header_16,
                                     const uint8_t *tag_bytes,
                                     size_t tag_len,
                                     const char *track_path,
                                     struct uipod_rs_track_meta *out_meta);

int      uipod_rs_midi_parse_metadata(const uint8_t *data,
                                      size_t data_len,
                                      const char *track_path,
                                      struct uipod_rs_track_meta *out_meta);

int      uipod_rs_midi_set_soundfont(const char *sf2_path);
int      uipod_rs_midi_get_soundfont(char *out_buf, size_t buf_len);

/* ---- Native Rust NSF/NSFe Runtime Controls, Scanner & Telemetry ---- */

uint16_t uipod_rs_nsf_get_channel_mute_mask(void);
void     uipod_rs_nsf_set_channel_mute_mask(uint16_t mask);
int      uipod_rs_nsf_get_active_chip_flags(void); /* -1 when inactive, 0..63 when active */
int      uipod_rs_nsf_get_endless_loop(void);
void     uipod_rs_nsf_set_endless_loop(int enabled);
void     uipod_rs_nsf_reset_session(void);

int      uipod_rs_nsf_get_telemetry(struct uipod_rs_apu_telemetry *out_telemetry);
size_t   uipod_rs_nsf_get_channel_descriptors(uint8_t chip_flags,
                                              struct uipod_rs_nsf_channel_desc *out_descs,
                                              size_t max_descs);

int      uipod_rs_nsf_parse_metadata(const uint8_t *data,
                                     size_t data_len,
                                     const char *track_path,
                                     struct uipod_rs_track_meta *out_meta);

size_t   uipod_rs_nsf_inspect_header_fast(const char *file_path,
                                          const uint8_t *data,
                                          size_t data_len,
                                          struct uipod_rs_nsf_container_header *out_header,
                                          struct uipod_rs_nsf_subtrack_info *out_subtracks,
                                          size_t max_subtracks);

size_t   uipod_rs_nsf_scan_container(const char *file_path,
                                     const uint8_t *data,
                                     size_t data_len,
                                     struct uipod_rs_nsf_container_header *out_header,
                                     struct uipod_rs_nsf_subtrack_info *out_subtracks,
                                     size_t max_subtracks);

int      uipod_rs_nsf_parse_uri(const char *uri,
                                char *out_file_path,
                                size_t out_cap);

/* ---- Dual-Deck Smart DJ Crossfade Engine (`uipod-dj`) ---- */

struct uipod_rs_dj_telemetry
{
    uint8_t active;
    uint8_t mix_kind;        /* 0=None, 1=Classic, 2=Beatmatch, 3=FilterWash */
    uint8_t bass_swap_flash; /* 1 across p=0.47..0.53 center Bass Swap */
    float   progress;        /* 0.0 .. 1.0 */
    float   bpm_a;
    float   bpm_b;
    float   current_bpm;
};

struct uipod_rs_dj_playhead
{
    uint8_t                      is_deck_b_foreground;
    uint8_t                      _pad[3];
    uint32_t                     audible_elapsed_ms;
    uint32_t                     audible_remaining_ms;
    uint32_t                     deck_a_elapsed_ms;
    uint32_t                     deck_a_remaining_ms;
    uint32_t                     deck_b_elapsed_ms;
    struct uipod_rs_dj_telemetry telemetry;
};

void   uipod_rs_dj_set_config(uint8_t mode,
                              uint8_t length,
                              uint8_t max_stretch,
                              int keep_album_gapless,
                              int quick_cut_on_next);
int    uipod_rs_dj_get_phase(void);
void   uipod_rs_dj_reset(void);
void   uipod_rs_dj_flush_to_gapless_drain(void);
void   uipod_rs_dj_set_track_timing(uint32_t deck_a_start_ms,
                                    uint32_t deck_a_duration_ms,
                                    uint32_t deck_b_duration_ms);
int    uipod_rs_dj_should_gapless_album(int shuffle_active,
                                        const char *album_a,
                                        int track_a,
                                        const char *album_b,
                                        int track_b);
void   uipod_rs_dj_begin_deck_a(uint32_t sample_rate,
                                const char *album_a,
                                int track_a,
                                int shuffle_active,
                                int manual_quick_cut);
void   uipod_rs_dj_push_deck_a_frames(const int16_t *stereo_s16,
                                      size_t frames);
size_t uipod_rs_dj_pop_spill_s16(int16_t *out_stereo_s16,
                                 size_t max_frames,
                                 uint32_t *out_elapsed_ms);
size_t uipod_rs_dj_pop_safe_premix_a_s16(int16_t *out_stereo_s16,
                                         size_t max_frames,
                                         uint32_t *out_elapsed_ms);
size_t uipod_rs_dj_push_deck_a(const int16_t *stereo_s16,
                               size_t frames,
                               int16_t *spill_out,
                               size_t spill_cap_frames);
void   uipod_rs_dj_finish_deck_a_begin_deck_b(const char *album_b,
                                              int track_b,
                                              int deck_b_is_nsf);
int    uipod_rs_dj_push_deck_b(const int16_t *stereo_s16,
                               size_t frames,
                               int is_eof);
size_t uipod_rs_dj_render_s16(int16_t *out_stereo_s16,
                              size_t max_frames,
                              int *out_midpoint_crossed);
size_t uipod_rs_dj_render_s16_timed(int16_t *out_stereo_s16,
                                    size_t max_frames,
                                    int *out_midpoint_crossed,
                                    uint32_t *out_elapsed_ms);
int    uipod_rs_dj_get_telemetry(struct uipod_rs_dj_telemetry *out_tel);
int    uipod_rs_dj_get_telemetry_synced(struct uipod_rs_dj_telemetry *out_tel,
                                        size_t downstream_frames);
int    uipod_rs_dj_get_playhead_synced(struct uipod_rs_dj_playhead *out_ph,
                                       size_t downstream_frames);

#ifdef __cplusplus
}
#endif

#endif /* UIPOD_RS_CODECS_H */
