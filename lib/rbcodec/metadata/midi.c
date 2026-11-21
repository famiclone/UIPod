/* UIPod: Standard MIDI (`.mid`, `.midi`, `.kar`, `.rmi`) metadata parser via `uipod-codecs`. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "platform.h"
#include "metadata.h"
#include "metadata_common.h"
#include "metadata_parsers.h"
#include "string-extra.h"
#include "uipod_rs_codecs.h"

#define MIDI_META_MAX_BYTES (4 * 1024 * 1024)

static char *pack_midi_field(char **cursor, const char *end, const char *src)
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

bool get_midi_metadata(int fd, struct mp3entry *id3)
{
    off_t fsz = filesize(fd);
    if (fsz < 14 || fsz > MIDI_META_MAX_BYTES)
        return false;

    uint8_t probe[12];
    if (lseek(fd, 0, SEEK_SET) < 0 || read(fd, probe, 12) < 12)
        return false;

    /* Fast header sanity check before allocating heap buffer */
    bool magic_ok = (memcmp(probe, "MThd", 4) == 0) ||
                    (memcmp(probe, "RIFF", 4) == 0 && memcmp(probe + 8, "RMID", 4) == 0) ||
                    (memcmp(probe, "ID3", 3) == 0);
    if (!magic_ok)
        return false;

    size_t len = (size_t)fsz;
    uint8_t *buf = malloc(len);
    if (!buf)
        return false;

    if (lseek(fd, 0, SEEK_SET) < 0 || read(fd, buf, len) != (ssize_t)len)
    {
        free(buf);
        return false;
    }

    struct uipod_rs_track_meta *meta = calloc(1, sizeof(*meta));
    if (!meta)
    {
        free(buf);
        return false;
    }

    int ok = uipod_rs_midi_parse_metadata(buf, len, id3->path, meta);
    free(buf);
    if (!ok)
    {
        free(meta);
        return false;
    }

    memset(id3->id3v2buf, 0, ID3V2_BUF_SIZE);
    char *cur = id3->id3v2buf;
    const char *end = id3->id3v2buf + ID3V2_BUF_SIZE;

    id3->title        = pack_midi_field(&cur, end, meta->title);
    id3->artist       = pack_midi_field(&cur, end, meta->artist);
    id3->album        = pack_midi_field(&cur, end, meta->album);
    id3->genre_string = pack_midi_field(&cur, end, meta->genre);
    id3->composer     = pack_midi_field(&cur, end, meta->composer);
    id3->comment      = pack_midi_field(&cur, end, meta->comment);

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
