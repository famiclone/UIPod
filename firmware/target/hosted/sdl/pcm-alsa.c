/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * Copyright (C) 2005 by Nick Lanham
 * Copyright (C) 2010 by Thomas Martitz
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

/* Trimpod: PCM output via ALSA, ported from upstream Rockbox
 * firmware/target/hosted/pcm-alsa.c.  set_hwparams(), set_swparams(),
 * copy_frames() and the XRUN/prepare/start state machine are upstream's.
 *
 * Six deviations, each forced by this device.  Do not "restore" them:
 *
 * 1. THE DEVICE IS NAMED PER ROUTE, never the hardware and never `default`.
 *    Upstream uses plughw:0,0, which does not even open here.  Naming the codec
 *    directly is worse: its driver advertises RATE (0 4294967295], claiming
 *    every rate and honouring none -- fed 44100 it still clocks 48000, so a
 *    4.000 s file plays in 3.66 s, 8.8% fast, about 1.5 semitones sharp.
 *    Measured.  That is the pitch shift v1.0.4 shipped.
 *
 * 2. NO SIGIO.  Upstream is driven by snd_async_add_pcm_handler(), which does
 *    not work on BlueALSA's ioplug, so a writer thread paces on snd_pcm_wait()
 *    instead.  This is the only structural change.
 *
 * 3. NOTHING IS FATAL.  Upstream panicf()s when a device will not open, which
 *    ends in a while(1) only adb can clear.  Upstream has no Bluetooth, so it
 *    never contemplates a device that vanishes mid-song; here that is routine.
 *
 * 4. BLUETOOTH OUTPUT RUNS IN A CHILD PROCESS.  bluez-alsa's plugin crashes
 *    its host process when a speaker is powered off; see bt_pipe below.
 *
 * 5. LOSING A PRIVATE ROUTE PAUSES PLAYBACK -- Bluetooth or USB-C headphones --
 *    rather than following the route back to the internal speaker; see
 *    route_notify_lost().
 *
 * 6. PRIVATE ROUTES CLOSE WHEN IDLE.  USB and Bluetooth have no dmix bus to
 *    run on silence, so they close when the sink stops and reopen on play.
 *    The speaker stays open: closing the codec powers it down, and powering up
 *    buzzes. */

#include "autoconf.h"

#include <stdlib.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <math.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <unistd.h>
#include <sys/syscall.h>
#include <pthread.h>
#include <alsa/asoundlib.h>
#include <SDL.h>
#include "config.h"
#include "debug.h"
#include "sound.h"
#include "audiohw.h"
#include "system.h"
#include "system-sdl.h"
#include "kernel.h"
#include "button.h"
#include "panic.h"
#include "trimpod_alsa.h"

#include "pcm.h"
#include "pcm-internal.h"
#include "pcm_sampr.h"
#include "pcm_mixer.h"
#include "pcm_sink.h"
#include "tape_deck_fx.h"

/*#define LOGF_ENABLE*/
#include "logf.h"


extern const char *audiodev;        /* --audiodev; NULL = ALSA "default" */

static const snd_pcm_format_t format = SND_PCM_FORMAT_S16_LE;
static const unsigned int channels = 2;
#define FRAME_BYTES 4               /* S16_LE stereo */

/* snd_pcm_wait() timeout.  Bounds how long a reopen or shutdown waits behind
 * the writer, and how quickly a dead route is noticed. */
#define WAIT_TIMEOUT_MS 50
#define REOPEN_DELAY_MS 500
/* Re-reading NextUI's settings block is a syscall round trip; keep it off the
 * ~23 ms write path. */
#define ROUTE_POLL_MS   500

/* Depth of the Bluetooth child's ALSA buffer, in periods of MIX_FRAME_SAMPLES
 * (~23 ms each at 44.1 kHz).  Eight is ~186 ms: comparable to the ~170 ms dmix
 * imposes on the speaker route, which plays without underruns, and short enough
 * to keep the visualiser tap near the sound. */
#define BT_BUFFER_PERIODS 8
/* The pipe to it holds this many periods. */
#define BT_PIPE_PERIODS   2

/* Linux-specific and only declared under _GNU_SOURCE, which this build does not
 * define.  The value is fixed (F_LINUX_SPECIFIC_BASE + 7) on every arch. */
#ifndef F_SETPIPE_SZ
#define F_SETPIPE_SZ 1031
#endif

/* handle, and everything derived from it, belongs to the writer thread alone.
 * Nothing else opens, closes or touches it -- see sink_set_freq(). */
static snd_pcm_t *handle;
static snd_pcm_uframes_t buffer_size;
static snd_pcm_uframes_t period_size;
static unsigned int real_sample_rate;
static unsigned int last_sample_rate;
static volatile unsigned int req_sample_rate;
static int16_t *frames;

/* Bluetooth output runs in a CHILD PROCESS.  Never open a BlueALSA PCM here.
 *
 * bluez-alsa's plugin starts its own "pcm-io" thread inside whatever process
 * opens it.  When the speaker is switched off (an abrupt link loss, not a clean
 * disconnect) that thread calls through a function pointer that is no longer
 * valid and dies, taking the process with it.  The kernel names it:
 *
 *     CPU: 3 PID: 21686 Comm: pcm-io
 *     pc : [<00000000052c8770>] lr : [<000000000044f7d0>]
 *
 * pc there is Rockbox's data buffer, i.e. it branched into non-executable
 * memory.  We do not own that thread and never call into it, and BOTH leaving
 * the dead PCM open AND closing it were tried -- it dies either way.
 *
 * Piping the samples to aplay puts the plugin in a child.  When the speaker
 * dies the child dies, our write returns EPIPE, and we reopen on whatever the
 * route became.  Measured: 4.22 s for a 4.000 s file, i.e. aplay paces itself
 * against the sink and the pipe back-pressures us, so no extra clock is needed
 * and the pitch is unaffected. */
static FILE *bt_pipe;
static bool route_lost_sent;        /* pause posted for this route already */
static int  open_route;             /* the sink dev_open() opened for */

static const void *pcm_data;
static size_t pcm_size;

/* Upstream's recursive pcm_mtx, and for the same reason: the pcm layer calls
 * ops.stop() from inside pcm_play_dma_complete_callback(), i.e. re-entrantly on
 * whichever thread is pulling data. */
static pthread_mutex_t pcm_mtx;

#define DIRECT_HW_RATE      48000U
#define DIRECT_HW_PERIOD    512U
#define DIRECT_HW_PERIODS   4U
#define DIRECT_BUSY_RETRIES 4

static SDL_Thread *writer_thread;
static SDL_sem *writer_wake;        /* posted on play and shutdown */
static volatile bool writer_run;
static volatile bool dev_playing;   /* Rockbox has handed us a buffer */
static bool open_shared;            /* the device is the "Playback" dmix bus */
static bool open_direct_hw;         /* direct "hw:0,0" 48 kHz exclusive mode */
static volatile bool s_want_direct_hw = true;
static volatile bool s_force_shared_dmix = false;
static bool s_direct_hw_fallback;
static int  s_direct_busy_retries;
static int cur_route;               /* sink audiomon last published */
static volatile bool private_open;      /* a USB or Bluetooth device is open */
static volatile unsigned int out_delay_us;  /* written to the output, not yet heard */
static volatile Uint32 out_delay_tick;      /* SDL_GetTicks() when out_delay_us was stamped */

static pthread_mutex_t click_pcm_mtx = PTHREAD_MUTEX_INITIALIZER;
static snd_pcm_t *s_click_pcm;
static volatile int s_direct_click_pos = 96;

static void viz_pcm_push(const void *src_v, unsigned frames_n);
static void direct_rs_reset(void);

static void close_click_dmix_sync(void)
{
    pthread_mutex_lock(&click_pcm_mtx);
    if (s_click_pcm)
    {
        trimpod_alsa_lock();
        snd_pcm_close(s_click_pcm);
        trimpod_alsa_unlock();
        s_click_pcm = NULL;
    }
    pthread_mutex_unlock(&click_pcm_mtx);
}

static bool alsa_recover_xrun_or_suspend(snd_pcm_t *pcm, int err)
{
    if (err == -ESTRPIPE)
    {
        int r = -EAGAIN;
        for (int tries = 0; tries < 25; tries++)
        {
            r = snd_pcm_resume(pcm);
            if (r != -EAGAIN)
                break;
            SDL_Delay(10);
        }
        if (r < 0)
            r = snd_pcm_prepare(pcm);
        return r >= 0;
    }
    if (err == -EPIPE || err == -EINTR)
        return snd_pcm_recover(pcm, err, 1) >= 0;
    return false;
}

/* Bluetooth uses bt_pipe and leaves `handle` NULL, so every "do we have an
 * output?" test must ask this, not `handle`.  Getting that wrong meant the
 * writer never reached writer_step() on Bluetooth -- no audio at all -- while
 * re-running dev_open() every 20 ms and spawning an aplay child each time. */
static bool dev_is_open(void)
{
    return handle != NULL || bt_pipe != NULL;
}

static void sink_lock(void)
{
    pthread_mutex_lock(&pcm_mtx);
}

static void sink_unlock(void)
{
    pthread_mutex_unlock(&pcm_mtx);
}

/* --- device parameters (upstream) ----------------------------------------- */

