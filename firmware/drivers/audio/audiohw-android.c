/***************************************************************************
 * UIPod: Android audiohw driver (hardware-first `STREAM_MUSIC` bridge).
 *
 * Mirrors `trimpod-alsa.c`:
 *  - Android's `AudioManager.STREAM_MUSIC` carries hardware speaker / 3.5mm /
 *    USB-C DAC / Bluetooth A2DP attenuation natively while `pcm_set_master_volume`
 *    stays at 0.0 dB unity gain (avoiding double attenuation and 16-bit floor loss).
 *  - Physical handheld volume rockers and Android's system volume slider sync
 *    into UIPod's `global_status.volume` via `SYS_VOLUME_CHANGED` so the Now
 *    Playing volume bar, status bar, and Sound menu stay in lockstep.
 *  - Falls back to Rockbox software attenuation on `AudioManager.isVolumeFixed()`
 *    outputs (e.g. fixed-level HDMI docks).
 ****************************************************************************/
#include <limits.h>
#include <stdbool.h>
#include <stdlib.h>
#include <math.h>
#include <jni.h>
#include <SDL.h>

#include "config.h"
#include "kernel.h"
#include "button.h"
#include "sound.h"
#include "pcm_sw_volume.h"
#include "trimpod_alsa.h"

static volatile bool s_kernel_ready = false;
static volatile bool s_initial_queried = false;
static volatile bool s_vol_fixed = false;
static volatile int  s_last_synced_ddb = INT_MIN;

int trimpod_audio_sink(void)
{
    return TRIMPOD_SINK_SPEAKER;
}

int trimpod_usb_card(void)
{
    return -1;
}

void trimpod_alsa_lock(void) {}
void trimpod_alsa_unlock(void) {}
void trimpod_alsa_forget_sink(void) {}
void trimpod_alsa_volume_overwritten(void) {}

/* Matches `apps/misc.c` `to_normalized_volume` / `from_normalized_volume` (60 dB law) */
static int permille_from_ddb(int vol_ddb, int lo, int hi)
{
    if (vol_ddb <= lo)
        return 0;
    if (vol_ddb >= hi)
        return 1000;
    double min_norm = pow(10.0, (double)(lo - hi) / 600.0);
    double norm = pow(10.0, (double)(vol_ddb - hi) / 600.0);
    double u = (norm - min_norm) / (1.0 - min_norm);
    if (u < 0.0) u = 0.0;
    if (u > 1.0) u = 1.0;
    return (int)lrint(u * 1000.0);
}

static int ddb_from_steps(int cur, int min_v, int max_v)
{
    int lo = sound_min(SOUND_VOLUME);
    int hi = sound_max(SOUND_VOLUME);
    int step = sound_steps(SOUND_VOLUME);
    if (step <= 0)
        step = 10;

    if (max_v <= min_v || cur >= max_v)
        return hi;
    if (cur <= min_v)
        return lo;

    double u = (double)(cur - min_v) / (double)(max_v - min_v);
    double min_norm = pow(10.0, (double)(lo - hi) / 600.0);
    double lin = min_norm + u * (1.0 - min_norm);
    double ddb_f = (double)hi + 600.0 * log10(lin);
    int ddb = (int)lrint(ddb_f / (double)step) * step;
    if (ddb <= lo)
        ddb = lo + step;
    if (ddb > hi)
        ddb = hi;
    return ddb;
}

static void android_set_stream_permille(int permille)
{
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    if (!env)
        return;
    jclass cls = (*env)->FindClass(env, "dev/famiclone/uipod/UIPodActivity");
    if (!cls)
    {
        (*env)->ExceptionClear(env);
        return;
    }
    jmethodID mid = (*env)->GetStaticMethodID(env, cls, "setStreamVolumePermille", "(I)V");
    if (mid)
        (*env)->CallStaticVoidMethod(env, cls, mid, (jint)permille);
    else
        (*env)->ExceptionClear(env);
    (*env)->DeleteLocalRef(env, cls);
}

