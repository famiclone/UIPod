/***************************************************************************
 * UIPod: custom 1024x768 HiDPI Now Playing (WPS) renderer & cover-art
 * inspector (see uipod_nowplaying.h).
 *
 * Implements:
 *   - Embedded cover art extraction for FLAC (METADATA_BLOCK_PICTURE),
 *     MP3/WAV/AIFF/DSF ID3v2.2/2.3/2.4 (PIC/APIC, incl. synchsafe & unsync),
 *     MP4/M4A/M4B/ALAC (covr/data atom), Ogg Vorbis & Opus
 *     (METADATA_BLOCK_PICTURE base64), and APEv2 (Cover Art (Front)),
 *     plus case-insensitive folder fallback (cover/folder/front/album/<stem>).
 *   - Lossless/PCM bit-depth inspection (FLAC STREAMINFO, WAV fmt, AIFF COMM,
 *     ALAC magic cookie, WavPack, APE, DSD).
 *   - Image decoding via SOIL2/stb_image (linked in libprojectM-4.a) with
 *     high-quality subpixel box-averaging center-crop to 256x256 RGB888,
 *     rendered monochrome (grayscale) by default or full-color when
 *     global_settings.mono_album_art is false.
 *   - 8x8 anti-aliased SDF vector shapes (Play/Pause/Seek icon, hollow-nub
 *     battery with split-XOR numeric percentage, [EQ] & [CODEC] badges, and
 *     rounded stadium seekbar / volume overlay).
 ****************************************************************************/
#include "config.h"

#include <ctype.h>
#include <inttypes.h>
#include <math.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>

#include "audio.h"
#include "button.h"
#include "cuesheet.h"
#include "dir.h"
#include "file.h"
#include "font.h"
#include "gui/viewport.h"
#include "gui/wps.h"
#include "kernel.h"
#include "lang.h"
#include "lcd.h"
#include "metadata.h"
#include "misc.h"
#include "playlist.h"
#include "powermgmt.h"
#include "rbpaths.h"
#include "rbunicode.h"
#include "screens.h"
#include "settings.h"
#include "sound.h"
#include "status.h"
#include "string-extra.h"
#include "system.h"
#include "uipod_nowplaying.h"
#include "uipod_navidrome.h"
#include "uipod_jellyfin.h"
#include "uipod_rs_ui.h"

/* Provided by SOIL2 / stb_image inside libprojectM-4.a */
extern unsigned char *stbi_load_from_memory(const unsigned char *buffer,
                                            int len, int *x, int *y,
                                            int *comp, int req_comp);
extern void stbi_image_free(void *retval_from_stbi_load);
extern char *realpath(const char *path, char *resolved);

/* ---- Layout Constants (1024x768 HiDPI) --------------------------------- */

#define NP_X_LEFT          33
#define NP_X_RIGHT         990
#define NP_CONTENT_W       (NP_X_RIGHT - NP_X_LEFT + 1) /* 958 */

#define NP_ART_X           33
#define NP_ART_Y           227
#define NP_ART_SIZE        256

#define NP_META_X          321
#define NP_META_MAX_W      (NP_X_RIGHT - NP_META_X + 1) /* 670 */

#define NP_MAX_ART_BYTES   (12 * 1024 * 1024)

#define NP_FONT_TITLE      FONT_DIR "/67-Geologica.fnt"
#define NP_FONT_BODY       FONT_DIR "/51-Geologica-Medium.fnt"
#define NP_FONT_SMALL_MED  FONT_DIR "/33-Geologica-Medium.fnt"
#define NP_FONT_SMALL_BOLD FONT_DIR "/33-Geologica-Bold.fnt"

/* ---- Cached Per-Track State -------------------------------------------- */

static char    s_cached_path[MAX_PATH];
static bool    s_has_art;
static int     s_bit_depth;      /* >0 when lossless/PCM bit depth known */
static bool    s_is_alac;
static uint8_t s_art_rgb[NP_ART_SIZE * NP_ART_SIZE * 3];

static int s_fid_title      = -1;
static int s_fid_body       = -1;
static int s_fid_small_med  = -1;
static int s_fid_small_bold = -1;

/* ---- Endian Helpers ---------------------------------------------------- */

static inline uint16_t rd_be16(const uint8_t *p)
{
    return (uint16_t)(((unsigned)p[0] << 8) | p[1]);
}

static inline uint16_t rd_le16(const uint8_t *p)
{
    return (uint16_t)(((unsigned)p[1] << 8) | p[0]);
}

static inline uint32_t rd_be24(const uint8_t *p)
{
    return ((uint32_t)p[0] << 16) | ((uint32_t)p[1] << 8) | p[2];
}

static inline uint32_t rd_be32(const uint8_t *p)
{
    return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) |
           ((uint32_t)p[2] << 8)  | (uint32_t)p[3];
}

static inline uint32_t rd_le32(const uint8_t *p)
{
    return ((uint32_t)p[3] << 24) | ((uint32_t)p[2] << 16) |
           ((uint32_t)p[1] << 8)  | (uint32_t)p[0];
}

static inline uint32_t rd_synchsafe32(const uint8_t *p)
{
    return ((uint32_t)(p[0] & 0x7f) << 21) |
           ((uint32_t)(p[1] & 0x7f) << 14) |
           ((uint32_t)(p[2] & 0x7f) << 7)  |
           (uint32_t)(p[3] & 0x7f);
}

static bool read_exact(int fd, void *buf, size_t n)
{
    uint8_t *p = (uint8_t *)buf;
    size_t got = 0;
    while (got < n)
    {
        ssize_t r = read(fd, p + got, n - got);
        if (r <= 0)
            return false;
        got += (size_t)r;
    }
    return true;
}

/* ---- Image Decode & High-Quality 256x256 Resample ---------------------- */

static bool decode_and_cache_art(const uint8_t *img_bytes, size_t img_len)
{
    if (!img_bytes || img_len < 16 || img_len > NP_MAX_ART_BYTES)
        return false;

    int w = 0, h = 0, comp = 0;
    unsigned char *src = stbi_load_from_memory(img_bytes, (int)img_len,
                                               &w, &h, &comp, 3);
    if (!src || w <= 0 || h <= 0)
    {
        if (src)
            stbi_image_free(src);
        return false;
    }

    /* Center-crop to square */
    int side = (w < h) ? w : h;
    int crop_x0 = (w - side) / 2;
    int crop_y0 = (h - side) / 2;

    if (side >= NP_ART_SIZE)
    {
        /* Exact subpixel area-averaging box filter (prevents moire/aliasing
         * on fine cover typography and vinyl grooves). */
        const float scale = (float)side / (float)NP_ART_SIZE;
        for (int dy = 0; dy < NP_ART_SIZE; dy++)
        {
            float sy0 = (float)crop_y0 + (float)dy * scale;
            float sy1 = sy0 + scale;
            int iy0 = (int)sy0;
            int iy1 = (int)sy1;
            if (iy1 >= crop_y0 + side)
                iy1 = crop_y0 + side - 1;

            for (int dx = 0; dx < NP_ART_SIZE; dx++)
            {
                float sx0 = (float)crop_x0 + (float)dx * scale;
                float sx1 = sx0 + scale;
                int ix0 = (int)sx0;
                int ix1 = (int)sx1;
                if (ix1 >= crop_x0 + side)
                    ix1 = crop_x0 + side - 1;

                float r_acc = 0.0f, g_acc = 0.0f, b_acc = 0.0f, w_acc = 0.0f;
                for (int sy = iy0; sy <= iy1; sy++)
                {
                    float wy = 1.0f;
                    if (sy == iy0)
                        wy = 1.0f - (sy0 - (float)iy0);
                    if (sy == iy1 && iy1 > iy0)
                        wy = sy1 - (float)iy1;
                    else if (iy0 == iy1)
                        wy = scale;
                    if (wy <= 0.0f)
                        continue;

                    const unsigned char *row = src + (size_t)sy * (size_t)w * 3;
                    for (int sx = ix0; sx <= ix1; sx++)
                    {
                        float wx = 1.0f;
                        if (sx == ix0)
                            wx = 1.0f - (sx0 - (float)ix0);
                        if (sx == ix1 && ix1 > ix0)
                            wx = sx1 - (float)ix1;
                        else if (ix0 == ix1)
                            wx = scale;
                        if (wx <= 0.0f)
                            continue;

                        float wt = wx * wy;
                        const unsigned char *px = row + sx * 3;
                        r_acc += (float)px[0] * wt;
                        g_acc += (float)px[1] * wt;
                        b_acc += (float)px[2] * wt;
                        w_acc += wt;
                    }
                }

                uint8_t *dst = s_art_rgb + (dy * NP_ART_SIZE + dx) * 3;
                if (w_acc > 0.0f)
                {
                    int r = (int)(r_acc / w_acc + 0.5f);
                    int g = (int)(g_acc / w_acc + 0.5f);
                    int b = (int)(b_acc / w_acc + 0.5f);
                    dst[0] = (uint8_t)(r < 0 ? 0 : (r > 255 ? 255 : r));
                    dst[1] = (uint8_t)(g < 0 ? 0 : (g > 255 ? 255 : g));
                    dst[2] = (uint8_t)(b < 0 ? 0 : (b > 255 ? 255 : b));
                }
                else
                {
                    dst[0] = dst[1] = dst[2] = 0;
                }
            }
        }
    }
    else
    {
        /* Smooth bilinear upscale for small (< 256x256) covers */
        const float scale = (float)(side > 1 ? side - 1 : 0) /
                            (float)(NP_ART_SIZE - 1);
        for (int dy = 0; dy < NP_ART_SIZE; dy++)
        {
            float fy = (float)crop_y0 + (float)dy * scale;
            int y0 = (int)fy;
            int y1 = (y0 + 1 < crop_y0 + side) ? (y0 + 1) : y0;
            float wy = fy - (float)y0;

            for (int dx = 0; dx < NP_ART_SIZE; dx++)
            {
                float fx = (float)crop_x0 + (float)dx * scale;
                int x0 = (int)fx;
                int x1 = (x0 + 1 < crop_x0 + side) ? (x0 + 1) : x0;
                float wx = fx - (float)x0;

                const unsigned char *p00 = src + ((size_t)y0 * w + x0) * 3;
                const unsigned char *p10 = src + ((size_t)y0 * w + x1) * 3;
                const unsigned char *p01 = src + ((size_t)y1 * w + x0) * 3;
                const unsigned char *p11 = src + ((size_t)y1 * w + x1) * 3;

                uint8_t *dst = s_art_rgb + (dy * NP_ART_SIZE + dx) * 3;
                for (int c = 0; c < 3; c++)
                {
                    float top = p00[c] * (1.0f - wx) + p10[c] * wx;
                    float bot = p01[c] * (1.0f - wx) + p11[c] * wx;
                    int v = (int)(top * (1.0f - wy) + bot * wy + 0.5f);
                    dst[c] = (uint8_t)(v < 0 ? 0 : (v > 255 ? 255 : v));
                }
            }
        }
    }

    stbi_image_free(src);
    return true;
}

static bool decode_art_from_fd_slice(int fd, off_t offset, size_t len)
{
    if (len < 16 || len > NP_MAX_ART_BYTES)
        return false;
    if (lseek(fd, offset, SEEK_SET) < 0)
        return false;
    uint8_t *buf = malloc(len);
    if (!buf)
        return false;
    bool ok = read_exact(fd, buf, len) && decode_and_cache_art(buf, len);
    free(buf);
    return ok;
}