static int set_hwparams(snd_pcm_t *pcm, unsigned long sampr)
{
    snd_pcm_hw_params_t *params;
    unsigned int srate;
    int err;

    snd_pcm_hw_params_malloc(&params);

    if (sampr > SAMPR_96) {
        buffer_size = MIX_FRAME_SAMPLES * 4 * 4;
        period_size = MIX_FRAME_SAMPLES * 4;
    } else if (sampr > SAMPR_48) {
        buffer_size = MIX_FRAME_SAMPLES * 2 * 4;
        period_size = MIX_FRAME_SAMPLES * 2;
    } else {
        /* >= 85 ms buffer headroom on shared `Playback` (`dmix`) so `minarch.elf`
         * ROM loading / SD card I/O bursts never cause background audio XRUNs. */
        period_size = (MIX_FRAME_SAMPLES >= 1024) ? MIX_FRAME_SAMPLES : 1024;
        buffer_size = period_size * 4;
    }

    err = snd_pcm_hw_params_any(pcm, params);
    if (err < 0)
    {
        logf("Broken configuration for playback: %s", snd_strerror(err));
        goto error;
    }
    err = snd_pcm_hw_params_set_access(pcm, params,
                                       SND_PCM_ACCESS_RW_INTERLEAVED);
    if (err < 0)
    {
        logf("Access type not available for playback: %s", snd_strerror(err));
        goto error;
    }
    err = snd_pcm_hw_params_set_format(pcm, params, format);
    if (err < 0)
    {
        logf("Sample format not available for playback: %s", snd_strerror(err));
        goto error;
    }
    err = snd_pcm_hw_params_set_channels(pcm, params, channels);
    if (err < 0)
    {
        logf("Channel count not available for playback: %s", snd_strerror(err));
        goto error;
    }
    srate = sampr;
    err = snd_pcm_hw_params_set_rate_near(pcm, params, &srate, 0);
    if (err < 0)
    {
        logf("Rate %luHz not available for playback: %s", sampr, snd_strerror(err));
        goto error;
    }
    real_sample_rate = srate;
    /* Upstream's check, and it must stay: a substituted rate is exactly how
     * playback ends up pitched.  Through `default` it never trips. */
    if (real_sample_rate != sampr)
    {
        logf("Rate doesn't match (requested %luHz, got %dHz)", sampr,
             real_sample_rate);
        err = -EINVAL;
        goto error;
    }

    err = snd_pcm_hw_params_set_buffer_size_near(pcm, params, &buffer_size);
    if (err < 0)
    {
        logf("Unable to set buffer size %ld: %s", buffer_size, snd_strerror(err));
        goto error;
    }
    err = snd_pcm_hw_params_set_period_size_near(pcm, params, &period_size, NULL);
    if (err < 0)
    {
        logf("Unable to set period size %ld: %s", period_size, snd_strerror(err));
        goto error;
    }

    if (frames) free(frames);
    frames = calloc(1, period_size * channels * sizeof(int16_t));

    /* Acquiring a Bluetooth transport happens here and takes ~1.8 s. */
    err = snd_pcm_hw_params(pcm, params);
    if (err < 0)
    {
        logf("Unable to set hw params: %s", snd_strerror(err));
        goto error;
    }

    err = 0;
error:
    snd_pcm_hw_params_free(params);
    return err;
}

/* Direct "hw:0,0" setup at the A133P audiocodec's true 48 kHz hardware clock */
static int set_hwparams_direct(snd_pcm_t *pcm)
{
    snd_pcm_hw_params_t *params;
    unsigned int srate = DIRECT_HW_RATE;
    int err;

    snd_pcm_hw_params_malloc(&params);
    period_size = DIRECT_HW_PERIOD;

    if ((err = snd_pcm_hw_params_any(pcm, params)) < 0 ||
        (err = snd_pcm_hw_params_set_access(pcm, params,
                                            SND_PCM_ACCESS_RW_INTERLEAVED)) < 0 ||
        (err = snd_pcm_hw_params_set_format(pcm, params, format)) < 0 ||
        (err = snd_pcm_hw_params_set_channels(pcm, params, channels)) < 0 ||
        (err = snd_pcm_hw_params_set_rate_near(pcm, params, &srate, 0)) < 0)
        goto error;

    if (srate != DIRECT_HW_RATE)
    {
        err = -EINVAL;
        goto error;
    }
    real_sample_rate = srate;

    if ((err = snd_pcm_hw_params_set_period_size_near(pcm, params,
                                                      &period_size, NULL)) < 0)
        goto error;

    buffer_size = period_size * DIRECT_HW_PERIODS;
    if ((err = snd_pcm_hw_params_set_buffer_size_near(pcm, params,
                                                      &buffer_size)) < 0)
        goto error;

    if (frames)
        free(frames);
    frames = calloc(1, period_size * channels * sizeof(int16_t));
    if (!frames)
    {
        err = -ENOMEM;
        goto error;
    }

    err = snd_pcm_hw_params(pcm, params);
error:
    snd_pcm_hw_params_free(params);
    return err;
}

static int set_swparams(snd_pcm_t *pcm)
{
    snd_pcm_sw_params_t *swparams;
    int err;

    snd_pcm_sw_params_malloc(&swparams);

    err = snd_pcm_sw_params_current(pcm, swparams);
    if (err < 0)
    {
        logf("Unable to determine current swparams: %s", snd_strerror(err));
        goto error;
    }
    err = snd_pcm_sw_params_set_start_threshold(pcm, swparams, buffer_size / 2);
    if (err < 0)
    {
        logf("Unable to set start threshold: %s", snd_strerror(err));
        goto error;
    }
    err = snd_pcm_sw_params_set_avail_min(pcm, swparams, period_size);
    if (err < 0)
    {
        logf("Unable to set avail min: %s", snd_strerror(err));
        goto error;
    }
    err = snd_pcm_sw_params(pcm, swparams);
    if (err < 0)
    {
        logf("Unable to set sw params: %s", snd_strerror(err));
        goto error;
    }

    err = 0;
error:
    snd_pcm_sw_params_free(swparams);
    return err;
}

/* --- device lifecycle ------------------------------------------------------ */

/* Name the device for the current route instead of opening `default`.
 *
 * `default` is a trap here.  audiomon rewrites and deletes ~/.asoundrc as
 * routes change, alsa-lib caches the tree, and the ONLY call that drops that
 * cache -- snd_config_update_free_global() -- unloads the BlueALSA plugin .so
 * out from under the ioplug we are still holding.  That, closing a dead
 * BlueALSA PCM, and closing a dead BlueALSA mixer all segfault; all three were
 * caught in trimpod.log switching the speaker off.
 *
 * All three routes have stable names that need no cache invalidation:
 *   speaker   "Playback" -- /etc/asound.conf, which audiomon never touches.
 *              Measured identical to `default` (4.21 s vs 4.22 s for a 4.000 s
 *              file), so plug -> softvol -> dmix, and no pitch shift.
 *   usb       the card itself, found the way NextUI finds it; see usb_device().
 *   bluetooth the address audiomon just wrote into ~/.asoundrc, read as plain
 *              text rather than through alsa-lib's cached view of it. */
static bool bt_address(char *mac, size_t len)
{
    const char *home = getenv("HOME");
    char line[256];
    FILE *f;

    if (!home)
        return false;

    snprintf(line, sizeof line, "%s/.asoundrc", home);
    f = fopen(line, "r");
    if (!f)
        return false;                   /* route says BT, audiomon disagrees */

    while (fgets(line, sizeof line, f))
    {
        if (sscanf(line, " defaults.bluealsa.device \"%31[^\"]\"", mac) == 1 &&
            strlen(mac) < len && !strpbrk(mac, "'\\\\"))
        {
            fclose(f);
            return true;
        }
    }

    fclose(f);
    return false;
}

/* The Bluetooth device is spelled "bluealsa:DEV=<mac>,PROFILE=a2dp".
 * /usr/share/alsa/alsa.conf.d/20-bluealsa.conf defines pcm.bluealsa as `type
 * plug` over `type bluealsa`, so this is the identical device NextUI's minarch
 * reaches via AUDIODEV=bluealsa. */
static bool route_is_bt(void)
{
    char mac[32];

    return !audiodev && trimpod_audio_sink() == TRIMPOD_SINK_BT &&
           bt_address(mac, sizeof mac);
}

/* USB-C headphones are a USB Audio Class card, so this route is a plain ALSA
 * device -- but through `plug`.  A USB card advertises only the rates it really
 * has (these earphones: 44.1/48/96k) and SUBSTITUTES one rather than failing,
 * which set_hwparams() rejects, so a bare `hw:` route would never open. */
static bool usb_device(char *buf, size_t len)
{
    int card;

    if (trimpod_audio_sink() != TRIMPOD_SINK_USB)
        return false;

    card = trimpod_usb_card();
    if (card < 0)
        return false;               /* route says USB, no card to name */

    snprintf(buf, len, "plughw:%d,0", card);
    return true;
}

