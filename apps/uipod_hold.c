/***************************************************************************
 * UIPod: the HOLD (pocket-lock) screen -- see uipod_hold.h.
 *
 * An ordinary uipod_page, drawn exactly like the "(empty)" / confirm pages:
 * the themed content viewport (uipod_centered_message) with the skin-owned
 * status-row header, no bespoke theme and no enter slide.  Because input is
 * muted while held, the page can't rely on a button to exit -- its on_action
 * polls the switch directly and returns DONE once it reads "off".
 ****************************************************************************/
#include "config.h"
#include "uipod_hold.h"


#include <stdbool.h>
#include "action.h"          /* get_action(), CONTEXT_STD */
#include "kernel.h"          /* HZ */
#include "button-target.h"   /* retrohh_hold_switch() */
#include "uipod_page.h"
#include "uipod_ui.h"      /* uipod_centered_message() */
#include "lang.h"            /* str() */

static void hold_draw(struct uipod_page *self)
{
    (void)self;
    uipod_centered_message(str(LANG_UIPOD_HOLD_HINT), str(LANG_UIPOD_HOLD_ON), NULL);
}

/* Input other than volume is muted while held, so poll the switch quickly (not
 * the HZ the loop suggests) so we notice the release promptly; SYS events still
 * flow through. */
static int hold_poll(struct uipod_page *self, int timeout)
{
    (void)timeout;
    return get_action(self->context, HZ / 10);
}

static enum uipod_page_result hold_on_action(struct uipod_page *self, int action)
{
    (void)self;
    (void)action;
    return retrohh_hold_switch() ? UIPOD_PAGE_STAY : UIPOD_PAGE_DONE;
}

static const struct uipod_page_vtable hold_vtable =
{
    .legend    = NULL,        /* nothing is actionable while held */
    .draw      = hold_draw,
    .poll      = hold_poll,
    .on_action = hold_on_action,
};

void uipod_hold_run(void)
{
    if (!retrohh_hold_switch())
        return;                 /* switch off at launch -> straight to the menu */

    struct uipod_page p =
    {
        .vt            = &hold_vtable,
        .context       = CONTEXT_STD,
        .allowed       = NULL,
        .no_enter_anim = true,   /* first screen at launch: no slide */
        .no_arm_back   = true,   /* the main menu draws itself, no back slide */
    };
    uipod_page_run(&p);
}

