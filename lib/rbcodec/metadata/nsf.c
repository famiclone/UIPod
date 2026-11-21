/* UIPod: NSF / NSFe (`.nsf`, `.nsfe`, `file.nsf#<idx>`) metadata parser via `uipod-nsf`. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "platform.h"
#include "metadata.h"
#include "metadata_common.h"
#include "metadata_parsers.h"
#include "string-extra.h"
#include "uipod_rs_codecs.h"

#define NSF_MAX_FILE_BYTES (8 * 1024 * 1024)

static char *pack_nsf_meta_field(char **cursor, const char *end, const char *src)
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

bool get_nsf_metadata(int fd, struct mp3entry *id3)
{
    off_t fsz = filesize(fd);
    if (fsz < 8 || fsz > NSF_MAX_FILE_BYTES)
        return false;

    uint8_t hdr[128];
    ssize_t hdr_read = 0;
    if (lseek(fd, 0, SEEK_SET) < 0)
        return false;
    while (hdr_read < (ssize_t)sizeof(hdr) && hdr_read < (ssize_t)fsz)
    {
        ssize_t n = read(fd, hdr + hdr_read,
                         (size_t)(((off_t)sizeof(hdr) < fsz ? (off_t)sizeof(hdr) : fsz) - hdr_read));
        if (n <= 0)
            return false;
        hdr_read += n;
    }
    if (hdr_read < 8)
        return false;

    bool is_nesm = (memcmp(hdr, "NESM\x1A", 5) == 0);
    bool is_nsfe = (memcmp(hdr, "NSFE", 4) == 0);
    if (!is_nesm && !is_nsfe)
        return false;

    struct uipod_rs_track_meta *meta = calloc(1, sizeof(*meta));
    if (!meta)
        return false;

    int ok = 0;
    bool nesm_header_only = is_nesm && hdr_read >= 128 &&
                            (hdr[0x05] < 2 ||
                             ((hdr[0x7D] | hdr[0x7E] | hdr[0x7F]) == 0));

    if (nesm_header_only)
    {
        ok = uipod_rs_nsf_parse_metadata(hdr, 128, id3->path, meta);
    }
    else
    {
        uint8_t *buf = malloc((size_t)fsz);
        if (!buf)
        {
            free(meta);
            return false;
        }
        memcpy(buf, hdr, (size_t)hdr_read);
        size_t got = (size_t)hdr_read;
        while (got < (size_t)fsz)
        {
            ssize_t n = read(fd, buf + got, (size_t)fsz - got);
            if (n <= 0)
            {
                free(buf);
                free(meta);
                return false;
            }
            got += (size_t)n;
        }
        ok = uipod_rs_nsf_parse_metadata(buf, (size_t)fsz, id3->path, meta);
        free(buf);
    }

    if (!ok)
    {
        free(meta);
        return false;
    }

    memset(id3->id3v2buf, 0, ID3V2_BUF_SIZE);
    char *cur = id3->id3v2buf;
    const char *end = id3->id3v2buf + ID3V2_BUF_SIZE;

    id3->title        = pack_nsf_meta_field(&cur, end, meta->title);
    id3->artist       = pack_nsf_meta_field(&cur, end, meta->artist);
    id3->album        = pack_nsf_meta_field(&cur, end, meta->album);
    id3->genre_string = pack_nsf_meta_field(&cur, end, meta->genre);
    id3->composer     = pack_nsf_meta_field(&cur, end, meta->composer);
    id3->comment      = pack_nsf_meta_field(&cur, end, meta->comment);

    id3->year      = meta->year;
    id3->tracknum  = meta->track_num;
    id3->length    = meta->duration_ms;
    id3->frequency = meta->sample_rate;
    id3->bitrate   = meta->bitrate_kbps;
    id3->filesize  = fsz;
    id3->vbr       = false;

    free(meta);
    return true;
}