/* Start the child that owns the BlueALSA plugin. */
static bool bt_open(void)
{
    char mac[32], cmd[256];

    if (!bt_address(mac, sizeof mac))
        return false;

    period_size = MIX_FRAME_SAMPLES;
    buffer_size = period_size * BT_BUFFER_PERIODS;
    if (frames) free(frames);
    frames = calloc(1, period_size * channels * sizeof(int16_t));
    if (!frames)
        return false;

    /* Bound the child's buffering.  Left to itself aplay takes a 500 ms buffer
     * and reads stdin in 125 ms chunks, so it released us one 125 ms chunk at a
     * time -- and the visualiser ring is filled in copy_frames(), at the moment
     * we hand bytes to the pipe.  The spectrum therefore advanced in jumps and
     * ran ahead of the sound by the whole depth of the pipe plus that buffer.
     * One period per read paces the tap like the ALSA route's snd_pcm_wait().
     *
     * exec so pclose() waits on aplay itself, not an intervening shell. */
    snprintf(cmd, sizeof cmd,
             "exec aplay -q -t raw -f S16_LE -c 2 -r %u "
             "--period-size=%lu --buffer-size=%lu "
             "-D 'bluealsa:DEV=%s,PROFILE=a2dp' -",
             last_sample_rate, (unsigned long)period_size,
             (unsigned long)buffer_size, mac);

    bt_pipe = popen(cmd, "w");
    if (!bt_pipe)
    {
        fprintf(stderr, "pcm: could not start bluetooth helper\n");
        return false;
    }

    setvbuf(bt_pipe, NULL, _IONBF, 0);   /* never hold a period in stdio */

    /* A pipe holds 64 KB by default -- another 372 ms of audio between the
     * visualiser tap and the child.  Two periods is all aplay needs to stay
     * fed, and the write side waits on poll() so the small buffer costs
     * nothing.  Best effort: an old kernel that refuses just keeps 64 KB. */
    fcntl(fileno(bt_pipe), F_SETPIPE_SZ, (int)(BT_PIPE_PERIODS * period_size * FRAME_BYTES));

    route_lost_sent = false;
    open_route = TRIMPOD_SINK_BT;
    private_open = true;
    /* aplay's buffer and the pipe; the speaker's own latency is unknown. */
    out_delay_us = (unsigned int)((buffer_size + BT_PIPE_PERIODS * period_size) * 1000000ull /
                                  last_sample_rate);
    out_delay_tick = SDL_GetTicks();
    real_sample_rate = last_sample_rate;
    fprintf(stderr, "pcm: bluetooth via child aplay (%s) at %u Hz\n",
            mac, last_sample_rate);
    return true;
}

static bool dev_open(void)
{
    char usb[32];
    const char *name;
    int route = TRIMPOD_SINK_SPEAKER;
    int err;

    open_shared = false;
    open_direct_hw = false;
    if (route_is_bt())
        return bt_open();

    if (audiodev)
        name = audiodev;                /* --audiodev overrides every route */
    else if (usb_device(usb, sizeof usb))
    {
        name = usb;
        route = TRIMPOD_SINK_USB;
    }
    else if (s_want_direct_hw && !s_direct_hw_fallback && !s_force_shared_dmix)
    {
        /* Release UIPod's own click dmix handle first so hw:0,0 is never busy */
        close_click_dmix_sync();

        trimpod_alsa_lock();
        err = snd_pcm_open(&handle, "hw:0,0", SND_PCM_STREAM_PLAYBACK,
                           SND_PCM_NONBLOCK);
        if (err == -EBUSY && s_direct_busy_retries < DIRECT_BUSY_RETRIES)
        {
            s_direct_busy_retries++;
            handle = NULL;
            trimpod_alsa_unlock();
            return false;
        }
        if (err == 0)
        {
            if (set_hwparams_direct(handle) == 0 &&
                set_swparams(handle) == 0 &&
                snd_pcm_prepare(handle) == 0)
            {
                trimpod_alsa_unlock();
                s_direct_busy_retries = 0;
                open_route = TRIMPOD_SINK_SPEAKER;
                private_open = false;
                open_shared = false;
                open_direct_hw = true;
                route_lost_sent = false;
                direct_rs_reset();
                fprintf(stderr,
                        "pcm: opened direct hw:0,0 at %u Hz (src %u Hz, period %lu, buffer %lu)\n",
                        real_sample_rate, last_sample_rate,
                        (unsigned long)period_size, (unsigned long)buffer_size);
                return true;
            }
            snd_pcm_close(handle);
            handle = NULL;
        }
        trimpod_alsa_unlock();
        s_direct_hw_fallback = true;
        fprintf(stderr, "pcm: direct hw:0,0 unavailable (%s), falling back to Playback\n",
                snd_strerror(err));
        name = "Playback";
    }
    else
        name = "Playback";

    /* Serialises against the volume driver's mixer opens: both rebuild
     * alsa-lib's global config on first use. */
    trimpod_alsa_lock();

    err = snd_pcm_open(&handle, name, SND_PCM_STREAM_PLAYBACK,
                       SND_PCM_NONBLOCK);
    if (err < 0)
    {
        /* Upstream panicf()s here.  A route can legitimately be gone. */
        handle = NULL;
        trimpod_alsa_unlock();
        fprintf(stderr, "pcm: open %s failed (%s)\n", name, snd_strerror(err));
        return false;
    }

    if (set_hwparams(handle, last_sample_rate) < 0 ||
        set_swparams(handle) < 0 ||
        snd_pcm_prepare(handle) < 0)
    {
        snd_pcm_close(handle);          /* never opened a stream; safe to close */
        handle = NULL;
        trimpod_alsa_unlock();
        fprintf(stderr, "pcm: configuring %s failed\n", name);
        return false;
    }

    trimpod_alsa_unlock();
    open_route = route;
    private_open = route != TRIMPOD_SINK_SPEAKER;
    open_shared = !strcmp(name, "Playback");
    route_lost_sent = false;
    fprintf(stderr, "pcm: opened %s at %u Hz (period %lu, buffer %lu)\n",
            name, real_sample_rate, (unsigned long)period_size,
            (unsigned long)buffer_size);
    return true;
}

/* Orderly teardown: shutdown, a rate change, or a route change.
 *
 * A BlueALSA PCM is NEVER closed, even here.  A route change is usually the
 * news that the speaker just died, and the route poll can beat the write error
 * to it, so "we chose to close it" is no evidence the device still exists.
 * Only the codec, which cannot disappear, is closed properly. */
static void dev_close(void)
{
    snd_pcm_t *h = handle;

    if (bt_pipe)
    {
        FILE *pipe = bt_pipe;

        bt_pipe = NULL;
        pclose(pipe);                   /* EOF; aplay drains and exits */
        private_open = false;
        fprintf(stderr, "pcm: bluetooth helper closed\n");
        return;
    }

    if (!h)
        return;

    handle = NULL;
    trimpod_alsa_lock();
    snd_pcm_drop(h);                    /* not drain: never wait on the sink */
    snd_pcm_close(h);
    trimpod_alsa_unlock();
    private_open = false;
    open_shared = false;
    open_direct_hw = false;
    logf("pcm: closed");
}

/* The device reported that it is gone.  CLOSE it -- do not merely drop the
 * pointer.  bluez-alsa's plugin runs its own "pcm-io" thread inside this
 * process; leaving a dead PCM open leaves that thread alive, and it then jumps
 * to a garbage address and kills us:
 *
 *     CPU: 2 PID: 20922 Comm: pcm-io
 *     pc : [<0000000000000011>] lr : [<0000000000000011>]
 *
 * NextUI's minarch closes the device as soon as audiomon rewrites ~/.asoundrc
 * and survives a speaker power-off, so closing is the behaviour that works. */
/* A private route is gone -- a Bluetooth speaker switched off, USB-C headphones
 * pulled out.  PAUSE rather than letting the writer follow the route home:
 * audiomon repoints it at the internal speaker within a second, and moving a
 * private listen to the loudspeaker unasked is the worse failure -- pausing is
 * what VLC and phones do when an output disappears.
 *
 * Posted, not called.  audio_pause() is a blocking queue_send and must run on a
 * Rockbox thread; sim_enter_irq_handler() is how a foreign thread reaches the
 * kernel here, the same way button-sdl.c's SDL event thread posts keypresses.
 * apps/misc.c handles the event.  Once per route: without the flag a route that
 * keeps reopening and dying would post one every REOPEN_DELAY_MS. */
static void route_notify_lost(void)
{
    if (route_lost_sent)
        return;
    route_lost_sent = true;

    sim_enter_irq_handler();
    button_queue_post(SYS_PHONE_UNPLUGGED, 0);
    sim_exit_irq_handler();
}

static void dev_lost(const char *why)
{
    bool was_private = open_route != TRIMPOD_SINK_SPEAKER;

    fprintf(stderr, "pcm: device lost (%s), closing\n", why);

    /* Pause BEFORE closing: closing waits on the Bluetooth child, and a child
     * wedged against a dead speaker must not hold up the one thing the user
     * hears. */
    if (was_private)
        route_notify_lost();

    dev_close();
    trimpod_alsa_forget_sink();
}

/* --- pulling audio from Rockbox (upstream copy_frames) --------------------- */

#define TAPE_SCRATCH_FRAMES 1024
static int16_t tape_raw_in[TAPE_SCRATCH_FRAMES * 2];
static int16_t tape_scratch[TAPE_SCRATCH_FRAMES * 2];