/* ---- Container / Tag Inspectors ---------------------------------------- */

/* Parse a FLAC METADATA_BLOCK_PICTURE payload in memory */
static bool parse_flac_picture_buf(const uint8_t *buf, size_t len,
                                   uint32_t *out_pic_type,
                                   const uint8_t **out_img, size_t *out_img_len)
{
    if (len < 32)
        return false;
    uint32_t pic_type = rd_be32(buf);
    uint32_t mime_len = rd_be32(buf + 4);
    size_t pos = 8;
    if (mime_len > len - pos)
        return false;
    /* Ignore URL reference ("-->") */
    if (mime_len == 3 && memcmp(buf + pos, "-->", 3) == 0)
        return false;
    pos += mime_len;

    if (len - pos < 4)
        return false;
    uint32_t desc_len = rd_be32(buf + pos);
    pos += 4;
    if (desc_len > len - pos)
        return false;
    pos += desc_len;

    /* 16 bytes: width, height, depth, indexed colors */
    if (len - pos < 20)
        return false;
    pos += 16;

    uint32_t data_len = rd_be32(buf + pos);
    pos += 4;
    if (data_len < 16 || data_len > len - pos || data_len > NP_MAX_ART_BYTES)
        return false;

    if (out_pic_type)
        *out_pic_type = pic_type;
    *out_img = buf + pos;
    *out_img_len = data_len;
    return true;
}

static size_t id3_unsync_inplace(uint8_t *buf, size_t len)
{
    size_t w = 0;
    for (size_t r = 0; r < len; r++)
    {
        buf[w++] = buf[r];
        if (buf[r] == 0xff && r + 1 < len && buf[r + 1] == 0x00)
            r++;
    }
    return w;
}

/* Inspect an ID3v2.2/2.3/2.4 tag starting at `tag_off`; returns end offset
 * of the ID3v2 tag (or `tag_off` if none). */
static off_t inspect_id3v2(int fd, off_t tag_off, bool *got_art)
{
    uint8_t hdr[10];
    if (lseek(fd, tag_off, SEEK_SET) < 0 || !read_exact(fd, hdr, 10))
        return tag_off;
    if (memcmp(hdr, "ID3", 3) != 0)
        return tag_off;

    uint8_t ver = hdr[3];
    uint8_t flags = hdr[5];
    uint32_t tag_size = rd_synchsafe32(hdr + 6);
    off_t tag_end = tag_off + 10 + (off_t)tag_size;
    if (ver < 2 || ver > 4 || tag_size < 10)
        return tag_end;

    bool tag_unsync = (flags & 0x80) != 0;
    off_t pos = tag_off + 10;

    /* Skip extended header if present (v2.3 / v2.4 bit 6) */
    if ((flags & 0x40) && ver >= 3)
    {
        uint8_t ext[6];
        if (lseek(fd, pos, SEEK_SET) < 0 || !read_exact(fd, ext, 6))
            return tag_end;
        uint32_t ext_sz = (ver == 4) ? rd_synchsafe32(ext) : rd_be32(ext);
        if (ver == 3)
            pos += 4 + (off_t)ext_sz;
        else
            pos += (off_t)ext_sz;
    }

    off_t fallback_pos = 0;
    size_t fallback_len = 0;
    uint16_t fallback_flags = 0;
    bool is_v22 = (ver == 2);

    while (pos + (is_v22 ? 6 : 10) <= tag_end)
    {
        uint8_t fhdr[10];
        size_t fhdr_sz = is_v22 ? 6 : 10;
        if (lseek(fd, pos, SEEK_SET) < 0 || !read_exact(fd, fhdr, fhdr_sz))
            break;
        if (fhdr[0] == 0)
            break; /* padding */

        uint32_t fsize = 0;
        uint16_t fflags = 0;
        bool is_pic = false;

        if (is_v22)
        {
            fsize = rd_be24(fhdr + 3);
            is_pic = (memcmp(fhdr, "PIC", 3) == 0);
        }
        else
        {
            /* Note: some broken taggers write non-synchsafe sizes in v2.4 APIC;
             * detect when high bits are set or when synchsafe doesn't land on a
             * valid next frame header. */
            if (ver == 4)
            {
                if ((fhdr[4] | fhdr[5] | fhdr[6] | fhdr[7]) & 0x80)
                    fsize = rd_be32(fhdr + 4);
                else
                {
                    uint32_t ss = rd_synchsafe32(fhdr + 4);
                    uint32_t be = rd_be32(fhdr + 4);
                    if (ss != be && pos + 10 + (off_t)be <= tag_end)
                    {
                        uint8_t peek[4] = {0};
                        if (lseek(fd, pos + 10 + (off_t)ss, SEEK_SET) >= 0 &&
                            read_exact(fd, peek, 4))
                        {
                            bool valid_next = (peek[0] == 0) ||
                                              (isupper(peek[0]) && isupper(peek[1]) &&
                                               isupper(peek[2]) &&
                                               (isupper(peek[3]) || isdigit(peek[3])));
                            fsize = valid_next ? ss : be;
                        }
                        else
                            fsize = ss;
                    }
                    else
                        fsize = ss;
                }
            }
            else
            {
                fsize = rd_be32(fhdr + 4);
            }
            fflags = rd_be16(fhdr + 8);
            is_pic = (memcmp(fhdr, "APIC", 4) == 0);
        }

        off_t payload_pos = pos + (off_t)fhdr_sz;
        if (fsize == 0 || payload_pos + (off_t)fsize > tag_end)
            break;

        if (is_pic && fsize <= NP_MAX_ART_BYTES && !*got_art)
        {
            /* Peek first 256 bytes of APIC/PIC header to check picture type */
            uint8_t ph[256];
            size_t peek_n = fsize < sizeof(ph) ? fsize : sizeof(ph);
            if (lseek(fd, payload_pos, SEEK_SET) >= 0 && read_exact(fd, ph, peek_n))
            {
                size_t p = 0;
                if (ver == 4 && (fflags & 0x0001))
                    p += 4; /* data length indicator */
                if (p < peek_n)
                {
                    p++; /* text encoding byte */
                    if (is_v22)
                    {
                        p += 3; /* 3-byte image format ("JPG"/"PNG") */
                    }
                    else
                    {
                        while (p < peek_n && ph[p] != 0)
                            p++;
                        p++; /* skip NUL */
                    }
                    uint8_t pic_type = (p < peek_n) ? ph[p] : 0;
                    if (pic_type == 3 || fallback_pos == 0)
                    {
                        fallback_pos = payload_pos;
                        fallback_len = fsize;
                        fallback_flags = fflags;
                        if (pic_type == 3)
                        {
                            /* Front cover found: decode right away */
                            uint8_t *raw = malloc(fsize);
                            if (raw && lseek(fd, payload_pos, SEEK_SET) >= 0 &&
                                read_exact(fd, raw, fsize))
                            {
                                size_t rlen = fsize;
                                size_t off = 0;
                                if (ver == 4 && (fflags & 0x0001) && rlen >= 4)
                                    off = 4;
                                if (tag_unsync || (ver == 4 && (fflags & 0x0002)))
                                    rlen = off + id3_unsync_inplace(raw + off, rlen - off);
                                /* Walk past encoding + mime + pic_type + desc */
                                size_t q = off;
                                if (q < rlen)
                                {
                                    uint8_t fenc = raw[q++];
                                    if (is_v22)
                                        q += 3;
                                    else
                                    {
                                        while (q < rlen && raw[q] != 0)
                                            q++;
                                        q++;
                                    }
                                    if (q < rlen)
                                        q++; /* pic_type */
                                    if (fenc == 1 || fenc == 2)
                                    {
                                        while (q + 1 < rlen &&
                                               !(raw[q] == 0 && raw[q + 1] == 0))
                                            q += 2;
                                        q += 2;
                                    }
                                    else
                                    {
                                        while (q < rlen && raw[q] != 0)
                                            q++;
                                        q++;
                                    }
                                    if (q < rlen &&
                                        decode_and_cache_art(raw + q, rlen - q))
                                    {
                                        *got_art = true;
                                    }
                                }
                            }
                            free(raw);
                        }
                    }
                }
            }
        }

        pos = payload_pos + (off_t)fsize;
    }

    if (!*got_art && fallback_pos > 0 && fallback_len > 0)
    {
        uint8_t *raw = malloc(fallback_len);
        if (raw && lseek(fd, fallback_pos, SEEK_SET) >= 0 &&
            read_exact(fd, raw, fallback_len))
        {
            size_t rlen = fallback_len;
            size_t off = 0;
            if (ver == 4 && (fallback_flags & 0x0001) && rlen >= 4)
                off = 4;
            if (tag_unsync || (ver == 4 && (fallback_flags & 0x0002)))
                rlen = off + id3_unsync_inplace(raw + off, rlen - off);
            size_t q = off;
            if (q < rlen)
            {
                uint8_t fenc = raw[q++];
                if (is_v22)
                    q += 3;
                else
                {
                    while (q < rlen && raw[q] != 0)
                        q++;
                    q++;
                }
                if (q < rlen)
                    q++;
                if (fenc == 1 || fenc == 2)
                {
                    while (q + 1 < rlen && !(raw[q] == 0 && raw[q + 1] == 0))
                        q += 2;
                    q += 2;
                }
                else
                {
                    while (q < rlen && raw[q] != 0)
                        q++;
                    q++;
                }
                if (q < rlen && decode_and_cache_art(raw + q, rlen - q))
                    *got_art = true;
            }
        }
        free(raw);
    }

    return tag_end;
}

