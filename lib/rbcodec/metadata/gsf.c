/* UIPod: GSF / MiniGSF (`.gsf`, `.minigsf`) metadata parser via `uipod-codecs`. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "platform.h"
#include "metadata.h"
#include "metadata_common.h"
#include "metadata_parsers.h"
#include "string-extra.h"
#include "uipod_rs_codecs.h"

static char *pack_meta_field(char **cursor, const char *end, const char *src)
{
    if (!src || !src[0])
        return NULL;
    size_t avail = (size_t)(end - *cursor);
    if (avail < 2)
        return NULL;
    char *dst = *cursor;
    size_t copied = strlcpy(dst, src, avail);
    if (copied >= avail)
        copied = avail - 1;
    *cursor += copied + 1;
    return dst;
}

bool get_gsf_metadata(int fd, struct mp3entry *id3)
{
    uint8_t hdr[16];
    if (lseek(fd, 0, SEEK_SET) < 0 || read(fd, hdr, sizeof(hdr)) != (ssize_t)sizeof(hdr))
        return false;

    /* GSF magic: "PSF" + version 0x22 */
    if (hdr[0] != 'P' || hdr[1] != 'S' || hdr[2] != 'F' || hdr[3] != 0x22)
        return false;

    uint32_t res_sz = (uint32_t)hdr[4] | ((uint32_t)hdr[5] << 8) |
                      ((uint32_t)hdr[6] << 16) | ((uint32_t)hdr[7] << 24);
    uint32_t cmp_sz = (uint32_t)hdr[8] | ((uint32_t)hdr[9] << 8) |
                      ((uint32_t)hdr[10] << 16) | ((uint32_t)hdr[11] << 24);

    off_t fsz = filesize(fd);
    uint64_t tag_off_u64 = 16ULL + (uint64_t)res_sz + (uint64_t)cmp_sz;
    if (fsz < 16 || tag_off_u64 > (uint64_t)fsz)
        return false;

    /* Heap-allocate temporary buffers to keep buffering_thread coroutine stack usage minimal */
    uint8_t *tag_buf = NULL;
    size_t tag_len = 0;
    if (tag_off_u64 + 5 <= (uint64_t)fsz)
    {
        size_t rem = (size_t)((uint64_t)fsz - tag_off_u64);
        if (rem > 4096)
            rem = 4096;
        tag_buf = malloc(rem);
        if (tag_buf && lseek(fd, (off_t)tag_off_u64, SEEK_SET) >= 0)
        {
            ssize_t n = read(fd, tag_buf, rem);
            if (n > 0)
                tag_len = (size_t)n;
        }
    }

    struct uipod_rs_track_meta *meta = calloc(1, sizeof(*meta));
    if (!meta)
    {
        free(tag_buf);
        return false;
    }

    int ok = uipod_rs_gsf_parse_metadata(hdr,
                                         tag_len > 0 ? tag_buf : NULL,
                                         tag_len,
                                         id3->path,
                                         meta);
    free(tag_buf);
    if (!ok)
    {
        free(meta);
        return false;
    }

    memset(id3->id3v2buf, 0, ID3V2_BUF_SIZE);
    char *cur = id3->id3v2buf;
    const char *end = id3->id3v2buf + ID3V2_BUF_SIZE;

    id3->title        = pack_meta_field(&cur, end, meta->title);
    id3->artist       = pack_meta_field(&cur, end, meta->artist);
    id3->album        = pack_meta_field(&cur, end, meta->album);
    id3->genre_string = pack_meta_field(&cur, end, meta->genre);
    id3->composer     = pack_meta_field(&cur, end, meta->composer);
    id3->comment      = pack_meta_field(&cur, end, meta->comment);

    id3->year       = meta->year;
    id3->tracknum   = meta->track_num;
    id3->length     = meta->duration_ms;
    id3->frequency  = meta->sample_rate;
    id3->bitrate    = meta->bitrate_kbps;
    id3->filesize   = fsz;
    id3->vbr        = false;

    free(meta);
    return true;
}