static bool tape_pull_one_frame(int16_t *out_l, int16_t *out_r)
{
    while (dev_playing)
    {
        if (!pcm_size)
        {
            if (!pcm_play_dma_complete_callback(PCM_DMAST_OK, &pcm_data,
                                                &pcm_size))
                return false;
            pcm_size &= ~(size_t)(FRAME_BYTES - 1);
            if (!pcm_size)
                continue;
            pcm_play_dma_status_callback(PCM_DMAST_STARTED);
        }

        const int16_t *s = (const int16_t *)pcm_data;
        *out_l = s[0];
        *out_r = s[1];
        pcm_data += FRAME_BYTES;
        pcm_size -= FRAME_BYTES;
        return true;
    }
    return false;
}

/* Fill one period, padding with silence when Rockbox runs short.  The stream is
 * never stopped while open -- a restart clicks -- so a private route is fed
 * silence until it closes for idle (see writer_fn). */
static void mix_direct_click_into_period(int16_t *dst_stereo, int nframes);
extern int32_t  uipod_rs_manual_dj_is_active(void);
extern uint32_t uipod_rs_manual_dj_sample_rate(void);
extern size_t   uipod_rs_manual_dj_render_s16(int16_t *out_stereo_s16,
                                              size_t frames,
                                              uint32_t sample_rate);

static void pull_source_frames(int16_t *out_buf, ssize_t want_frames, unsigned int sr)
{
    ssize_t frames_left = want_frames;
    bool new_buffer = false;
    if (!sr)
        sr = 44100;

    if (uipod_rs_manual_dj_is_active())
    {
        ssize_t rem = want_frames;
        int16_t *out_cursor = out_buf;
        while (rem > 0)
        {
            ssize_t chunk = MIN(rem, (ssize_t)TAPE_SCRATCH_FRAMES);
            uipod_rs_manual_dj_render_s16(tape_raw_in, (size_t)chunk, (uint32_t)sr);
            const int16_t *src_blk = tape_raw_in;
            if (tape_deck_fx_is_enabled() && !tape_deck_fx_ff_active())
            {
                tape_deck_fx_process_steady_block(tape_raw_in, tape_scratch,
                                                  (int)chunk, sr);
                src_blk = tape_scratch;
            }
            viz_pcm_push(src_blk, (unsigned)chunk);
            sink_lock();
            pcm_copy_buffer(out_cursor, src_blk, (size_t)chunk * FRAME_BYTES);
            sink_unlock();
            out_cursor += chunk * 2;
            rem -= chunk;
        }
        return;
    }

    sink_lock();

    if (tape_deck_fx_is_enabled() &&
        !tape_deck_fx_is_stopped_idle() &&
        !tape_deck_fx_ff_active() &&
        dev_playing)
    {
        int seed = tape_deck_fx_need_seed_frames(sr);
        while (seed > 0)
        {
            int16_t sl = 0, sr_in = 0;
            if (!tape_pull_one_frame(&sl, &sr_in))
                break;
            tape_deck_fx_push_input(sl, sr_in, true);
            seed--;
        }

        if (tape_deck_fx_is_steady_run())
        {
            /* Fast lock-free 1:1 steady playback path: copy raw input under
             * `sink_lock` in ~2 us, then run cassette DSP without blocking
             * Rockbox or plugin threads on `pcm_mtx`. */
            while (frames_left > 0 && dev_playing)
            {
                if (!pcm_size)
                {
                    new_buffer = true;
                    if (!pcm_play_dma_complete_callback(PCM_DMAST_OK, &pcm_data,
                                                        &pcm_size))
                        break;
                }

                if (pcm_size % FRAME_BYTES)
                {
                    pcm_size &= ~(size_t)(FRAME_BYTES - 1);
                    if (!pcm_size)
                        break;
                }

                ssize_t nframes = MIN((ssize_t)pcm_size / FRAME_BYTES, frames_left);
                if (nframes > (ssize_t)TAPE_SCRATCH_FRAMES)
                    nframes = (ssize_t)TAPE_SCRATCH_FRAMES;

                memcpy(tape_raw_in, pcm_data, (size_t)nframes * FRAME_BYTES);
                pcm_data += nframes * FRAME_BYTES;
                pcm_size -= nframes * FRAME_BYTES;

                if (new_buffer)
                {
                    new_buffer = false;
                    pcm_play_dma_status_callback(PCM_DMAST_STARTED);
                }

                sink_unlock();
                tape_deck_fx_process_steady_block(tape_raw_in, tape_scratch,
                                                  (int)nframes, sr);
                viz_pcm_push(tape_scratch, (unsigned)nframes);
                sink_lock();

                int16_t *dst = &out_buf[2 * (want_frames - frames_left)];
                pcm_copy_buffer(dst, tape_scratch, (size_t)nframes * FRAME_BYTES);
                frames_left -= nframes;

                if (!tape_deck_fx_is_steady_run())
                    break;
            }
        }
        else
        {
            /* Variable-rate capstan path during ~300 ms Pause Spin-Down / Resume Spin-Up */
            bool paused_now = false;
            while (frames_left > 0 && dev_playing && !paused_now)
            {
                ssize_t chunk = MIN(frames_left, (ssize_t)TAPE_SCRATCH_FRAMES);
                ssize_t produced = 0;

                while (produced < chunk)
                {
                    bool exhausted = false;
                    while (tape_deck_fx_in_demand() >= 1.0)
                    {
                        int16_t sl = 0, sr_in = 0;
                        if (!tape_pull_one_frame(&sl, &sr_in))
                        {
                            exhausted = true;
                            break;
                        }
                        tape_deck_fx_push_input(sl, sr_in, false);
                    }
                    if (exhausted)
                        break;

                    tape_deck_fx_render_one(&tape_scratch[2 * produced],
                                            &tape_scratch[2 * produced + 1]);
                    produced++;

                    if (tape_deck_fx_take_pause_request())
                    {
                        mixer_channel_play_pause(PCM_MIXER_CHAN_PLAYBACK, false);
                        pcm_size = 0;
                        paused_now = true;
                        break;
                    }
                }

                if (produced <= 0)
                    break;

                int16_t *dst = &out_buf[2 * (want_frames - frames_left)];
                pcm_copy_buffer(dst, tape_scratch, (size_t)produced * FRAME_BYTES);
                viz_pcm_push(tape_scratch, (unsigned)produced);
                frames_left -= produced;
            }
        }
    }
    else
    {
        /* When the tape transport is stopped on Pause, still advance Rockbox's
         * mixer DMA callbacks so its 3s idle countdown finishes cleanly, but
         * mute any stale pre-mixed `downmix_buf` frame so no micro-burst of
         * full-speed audio leaks out at the tail of the spin-down. */
        bool mute_stopped_tape = tape_deck_fx_is_enabled() &&
                                 tape_deck_fx_is_stopped_idle() &&
                                 !tape_deck_fx_ff_active() &&
                                 (mixer_channel_status(PCM_MIXER_CHAN_BEEP) != CHANNEL_PLAYING);

        while (frames_left > 0 && dev_playing)
        {
            if (!pcm_size)
            {
                new_buffer = true;
                if (!pcm_play_dma_complete_callback(PCM_DMAST_OK, &pcm_data,
                                                    &pcm_size))
                    break;
            }

            if (pcm_size % FRAME_BYTES)
            {
                logf("Wrong pcm_size");     /* upstream panicf()s; just resync */
                pcm_size &= ~(size_t)(FRAME_BYTES - 1);
                if (!pcm_size)
                    break;
            }

            ssize_t nframes = MIN((ssize_t)pcm_size / FRAME_BYTES, frames_left);
            int16_t *dst = &out_buf[2 * (want_frames - frames_left)];

            if (mute_stopped_tape)
            {
                memset(dst, 0, (size_t)nframes * FRAME_BYTES);
            }
            else
            {
                /* Volume is applied by the copy; the visualizer wants the source before
                 * it, so the spectrum does not shrink with the volume. */
                pcm_copy_buffer(dst, pcm_data, nframes * FRAME_BYTES);
                viz_pcm_push(pcm_data, nframes);
            }

            pcm_data    += nframes * FRAME_BYTES;
            pcm_size    -= nframes * FRAME_BYTES;
            frames_left -= nframes;

            if (new_buffer)
            {
                new_buffer = false;
                pcm_play_dma_status_callback(PCM_DMAST_STARTED);
            }
        }
    }

    if (frames_left > 0)
        memset(&out_buf[2 * (want_frames - frames_left)], 0,
               (size_t)frames_left * FRAME_BYTES);

    /* Overlay electro-mechanical FF / Rewind motor sound across the full period
     * even during the 3s mixer silence window right after `audio_pre_ff_rewind`. */
    if (tape_deck_fx_ff_active())
    {
        ssize_t rem = want_frames;
        while (rem > 0)
        {
            ssize_t chunk = MIN(rem, (ssize_t)TAPE_SCRATCH_FRAMES);
            int16_t *dst = &out_buf[2 * (want_frames - rem)];
            tape_deck_fx_render_ff_period(tape_scratch, (int)chunk, sr);
            pcm_copy_buffer(dst, tape_scratch, (size_t)chunk * FRAME_BYTES);
            rem -= chunk;
        }
    }

    sink_unlock();
}

/* --- 16-tap Nuttall Polyphase Windowed-Sinc Stereo Resampler (Direct hw:0,0) -
 * Converts arbitrary Rockbox rates (44.1 / 22.05 / 88.2 / 96 / 192 kHz) cleanly
 * to the A133P audiocodec's native 48 kHz clock (>90 dB stopband, C1-continuous
 * zero-endpoint Nuttall window, plus 2nd-order Butterworth 21 kHz anti-aliasing
 * pre-filter when downsampling Hi-Res >48 kHz sources).  48 kHz passes 1:1. */
