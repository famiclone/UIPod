/***************************************************************************
 * UIPod: Platform storage & fast-cache root directories.
 *
 * - TrimUI Brick / Linux (`!defined(__ANDROID__)`):
 *     UIPOD_SDCARD_ROOT = "/mnt/SDCARD"
 *     UIPOD_TMP_ROOT    = "/tmp"
 * - Android Handhelds (`defined(__ANDROID__)`):
 *     UIPOD_SDCARD_ROOT = "/storage/emulated/0"
 *     UIPOD_TMP_ROOT    = "/data/data/dev.famiclone.uipod/cache"
 ****************************************************************************/
#ifndef UIPOD_PATHS_H
#define UIPOD_PATHS_H

#ifdef __ANDROID__
#define UIPOD_SDCARD_ROOT "/storage/emulated/0"
#define UIPOD_TMP_ROOT    "/data/data/dev.famiclone.uipod/cache"
#else
#define UIPOD_SDCARD_ROOT "/mnt/SDCARD"
#define UIPOD_TMP_ROOT    "/tmp"
#endif

#endif /* UIPOD_PATHS_H */