static void android_query_initial_volume(void)
{
    JNIEnv *env = (JNIEnv *)SDL_AndroidGetJNIEnv();
    if (!env)
        return;
    jclass cls = (*env)->FindClass(env, "dev/famiclone/uipod/UIPodActivity");
    if (!cls)
    {
        (*env)->ExceptionClear(env);
        return;
    }
    jmethodID mid = (*env)->GetStaticMethodID(env, cls, "queryAndPushStreamVolume", "()V");
    if (mid)
        (*env)->CallStaticVoidMethod(env, cls, mid);
    else
        (*env)->ExceptionClear(env);
    (*env)->DeleteLocalRef(env, cls);
}

JNIEXPORT void JNICALL
Java_dev_famiclone_uipod_UIPodActivity_nativeOnAndroidVolumeChanged(
    JNIEnv *env, jclass cls, jint cur, jint min_v, jint max_v, jboolean is_fixed)
{
    (void)env;
    (void)cls;

    s_vol_fixed = (is_fixed != JNI_FALSE);
    if (s_vol_fixed)
        return;

    int ddb = ddb_from_steps((int)cur, (int)min_v, (int)max_v);
    s_last_synced_ddb = ddb;

    int unity_or_mute = (cur <= min_v) ? PCM_MUTE_LEVEL : 0;
    pcm_set_master_volume(unity_or_mute, unity_or_mute);

    if (s_kernel_ready)
    {
        sim_enter_irq_handler();
        button_queue_post(SYS_VOLUME_CHANGED, (intptr_t)ddb);
        sim_exit_irq_handler();
    }
}

void audiohw_set_volume(int vol_l, int vol_r)
{
    int lo = sound_min(SOUND_VOLUME);
    int hi = sound_max(SOUND_VOLUME);
    int step = sound_steps(SOUND_VOLUME);
    if (step <= 0)
        step = 10;

    if (vol_l < lo) vol_l = lo;
    if (vol_l > hi) vol_l = hi;
    if (vol_r < lo) vol_r = lo;
    if (vol_r > hi) vol_r = hi;

    s_kernel_ready = true;
    if (!s_initial_queried)
    {
        s_initial_queried = true;
        android_query_initial_volume();
        if (!s_vol_fixed && s_last_synced_ddb != INT_MIN)
            return;
    }

    if (s_vol_fixed)
    {
        int rem_l = (vol_l <= lo) ? PCM_MUTE_LEVEL : vol_l;
        int rem_r = (vol_r <= lo) ? PCM_MUTE_LEVEL : vol_r;
        pcm_set_master_volume(rem_l, rem_r);
        return;
    }

    int common = (vol_l > vol_r) ? vol_l : vol_r;
    if (common <= lo)
    {
        pcm_set_master_volume(PCM_MUTE_LEVEL, PCM_MUTE_LEVEL);
        if (s_last_synced_ddb != lo)
        {
            s_last_synced_ddb = lo;
            android_set_stream_permille(0);
        }
        return;
    }

    /* Keep PCM at 0.0 dB unity gain (preserving only L/R balance offset if any)
     * while Android's `STREAM_MUSIC` applies hardware loudness natively. */
    int bal_l = (vol_l <= lo) ? PCM_MUTE_LEVEL : (vol_l - common);
    int bal_r = (vol_r <= lo) ? PCM_MUTE_LEVEL : (vol_r - common);
    pcm_set_master_volume(bal_l, bal_r);

    if (s_last_synced_ddb == INT_MIN || abs(common - s_last_synced_ddb) >= step)
    {
        s_last_synced_ddb = common;
        android_set_stream_permille(permille_from_ddb(common, lo, hi));
    }
}

void audiohw_close(void)
{
    s_kernel_ready = false;
    pcm_alsa_shutdown();
}
