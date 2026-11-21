/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * Copyright (C) 2002 Björn Stenberg
 *
 * This program is free software; you can redistribute it and/or
 * modify it under the terms of the GNU General Public License
 * as published by the Free Software Foundation; either version 2
 * of the License, or (at your option) any later version.
 *
 * This software is distributed on an "AS IS" basis, WITHOUT WARRANTY OF ANY
 * KIND, either express or implied.
 *
 ****************************************************************************/
#include "config.h"

#include <stdbool.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
#include <timefuncs.h>
#include <ctype.h>
#include <stdarg.h>
#include "string-extra.h"
#include "load_code.h"
#include "debug.h"
#include "button.h"
#include "dir.h"
#include "file.h"
#include "kernel.h"
#include "screens.h"
#include "misc.h"
#include "codecs.h"
#include "lang.h"
#include "buffering.h"
#include "backlight.h"
#include "storage.h"
#include "mp3data.h"
#include "powermgmt.h"
#include "system.h"
#include "sound.h"
#include "splash.h"
#include "general.h"
#include "rbpaths.h"
#include "panic.h"
#include "pcmbuf.h"
#include "dsp_proc_entry.h"
#include "uipod_rs_codecs.h"

#define LOGF_ENABLE
#include "logf.h"

#define PREFIX(_x_) sim_ ## _x_

/* For PLATFORM_HOSTED this buffer must be define here. */
static unsigned char codecbuf[CODEC_SIZE];

static size_t codec_size;

struct codec_api ci = {

    0,    /* filesize */
    0,    /* curpos */
    NULL, /* id3 */
    ERR_HANDLE_NOT_FOUND, /* audio_hid */
    NULL, /* struct dsp_config *dsp */
    NULL, /* codec_get_buffer */
    NULL, /* pcmbuf_insert */
    NULL, /* set_elapsed */
    NULL, /* read_filebuf */
    NULL, /* request_buffer */
    NULL, /* advance_buffer */
    NULL, /* seek_buffer */
    NULL, /* seek_complete */
    NULL, /* set_offset */
    NULL, /* configure */
    NULL, /* get_command */
    NULL, /* loop_track */
    NULL, /* strip_filesize */

    /* kernel/ system */
#if defined(ARM_NEED_DIV0)
    __div0,
#endif
    sleep,
    yield,

#if NUM_CORES > 1
    create_thread,
    thread_thaw,
    thread_wait,
    semaphore_init,
    semaphore_wait,
    semaphore_release,
#endif

    commit_dcache,
    commit_discard_dcache,
    commit_discard_idcache,

    /* strings and memory */
    strcpy,
    strlen,
    strcmp,
    strcat,
    memset,
    memcpy,
    memmove,
    memcmp,
    memchr,
#ifdef ROCKBOX_HAS_LOGF
    logf,
#endif

    (void *)qsort,



    /* new stuff at the end, sort into place next time
       the API gets incompatible */
    panicf,

};

void codec_get_full_path(char *path, const char *codec_root_fn)
{
    snprintf(path, MAX_PATH-1, CODECS_DIR "/" CODEC_PREFIX "%s."
            CODEC_EXTENSION, codec_root_fn);
}

/* Returns pointer to and size of free codec RAM. Aligns to MEM_ALIGN_SIZE. */
void *codec_get_buffer_callback(size_t *size)
{
    void *buf = &codecbuf[codec_size];
    ssize_t s = CODEC_SIZE - codec_size;

    if (s <= 0)
        return NULL;

    *size = s;
    ALIGN_BUFFER(buf, *size, MEM_ALIGN_SIZE);

    return buf;
}

/** codec loading and call interface **/
static void *curr_handle = NULL;
static struct codec_header *c_hdr = NULL;
static char curr_rs_codec[32] = "";
static struct codec_api *curr_rs_api = NULL;

#define RS_PCM_FRAMES 1024
static int16_t rs_pcm_buf[RS_PCM_FRAMES * 2];

