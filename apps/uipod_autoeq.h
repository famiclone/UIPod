/***************************************************************************
 * UIPod: AutoEQ headphone database search, ParametricEQ downloader/converter,
 * and local EQ preset browser (EQS_DIR).
 ****************************************************************************/
#ifndef _UIPOD_AUTOEQ_H
#define _UIPOD_AUTOEQ_H

#include <stdbool.h>
#include "settings.h"

/* Settings -> Sound Settings -> Equalizer -> AutoEQ */
int uipod_autoeq_page(void);

/* Settings -> Sound Settings -> Equalizer -> Browse EQ Presets */
int uipod_eq_presets_page(void);

/* Parse EqualizerAPO ParametricEQ.txt text into Rockbox's precut (0..240 tenths
 * of dB) and 10-band Parametric EQ settings (1 LS + 8 PK + 1 HS). */
bool uipod_autoeq_parse_parametric_eq(
    const char *txt,
    int *out_precut,
    struct eq_band_setting out_bands[EQ_NUM_BANDS]);

#endif /* _UIPOD_AUTOEQ_H */
