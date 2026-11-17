/***************************************************************************
 * UIPod: custom HiDPI Now Playing (WPS) renderer & cover-art inspector.
 ****************************************************************************/
#ifndef _UIPOD_NOWPLAYING_H
#define _UIPOD_NOWPLAYING_H

#include <stdbool.h>

/* Render the full 1024x768 Now Playing screen into the main LCD framebuffer
 * and present it (unless lcd_is_update_suppressed() is active for a slide
 * transition). */
void uipod_nowplaying_draw(void);

/* Toggle between standard Now Playing and Now Playing (Alt) large cover art. */
bool uipod_nowplaying_is_alt(void);
void uipod_nowplaying_toggle_alt(void);

#endif /* _UIPOD_NOWPLAYING_H */