#define RS_TAPS        16
#define RS_PHASES      256
#define RS_RING_FRAMES 2048U
#define RS_RING_MASK   (RS_RING_FRAMES - 1U)
#define RS_PULL_CHUNK  256

static float s_sinc_tbl[RS_PHASES + 1][RS_TAPS];
static struct
{
    float    ring_l[RS_RING_FRAMES];
    float    ring_r[RS_RING_FRAMES];
    int16_t  pull_buf[RS_PULL_CHUNK * 2];
    uint32_t w_pos;
    uint32_t r_pos;
    double   frac;
    bool     seeded;
    uint32_t drain_zeros_pushed;
    unsigned int aa_sr;
    unsigned int aa_dst_sr;
    float    aa_b0, aa_b1, aa_b2, aa_a1, aa_a2;
    float    aa_z1_l, aa_z2_l, aa_z1_r, aa_z2_r;
} s_rs;

static void init_direct_resampler(void)
{
    const double pi = 3.14159265358979323846;
    const double fc = 0.455; /* flat to ~20.1 kHz at 44.1 kHz, >92 dB stopband */

    for (int p = 0; p <= RS_PHASES; p++)
    {
        double frac = (double)p / (double)RS_PHASES;
        double sum = 0.0;
        double row[RS_TAPS];

        for (int k = 0; k < RS_TAPS; k++)
        {
            double x = (double)k - (7.0 + frac);
            double u = 0.5 + x / (double)RS_TAPS;
            double win = 0.0;
            if (u > 0.0 && u < 1.0)
            {
                win = 0.355768
                    - 0.487396 * cos(2.0 * pi * u)
                    + 0.144232 * cos(4.0 * pi * u)
                    - 0.012604 * cos(6.0 * pi * u);
            }
            double sinc = (fabs(x) < 1e-9)
                        ? (2.0 * fc)
                        : (sin(2.0 * pi * fc * x) / (pi * x));
            row[k] = sinc * win;
            sum += row[k];
        }

        double inv = (fabs(sum) > 1e-12) ? (1.0 / sum) : 1.0;
        for (int k = 0; k < RS_TAPS; k++)
            s_sinc_tbl[p][k] = (float)(row[k] * inv);
    }
}

static void direct_rs_reset(void)
{
    s_rs.w_pos = 0;
    s_rs.r_pos = 0;
    s_rs.frac = 0.0;
    s_rs.seeded = false;
    s_rs.drain_zeros_pushed = 0;
    s_rs.aa_z1_l = s_rs.aa_z2_l = 0.0f;
    s_rs.aa_z1_r = s_rs.aa_z2_r = 0.0f;
}

static void direct_rs_update_aa(unsigned int src_sr, unsigned int dst_sr)
{
    if (s_rs.aa_sr == src_sr && s_rs.aa_dst_sr == dst_sr)
        return;
    s_rs.aa_sr = src_sr;
    s_rs.aa_dst_sr = dst_sr;
    s_rs.aa_z1_l = s_rs.aa_z2_l = 0.0f;
    s_rs.aa_z1_r = s_rs.aa_z2_r = 0.0f;
    if (src_sr <= dst_sr || dst_sr == 0)
        return;

    const double pi = 3.14159265358979323846;
    double fc_hz = 0.4375 * (double)dst_sr;
    double w0 = 2.0 * pi * fc_hz / (double)src_sr;
    double cw = cos(w0);
    double alpha = sin(w0) / 1.4142135623730951;
    double b0 = (1.0 - cw) * 0.5;
    double b1 = 1.0 - cw;
    double b2 = (1.0 - cw) * 0.5;
    double a0 = 1.0 + alpha;
    double a1 = -2.0 * cw;
    double a2 = 1.0 - alpha;
    double inv_a0 = 1.0 / a0;
    s_rs.aa_b0 = (float)(b0 * inv_a0);
    s_rs.aa_b1 = (float)(b1 * inv_a0);
    s_rs.aa_b2 = (float)(b2 * inv_a0);
    s_rs.aa_a1 = (float)(a1 * inv_a0);
    s_rs.aa_a2 = (float)(a2 * inv_a0);
}

static void direct_rs_push_chunk(unsigned int src_sr, unsigned int dst_sr)
{
    pull_source_frames(s_rs.pull_buf, RS_PULL_CHUNK, src_sr);
    bool use_aa = (src_sr > dst_sr);
    if (use_aa)
        direct_rs_update_aa(src_sr, dst_sr);

    for (int i = 0; i < RS_PULL_CHUNK; i++)
    {
        float sl = (float)s_rs.pull_buf[2 * i];
        float sr = (float)s_rs.pull_buf[2 * i + 1];
        if (use_aa)
        {
            float yl = s_rs.aa_b0 * sl + s_rs.aa_z1_l;
            s_rs.aa_z1_l = s_rs.aa_b1 * sl - s_rs.aa_a1 * yl + s_rs.aa_z2_l;
            s_rs.aa_z2_l = s_rs.aa_b2 * sl - s_rs.aa_a2 * yl;
            sl = yl;

            float yr = s_rs.aa_b0 * sr + s_rs.aa_z1_r;
            s_rs.aa_z1_r = s_rs.aa_b1 * sr - s_rs.aa_a1 * yr + s_rs.aa_z2_r;
            s_rs.aa_z2_r = s_rs.aa_b2 * sr - s_rs.aa_a2 * yr;
            sr = yr;
        }
        uint32_t m = s_rs.w_pos & RS_RING_MASK;
        s_rs.ring_l[m] = sl;
        s_rs.ring_r[m] = sr;
        s_rs.w_pos++;
    }
}

static void copy_frames(void)
{
    unsigned int dst_sr = open_direct_hw
                        ? DIRECT_HW_RATE
                        : (real_sample_rate
                             ? real_sample_rate
                             : (last_sample_rate ? last_sample_rate : 44100U));
    unsigned int src_sr = uipod_rs_manual_dj_is_active()
                        ? (unsigned int)uipod_rs_manual_dj_sample_rate()
                        : (last_sample_rate ? last_sample_rate : dst_sr);

    bool active_src = dev_playing || tape_deck_fx_ff_active() ||
                      uipod_rs_manual_dj_is_active();
    if (active_src)
    {
        s_rs.drain_zeros_pushed = 0;
    }
    else if (!s_rs.seeded || s_rs.drain_zeros_pushed >= (uint32_t)(RS_PULL_CHUNK * 2))
    {
        /* Only reset sinc ring AFTER all real buffered tail samples have drained out */
        if (s_rs.seeded)
            direct_rs_reset();
        memset(frames, 0, (size_t)period_size * FRAME_BYTES);
        if (open_direct_hw)
            mix_direct_click_into_period(frames, (int)period_size);
        return;
    }

    if (src_sr == dst_sr && !s_rs.seeded)
    {
        pull_source_frames(frames, (ssize_t)period_size, dst_sr);
        if (open_direct_hw)
            mix_direct_click_into_period(frames, (int)period_size);
        return;
    }

    if (!s_rs.seeded)
    {
        direct_rs_reset();
        for (uint32_t z = 0; z < 8U; z++)
        {
            s_rs.ring_l[z] = 0.0f;
            s_rs.ring_r[z] = 0.0f;
        }
        s_rs.w_pos = 8U;
        s_rs.seeded = true;
    }

    const double step = (double)src_sr / (double)dst_sr;
    for (snd_pcm_uframes_t i = 0; i < period_size; i++)
    {
        while ((s_rs.w_pos - s_rs.r_pos) < (uint32_t)(RS_TAPS + 4))
        {
            direct_rs_push_chunk(src_sr, dst_sr);
            if (!active_src)
                s_rs.drain_zeros_pushed += (uint32_t)RS_PULL_CHUNK;
        }

        float pf = (float)(s_rs.frac * (double)RS_PHASES);
        int pi = (int)pf;
        float mu = pf - (float)pi;
        if (pi >= RS_PHASES)
        {
            pi = RS_PHASES - 1;
            mu = 1.0f;
        }

        const float *c0 = s_sinc_tbl[pi];
        const float *c1 = s_sinc_tbl[pi + 1];
        float acc_l = 0.0f;
        float acc_r = 0.0f;
        uint32_t base = s_rs.r_pos;
        for (int k = 0; k < RS_TAPS; k++)
        {
            float w = c0[k] + (c1[k] - c0[k]) * mu;
            uint32_t m = (base + (uint32_t)k) & RS_RING_MASK;
            acc_l += s_rs.ring_l[m] * w;
            acc_r += s_rs.ring_r[m] * w;
        }

        if (acc_l > 32767.0f)       acc_l = 32767.0f;
        else if (acc_l < -32768.0f) acc_l = -32768.0f;
        if (acc_r > 32767.0f)       acc_r = 32767.0f;
        else if (acc_r < -32768.0f) acc_r = -32768.0f;

        frames[2 * i]     = (int16_t)acc_l;
        frames[2 * i + 1] = (int16_t)acc_r;

        s_rs.frac += step;
        uint32_t adv = (uint32_t)s_rs.frac;
        s_rs.r_pos += adv;
        s_rs.frac -= (double)adv;
    }

    if (open_direct_hw)
        mix_direct_click_into_period(frames, (int)period_size);
}

