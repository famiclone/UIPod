/***************************************************************************
 *             __________               __   ___.
 *   Open      \______   \ ____   ____ |  | _\_ |__   _______  ___
 *   Source     |       _//  _ \_/ ___\|  |/ /| __ \ /  _ \  \/  /
 *   Jukebox    |    |   (  <_> )  \___|    < | \_\ (  <_> > <  <
 *   Firmware   |____|_  /\____/ \___  >__|_ \|___  /\____/__/\_ \
 *                     \/            \/     \/    \/            \/
 *
 * Copyright (C) 2010 Thomas Martitz
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
#define RB_FILESYSTEM_OS
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include "system.h"
#include "file.h"
#include "load_code.h"

void *lc_open(const char *filename, unsigned char *buf, size_t buf_size)
{
#ifdef __ANDROID__
    if (filename)
    {
        const char *slash = strrchr(filename, '/');
        const char *base  = slash ? (slash + 1) : filename;
        const char *dot   = strrchr(base, '.');
        const char *nlib  = getenv("UIPOD_NATIVE_LIB_DIR");
        if (dot && strcmp(dot, ".codec") == 0 && nlib && nlib[0])
        {
            char so_path[MAX_PATH];
            size_t stem_len = (size_t)(dot - base);
            snprintf(so_path, sizeof(so_path), "%s/libcodec_%.*s.so",
                     nlib, (int)stem_len, base);
            void *h = os_lc_open(so_path);
            if (h)
                return h;
        }
    }
#endif
    char realpath[MAX_PATH];
    const char *fpath = handle_special_dirs(filename, 0, realpath,
                                            sizeof (realpath));
    if (!fpath)
        return NULL;

    return os_lc_open(fpath);
    (void)buf; (void)buf_size;
}
