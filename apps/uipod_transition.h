/***************************************************************************
 * UIPod: screen transition animations.
 *
 * Supports selectable transition styles (`Settings -> Display -> Transition`):
 *   0 = Slide   (classic iPod horizontal push)
 *   1 = Cover   (parallax card stack with fg1 divider)
 *   2 = Fade    (smooth alpha crossfade)
 *   3 = Pixels  (4x4 HiDPI Bayer LCD pixel dissolve — pure 4-color palette)
 *   4 = Blinds  (8-band horizontal shutter wipe matching list rows)
 *   5 = Off     (instant screen switch)
 ****************************************************************************/
#ifndef _UIPOD_TRANSITION_H
#define _UIPOD_TRANSITION_H

#include <stdbool.h>

#define UIPOD_TRANSITION_MS  100   /* base duration for Slide (ms) */

enum uipod_transition_dir
{
    UIPOD_TRANS_FORWARD,   /* destination enters from the right (R->L) */
    UIPOD_TRANS_BACK,      /* destination enters from the left  (L->R) */
};

enum uipod_transition_style
{
    UIPOD_TRANS_STYLE_SLIDE  = 0,
    UIPOD_TRANS_STYLE_COVER  = 1,
    UIPOD_TRANS_STYLE_FADE   = 2,
    UIPOD_TRANS_STYLE_PIXELS = 3,
    UIPOD_TRANS_STYLE_BLINDS = 4,
    UIPOD_TRANS_STYLE_OFF    = 5,
    UIPOD_TRANS_STYLE_COUNT  = 6,
};

/* Called to paint the destination screen off-screen during a transition. */
typedef void (*uipod_render_fn)(void *ctx);

/* Animate a screen change: snapshot the current screen, render the destination
 * off-screen via render(ctx), then play the active transition in `dir`. */
void uipod_transition_animate(enum uipod_transition_dir dir,
                              uipod_render_fn render, void *ctx);

/* Cross-call back signal: a page/menu about to return flags that its caller
 * should play a BACK transition on its next redraw. */
void uipod_transition_arm_back(void);
bool uipod_transition_take_back(void);

/* Suppress the next screen-entry transition entirely (one-shot). */
void uipod_transition_suppress_next(void);

/* True exactly once -- for the very FIRST screen shown at app launch. */
bool uipod_transition_first_screen(void);

#endif /* _UIPOD_TRANSITION_H */