/* --- the writer thread (replaces upstream's SIGIO handler) ----------------- */

/* Returns false if the device died and was abandoned. */
static bool writer_step(snd_pcm_uframes_t *pending_rem,
                        snd_pcm_uframes_t *pending_off)
{
    snd_pcm_sframes_t space, n, delay;
    int err;

    /* Bluetooth: hand the period to the child; its read paces us. */
    if (bt_pipe)
    {
        size_t want = period_size * FRAME_BYTES;
        struct pollfd pfd = { .fd = fileno(bt_pipe), .events = POLLOUT };

        if (*pending_rem == 0)
        {
            copy_frames();
            *pending_off = 0;
            *pending_rem = period_size;
        }

        /* The pipe holds two periods, so writing blind would park us in the
         * kernel for a whole period.  Wait with a timeout instead, mirroring
         * snd_pcm_wait() below, so a route change or shutdown is never stuck
         * behind the child.  poll() also reports a dead reader before the
         * write does. */
        err = poll(&pfd, 1, WAIT_TIMEOUT_MS);
        if (err == 0)
            return true;                /* pipe full; re-check the flags */
        if (err < 0)
        {
            if (errno == EINTR)
                return true;
            dev_lost(strerror(errno));
            return false;
        }
        if (pfd.revents & (POLLERR | POLLHUP | POLLNVAL))
        {
            dev_lost("bluetooth helper gone");
            return false;
        }

        if (fwrite(frames, 1, want, bt_pipe) != want)
        {
            dev_lost("bluetooth helper gone");
            return false;
        }
        out_delay_tick = SDL_GetTicks();
        *pending_off = 0;
        *pending_rem = 0;
        return true;
    }

    err = snd_pcm_wait(handle, WAIT_TIMEOUT_MS);
    if (err == 0)
        return true;                    /* timeout; re-check the flags */
    if (err < 0)
    {
        if (alsa_recover_xrun_or_suspend(handle, err))
            return true;
        dev_lost(snd_strerror(err));
        return false;
    }

    space = snd_pcm_avail_update(handle);
    if (space < 0)
    {
        if (alsa_recover_xrun_or_suspend(handle, (int)space))
            return true;
        dev_lost(snd_strerror(space));
        return false;
    }

    /* Top the device up as far as it will take rather than handing over one
     * period per wakeup, preserving any partial non-blocking write remainder
     * so zero PCM frames are ever dropped! */
    while (writer_run &&
           (space >= (snd_pcm_sframes_t)period_size ||
            (*pending_rem > 0 && space > 0)))
    {
        if (*pending_rem == 0)
        {
            copy_frames();
            *pending_off = 0;
            *pending_rem = period_size;
        }

        n = snd_pcm_writei(handle,
                           frames + (*pending_off) * 2U,
                           *pending_rem);
        if (n > 0)
        {
            snd_pcm_uframes_t u = (snd_pcm_uframes_t)n;
            if (u >= *pending_rem)
            {
                *pending_off = 0;
                *pending_rem = 0;
            }
            else
            {
                *pending_off += u;
                *pending_rem -= u;
            }
            space -= n;
            continue;
        }
        if (n == 0 || n == -EAGAIN)
            break;                      /* no room after all; go re-wait */
        if (alsa_recover_xrun_or_suspend(handle, (int)n))
            break;
        dev_lost(snd_strerror(n));
        return false;
    }

    if (snd_pcm_state(handle) == SND_PCM_STATE_PREPARED)
        snd_pcm_start(handle);

    if (snd_pcm_delay(handle, &delay) == 0 && delay > 0)
    {
        out_delay_us = (unsigned int)(delay * 1000000ull / real_sample_rate);
        out_delay_tick = SDL_GetTicks();
    }
    return true;
}

/* The speaker's dmix bus stays open while idle; a private route opens only to
 * play (or produce FF/Rewind tape motor sound). */
static bool want_open(void)
{
    return audiodev || cur_route == TRIMPOD_SINK_SPEAKER ||
           dev_playing || tape_deck_fx_ff_active() ||
           uipod_rs_manual_dj_is_active();
}

static int writer_fn(void *param)
{
    snd_pcm_uframes_t pending_rem = 0;
    snd_pcm_uframes_t pending_off = 0;
    Uint32 retry_at = 0, route_at = 0, last_active_tick = 0;

    (void)param;

    /* The priority SDL gave its audio thread. */
    SDL_SetThreadPriority(SDL_THREAD_PRIORITY_TIME_CRITICAL);

    /* So a crash can be attributed to this thread or to one inside a plugin. */
    fprintf(stderr, "pcm: writer thread tid %ld\n", (long)syscall(SYS_gettid));

    while (writer_run)
    {
        if (dev_playing || tape_deck_fx_ff_active() || uipod_rs_manual_dj_is_active())
            last_active_tick = SDL_GetTicks();

        /* Poll the route whether or not a device is open: with the device
         * closed this is the only signal that audiomon has repointed
         * `default`, and therefore that reopening is worth trying. */
        if (SDL_TICKS_PASSED(SDL_GetTicks(), route_at))
        {
            int route = trimpod_audio_sink();
            route_at = SDL_GetTicks() + ROUTE_POLL_MS;
            if (route != cur_route)
            {
                fprintf(stderr, "pcm: route %d -> %d\n", cur_route, route);
                cur_route = route;
                tape_deck_fx_set_hp35_route_codec(route == TRIMPOD_SINK_SPEAKER);
                /* Either notice can arrive first -- this poll, or the write
                 * error from a device that has gone -- so leaving a private
                 * route this way is a loss too. */
                if (dev_is_open() && open_route != TRIMPOD_SINK_SPEAKER)
                    dev_lost("route changed");
                else
                    dev_close();
                pending_rem = 0;
                pending_off = 0;
            }
        }

        if (dev_is_open() && open_route == TRIMPOD_SINK_SPEAKER && !audiodev)
        {
            bool want_direct = s_want_direct_hw && !s_direct_hw_fallback && !s_force_shared_dmix;
            if (open_direct_hw != want_direct)
            {
                dev_close();
                pending_rem = 0;
                pending_off = 0;
                continue;
            }
        }

        if (dev_is_open() && req_sample_rate &&
            req_sample_rate != last_sample_rate)
        {
            /* Keep ALSA / USB-C / BlueALSA stream continuously open across
             * sample-rate changes (`44.1 kHz <-> 48 kHz`): `copy_frames()`
             * resamples seamlessly via the 16-tap Nuttall polyphase sinc ring
             * without dropping buffered samples or closing the device! */
            logf("pcm: rate %u -> %u (warm seamless resample)",
                 last_sample_rate, req_sample_rate);
            last_sample_rate = req_sample_rate;
        }

        if (!dev_is_open())
        {
            if (!want_open())
            {
                SDL_SemWaitTimeout(writer_wake, ROUTE_POLL_MS);
                continue;
            }
            if (SDL_TICKS_PASSED(SDL_GetTicks(), retry_at))
            {
                if (req_sample_rate)
                    last_sample_rate = req_sample_rate;
                if (!dev_open())
                {
                    Uint32 dly = (s_want_direct_hw && !s_direct_hw_fallback &&
                                  s_direct_busy_retries > 0)
                               ? 150U
                               : REOPEN_DELAY_MS;
                    retry_at = SDL_GetTicks() + dly;
                }
            }
            SDL_Delay(20);
            continue;
        }

        /* A private route closes when the sink stops, which the mixer does
         * after three seconds of silence, so skips and short pauses keep it
         * open. */
        if (private_open && !dev_playing && !tape_deck_fx_ff_active() &&
            !uipod_rs_manual_dj_is_active() &&
            !s_rs.seeded && pending_rem == 0 &&
            SDL_TICKS_PASSED(SDL_GetTicks(), last_active_tick + 3000U))
        {
            fprintf(stderr, "pcm: idle, closing\n");
            dev_close();
            pending_rem = 0;
            pending_off = 0;
            continue;
        }

        /* Keep shared dmix stream warm for 3s after playback stops (and until
         * sinc tail drains) so track transitions never trigger -EPIPE XRUNs. */
        if (open_shared && !dev_playing && !tape_deck_fx_ff_active() &&
            !uipod_rs_manual_dj_is_active() &&
            !s_rs.seeded && pending_rem == 0 &&
            SDL_TICKS_PASSED(SDL_GetTicks(), last_active_tick + 3000U))
        {
            SDL_SemWaitTimeout(writer_wake, ROUTE_POLL_MS);
            continue;
        }

        if (!writer_step(&pending_rem, &pending_off))
            retry_at = SDL_GetTicks() + REOPEN_DELAY_MS;
    }

    dev_close();
    return 0;
}

/* --- UIPod visualizer PCM tap -------------------------------------------
 * The 4-shade GPU visualizer and audio-reactive plugins need the stereo PCM
 * waveform and spectrum synchronized to speaker/Bluetooth output.  We mirror
 * the PRE-volume Rockbox S16 stereo source (see copy_frames) into a lock-free
 * 16384-frame (~371 ms at 44.1 kHz) ring buffer so visuals stay independent of
 * volume and can step back by `out_delay_us` to match ear arrival time. */
