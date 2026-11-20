/***************************************************************************
 * UIPod: Main Menu Settings -- toggle which root-menu entries are shown.
 *
 * A small set of root-menu entries (Music, Playlists, Browse) can be hidden
 * or shown by the user.  State is persisted in ROCKBOX_DIR/mainmenu.txt and
 * queried by root_menu.c's item_callback (ACTION_REQUEST_MENUITEM) to filter
 * the live menu, exactly like the bookmarks entry keys off global_settings.
 ****************************************************************************/
#ifndef _UIPOD_MAINMENU_H
#define _UIPOD_MAINMENU_H

#include <stdbool.h>

enum uipod_mainmenu_id
{
    UIPOD_MM_BROWSE = 0,      /* Browse (first item) */
    UIPOD_MM_EXTRAS,          /* Extras */
    UIPOD_MM_ARTISTS,         /* Artists */
    UIPOD_MM_ALBUMS,          /* Albums */
    UIPOD_MM_GENRES,          /* Genres */
    UIPOD_MM_MOVIES,          /* Videos */
    UIPOD_MM_SONGS,
    UIPOD_MM_PLAYLISTS,
    UIPOD_MM_PODCASTS,
    UIPOD_MM_AUDIOBOOKS,
    UIPOD_MM_TELEVISION,
    UIPOD_MM_SHUFFLE,
    UIPOD_MM_COUNT,
};

/* True if the given root-menu entry should be shown. */
bool uipod_mainmenu_is_enabled(enum uipod_mainmenu_id id);

/* Settings -> Main Menu Settings page (MENUITEM_FUNCTION target). */
int uipod_mainmenu_settings(void);

#endif /* _UIPOD_MAINMENU_H */
