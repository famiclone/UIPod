/* UIPod: user-managed virtual folders -- Music, Podcasts, Audiobooks, Movies,
 * Television -- plus the Browse entry (whole filesystem, opens at the card), all
 * in one browser (see uipod_folders.c).  Each is a virtual folder merging a
 * user-managed set of source folders plus an unremovable default; all share one
 * engine. */
#ifndef _UIPOD_FOLDERS_H
#define _UIPOD_FOLDERS_H

/* Settings -> *Folders pages (MENUITEM_FUNCTION targets) */
int uipod_music_settings(void);
int uipod_podcast_settings(void);
int uipod_audiobook_settings(void);
int uipod_movies_settings(void);
int uipod_television_settings(void);

/* root menu virtual-folder entries (root_menu items[] functions) */
int uipod_music_browse(void *param);
int uipod_podcast_browse(void *param);
int uipod_audiobook_browse(void *param);
int uipod_movies_browse(void *param);
int uipod_television_browse(void *param);
/* root menu "Browse": the whole filesystem in the same browser */
int uipod_files_browse(void *param);

/* root menu "Shuffle Songs": build one recursive playlist of every track in the
 * virtual Music folder, shuffle it and start playing.  Returns GO_TO_WPS. */
int uipod_shuffle_all(void *param);

/* Virtual-folder categories in the library index -- indices shared with
 * uipod_library.c.  Movies and Television are deliberately not listed. */
enum { TP_CAT_MUSIC = 0, TP_CAT_PODCAST, TP_CAT_AUDIOBOOK, TP_CAT_COUNT };

/* Load every category's source-folder list from disk (idempotent).  The library
 * scanner calls this before enumerating roots. */
void uipod_folders_load_all(void);
/* Enumerate a category's configured source roots. */
int uipod_folders_root_count(int cat);
const char *uipod_folders_root_path(int cat, int idx);

#endif /* _UIPOD_FOLDERS_H */
