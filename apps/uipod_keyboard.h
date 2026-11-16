/* UIPod: a minimal on-screen keyboard (see uipod_keyboard.c).
 *
 * An iPod-aesthetic grid keyboard rendered in Geologica: A-Z (case via an
 * on-screen capslock), 0-9, hyphen, optional URL/password symbols (`sym`),
 * space, backspace and enter.  Replaces the stock Rockbox kbd_input for
 * UIPod text entry. */
#ifndef _UIPOD_KEYBOARD_H
#define _UIPOD_KEYBOARD_H

#include <stdbool.h>

/* Edit `buffer` (NUL-terminated, capacity `buflen`) with the on-screen keyboard.
 * Returns 0 when the on-screen enter key is engaged, -1 on cancel (B).  On
 * cancel `buffer` is restored to its initial contents. */
int uipod_kbd_input(char *buffer, int buflen);

/* Extended keyboard with `sym` toggle (`: / . - _ @ ? = & + # ! $ % * ...`)
 * for server URLs, usernames, and passwords. */
int uipod_kbd_input_ex(char *buffer, int buflen, bool allow_symbols);

#endif /* _UIPOD_KEYBOARD_H */
