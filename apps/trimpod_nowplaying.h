/***************************************************************************
 * Trimpod: custom HiDPI Now Playing (WPS) renderer & cover-art inspector.
 ****************************************************************************/
#ifndef _TRIMPOD_NOWPLAYING_H
#define _TRIMPOD_NOWPLAYING_H

/* Render the full 1024x768 Now Playing screen into the main LCD framebuffer
 * and present it (unless lcd_is_update_suppressed() is active for a slide
 * transition). */
void trimpod_nowplaying_draw(void);

#endif /* _TRIMPOD_NOWPLAYING_H */