#define VIZ_PCM_FRAMES 16384           /* must be a power of two */
static int16_t viz_pcm_ring[VIZ_PCM_FRAMES * 2];
static volatile unsigned viz_pcm_w;    /* total stereo frames ever written */

extern void uipod_rs_intel_audio_tap_push(const int16_t *stereo_s16,
                                          size_t frames,
                                          uint32_t sample_rate,
                                          uint32_t output_delay_us,
                                          uint64_t now_ms);
extern int32_t uipod_rs_manual_dj_is_active(void);
extern size_t  uipod_rs_manual_dj_render_s16(int16_t *out_stereo_s16,
                                             size_t frames,
                                             uint32_t sample_rate);

static unsigned int viz_sample_rate(void)
{
    unsigned int sr = last_sample_rate ? last_sample_rate : real_sample_rate;
    return sr ? sr : 44100U;
}

static void viz_pcm_push(const void *src_v, unsigned frames_n)
{
    const int16_t *src = src_v;
    unsigned w = viz_pcm_w;

    for (unsigned i = 0; i < frames_n; i++)
    {
        unsigned idx = (w & (VIZ_PCM_FRAMES - 1)) * 2;
        viz_pcm_ring[idx]     = src[2 * i];
        viz_pcm_ring[idx + 1] = src[2 * i + 1];
        w++;
    }
    viz_pcm_w = w;

    unsigned sr = viz_sample_rate();
    uipod_rs_intel_audio_tap_push(src,
                                  (size_t)frames_n,
                                  (uint32_t)sr,
                                  (uint32_t)out_delay_us,
                                  (uint64_t)SDL_GetTicks());
}

/* Copy the most recent up-to max_frames stereo frames into out (LRLR).
 * Returns the number of frames written. */
unsigned pcm_viz_latest(int16_t *out, unsigned max_frames)
{
    unsigned w = viz_pcm_w;
    unsigned n = max_frames < VIZ_PCM_FRAMES ? max_frames : VIZ_PCM_FRAMES;
    if (n > w)
        n = w;
    unsigned start = w - n;
    for (unsigned i = 0; i < n; i++)
    {
        unsigned idx = ((start + i) & (VIZ_PCM_FRAMES - 1)) * 2;
        out[2 * i]     = viz_pcm_ring[idx];
        out[2 * i + 1] = viz_pcm_ring[idx + 1];
    }
    return n;
}

/* Live remaining output buffer latency in microseconds, interpolating drain
 * elapsed since the last period write so 60 FPS visualizers never lag behind
 * by up to a full ALSA period (~23 ms) between writer wakeups. */
static unsigned int live_output_delay_us(void)
{
    if (!dev_playing || !dev_is_open())
        return 0U;
    unsigned int base = out_delay_us;
    Uint32 dt_ms = SDL_GetTicks() - out_delay_tick;
    unsigned int dt_us = (dt_ms < 250U) ? (dt_ms * 1000U) : 250000U;
    return (base > dt_us) ? (base - dt_us) : 0U;
}

/* Display vsync + LCD scanout presentation lead (~22 ms at 60 Hz) so the
 * rendered GPU frame lights up on screen at the exact instant the analyzed
 * audio frame leaves the speaker. */
#define VIZ_DISPLAY_LEAD_US 22000U

/* Delay-compensated stereo PCM reader for the 4-shade visualizer engine.
 * Steps back in the ring by live speaker latency (minus display presentation
 * lead) and centers the `max_frames` Hann window on that exact frame so
 * visual beats land dead-on ear arrival time.  Returns 0 when paused/stopped. */
unsigned pcm_viz_read_synced(int16_t *out, unsigned max_frames,
                             unsigned *sample_rate_out)
{
    unsigned sr = viz_sample_rate();
    if (sample_rate_out)
        *sample_rate_out = sr;
    if (!dev_playing || !out || max_frames == 0)
        return 0;

    unsigned w = viz_pcm_w;
    unsigned n = max_frames < (VIZ_PCM_FRAMES / 2)
                     ? max_frames
                     : (VIZ_PCM_FRAMES / 2);
    unsigned max_delay = VIZ_PCM_FRAMES - n - 1024;
    unsigned int raw_dly_us = live_output_delay_us();
    unsigned int eff_dly_us = (raw_dly_us > VIZ_DISPLAY_LEAD_US)
                            ? (raw_dly_us - VIZ_DISPLAY_LEAD_US)
                            : 0U;
    unsigned delay_frames =
        (unsigned)(((uint64_t)eff_dly_us * (uint64_t)sr) / 1000000ULL);
    if (delay_frames > max_delay)
        delay_frames = max_delay;
    if (w <= delay_frames)
        return 0;

    unsigned speaker_frame = w - delay_frames;
    unsigned end = speaker_frame + (n / 2);
    if (end > w)
        end = w;
    if (n > end)
        n = end;
    unsigned start = end - n;
    for (unsigned i = 0; i < n; i++)
    {
        unsigned idx = ((start + i) & (VIZ_PCM_FRAMES - 1)) * 2;
        out[2 * i]     = viz_pcm_ring[idx];
        out[2 * i + 1] = viz_pcm_ring[idx + 1];
    }
    return n;
}

/* --- Ultra-low-latency iPod Click Wheel synthesizer ------------------------
 * Synthesizes a 3-mode damped mechanical detent impulse (5.6 kHz piezo snap +
 * 2.35 kHz click body + 1.18 kHz warm housing) and plays it directly onto the
 * internal `Playback` dmix bus with a tiny 512-frame (~10 ms) buffer so menu
 * scrolling feels instantaneous whether music is playing or stopped, without
 * polluting `viz_pcm_ring`.  When a private USB-C or Bluetooth stream is
 * actively playing, `pcm_alsa_menu_click()` returns false so `uipod_menu_click`
 * mixes the exact same waveform into `PCM_MIXER_CHAN_BEEP`. */
#define CLICK_RATE        48000
#define CLICK_WAVE_FRAMES 96     /* 2.0 ms physical impulse */
#define CLICK_BUF_FRAMES  256    /* 5.3 ms write chunk */
#define CLICK_MIN_GAP_MS  22     /* max ~45 clicks/sec on fast-scroll */

static int16_t s_click_stereo[CLICK_BUF_FRAMES * 2];
static SDL_Thread *click_thread;
static SDL_sem    *click_sem;
static volatile bool click_run;
static volatile bool click_pending;
static Uint32 s_last_click_ms;

static void init_click_waveform(void)
{
    double tmp[CLICK_WAVE_FRAMES];
    double sum = 0.0;
    const double pi = 3.14159265358979323846;

    for (int i = 0; i < CLICK_WAVE_FRAMES; i++)
    {
        double t = (double)i / (double)CLICK_RATE;
        double u = (double)i / (double)(CLICK_WAVE_FRAMES - 1);
        double atk  = 1.0 - exp(-t / 0.00004);
        double win  = 0.5 * (1.0 + cos(pi * u));
        double snap = 0.62 * sin(2.0 * pi * 5600.0 * t) * exp(-t / 0.00024);
        double body = 0.48 * sin(2.0 * pi * 2350.0 * t + 0.35) * exp(-t / 0.00042);
        double warm = 0.24 * sin(2.0 * pi * 1180.0 * t) * exp(-t / 0.00065);
        tmp[i] = (snap + body + warm) * atk * win;
        sum += tmp[i];
    }

    double dc = sum / (double)CLICK_WAVE_FRAMES;
    memset(s_click_stereo, 0, sizeof(s_click_stereo));
    for (int i = 0; i < CLICK_WAVE_FRAMES; i++)
    {
        double v = (tmp[i] - dc) * 11500.0;
        if (v > 32000.0) v = 32000.0;
        if (v < -32000.0) v = -32000.0;
        int16_t s = (int16_t)v;
        s_click_stereo[2 * i]     = s;
        s_click_stereo[2 * i + 1] = s;
    }
}

static snd_pcm_t *open_click_dmix(void)
{
    snd_pcm_t *pcm = NULL;
    snd_pcm_hw_params_t *hw = NULL;
    snd_pcm_sw_params_t *sw = NULL;
    unsigned int rate = CLICK_RATE;
    snd_pcm_uframes_t period = CLICK_BUF_FRAMES;
    snd_pcm_uframes_t buf_sz = CLICK_BUF_FRAMES * 2;

    trimpod_alsa_lock();
    if (snd_pcm_open(&pcm, "Playback", SND_PCM_STREAM_PLAYBACK, 0) < 0)
    {
        trimpod_alsa_unlock();
        return NULL;
    }

    snd_pcm_hw_params_malloc(&hw);
    snd_pcm_sw_params_malloc(&sw);

    if (snd_pcm_hw_params_any(pcm, hw) < 0 ||
        snd_pcm_hw_params_set_access(pcm, hw, SND_PCM_ACCESS_RW_INTERLEAVED) < 0 ||
        snd_pcm_hw_params_set_format(pcm, hw, SND_PCM_FORMAT_S16_LE) < 0 ||
        snd_pcm_hw_params_set_channels(pcm, hw, 2) < 0 ||
        snd_pcm_hw_params_set_rate_near(pcm, hw, &rate, 0) < 0 ||
        snd_pcm_hw_params_set_period_size_near(pcm, hw, &period, NULL) < 0 ||
        snd_pcm_hw_params_set_buffer_size_near(pcm, hw, &buf_sz) < 0 ||
        snd_pcm_hw_params(pcm, hw) < 0 ||
        snd_pcm_sw_params_current(pcm, sw) < 0 ||
        snd_pcm_sw_params_set_start_threshold(pcm, sw, 64) < 0 ||
        snd_pcm_sw_params(pcm, sw) < 0 ||
        snd_pcm_prepare(pcm) < 0)
    {
        snd_pcm_hw_params_free(hw);
        snd_pcm_sw_params_free(sw);
        snd_pcm_close(pcm);
        trimpod_alsa_unlock();
        return NULL;
    }

    snd_pcm_hw_params_free(hw);
    snd_pcm_sw_params_free(sw);
    trimpod_alsa_unlock();
    return pcm;
}

