/***************************************************************************
 * UIPod: Native Plugin Host Bridge, `Extras -> Plugins` UI, and File Viewers.
 ****************************************************************************/
#ifndef UIPOD_PLUGINS_H
#define UIPOD_PLUGINS_H

#include <stdbool.h>

/* Scan bundled (`/tmp/uipod/plugins`) and SD-card (`/mnt/SDCARD/Plugins/UIPod`)
 * plugins at startup. */
void uipod_plugins_init(void);

/* Interactive `Extras -> Plugins` hub (`Games`, `Apps`, `Demos`, `Viewers`). */
int uipod_plugins_page(void);

/* Returns true if `path` is a `.uipod` binary or matches a registered Viewer plugin. */
bool uipod_plugins_can_open_file(const char *path);

/* Opens `path` in its matching Viewer plugin (or runs a `.uipod` binary directly).
 * Returns true if handled by the plugin engine. */
bool uipod_plugins_open_file(const char *path);

/* Returns true while a `MenuDriven` plugin (where `START` owns in-plugin pause/menu
 * rather than UIPod's global background play/pause toggle) is active. */
bool uipod_plugins_menu_driven_active(void);

#endif /* UIPOD_PLUGINS_H */