static int rust_codec_run(struct codec_api *api)
{
    if (!api || !api->id3 || !curr_rs_codec[0])
        return CODEC_ERROR;

    struct dsp_replay_gains gains =
    {
        .track_gain = api->id3->track_gain,
        .album_gain = api->id3->album_gain,
        .track_peak = api->id3->track_peak,
        .album_peak = api->id3->album_peak,
    };
    api->configure(REPLAYGAIN_SET_GAINS, (intptr_t)&gains);

    size_t buf_n = 0;
    const uint8_t *buf = NULL;
    if (api->filesize > 0)
    {
        api->seek_buffer(0);
        buf = (const uint8_t *)api->request_buffer(&buf_n, (size_t)api->filesize);
        if (buf && buf_n < (size_t)api->filesize)
            buf = NULL;
    }

    if (uipod_rs_codec_open(curr_rs_codec,
                            api->id3->path,
                            buf,
                            buf ? (size_t)api->filesize : 0) < 0)
    {
        logf("Rust codec open failed: %s", api->id3->path);
        return CODEC_ERROR;
    }

    uint32_t sr = uipod_rs_codec_sample_rate();
    api->configure(DSP_SET_SAMPLE_DEPTH, 16);
    api->configure(DSP_SET_FREQUENCY, (intptr_t)(sr ? sr : 44100));
    api->configure(DSP_SET_STEREO_MODE, STEREO_INTERLEAVED);

    /* Keep pcmbuf lookahead tight (~250 ms) for interactive synth codecs (`nsf`, `midi`)
     * so live channel mute toggles, endless-loop toggles, SoundFont swaps, and 60 Hz
     * `ApuFrameTelemetry` respond in real time. */
    bool low_lat = (strcasecmp(curr_rs_codec, "nsf") == 0 ||
                    strcasecmp(curr_rs_codec, "midi") == 0);
    pcmbuf_set_low_latency(low_lat);

    if (api->id3->elapsed > 0)
    {
        uint32_t resume_ms = (uint32_t)api->id3->elapsed;
        while (uipod_rs_codec_seek_step_ms(resume_ms, 4000) == 0)
            api->yield();
        api->set_elapsed(api->id3->elapsed);
    }
    else
    {
        api->set_elapsed(0);
    }

    api->yield();

    while (1)
    {
        pcmbuf_set_low_latency(low_lat && uipod_rs_dj_get_phase() == 0);

        intptr_t param = 0;
        long action = api->get_command(&param);
        if (action == CODEC_ACTION_HALT)
            break;

        if (action == CODEC_ACTION_SEEK_TIME)
        {
            uint32_t target_ms = param > 0 ? (uint32_t)param : 0;
            while (uipod_rs_codec_seek_step_ms(target_ms, 4000) == 0)
                api->yield();
            api->set_elapsed(target_ms);
            api->seek_complete();
        }

        int frames = uipod_rs_codec_render_s16(rs_pcm_buf,
                                               RS_PCM_FRAMES,
                                               api->loop_track() ? 1 : 0);
        if (frames <= 0)
            break;

        api->pcmbuf_insert(rs_pcm_buf, NULL, frames);
        api->set_elapsed(uipod_rs_codec_elapsed_ms());
        api->yield();
    }

    pcmbuf_set_low_latency(false);
    uipod_rs_codec_close();
    return CODEC_OK;
}

static int codec_load_ram(struct codec_api *api)
{
    struct lc_header *hdr;

    c_hdr = lc_get_header(curr_handle);
    hdr   = c_hdr ? &c_hdr->lc_hdr : NULL;

    if (hdr == NULL
        || (hdr->magic != CODEC_MAGIC
            )
        || hdr->target_id != TARGET_ID
        )
    {
        logf("codec header error");
        lc_close(curr_handle);
        curr_handle = NULL;
        return CODEC_ERROR;
    }

    if (hdr->api_version != CODEC_API_VERSION ||
        c_hdr->api_size > sizeof(struct codec_api))
    {
        logf("codec api version error");
        lc_close(curr_handle);
        curr_handle = NULL;
        return CODEC_ERROR;
    }

    codec_size = 0;

    *(c_hdr->api) = api;

    logf("Codec: calling entrypoint");
    return c_hdr->entry_point(CODEC_LOAD);
}


int codec_load_file(const char *plugin, struct codec_api *api)
{
    char path[MAX_PATH];

    if (plugin && uipod_rs_codec_is_builtin(plugin))
    {
        strlcpy(curr_rs_codec, plugin, sizeof(curr_rs_codec));
        curr_rs_api = api;
        api->configure(DSP_SET_SAMPLE_DEPTH, 16);
        api->configure(DSP_SET_FREQUENCY, 44100);
        api->configure(DSP_SET_STEREO_MODE, STEREO_INTERLEAVED);
        return CODEC_OK;
    }

    curr_rs_codec[0] = '\0';
    curr_rs_api = NULL;
    pcmbuf_set_low_latency(false);
    uipod_rs_nsf_reset_session();

    codec_get_full_path(path, plugin);

    curr_handle = lc_open(path, codecbuf, CODEC_SIZE);

    if (curr_handle == NULL) {
        logf("Codec: cannot read file");
        return CODEC_ERROR;
    }

    return codec_load_ram(api);
}

int codec_run_proc(void)
{
    if (curr_rs_codec[0] != '\0')
        return rust_codec_run(curr_rs_api);

    if (curr_handle == NULL) {
        logf("Codec: no codec to run");
        return CODEC_ERROR;
    }

    logf("Codec: entering run state");
    return c_hdr->run_proc();
}

int codec_close(void)
{
    int status = CODEC_OK;

    if (curr_rs_codec[0] != '\0')
    {
        pcmbuf_set_low_latency(false);
        uipod_rs_codec_close();
        curr_rs_codec[0] = '\0';
        curr_rs_api = NULL;
        return CODEC_OK;
    }

    if (curr_handle != NULL) {
        logf("Codec: cleaning up");
        status = c_hdr->entry_point(CODEC_UNLOAD);
        lc_close(curr_handle);
        curr_handle = NULL;
    }

    return status;
}