static void mix_direct_click_into_period(int16_t *dst_stereo, int nframes)
{
    int cur = __atomic_load_n(&s_direct_click_pos, __ATOMIC_ACQUIRE);
    if (cur >= CLICK_WAVE_FRAMES)
        return;
    for (int i = 0; i < nframes && cur < CLICK_WAVE_FRAMES; i++, cur++)
    {
        int32_t c = s_click_stereo[2 * cur];
        int32_t l = (int32_t)dst_stereo[2 * i] + c;
        int32_t r = (int32_t)dst_stereo[2 * i + 1] + c;
        if (l > 32767)       l = 32767;
        else if (l < -32768) l = -32768;
        if (r > 32767)       r = 32767;
        else if (r < -32768) r = -32768;
        dst_stereo[2 * i]     = (int16_t)l;
        dst_stereo[2 * i + 1] = (int16_t)r;
    }
    __atomic_store_n(&s_direct_click_pos, cur, __ATOMIC_RELEASE);
}

static int click_fn(void *param)
{
    (void)param;

    while (click_run)
    {
        SDL_SemWait(click_sem);
        if (!click_run)
            break;

        click_pending = false;

        pthread_mutex_lock(&click_pcm_mtx);
        if (audiodev || cur_route != TRIMPOD_SINK_SPEAKER ||
            open_direct_hw || (s_want_direct_hw && !s_direct_hw_fallback))
        {
            if (s_click_pcm)
            {
                trimpod_alsa_lock();
                snd_pcm_close(s_click_pcm);
                trimpod_alsa_unlock();
                s_click_pcm = NULL;
            }
            pthread_mutex_unlock(&click_pcm_mtx);
            continue;
        }

        if (!s_click_pcm)
        {
            s_click_pcm = open_click_dmix();
            if (!s_click_pcm)
            {
                pthread_mutex_unlock(&click_pcm_mtx);
                continue;
            }
        }

        snd_pcm_state_t st = snd_pcm_state(s_click_pcm);
        if (st == SND_PCM_STATE_XRUN || st == SND_PCM_STATE_SETUP ||
            st == SND_PCM_STATE_SUSPENDED)
        {
            snd_pcm_prepare(s_click_pcm);
        }

        snd_pcm_sframes_t wr = snd_pcm_writei(s_click_pcm, s_click_stereo,
                                              CLICK_BUF_FRAMES);
        if (wr < 0)
        {
            wr = snd_pcm_recover(s_click_pcm, (int)wr, 1);
            if (wr == 0)
                (void)snd_pcm_writei(s_click_pcm, s_click_stereo, CLICK_BUF_FRAMES);
            else
            {
                trimpod_alsa_lock();
                snd_pcm_close(s_click_pcm);
                trimpod_alsa_unlock();
                s_click_pcm = NULL;
            }
        }
        pthread_mutex_unlock(&click_pcm_mtx);
    }

    close_click_dmix_sync();
    return 0;
}

const void *pcm_alsa_click_samples(size_t *byte_size_out)
{
    if (byte_size_out)
        *byte_size_out = CLICK_WAVE_FRAMES * 2 * sizeof(int16_t);
    return s_click_stereo;
}

/* Returns true if handled (played on direct hw:0,0 edge, low-latency dmix,
 * rate-limited, or suppressed because a private BT/USB route is closed/idle).
 * Returns false when a private BT/USB stream is actively playing so the caller
 * can mix the click into `PCM_MIXER_CHAN_BEEP`. */
bool pcm_alsa_menu_click(void)
{
    Uint32 now = SDL_GetTicks();
    if (now - s_last_click_ms < CLICK_MIN_GAP_MS)
        return true;
    s_last_click_ms = now;

    if (!audiodev && cur_route == TRIMPOD_SINK_SPEAKER)
    {
        if (open_direct_hw || (s_want_direct_hw && !s_direct_hw_fallback && !s_force_shared_dmix))
        {
            __atomic_store_n(&s_direct_click_pos, 0, __ATOMIC_RELEASE);
            if (writer_wake)
                SDL_SemPost(writer_wake);
            return true;
        }
        if (click_sem && !click_pending)
        {
            click_pending = true;
            SDL_SemPost(click_sem);
        }
        return true;
    }

    /* Private route (Bluetooth / USB-C DAC): never reopen an idle device just
     * for a 2 ms click; mix into PCM_MIXER_CHAN_BEEP only while active. */
    if (!dev_playing || !dev_is_open())
        return true;

    return false;
}

/* --- pcm_sink ops ---------------------------------------------------------- */

/* Publish the rate; the writer reopens at it.  Closing the device from this
 * thread would pull it out from under a writer sitting in snd_pcm_wait(). */
static void sink_set_freq(uint16_t freq)
{
    req_sample_rate = hw_freq_sampr[freq];
}

/* No locking here.  The pcm layer holds it when calling from a Rockbox thread,
 * and calls these re-entrantly from inside copy_frames() on the writer thread,
 * where it is already held. */
static void sink_dma_start(const void *addr, size_t size)
{
    pcm_data = addr;
    pcm_size = size;
    dev_playing = true;
    SDL_SemPost(writer_wake);
}

/* Upstream snd_pcm_drain()s here.  We must not: this is reached from inside
 * copy_frames() on the writer thread, so draining would block the very thread
 * that empties the buffer. */
static void sink_dma_stop(void)
{
    dev_playing = false;
    pcm_size = 0;
}

static void sink_dma_init(void)
{
    pthread_mutexattr_t attr;

    pthread_mutexattr_init(&attr);
    pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);
    pthread_mutex_init(&pcm_mtx, &attr);

    /* The Bluetooth helper dying mid-write must surface as an fwrite error,
     * not as a signal that takes the app down with it. */
    signal(SIGPIPE, SIG_IGN);

    last_sample_rate = hw_freq_sampr[HW_FREQ_DEFAULT];
    cur_route = trimpod_audio_sink();
    init_click_waveform();
    init_direct_resampler();
    tape_deck_fx_init();
    tape_deck_fx_set_hp35_route_codec(cur_route == TRIMPOD_SINK_SPEAKER);

    writer_wake = SDL_CreateSemaphore(0);
    writer_run = true;
    writer_thread = SDL_CreateThread(writer_fn, "trimpod-pcm", NULL);

    if (!writer_thread)
        panicf("Could not create audio thread");

    click_sem = SDL_CreateSemaphore(0);
    if (click_sem)
    {
        click_run = true;
        click_thread = SDL_CreateThread(click_fn, "uipod-click", NULL);
    }
}

static void sink_dma_postinit(void)
{
}

unsigned int pcm_alsa_output_delay_us(void)
{
    return live_output_delay_us();
}

void pcm_alsa_wake_writer(void)
{
    if (writer_wake)
        SDL_SemPost(writer_wake);
}

void pcm_alsa_set_background_shared(bool force_shared)
{
    if (s_force_shared_dmix == force_shared)
        return;
    s_force_shared_dmix = force_shared;
    if (writer_wake)
        SDL_SemPost(writer_wake);
}

void pcm_alsa_set_direct_hw(bool enabled)
{
    bool prev = s_want_direct_hw;
    s_want_direct_hw = enabled;
    if (enabled && (!prev || s_direct_hw_fallback))
    {
        s_direct_hw_fallback = false;
        s_direct_busy_retries = 0;
    }
    if (enabled)
        close_click_dmix_sync();
    if (writer_wake)
        SDL_SemPost(writer_wake);
}

/* Called from audiohw_close() on the way out.  Every alsa-lib call the writer
 * makes is bounded, so this returns promptly -- which is the point: the SDL
 * path could not be closed at all and hung the "Shutting Down" screen. */
void pcm_alsa_shutdown(void)
{
    if (click_thread)
    {
        click_run = false;
        SDL_SemPost(click_sem);
        SDL_WaitThread(click_thread, NULL);
        click_thread = NULL;
    }

    if (!writer_thread)
        return;

    writer_run = false;
    SDL_SemPost(writer_wake);
    SDL_WaitThread(writer_thread, NULL);
    writer_thread = NULL;
}

struct pcm_sink builtin_pcm_sink = {
    .caps = {
        .samprs       = hw_freq_sampr,
        .num_samprs   = HW_NUM_FREQ,
        .default_freq = HW_FREQ_DEFAULT,
    },
    .ops = {
        .init     = sink_dma_init,
        .postinit = sink_dma_postinit,
        .set_freq = sink_set_freq,
        .lock     = sink_lock,
        .unlock   = sink_unlock,
        .play     = sink_dma_start,
        .stop     = sink_dma_stop,
    },
};