/* Inspect native FLAC stream starting at `flac_off` (after any prepended ID3) */
static void inspect_flac(int fd, off_t flac_off, bool *got_art, int *out_bits)
{
    uint8_t magic[4];
    if (lseek(fd, flac_off, SEEK_SET) < 0 || !read_exact(fd, magic, 4))
        return;
    if (memcmp(magic, "fLaC", 4) != 0)
        return;

    off_t pos = flac_off + 4;
    off_t fb_img_off = 0;
    size_t fb_img_len = 0;

    for (int iter = 0; iter < 128; iter++)
    {
        uint8_t bh[4];
        if (lseek(fd, pos, SEEK_SET) < 0 || !read_exact(fd, bh, 4))
            break;
        bool is_last = (bh[0] & 0x80) != 0;
        uint8_t btype = bh[0] & 0x7f;
        uint32_t blen = rd_be24(bh + 1);
        off_t body = pos + 4;

        if (btype == 0 && blen >= 18)
        {
            /* STREAMINFO: bits-per-sample is 5 bits spanning bytes 12..13 */
            uint8_t si[18];
            if (read_exact(fd, si, 18))
            {
                int bps = (((si[12] & 0x01) << 4) | ((si[13] >> 4) & 0x0f)) + 1;
                if (bps >= 8 && bps <= 32)
                    *out_bits = bps;
            }
        }
        else if (btype == 6 && !*got_art && blen >= 32 && blen <= NP_MAX_ART_BYTES)
        {
            /* METADATA_BLOCK_PICTURE: read header fields first without allocating
             * the entire image until we know whether it's Front Cover (type 3). */
            uint8_t ph[8];
            if (read_exact(fd, ph, 8))
            {
                uint32_t pic_type = rd_be32(ph);
                uint32_t mime_len = rd_be32(ph + 4);
                if (mime_len < 256 && 8 + mime_len + 4 <= blen)
                {
                    if (lseek(fd, body + 8 + (off_t)mime_len, SEEK_SET) >= 0)
                    {
                        uint8_t dl[4];
                        if (read_exact(fd, dl, 4))
                        {
                            uint32_t desc_len = rd_be32(dl);
                            size_t hdr_total = 8 + (size_t)mime_len + 4 +
                                               (size_t)desc_len + 16;
                            if (hdr_total + 4 <= blen &&
                                lseek(fd, body + (off_t)hdr_total, SEEK_SET) >= 0)
                            {
                                uint8_t il[4];
                                if (read_exact(fd, il, 4))
                                {
                                    uint32_t img_len = rd_be32(il);
                                    off_t img_off = body + (off_t)hdr_total + 4;
                                    if (img_len >= 16 &&
                                        hdr_total + 4 + img_len <= blen)
                                    {
                                        if (pic_type == 3)
                                        {
                                            if (decode_art_from_fd_slice(fd, img_off,
                                                                         img_len))
                                                *got_art = true;
                                        }
                                        else if (fb_img_off == 0)
                                        {
                                            fb_img_off = img_off;
                                            fb_img_len = img_len;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if (is_last)
            break;
        pos = body + (off_t)blen;
    }

    if (!*got_art && fb_img_off > 0 && fb_img_len > 0)
    {
        if (decode_art_from_fd_slice(fd, fb_img_off, fb_img_len))
            *got_art = true;
    }
}

/* Base64 decoder for Ogg Vorbis / Opus METADATA_BLOCK_PICTURE */
static int b64_val(unsigned char c)
{
    if (c >= 'A' && c <= 'Z') return c - 'A';
    if (c >= 'a' && c <= 'z') return c - 'a' + 26;
    if (c >= '0' && c <= '9') return c - '0' + 52;
    if (c == '+') return 62;
    if (c == '/') return 63;
    return -1;
}

static uint8_t *base64_decode_alloc(const char *src, size_t src_len, size_t *out_len)
{
    size_t cap = (src_len / 4 + 1) * 3;
    if (cap > NP_MAX_ART_BYTES)
        return NULL;
    uint8_t *out = malloc(cap);
    if (!out)
        return NULL;
    size_t o = 0;
    uint32_t acc = 0;
    int bits = 0;
    for (size_t i = 0; i < src_len; i++)
    {
        unsigned char c = (unsigned char)src[i];
        if (c == '=')
            break;
        int v = b64_val(c);
        if (v < 0)
            continue;
        acc = (acc << 6) | (uint32_t)v;
        bits += 6;
        if (bits >= 8)
        {
            bits -= 8;
            out[o++] = (uint8_t)((acc >> bits) & 0xff);
        }
    }
    *out_len = o;
    return out;
}

static void inspect_ogg(int fd, bool *got_art)
{
    if (lseek(fd, 0, SEEK_SET) < 0)
        return;

    /* Reassemble Ogg packets until we read packet #1 (Vorbis/Opus tags) */
    const size_t max_pkt = 8 * 1024 * 1024;
    uint8_t *pkt = NULL;
    size_t pkt_len = 0, pkt_cap = 0;
    int pkt_idx = 0;

    for (int page = 0; page < 4096 && pkt_idx <= 1; page++)
    {
        uint8_t ph[27];
        if (!read_exact(fd, ph, 27) || memcmp(ph, "OggS", 4) != 0)
            break;
        uint8_t nsegs = ph[26];
        uint8_t segs[255];
        if (nsegs > 0 && !read_exact(fd, segs, nsegs))
            break;

        for (int s = 0; s < nsegs; s++)
        {
            uint8_t slen = segs[s];
            if (pkt_idx == 1)
            {
                if (pkt_len + slen > max_pkt)
                {
                    free(pkt);
                    return;
                }
                if (pkt_len + slen > pkt_cap)
                {
                    size_t ncap = pkt_cap ? pkt_cap * 2 : 65536;
                    while (ncap < pkt_len + slen)
                        ncap *= 2;
                    if (ncap > max_pkt)
                        ncap = max_pkt;
                    uint8_t *np = realloc(pkt, ncap);
                    if (!np)
                    {
                        free(pkt);
                        return;
                    }
                    pkt = np;
                    pkt_cap = ncap;
                }
                if (slen > 0 && !read_exact(fd, pkt + pkt_len, slen))
                {
                    free(pkt);
                    return;
                }
                pkt_len += slen;
            }
            else
            {
                if (slen > 0 && lseek(fd, slen, SEEK_CUR) < 0)
                {
                    free(pkt);
                    return;
                }
            }

            if (slen < 255)
            {
                if (pkt_idx == 1)
                {
                    pkt_idx++;
                    break;
                }
                pkt_idx++;
            }
        }
    }

    if (pkt && pkt_len > 16)
    {
        size_t pos = 0;
        if (pkt_len >= 7 && pkt[0] == 0x03 && memcmp(pkt + 1, "vorbis", 6) == 0)
            pos = 7;
        else if (pkt_len >= 8 && memcmp(pkt, "OpusTags", 8) == 0)
            pos = 8;

        if (pos > 0 && pos + 4 <= pkt_len)
        {
            uint32_t vendor_len = rd_le32(pkt + pos);
            pos += 4;
            if (vendor_len <= pkt_len - pos && pkt_len - pos - vendor_len >= 4)
            {
                pos += vendor_len;
                uint32_t ncomments = rd_le32(pkt + pos);
                pos += 4;
                const uint8_t *fb_b64 = NULL;
                size_t fb_b64_len = 0;

                for (uint32_t c = 0; c < ncomments && pos + 4 <= pkt_len; c++)
                {
                    uint32_t clen = rd_le32(pkt + pos);
                    pos += 4;
                    if (clen > pkt_len - pos)
                        break;
                    const char *str = (const char *)(pkt + pos);
                    const char *prefix = "METADATA_BLOCK_PICTURE=";
                    const size_t plen = 23;
                    if (clen > plen && strncasecmp(str, prefix, plen) == 0)
                    {
                        size_t dlen = 0;
                        uint8_t *dec = base64_decode_alloc(str + plen,
                                                           clen - plen, &dlen);
                        if (dec)
                        {
                            uint32_t ptype = 0;
                            const uint8_t *img = NULL;
                            size_t ilen = 0;
                            if (parse_flac_picture_buf(dec, dlen, &ptype, &img, &ilen))
                            {
                                if (ptype == 3)
                                {
                                    if (decode_and_cache_art(img, ilen))
                                    {
                                        *got_art = true;
                                        free(dec);
                                        break;
                                    }
                                }
                                else if (!fb_b64)
                                {
                                    fb_b64 = (const uint8_t *)(str + plen);
                                    fb_b64_len = clen - plen;
                                }
                            }
                            free(dec);
                        }
                    }
                    pos += clen;
                }

                if (!*got_art && fb_b64)
                {
                    size_t dlen = 0;
                    uint8_t *dec = base64_decode_alloc((const char *)fb_b64,
                                                       fb_b64_len, &dlen);
                    if (dec)
                    {
                        const uint8_t *img = NULL;
                        size_t ilen = 0;
                        if (parse_flac_picture_buf(dec, dlen, NULL, &img, &ilen) &&
                            decode_and_cache_art(img, ilen))
                        {
                            *got_art = true;
                        }
                        free(dec);
                    }
                }
            }
        }
    }

    free(pkt);
}

/* Walk ISO BMFF (MP4 / M4A / M4B / ALAC) boxes for `covr` art + `alac` bit depth */
static void inspect_mp4_boxes(int fd, off_t start, off_t end, int depth,
                              bool *got_art, int *out_bits, bool *out_is_alac)
{
    if (depth > 10)
        return;
    off_t pos = start;
    while (pos + 8 <= end)
    {
        uint8_t bh[16];
        if (lseek(fd, pos, SEEK_SET) < 0 || !read_exact(fd, bh, 8))
            break;
        uint64_t bsize = rd_be32(bh);
        off_t hdr_len = 8;
        if (bsize == 1)
        {
            if (pos + 16 > end || !read_exact(fd, bh + 8, 8))
                break;
            bsize = ((uint64_t)rd_be32(bh + 8) << 32) | rd_be32(bh + 12);
            hdr_len = 16;
        }
        else if (bsize == 0)
        {
            bsize = (uint64_t)(end - pos);
        }
        if (bsize < (uint64_t)hdr_len || pos + (off_t)bsize > end)
            break;

        const uint8_t *t = bh + 4;
        off_t body = pos + hdr_len;
        off_t box_end = pos + (off_t)bsize;

        if (memcmp(t, "moov", 4) == 0 || memcmp(t, "trak", 4) == 0 ||
            memcmp(t, "mdia", 4) == 0 || memcmp(t, "minf", 4) == 0 ||
            memcmp(t, "stbl", 4) == 0 || memcmp(t, "udta", 4) == 0 ||
            memcmp(t, "ilst", 4) == 0)
        {
            inspect_mp4_boxes(fd, body, box_end, depth + 1,
                              got_art, out_bits, out_is_alac);
        }
        else if (memcmp(t, "meta", 4) == 0 && body + 4 < box_end)
        {
            /* QuickTime/ISO `meta` isusually a FullBox (4-byte ver/flags),
             * though rare files omit the 4 bytes: peek for a sane child size. */
            uint8_t peek[8];
            off_t child_start = body + 4;
            if (lseek(fd, body, SEEK_SET) >= 0 && read_exact(fd, peek, 8))
            {
                if (memcmp(peek + 4, "hdlr", 4) == 0 ||
                    memcmp(peek + 4, "ilst", 4) == 0)
                    child_start = body;
            }
            inspect_mp4_boxes(fd, child_start, box_end, depth + 1,
                              got_art, out_bits, out_is_alac);
        }
        else if (memcmp(t, "covr", 4) == 0 && !*got_art)
        {
            /* Inside `covr`, look for a `data` atom */
            off_t cpos = body;
            while (cpos + 16 <= box_end)
            {
                uint8_t dh[16];
                if (lseek(fd, cpos, SEEK_SET) < 0 || !read_exact(fd, dh, 16))
                    break;
                uint32_t dsz = rd_be32(dh);
                if (dsz < 16 || cpos + (off_t)dsz > box_end)
                    break;
                if (memcmp(dh + 4, "data", 4) == 0 && dsz > 24)
                {
                    if (decode_art_from_fd_slice(fd, cpos + 16, dsz - 16))
                    {
                        *got_art = true;
                        break;
                    }
                }
                cpos += (off_t)dsz;
            }
        }
        else if (memcmp(t, "stsd", 4) == 0 && body + 16 <= box_end)
        {
            /* AudioSampleEntry inside `stsd` (skip 8-byte version/flags + count) */
            uint8_t sh[44];
            size_t sn = (size_t)(box_end - (body + 8));
            if (sn > sizeof(sh))
                sn = sizeof(sh);
            if (sn >= 28 && lseek(fd, body + 8, SEEK_SET) >= 0 &&
                read_exact(fd, sh, sn))
            {
                uint32_t esz = rd_be32(sh);
                const uint8_t *efmt = sh + 4;
                if (memcmp(efmt, "alac", 4) == 0)
                {
                    *out_is_alac = true;
                    /* Scan inside AudioSampleEntry (after 36-byte header) for
                     * inner `alac` magic cookie box */
                    off_t inner = body + 8 + 36;
                    off_t iend = body + 8 + (off_t)esz;
                    if (iend > box_end)
                        iend = box_end;
                    while (inner + 24 <= iend)
                    {
                        uint8_t ah[24];
                        if (lseek(fd, inner, SEEK_SET) < 0 ||
                            !read_exact(fd, ah, 24))
                            break;
                        uint32_t asz = rd_be32(ah);
                        if (asz < 24 || inner + (off_t)asz > iend)
                            break;
                        if (memcmp(ah + 4, "alac", 4) == 0)
                        {
                            int bps = ah[17]; /* [8 hdr][4 ver][4 frameLen][1 compat][1 bitDepth] */
                            if (bps == 16 || bps == 20 || bps == 24 || bps == 32)
                                *out_bits = bps;
                            break;
                        }
                        inner += (off_t)asz;
                    }
                    if (*out_bits == 0)
                    {
                        int bps = rd_be16(sh + 24);
                        if (bps >= 8 && bps <= 32)
                            *out_bits = bps;
                    }
                }
                else if (memcmp(efmt, "ipcm", 4) == 0 ||
                         memcmp(efmt, "lpcm", 4) == 0 ||
                         memcmp(efmt, "sowt", 4) == 0 ||
                         memcmp(efmt, "twos", 4) == 0)
                {
                    int bps = rd_be16(sh + 24);
                    if (bps >= 8 && bps <= 32)
                        *out_bits = bps;
                }
                else if (memcmp(efmt, "in24", 4) == 0)
                    *out_bits = 24;
                else if (memcmp(efmt, "in32", 4) == 0 ||
                         memcmp(efmt, "fl32", 4) == 0)
                    *out_bits = 32;
            }
        }

        pos = box_end;
    }
}

static void inspect_wav_or_aiff(int fd, off_t fsize, bool *got_art, int *out_bits)
{
    uint8_t hdr[12];
    if (lseek(fd, 0, SEEK_SET) < 0 || !read_exact(fd, hdr, 12))
        return;

    if ((memcmp(hdr, "RIFF", 4) == 0 || memcmp(hdr, "RF64", 4) == 0) &&
        memcmp(hdr + 8, "WAVE", 4) == 0)
    {
        off_t pos = 12;
        while (pos + 8 <= fsize)
        {
            uint8_t ch[8];
            if (lseek(fd, pos, SEEK_SET) < 0 || !read_exact(fd, ch, 8))
                break;
            uint32_t csz = rd_le32(ch + 4);
            off_t body = pos + 8;
            if (memcmp(ch, "fmt ", 4) == 0 && csz >= 16)
            {
                uint8_t fmt[16];
                if (read_exact(fd, fmt, 16))
                {
                    int bps = rd_le16(fmt + 14);
                    if (bps >= 8 && bps <= 64)
                        *out_bits = bps;
                }
            }
            else if ((memcmp(ch, "id3 ", 4) == 0 || memcmp(ch, "ID3 ", 4) == 0) &&
                     !*got_art)
            {
                inspect_id3v2(fd, body, got_art);
            }
            pos = body + (off_t)csz + (csz & 1);
        }
    }
    else if (memcmp(hdr, "FORM", 4) == 0 &&
             (memcmp(hdr + 8, "AIFF", 4) == 0 || memcmp(hdr + 8, "AIFC", 4) == 0))
    {
        off_t pos = 12;
        while (pos + 8 <= fsize)
        {
            uint8_t ch[8];
            if (lseek(fd, pos, SEEK_SET) < 0 || !read_exact(fd, ch, 8))
                break;
            uint32_t csz = rd_be32(ch + 4);
            off_t body = pos + 8;
            if (memcmp(ch, "COMM", 4) == 0 && csz >= 8)
            {
                uint8_t comm[8];
                if (read_exact(fd, comm, 8))
                {
                    int bps = rd_be16(comm + 6);
                    if (bps >= 8 && bps <= 64)
                        *out_bits = bps;
                }
            }
            else if (memcmp(ch, "ID3 ", 4) == 0 && !*got_art)
            {
                inspect_id3v2(fd, body, got_art);
            }
            pos = body + (off_t)csz + (csz & 1);
        }
    }
}

/* Inspect APEv2 tag at end of file (used by APE, WavPack, Musepack, MP3) */
static void inspect_apev2_footer(int fd, off_t fsize, bool *got_art)
{
    if (*got_art || fsize < 64)
        return;

    off_t footer_off = fsize - 32;
    uint8_t ftr[32];
    if (lseek(fd, footer_off, SEEK_SET) < 0 || !read_exact(fd, ftr, 32))
        return;
    if (memcmp(ftr, "APETAGEX", 8) != 0 && fsize >= 160)
    {
        /* May sit just before a 128-byte ID3v1 trailer */
        footer_off = fsize - 160;
        if (lseek(fd, footer_off, SEEK_SET) < 0 || !read_exact(fd, ftr, 32))
            return;
    }
    if (memcmp(ftr, "APETAGEX", 8) != 0)
        return;

    uint32_t tag_sz = rd_le32(ftr + 12); /* items + 32-byte footer */
    uint32_t nitems = rd_le32(ftr + 16);
    if (tag_sz <= 32 || tag_sz > (uint32_t)(NP_MAX_ART_BYTES + 65536) ||
        (off_t)tag_sz > footer_off + 32)
        return;

    off_t pos = footer_off + 32 - (off_t)tag_sz;
    off_t items_end = footer_off;

    for (uint32_t i = 0; i < nitems && pos + 10 <= items_end; i++)
    {
        uint8_t ih[8];
        if (lseek(fd, pos, SEEK_SET) < 0 || !read_exact(fd, ih, 8))
            break;
        uint32_t vlen = rd_le32(ih);
        uint32_t iflags = rd_le32(ih + 4);
        pos += 8;

        char key[64];
        size_t k = 0;
        while (pos < items_end && k + 1 < sizeof(key))
        {
            char c = 0;
            if (!read_exact(fd, &c, 1))
                return;
            pos++;
            if (c == '\0')
                break;
            key[k++] = c;
        }
        key[k] = '\0';
        if (pos + (off_t)vlen > items_end)
            break;

        bool is_binary = ((iflags >> 1) & 3) == 1;
        if (is_binary && vlen > 16 && vlen <= NP_MAX_ART_BYTES &&
            strncasecmp(key, "Cover Art", 9) == 0)
        {
            uint8_t *val = malloc(vlen);
            if (val && lseek(fd, pos, SEEK_SET) >= 0 && read_exact(fd, val, vlen))
            {
                /* Binary APEv2 cover value is: [filename\0][raw image data] */
                size_t z = 0;
                while (z < vlen && z < 256 && val[z] != 0)
                    z++;
                if (z + 1 < vlen &&
                    decode_and_cache_art(val + z + 1, vlen - (z + 1)))
                {
                    *got_art = true;
                    free(val);
                    return;
                }
            }
            free(val);
        }
        pos += (off_t)vlen;
    }
}

/* Folder fallback: search the audio track's real directory for cover images */
static bool is_img_ext(const char *ext)
{
    return strcasecmp(ext, ".jpg") == 0 ||
           strcasecmp(ext, ".jpeg") == 0 ||
           strcasecmp(ext, ".png") == 0 ||
           strcasecmp(ext, ".bmp") == 0;
}

static int score_folder_cover(const char *fname, const char *track_stem,
                              const char *album_name)
{
    const char *dot = strrchr(fname, '.');
    if (!dot || !is_img_ext(dot))
        return 0;

    size_t slen = (size_t)(dot - fname);
    if (slen == 0 || fname[0] == '.')
        return 0;

    char stem[MAX_PATH];
    if (slen >= sizeof(stem))
        slen = sizeof(stem) - 1;
    memcpy(stem, fname, slen);
    stem[slen] = '\0';

    if (track_stem && track_stem[0] && strcasecmp(stem, track_stem) == 0)
        return 100;
    if (album_name && album_name[0] && strcasecmp(stem, album_name) == 0)
        return 95;
    if (strcasecmp(stem, "cover") == 0)   return 90;
    if (strcasecmp(stem, "folder") == 0)  return 85;
    if (strcasecmp(stem, "front") == 0)   return 80;
    if (strcasecmp(stem, "album") == 0)   return 75;
    if (strcasecmp(stem, "artwork") == 0) return 70;
    if (strcasecmp(stem, "thumb") == 0)   return 60;
    if (strncasecmp(stem, "cover", 5) == 0 ||
        strncasecmp(stem, "folder", 6) == 0 ||
        strncasecmp(stem, "front", 5) == 0)
        return 50;
    return 10; /* any image in the album folder as last resort */
}

static void inspect_folder_art(const char *track_path, const char *album_name,
                               bool *got_art)
{
    if (*got_art || !track_path || !track_path[0])
        return;

    /* Resolve tmpfs library farm symlinks to the real SD card folder */
    char resolved[MAX_PATH];
    const char *real = realpath(track_path, resolved);
    if (!real)
        real = track_path;

    const char *slash = strrchr(real, '/');
    if (!slash || slash == real)
        return;

    char dirpath[MAX_PATH];
    size_t dlen = (size_t)(slash - real);
    if (dlen >= sizeof(dirpath))
        return;
    memcpy(dirpath, real, dlen);
    dirpath[dlen] = '\0';

    char track_stem[MAX_PATH];
    strlcpy(track_stem, slash + 1, sizeof(track_stem));
    char *tdot = strrchr(track_stem, '.');
    if (tdot)
        *tdot = '\0';

    DIR *d = opendir(dirpath);
    if (!d)
        return;

    char best_name[MAX_PATH] = "";
    int best_score = 0;
    struct dirent *de;
    while ((de = readdir(d)) != NULL)
    {
        int sc = score_folder_cover(de->d_name, track_stem, album_name);
        if (sc > best_score)
        {
            best_score = sc;
            strlcpy(best_name, de->d_name, sizeof(best_name));
        }
    }
    closedir(d);

    if (best_score > 0 && best_name[0])
    {
        char full[MAX_PATH];
        snprintf(full, sizeof(full), "%s/%s", dirpath, best_name);
        int fd = open(full, O_RDONLY);
        if (fd >= 0)
        {
            off_t sz = filesize(fd);
            if (sz >= 16 && sz <= NP_MAX_ART_BYTES)
            {
                if (decode_art_from_fd_slice(fd, 0, (size_t)sz))
                    *got_art = true;
            }
            close(fd);
        }
    }
}

static void ensure_track_inspected(const struct mp3entry *id3)
{
    if (!id3 || !id3->path[0])
    {
        s_cached_path[0] = '\0';
        s_has_art = false;
        s_bit_depth = 0;
        s_is_alac = false;
        return;
    }

    if (strcmp(s_cached_path, id3->path) == 0)
        return;

    strlcpy(s_cached_path, id3->path, sizeof(s_cached_path));
    s_has_art = false;
    s_bit_depth = 0;
    s_is_alac = (id3->codectype == AFMT_MP4_ALAC);

    int fd = open(id3->path, O_RDONLY);
    if (fd >= 0)
    {
        off_t fsize = filesize(fd);
        uint8_t head[12] = {0};
        if (fsize >= 12 && read_exact(fd, head, 12))
        {
            off_t stream_off = 0;
            if (memcmp(head, "ID3", 3) == 0)
            {
                stream_off = inspect_id3v2(fd, 0, &s_has_art);
                if (lseek(fd, stream_off, SEEK_SET) >= 0)
                    read_exact(fd, head, 12);
            }

            if (memcmp(head, "fLaC", 4) == 0)
            {
                inspect_flac(fd, stream_off, &s_has_art, &s_bit_depth);
            }
            else if (memcmp(head + 4, "ftyp", 4) == 0)
            {
                inspect_mp4_boxes(fd, 0, fsize, 0,
                                  &s_has_art, &s_bit_depth, &s_is_alac);
            }
            else if (memcmp(head, "OggS", 4) == 0)
            {
                inspect_ogg(fd, &s_has_art);
            }
            else if (memcmp(head, "RIFF", 4) == 0 ||
                     memcmp(head, "RF64", 4) == 0 ||
                     memcmp(head, "FORM", 4) == 0)
            {
                inspect_wav_or_aiff(fd, fsize, &s_has_art, &s_bit_depth);
            }
            else if (memcmp(head, "wvpk", 4) == 0 && fsize >= 32)
            {
                uint8_t wvh[32];
                if (lseek(fd, stream_off, SEEK_SET) >= 0 &&
                    read_exact(fd, wvh, 32))
                {
                    uint32_t flags = rd_le32(wvh + 24);
                    s_bit_depth = (int)(((flags & 3) + 1) * 8);
                }
                inspect_apev2_footer(fd, fsize, &s_has_art);
            }
            else if (memcmp(head, "MAC ", 4) == 0 && fsize >= 76)
            {
                uint8_t mh[76];
                if (lseek(fd, stream_off, SEEK_SET) >= 0 &&
                    read_exact(fd, mh, 76))
                {
                    uint16_t ver = rd_le16(mh + 4);
                    if (ver >= 3980)
                    {
                        uint32_t dlen = rd_le32(mh + 8);
                        if (dlen >= 24 && dlen + 8 <= sizeof(mh))
                        {
                            int bps = rd_le16(mh + dlen + 6);
                            if (bps >= 8 && bps <= 32)
                                s_bit_depth = bps;
                        }
                    }
                }
                inspect_apev2_footer(fd, fsize, &s_has_art);
            }
            else if (memcmp(head, "DSD ", 4) == 0 ||
                     memcmp(head, "FRM8", 4) == 0)
            {
                s_bit_depth = 1;
            }
            else
            {
                inspect_apev2_footer(fd, fsize, &s_has_art);
            }
        }
        close(fd);
    }

    if (!s_has_art &&
        (uipod_navidrome_is_track_path(id3->path) ||
         uipod_jellyfin_is_track_path(id3->path)))
    {
        char cover_path[MAX_PATH];
        int remote_bits = 0;
        bool got_cov = uipod_navidrome_is_track_path(id3->path)
            ? uipod_navidrome_get_cover_info(id3->path, cover_path,
                                               sizeof(cover_path), &remote_bits)
            : uipod_jellyfin_get_cover_info(id3->path, cover_path,
                                              sizeof(cover_path), &remote_bits);
        if (got_cov)
        {
            int cfd = open(cover_path, O_RDONLY);
            if (cfd >= 0)
            {
                off_t sz = filesize(cfd);
                if (sz >= 16 && sz <= NP_MAX_ART_BYTES)
                {
                    if (decode_art_from_fd_slice(cfd, 0, (size_t)sz))
                        s_has_art = true;
                }
                close(cfd);
            }
        }
        if (s_bit_depth <= 0 && remote_bits > 0)
            s_bit_depth = remote_bits;
    }

    if (!s_has_art)
        inspect_folder_art(id3->path, id3->album, &s_has_art);
}

/* ---- Anti-Aliased SDF Drawing Primitives ------------------------------- */

static inline void blend_fg_over_bg(fb_data *px, unsigned fg, unsigned bg,
                                    int cov /* 0..255 */)
{
    if (cov <= 0)
        return;
    int fr = RGB_UNPACK_RED(fg),   fg_g = RGB_UNPACK_GREEN(fg), fb_b = RGB_UNPACK_BLUE(fg);
    if (cov >= 255)
    {
        *px = FB_RGBPACK(fr, fg_g, fb_b);
        return;
    }
    int br = RGB_UNPACK_RED(bg),   bg_g = RGB_UNPACK_GREEN(bg), bb_b = RGB_UNPACK_BLUE(bg);
    int r = (fr * cov + br * (255 - cov) + 127) / 255;
    int g = (fg_g * cov + bg_g * (255 - cov) + 127) / 255;
    int b = (fb_b * cov + bb_b * (255 - cov) + 127) / 255;
    *px = FB_RGBPACK(r, g, b);
}

static inline int sdf_to_cov(float d)
{
    /* Map signed distance (negative = inside, positive = outside) across 1px
     * smooth anti-aliased edge [-0.5, +0.5] to [255, 0]. */
    float a = 0.5f - d;
    if (a <= 0.0f) return 0;
    if (a >= 1.0f) return 255;
    return (int)(a * 255.0f + 0.5f);
}

/* Signed distance to a rounded rectangle spanning [x0, x1] x [y0, y1] */
static inline float sdf_rrect(float px, float py,
                              float x0, float y0, float x1, float y1, float r)
{
    float cx = 0.5f * (x0 + x1);
    float cy = 0.5f * (y0 + y1);
    float hx = 0.5f * (x1 - x0) - r;
    float hy = 0.5f * (y1 - y0) - r;
    if (hx < 0.0f) hx = 0.0f;
    if (hy < 0.0f) hy = 0.0f;
    float dx = fabsf(px - cx) - hx;
    float dy = fabsf(py - cy) - hy;
    float ox = dx > 0.0f ? dx : 0.0f;
    float oy = dy > 0.0f ? dy : 0.0f;
    float out = sqrtf(ox * ox + oy * oy);
    float in = (dx > dy ? dx : dy);
    if (in > 0.0f) in = 0.0f;
    return out + in - r;
}

/* Signed distance to a 2D triangle (CCW or CW) */
static float sdf_triangle(float px, float py,
                          float ax, float ay,
                          float bx, float by,
                          float cx, float cy)
{
    float e0x = bx - ax, e0y = by - ay;
    float e1x = cx - bx, e1y = cy - by;
    float e2x = ax - cx, e2y = ay - cy;

    float v0x = px - ax, v0y = py - ay;
    float v1x = px - bx, v1y = py - by;
    float v2x = px - cx, v2y = py - cy;

    float t0 = v0x * e0x + v0y * e0y;
    float l0 = e0x * e0x + e0y * e0y;
    t0 = t0 < 0.0f ? 0.0f : (t0 > l0 ? 1.0f : t0 / l0);
    float d0x = v0x - e0x * t0, d0y = v0y - e0y * t0;

    float t1 = v1x * e1x + v1y * e1y;
    float l1 = e1x * e1x + e1y * e1y;
    t1 = t1 < 0.0f ? 0.0f : (t1 > l1 ? 1.0f : t1 / l1);
    float d1x = v1x - e1x * t1, d1y = v1y - e1y * t1;

    float t2 = v2x * e2x + v2y * e2y;
    float l2 = e2x * e2x + e2y * e2y;
    t2 = t2 < 0.0f ? 0.0f : (t2 > l2 ? 1.0f : t2 / l2);
    float d2x = v2x - e2x * t2, d2y = v2y - e2y * t2;

    float min_d2 = d0x * d0x + d0y * d0y;
    float m1 = d1x * d1x + d1y * d1y;
    float m2 = d2x * d2x + d2y * d2y;
    if (m1 < min_d2) min_d2 = m1;
    if (m2 < min_d2) min_d2 = m2;

    float s = e0x * e2y - e0y * e2x;
    float sg = (s >= 0.0f) ? 1.0f : -1.0f;
    float c0 = sg * (v0x * e0y - v0y * e0x);
    float c1 = sg * (v1x * e1y - v1y * e1x);
    float c2 = sg * (v2x * e2y - v2y * e2x);
    bool inside = (c0 >= 0.0f && c1 >= 0.0f && c2 >= 0.0f);
    return (inside ? -1.0f : 1.0f) * sqrtf(min_d2);
}

static void draw_aa_rrect_stroke(fb_data *fb, int x0, int y0, int x1, int y1,
                                 float radius, float stroke,
                                 unsigned fg, unsigned bg)
{
    int bx0 = x0 - 2 < 0 ? 0 : x0 - 2;
    int by0 = y0 - 2 < 0 ? 0 : y0 - 2;
    int bx1 = x1 + 2 >= LCD_WIDTH ? LCD_WIDTH - 1 : x1 + 2;
    int by1 = y1 + 2 >= LCD_HEIGHT ? LCD_HEIGHT - 1 : y1 + 2;

    float inner_r = radius - stroke;
    if (inner_r < 0.0f)
        inner_r = 0.0f;

    for (int y = by0; y <= by1; y++)
    {
        fb_data *row = fb + y * LCD_FBWIDTH;
        for (int x = bx0; x <= bx1; x++)
        {
            float d_out = sdf_rrect((float)x, (float)y,
                                    (float)x0, (float)y0,
                                    (float)x1, (float)y1, radius);
            float d_in  = sdf_rrect((float)x, (float)y,
                                    (float)x0 + stroke, (float)y0 + stroke,
                                    (float)x1 - stroke, (float)y1 - stroke,
                                    inner_r);
            float d_ring = d_out > -d_in ? d_out : -d_in;
            int cov = sdf_to_cov(d_ring);
            if (cov > 0)
                blend_fg_over_bg(&row[x], fg, bg, cov);
        }
    }
}

static void draw_aa_rrect_fill(fb_data *fb, float x0, float y0,
                               float x1, float y1, float radius,
                               unsigned fg, unsigned bg)
{
    int bx0 = (int)floorf(x0) - 2;
    int by0 = (int)floorf(y0) - 2;
    int bx1 = (int)ceilf(x1) + 2;
    int by1 = (int)ceilf(y1) + 2;
    if (bx0 < 0) bx0 = 0;
    if (by0 < 0) by0 = 0;
    if (bx1 >= LCD_WIDTH) bx1 = LCD_WIDTH - 1;
    if (by1 >= LCD_HEIGHT) by1 = LCD_HEIGHT - 1;

    for (int y = by0; y <= by1; y++)
    {
        fb_data *row = fb + y * LCD_FBWIDTH;
        for (int x = bx0; x <= bx1; x++)
        {
            float d = sdf_rrect((float)x, (float)y, x0, y0, x1, y1, radius);
            int cov = sdf_to_cov(d);
            if (cov > 0)
                blend_fg_over_bg(&row[x], fg, bg, cov);
        }
    }
}

static void draw_play_state_icon(fb_data *fb, bool paused, int ffmode,
                                 unsigned fg, unsigned bg)
{
    if (ffmode == STATUS_FASTFORWARD)
    {
        for (int y = 37; y <= 92; y++)
        {
            fb_data *row = fb + y * LCD_FBWIDTH;
            for (int x = 39; x <= 93; x++)
            {
                float d1 = sdf_triangle((float)x, (float)y,
                                        41.0f, 43.0f, 41.0f, 86.0f, 66.0f, 64.5f) - 0.6f;
                float d2 = sdf_triangle((float)x, (float)y,
                                        66.0f, 43.0f, 66.0f, 86.0f, 91.0f, 64.5f) - 0.6f;
                float d = d1 < d2 ? d1 : d2;
                blend_fg_over_bg(&row[x], fg, bg, sdf_to_cov(d));
            }
        }
    }
    else if (ffmode == STATUS_FASTBACKWARD)
    {
        for (int y = 37; y <= 92; y++)
        {
            fb_data *row = fb + y * LCD_FBWIDTH;
            for (int x = 39; x <= 93; x++)
            {
                float d1 = sdf_triangle((float)x, (float)y,
                                        66.0f, 43.0f, 66.0f, 86.0f, 41.0f, 64.5f) - 0.6f;
                float d2 = sdf_triangle((float)x, (float)y,
                                        91.0f, 43.0f, 91.0f, 86.0f, 66.0f, 64.5f) - 0.6f;
                float d = d1 < d2 ? d1 : d2;
                blend_fg_over_bg(&row[x], fg, bg, sdf_to_cov(d));
            }
        }
    }
    else if (paused)
    {
        draw_aa_rrect_fill(fb, 45.0f, 39.0f, 58.0f, 90.0f, 2.5f, fg, bg);
        draw_aa_rrect_fill(fb, 70.0f, 39.0f, 83.0f, 90.0f, 2.5f, fg, bg);
    }
    else
    {
        /* Solid Play triangle at x = 43..88, y = 39..90 with subtle 1.2px
         * corner rounding matching the mockup */
        for (int y = 37; y <= 92; y++)
        {
            fb_data *row = fb + y * LCD_FBWIDTH;
            for (int x = 41; x <= 90; x++)
            {
                float d = sdf_triangle((float)x, (float)y,
                                       44.2f, 40.4f,
                                       44.2f, 88.6f,
                                       86.6f, 64.5f) - 1.2f;
                blend_fg_over_bg(&row[x], fg, bg, sdf_to_cov(d));
            }
        }
    }
}

static void draw_hold_lock_icon(fb_data *fb, int x0, int y0,
                                unsigned fg, unsigned bg)
{
    /* Clean vector padlock (32x42): body + shackle */
    draw_aa_rrect_fill(fb, (float)x0 + 2.0f, (float)y0 + 18.0f,
                       (float)x0 + 30.0f, (float)y0 + 41.0f, 4.0f, fg, bg);
    draw_aa_rrect_stroke(fb, x0 + 7, y0 + 3, x0 + 25, y0 + 26,
                         9.0f, 3.5f, fg, bg);
    /* Keyhole dot in bg */
    draw_aa_rrect_fill(fb, (float)x0 + 14.0f, (float)y0 + 26.0f,
                       (float)x0 + 18.0f, (float)y0 + 33.0f, 2.0f, bg, fg);
}

/* Sample 4-bit AA glyph alpha (0..255) from a loaded RB12 font for battery
 * split-contrast percentage digits */
static int sample_text_alpha(struct font *pf, const char *str,
                             int text_x, int text_y, int px, int py)
{
    if (!pf || py < text_y || py >= text_y + (int)pf->height || px < text_x)
        return 0;

    int row = py - text_y;
    int cur_x = text_x;
    for (const char *s = str; *s; s++)
    {
        ucschar_t ch = (unsigned char)*s;
        int gw = font_get_width(pf, ch);
        if (gw <= 0)
            continue;
        if (px >= cur_x && px < cur_x + gw)
        {
            const unsigned char *bits = font_get_bits(pf, ch);
            if (!bits)
                return 0;
            int col = px - cur_x;
            int idx = row * gw + col;
            unsigned char b = bits[idx >> 1];
            int nib = (idx & 1) ? (b >> 4) : (b & 0x0f);
            return (15 - nib) * 17;
        }
        cur_x += gw;
    }
    return 0;
}

static void draw_battery_icon(fb_data *fb, int pct, int fid_small,
                              unsigned fg, unsigned bg)
{
    if (pct < 0) pct = 0;
    if (pct > 100) pct = 100;

    /* 1. Outer body (868..983, 39..91, r=9) unified with right terminal notch
     *    (980..990, 54..76, r=6), stroked at 4.0px so the notch interior is
     *    smoothly hollow just like the mockup! */
    for (int y = 37; y <= 93; y++)
    {
        fb_data *row = fb + y * LCD_FBWIDTH;
        for (int x = 866; x <= 992; x++)
        {
            float db_out = sdf_rrect((float)x, (float)y,
                                     868.0f, 39.0f, 983.0f, 91.0f, 9.0f);
            float dn_out = sdf_rrect((float)x, (float)y,
                                     976.0f, 54.0f, 990.0f, 76.0f, 6.0f);
            float d_out = db_out < dn_out ? db_out : dn_out;

            float db_in = sdf_rrect((float)x, (float)y,
                                    872.0f, 43.0f, 979.0f, 87.0f, 5.0f);
            float dn_in = sdf_rrect((float)x, (float)y,
                                    974.0f, 58.0f, 986.0f, 72.0f, 2.5f);
            float d_in = db_in < dn_in ? db_in : dn_in;

            float d_ring = d_out > -d_in ? d_out : -d_in;
            int cov = sdf_to_cov(d_ring);
            if (cov > 0)
                blend_fg_over_bg(&row[x], fg, bg, cov);
        }
    }

    /* 2. Inner fill bar (878..973, 49..81 -> max_w=96, h=33, r=4) + centered
     *    numeric percentage with smooth per-pixel XOR split contrast! */
    char pct_str[8];
    snprintf(pct_str, sizeof(pct_str), "%d", pct);
    struct font *pf = (fid_small >= 0) ? font_get(fid_small) : NULL;
    int tw = 0;
    if (pf)
    {
        for (const char *s = pct_str; *s; s++)
            tw += font_get_width(pf, (unsigned char)*s);
    }
    int tx = 868 + (116 - tw) / 2 + 1;
    int ty = 48; /* places 17px digits at y = 57..73, centered in 39..91 */

    float fill_w = (96.0f * (float)pct) / 100.0f;

    for (int y = 47; y <= 83; y++)
    {
        fb_data *row = fb + y * LCD_FBWIDTH;
        for (int x = 876; x <= 975; x++)
        {
            int cov_fill = 0;
            if (fill_w >= 0.5f)
            {
                float d_max = sdf_rrect((float)x, (float)y,
                                        878.0f, 49.0f, 973.0f, 81.0f, 4.0f);
                float fx0 = (fill_w >= 8.0f) ? 878.0f : (878.0f + fill_w - 8.0f);
                float fx1 = 878.0f + fill_w - 1.0f;
                if (fx1 < fx0) fx1 = fx0;
                float d_bar = sdf_rrect((float)x, (float)y,
                                        fx0, 49.0f, fx1, 81.0f, 4.0f);
                cov_fill = sdf_to_cov(d_max > d_bar ? d_max : d_bar);
            }

            int cov_txt = sample_text_alpha(pf, pct_str, tx, ty, x, y);
            if (cov_fill == 0 && cov_txt == 0)
                continue;

            /* Smooth anti-aliased XOR:
             *   fill=255, txt=255 -> 0   (bg_color digit on filled half)
             *   fill=0,   txt=255 -> 255 (fg_color digit on empty half)
             *   fill=255, txt=0   -> 255 (fg_color fill) */
            int cov = (cov_fill * (255 - cov_txt) +
                       (255 - cov_fill) * cov_txt + 127) / 255;
            fb_data bg_px = FB_RGBPACK(RGB_UNPACK_RED(bg),
                                       RGB_UNPACK_GREEN(bg),
                                       RGB_UNPACK_BLUE(bg));
            row[x] = bg_px;
            blend_fg_over_bg(&row[x], fg, bg, cov);
        }
    }
}

static void draw_seekbar_pill(fb_data *fb, float ratio, unsigned fg, unsigned bg)
{
    if (ratio < 0.0f) ratio = 0.0f;
    if (ratio > 1.0f) ratio = 1.0f;

    /* Outer stadium: x = 33..990 (958px), y = 594..646 (53px), r = 26.5, stroke = 4 */
    draw_aa_rrect_stroke(fb, 33, 594, 990, 646, 26.5f, 4.0f, fg, bg);

    /* Inner stadium fill: x = 43..980 (max_w = 938px), y = 604..636 (33px), r = 16.5 */
    float fill_w = 938.0f * ratio;
    if (fill_w < 1.0f)
        return;

    int bx1 = (int)ceilf(43.0f + fill_w) + 2;
    if (bx1 >= LCD_WIDTH) bx1 = LCD_WIDTH - 1;

    for (int y = 602; y <= 638; y++)
    {
        fb_data *row = fb + y * LCD_FBWIDTH;
        for (int x = 41; x <= bx1; x++)
        {
            float d_max = sdf_rrect((float)x, (float)y,
                                    43.0f, 604.0f, 980.0f, 636.0f, 16.5f);
            float fx0 = (fill_w >= 33.0f) ? 43.0f : (43.0f + fill_w - 33.0f);
            float fx1 = 43.0f + fill_w - 1.0f;
            if (fx1 < fx0) fx1 = fx0;
            float d_bar = sdf_rrect((float)x, (float)y,
                                    fx0, 604.0f, fx1, 636.0f, 16.5f);
            int cov = sdf_to_cov(d_max > d_bar ? d_max : d_bar);
            if (cov > 0)
                blend_fg_over_bg(&row[x], fg, bg, cov);
        }
    }
}

/* ---- Album Art & Fallback Placeholder Blitter -------------------------- */

static void draw_album_art(fb_data *fb, unsigned fg, unsigned bg)
{
    if (s_has_art)
    {
        const bool mono = global_settings.mono_album_art;
        for (int dy = 0; dy < NP_ART_SIZE; dy++)
        {
            fb_data *dst = fb + (NP_ART_Y + dy) * LCD_FBWIDTH + NP_ART_X;
            const uint8_t *src = s_art_rgb + dy * NP_ART_SIZE * 3;
            if (mono)
            {
                for (int dx = 0; dx < NP_ART_SIZE; dx++)
                {
                    int r = src[dx * 3 + 0];
                    int g = src[dx * 3 + 1];
                    int b = src[dx * 3 + 2];
                    int l = (77 * r + 150 * g + 29 * b + 128) >> 8;
                    dst[dx] = FB_RGBPACK(l, l, l);
                }
            }
            else
            {
                for (int dx = 0; dx < NP_ART_SIZE; dx++)
                {
                    dst[dx] = FB_RGBPACK(src[dx * 3 + 0],
                                         src[dx * 3 + 1],
                                         src[dx * 3 + 2]);
                }
            }
        }
    }
    else
    {
        /* Clean minimalist vinyl-record placeholder in theme fg/bg */
        const int x0 = NP_ART_X, y0 = NP_ART_Y;
        const int x1 = NP_ART_X + NP_ART_SIZE - 1;
        const int y1 = NP_ART_Y + NP_ART_SIZE - 1;
        const float cx = (float)x0 + 127.5f;
        const float cy = (float)y0 + 127.5f;

        draw_aa_rrect_stroke(fb, x0, y0, x1, y1, 4.0f, 3.0f, fg, bg);
        for (int y = y0 + 24; y <= y1 - 24; y++)
        {
            fb_data *row = fb + y * LCD_FBWIDTH;
            for (int x = x0 + 24; x <= x1 - 24; x++)
            {
                float dx = (float)x - cx, dy = (float)y - cy;
                float r = sqrtf(dx * dx + dy * dy);
                float d_disc  = fabsf(r - 92.0f) - 1.5f;
                float d_ring1 = fabsf(r - 68.0f) - 0.8f;
                float d_label = fabsf(r - 36.0f) - 1.5f;
                float d_hole  = r - 8.0f;
                float d = d_disc;
                if (d_ring1 < d) d = d_ring1;
                if (d_label < d) d = d_label;
                if (d_hole < d)  d = d_hole;
                int cov = sdf_to_cov(d);
                if (cov > 0)
                    blend_fg_over_bg(&row[x], fg, bg, cov);
            }
        }
    }
}

/* ---- Font & UTF-8 Truncation Helpers ----------------------------------- */

static int ensure_font(int *slot, const char *path)
{
    if (*slot >= 0 && font_filename_matches_loaded_id(*slot, path))
        return *slot;
    int id = font_load(path);
    if (id < 0)
        id = FONT_UI;
    *slot = id;
    return id;
}

/* Truncate UTF-8 `src` into `out` with "..." so its rendered width in the
 * active screen font never exceeds `max_w` pixels. */
static void truncate_utf8_ellipsis(struct screen *s, const char *src,
                                   int max_w, char *out, size_t out_sz)
{
    if (!src || !src[0] || out_sz == 0)
    {
        if (out_sz > 0) out[0] = '\0';
        return;
    }

    int full_w = 0, full_h = 0;
    s->getstringsize((const unsigned char *)src, &full_w, &full_h);
    if (full_w <= max_w)
    {
        strlcpy(out, src, out_sz);
        return;
    }

    int dots_w = 0, dots_h = 0;
    s->getstringsize((const unsigned char *)"...", &dots_w, &dots_h);
    int budget = max_w - dots_w;
    if (budget <= 0 || out_sz < 5)
    {
        strlcpy(out, "...", out_sz);
        return;
    }

    /* Binary-search / step UTF-8 codepoint boundaries up to `budget` */
    size_t len = strlen(src);
    size_t best_end = 0;
    size_t pos = 0;
    while (pos < len)
    {
        size_t next = pos + 1;
        while (next < len && ((unsigned char)src[next] & 0xc0) == 0x80)
            next++;
        if (next + 4 >= out_sz)
            break;

        memcpy(out, src, next);
        out[next] = '\0';
        int w = 0, h = 0;
        s->getstringsize((const unsigned char *)out, &w, &h);
        if (w > budget)
            break;
        best_end = next;
        pos = next;
    }

    /* Trim trailing spaces before "..." unless the original had space + words */
    while (best_end > 0 && src[best_end - 1] == ' ' &&
           best_end >= 2 && src[best_end - 2] == ' ')
        best_end--;

    memcpy(out, src, best_end);
    memcpy(out + best_end, "...", 4);
}

static void format_codec_badge(const struct mp3entry *id3, char *out, size_t sz)
{
    if (!id3)
    {
        strlcpy(out, "AUDIO", sz);
        return;
    }

    /* Check .m4b explicitly so audiobooks show M4B */
    const char *dot = strrchr(id3->path, '.');
    if (dot && strcasecmp(dot, ".m4b") == 0)
    {
        strlcpy(out, "M4B", sz);
        return;
    }
    if (s_is_alac || id3->codectype == AFMT_MP4_ALAC)
    {
        strlcpy(out, "ALAC", sz);
        return;
    }

    switch (id3->codectype)
    {
        case AFMT_FLAC:        strlcpy(out, "FLAC", sz); return;
        case AFMT_MPA_L3:      strlcpy(out, "MP3",  sz); return;
        case AFMT_MPA_L2:      strlcpy(out, "MP2",  sz); return;
        case AFMT_MPA_L1:      strlcpy(out, "MP1",  sz); return;
        case AFMT_PCM_WAV:
        case AFMT_WAVE64:      strlcpy(out, "WAV",  sz); return;
        case AFMT_AIFF:        strlcpy(out, "AIFF", sz); return;
        case AFMT_MP4_AAC:
        case AFMT_MP4_AAC_HE:
        case AFMT_AAC_BSF:     strlcpy(out, "AAC",  sz); return;
        case AFMT_OPUS:        strlcpy(out, "OPUS", sz); return;
        case AFMT_OGG_VORBIS:  strlcpy(out, "OGG",  sz); return;
        case AFMT_SPEEX:       strlcpy(out, "SPX",  sz); return;
        case AFMT_WAVPACK:     strlcpy(out, "WV",   sz); return;
        case AFMT_APE:         strlcpy(out, "APE",  sz); return;
        case AFMT_TTA:         strlcpy(out, "TTA",  sz); return;
        case AFMT_MPC_SV7:
        case AFMT_MPC_SV8:     strlcpy(out, "MPC",  sz); return;
        case AFMT_WMA:
        case AFMT_WMAPRO:
        case AFMT_WMAVOICE:    strlcpy(out, "WMA",  sz); return;
        case AFMT_A52:         strlcpy(out, "AC3",  sz); return;
        default: break;
    }

    if (id3->codectype < AFMT_NUM_CODECS &&
        audio_formats[id3->codectype].label &&
        audio_formats[id3->codectype].label[0])
    {
        strlcpy(out, audio_formats[id3->codectype].label, sz);
    }
    else if (dot && dot[1])
    {
        size_t i = 0;
        for (const char *p = dot + 1; *p && i + 1 < sz && i < 6; p++, i++)
            out[i] = (char)toupper((unsigned char)*p);
        out[i] = '\0';
    }
    else
    {
        strlcpy(out, "PCM", sz);
    }
}

static void format_mmss(long sec, bool neg, char *out, size_t sz)
{
    if (sec < 0)
        sec = 0;
    long h = sec / 3600;
    long m = (sec % 3600) / 60;
    long s = sec % 60;
    const char *pfx = neg ? "-" : "";
    if (h > 0)
        snprintf(out, sz, "%s%ld:%02ld:%02ld", pfx, h, m, s);
    else
        snprintf(out, sz, "%s%ld:%02ld", pfx, m, s);
}

static void extract_filename_stem(const char *path, char *out, size_t sz)
{
    if (!path || !path[0])
    {
        strlcpy(out, "", sz);
        return;
    }
    const char *base = strrchr(path, '/');
    base = base ? base + 1 : path;
    strlcpy(out, base, sz);
    char *dot = strrchr(out, '.');
    if (dot && dot != out)
        *dot = '\0';
}

/* ---- Main 1024x768 Now Playing Renderer -------------------------------- */

void uipod_nowplaying_draw(void)
{
    struct screen *s = &screens[SCREEN_MAIN];
    struct wps_state *state = get_wps_state();
    struct mp3entry *id3 = state ? state->id3 : NULL;

    ensure_track_inspected(id3);

    /* ---- Pure-Rust GPU UI Engine (`taffy` + `femtovg` on GLES 3.0) ----- */
    if (uipod_gpu_ui_ensure_ready())
    {
        const char *hdr_title = str(LANG_NOW_PLAYING);
        if (!hdr_title || !hdr_title[0])
            hdr_title = "Now Playing";

        int pl_pos = playlist_get_display_index();
        int pl_amt = playlist_amount();
        if (pl_amt <= 0) { pl_pos = 1; pl_amt = 1; }
        if (pl_pos <= 0) pl_pos = 1;

        char pos_buf[48];
        snprintf(pos_buf, sizeof(pos_buf), "%d of %d", pl_pos, pl_amt);

        char stem_buf[MAX_PATH];
        const char *raw_title  = "";
        const char *raw_artist = "";
        const char *raw_album  = "";
        char codec_str[16]     = "";
        char spec_bits[24]     = "";
        char spec_khz[24]      = "";

        if (id3)
        {
            if (id3->cuesheet && id3->cuesheet->curr_track &&
                id3->cuesheet->curr_track->title[0])
            {
                raw_title = id3->cuesheet->curr_track->title;
            }
            else if (id3->title && id3->title[0])
            {
                raw_title = id3->title;
            }
            else
            {
                extract_filename_stem(id3->path, stem_buf, sizeof(stem_buf));
                raw_title = stem_buf;
            }

            if (id3->cuesheet && id3->cuesheet->curr_track &&
                id3->cuesheet->curr_track->performer[0])
                raw_artist = id3->cuesheet->curr_track->performer;
            else if (id3->artist && id3->artist[0])
                raw_artist = id3->artist;
            else if (id3->albumartist && id3->albumartist[0])
                raw_artist = id3->albumartist;

            if (id3->album && id3->album[0])
                raw_album = id3->album;
            else if (id3->cuesheet && id3->cuesheet->title[0])
                raw_album = id3->cuesheet->title;

            format_codec_badge(id3, codec_str, sizeof(codec_str));

            if (s_bit_depth > 0)
                snprintf(spec_bits, sizeof(spec_bits), "%dbit", s_bit_depth);
            else if (id3->bitrate > 0)
                snprintf(spec_bits, sizeof(spec_bits), "%ukbps", id3->bitrate);

            if (id3->frequency > 0)
            {
                unsigned long hz = id3->frequency;
                if (hz % 1000 == 0)
                    snprintf(spec_khz, sizeof(spec_khz), "%lukHz", hz / 1000);
                else
                    snprintf(spec_khz, sizeof(spec_khz), "%lu.%lukHz",
                             hz / 1000, (hz % 1000) / 100);
            }
        }

        bool show_vol = global_status.last_volume_change &&
                        TIME_BEFORE(current_tick,
                                    global_status.last_volume_change + HZ * 6 / 5);

        float bar_ratio = 0.0f;
        char el_str[24]     = "";
        char rem_str[24]    = "";
        char vol_str[32]    = "";

        if (show_vol)
        {
            int steps = get_normalized_volume_steps();
            int notch = get_normalized_volume();
            bar_ratio = (steps > 0) ? ((float)notch / (float)steps) : 0.0f;
            snprintf(vol_str, sizeof(vol_str), "Volume %d/%d", notch, steps);
        }
        else
        {
            long pos_ms = 0, len_ms = 0;
            if (id3 && id3->length > 0)
            {
                pos_ms = (long)id3->elapsed + (state ? state->ff_rewind_count : 0);
                len_ms = (long)id3->length;

                if (id3->cuesheet && id3->cuesheet->track_count > 1)
                {
                    struct cuesheet *cue = id3->cuesheet;
                    int idx = cue_find_current_track(cue,
                                  pos_ms > 0 ? (unsigned long)pos_ms : 0);
                    if (idx >= 0 && idx < cue->track_count)
                    {
                        long ch_start = (long)cue->tracks[idx].offset;
                        long ch_end = (idx + 1 < cue->track_count)
                                    ? (long)cue->tracks[idx + 1].offset
                                    : len_ms;
                        if (ch_end > ch_start)
                        {
                            pos_ms -= ch_start;
                            len_ms = ch_end - ch_start;
                        }
                    }
                }

                if (pos_ms < 0) pos_ms = 0;
                if (pos_ms > len_ms) pos_ms = len_ms;
            }

            bar_ratio = (len_ms > 0) ? ((float)pos_ms / (float)len_ms) : 0.0f;
            long el_sec  = pos_ms / 1000;
            long tot_sec = len_ms / 1000;
            long rem_sec = tot_sec - el_sec;
            if (rem_sec < 0) rem_sec = 0;

            format_mmss(el_sec, false, el_str, sizeof(el_str));
            format_mmss(rem_sec, true, rem_str, sizeof(rem_str));
        }

        struct UIPodRsNowPlaying np;
        memset(&np, 0, sizeof(np));
        np.bg_rgb = (uint32_t)global_settings.bg_color;
        np.fg_rgb = (uint32_t)global_settings.fg_color;
        uipod_gpu_fill_header(&np.header, hdr_title);
        np.track_pos_label     = pos_buf;
        np.eq_enabled          = global_settings.eq_enabled;
        np.title               = raw_title;
        np.artist              = raw_artist;
        np.album               = raw_album;
        np.codec_label         = codec_str;
        np.bitdepth_label      = spec_bits;
        np.samplerate_label    = spec_khz;
        np.progress            = bar_ratio;
        np.elapsed_label       = el_str;
        np.remaining_label     = rem_str;
        np.center_status_label = vol_str;
        np.art_rgb24           = s_has_art ? s_art_rgb : NULL;
        np.mono_album_art      = global_settings.mono_album_art;

        struct viewport *prev_vp = lcd_set_viewport(NULL);
        fb_data *fb_gpu = FBADDR(0, 0);
        bool ok = uipod_rs_ui_render_now_playing(&np, (uint8_t *)fb_gpu);
        lcd_set_viewport(prev_vp);

        if (ok)
        {
            lcd_update();
            return;
        }
    }

    int fid_title = ensure_font(&s_fid_title, NP_FONT_TITLE);
    int fid_body  = ensure_font(&s_fid_body, NP_FONT_BODY);
    int fid_smed  = ensure_font(&s_fid_small_med, NP_FONT_SMALL_MED);
    int fid_sbold = ensure_font(&s_fid_small_bold, NP_FONT_SMALL_BOLD);

    const unsigned fg = global_settings.fg_color;
    const unsigned bg = global_settings.bg_color;

    struct viewport vp = {0};
    viewport_set_fullscreen(&vp, SCREEN_MAIN);
    s->set_viewport(&vp);
    s->set_foreground(fg);
    s->set_background(bg);
    s->set_drawmode(DRMODE_SOLID);
    s->clear_viewport();

    fb_data *fb = FBADDR(0, 0);

    /* ---- 1. Header (y = 0..124) ---------------------------------------- */
    bool paused = (audio_status() & AUDIO_STATUS_PAUSE) != 0 ||
                  (state && state->paused);
    draw_play_state_icon(fb, paused, current_playmode(), fg, bg);

    s->setfont(fid_title);
    s->set_drawmode(DRMODE_FG);
    const char *hdr_title = str(LANG_NOW_PLAYING);
    if (!hdr_title || !hdr_title[0])
        hdr_title = "Now Playing";
    int hw = 0, hh = 0;
    s->getstringsize((const unsigned char *)hdr_title, &hw, &hh);
    s->putsxy((LCD_WIDTH - hw) / 2, 28, (const unsigned char *)hdr_title);

#ifdef HAS_BUTTON_HOLD
    if (button_hold())
#else
    if (is_keys_locked())
#endif
        draw_hold_lock_icon(fb, 822, 44, fg, bg);

    draw_battery_icon(fb, battery_level_display(), fid_smed, fg, bg);

    s->set_drawmode(DRMODE_SOLID);
    s->fillrect(NP_X_LEFT, 118, NP_CONTENT_W, 4);

    /* ---- 2. Sub-header: "21 of 500" + [EQ] (y = 144..188) -------------- */
    int pl_pos = playlist_get_display_index();
    int pl_amt = playlist_amount();
    if (pl_amt <= 0) { pl_pos = 1; pl_amt = 1; }
    if (pl_pos <= 0) pl_pos = 1;

    char pos_buf[48];
    snprintf(pos_buf, sizeof(pos_buf), "%d of %d", pl_pos, pl_amt);
    s->setfont(fid_body);
    s->set_drawmode(DRMODE_FG);
    s->putsxy(NP_X_LEFT, 144, (const unsigned char *)pos_buf);

    if (global_settings.eq_enabled)
    {
        draw_aa_rrect_stroke(fb, 936, 155, 990, 188, 5.0f, 2.2f, fg, bg);
        s->setfont(fid_sbold);
        s->set_drawmode(DRMODE_FG);
        int eqw = 0, eqh = 0;
        s->getstringsize((const unsigned char *)"EQ", &eqw, &eqh);
        s->putsxy(936 + (55 - eqw) / 2, 155, (const unsigned char *)"EQ");
    }

    /* ---- 3. Album Art (x = 33..288, y = 227..482, 256x256) ------------- */
    draw_album_art(fb, fg, bg);

    /* ---- 4. Metadata Column (x = 321..990) ----------------------------- */
    char stem_buf[MAX_PATH];
    const char *raw_title = NULL;
    const char *raw_artist = NULL;
    const char *raw_album = NULL;

    if (id3)
    {
        if (id3->cuesheet && id3->cuesheet->curr_track &&
            id3->cuesheet->curr_track->title[0])
        {
            raw_title = id3->cuesheet->curr_track->title;
        }
        else if (id3->title && id3->title[0])
        {
            raw_title = id3->title;
        }
        else
        {
            extract_filename_stem(id3->path, stem_buf, sizeof(stem_buf));
            raw_title = stem_buf;
        }

        if (id3->cuesheet && id3->cuesheet->curr_track &&
            id3->cuesheet->curr_track->performer[0])
            raw_artist = id3->cuesheet->curr_track->performer;
        else if (id3->artist && id3->artist[0])
            raw_artist = id3->artist;
        else if (id3->albumartist && id3->albumartist[0])
            raw_artist = id3->albumartist;

        if (id3->album && id3->album[0])
            raw_album = id3->album;
        else if (id3->cuesheet && id3->cuesheet->title[0])
            raw_album = id3->cuesheet->title;
    }

    char line_buf[512];

    /* Track Title (67-Geologica.fnt at y = 247 -> cap y = 265..299) */
    if (raw_title && raw_title[0])
    {
        s->setfont(fid_title);
        s->set_drawmode(DRMODE_FG);
        truncate_utf8_ellipsis(s, raw_title, NP_META_MAX_W,
                               line_buf, sizeof(line_buf));
        s->putsxy(NP_META_X, 247, (const unsigned char *)line_buf);
    }

    /* Artist (51-Geologica-Medium.fnt at y = 309 -> cap y = 323..348) */
    if (raw_artist && raw_artist[0])
    {
        s->setfont(fid_body);
        s->set_drawmode(DRMODE_FG);
        truncate_utf8_ellipsis(s, raw_artist, NP_META_MAX_W,
                               line_buf, sizeof(line_buf));
        s->putsxy(NP_META_X, 309, (const unsigned char *)line_buf);
    }

    /* Album (51-Geologica-Medium.fnt at y = 366 -> cap y = 380..405) */
    if (raw_album && raw_album[0])
    {
        s->setfont(fid_body);
        s->set_drawmode(DRMODE_FG);
        truncate_utf8_ellipsis(s, raw_album, NP_META_MAX_W,
                               line_buf, sizeof(line_buf));
        s->putsxy(NP_META_X, 366, (const unsigned char *)line_buf);
    }

    /* Codec Badge + Audio Specs Row (y = 421..454) */
    if (id3)
    {
        char codec_str[16];
        format_codec_badge(id3, codec_str, sizeof(codec_str));

        s->setfont(fid_sbold);
        int cw = 0, ch = 0;
        s->getstringsize((const unsigned char *)codec_str, &cw, &ch);
        int badge_w = cw + 19;
        if (badge_w < 56)
            badge_w = 56;

        draw_aa_rrect_stroke(fb, NP_META_X, 421,
                             NP_META_X + badge_w - 1, 454,
                             5.0f, 2.2f, fg, bg);
        s->set_drawmode(DRMODE_FG);
        s->putsxy(NP_META_X + (badge_w - cw) / 2, 421,
                  (const unsigned char *)codec_str);

        s->setfont(fid_smed);
        int spec_x = NP_META_X + badge_w + 22;

        char spec1[24] = "";
        if (s_bit_depth > 0)
            snprintf(spec1, sizeof(spec1), "%dbit", s_bit_depth);
        else if (id3->bitrate > 0)
            snprintf(spec1, sizeof(spec1), "%ukbps", id3->bitrate);

        if (spec1[0])
        {
            int sw1 = 0, sh1 = 0;
            s->getstringsize((const unsigned char *)spec1, &sw1, &sh1);
            s->putsxy(spec_x, 420, (const unsigned char *)spec1);
            spec_x += sw1 + 20;
        }

        if (id3->frequency > 0)
        {
            char spec2[24];
            unsigned long hz = id3->frequency;
            if (hz % 1000 == 0)
                snprintf(spec2, sizeof(spec2), "%lukHz", hz / 1000);
            else
                snprintf(spec2, sizeof(spec2), "%lu.%lukHz",
                         hz / 1000, (hz % 1000) / 100);
            s->putsxy(spec_x, 420, (const unsigned char *)spec2);
        }
    }

    /* ---- 5. Bottom Progress / Volume Pill & Time Labels (y = 594..685) - */
    bool show_vol = global_status.last_volume_change &&
                    TIME_BEFORE(current_tick,
                                global_status.last_volume_change + HZ * 6 / 5);

    if (show_vol)
    {
        int steps = get_normalized_volume_steps();
        int notch = get_normalized_volume();
        float vratio = (steps > 0) ? ((float)notch / (float)steps) : 0.0f;
        draw_seekbar_pill(fb, vratio, fg, bg);

        char vbuf[32];
        snprintf(vbuf, sizeof(vbuf), "Volume %d/%d", notch, steps);
        s->setfont(fid_body);
        s->set_drawmode(DRMODE_FG);
        int vw = 0, vh = 0;
        s->getstringsize((const unsigned char *)vbuf, &vw, &vh);
        s->putsxy((LCD_WIDTH - vw) / 2, 642, (const unsigned char *)vbuf);
    }
    else
    {
        long pos_ms = 0, len_ms = 0;
        if (id3 && id3->length > 0)
        {
            pos_ms = (long)id3->elapsed + (state ? state->ff_rewind_count : 0);
            len_ms = (long)id3->length;

            /* Cuesheet / chapter-relative progress if subtracks are active */
            if (id3->cuesheet && id3->cuesheet->track_count > 1)
            {
                struct cuesheet *cue = id3->cuesheet;
                int idx = cue_find_current_track(cue,
                              pos_ms > 0 ? (unsigned long)pos_ms : 0);
                if (idx >= 0 && idx < cue->track_count)
                {
                    long ch_start = (long)cue->tracks[idx].offset;
                    long ch_end = (idx + 1 < cue->track_count)
                                ? (long)cue->tracks[idx + 1].offset
                                : len_ms;
                    if (ch_end > ch_start)
                    {
                        pos_ms -= ch_start;
                        len_ms = ch_end - ch_start;
                    }
                }
            }

            if (pos_ms < 0) pos_ms = 0;
            if (pos_ms > len_ms) pos_ms = len_ms;
        }

        float pratio = (len_ms > 0) ? ((float)pos_ms / (float)len_ms) : 0.0f;
        draw_seekbar_pill(fb, pratio, fg, bg);

        long el_sec  = pos_ms / 1000;
        long tot_sec = len_ms / 1000;
        long rem_sec = tot_sec - el_sec;
        if (rem_sec < 0) rem_sec = 0;

        char el_str[24], rem_str[24];
        format_mmss(el_sec, false, el_str, sizeof(el_str));
        format_mmss(rem_sec, true, rem_str, sizeof(rem_str));

        s->setfont(fid_body);
        s->set_drawmode(DRMODE_FG);
        s->putsxy(NP_X_LEFT, 642, (const unsigned char *)el_str);

        int rw = 0, rh = 0;
        s->getstringsize((const unsigned char *)rem_str, &rw, &rh);
        s->putsxy(NP_X_RIGHT - rw + 1, 642, (const unsigned char *)rem_str);
    }

    s->setfont(FONT_UI);
    s->set_drawmode(DRMODE_SOLID);
    s->update_viewport();
    s->set_viewport(NULL);
}
