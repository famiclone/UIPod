/***************************************************************************
 * Trimpod: runtime SVG file-type icons for media lists.
 *
 * Loads ROCKBOX_DIR/icons/<ext>.svg on demand (e.g. mp3.svg for *.mp3),
 * rasterizes once at 8x8 supersampling to a 56x56 4-bit Rockbox alpha mask,
 * and caches hits and misses in memory so scrolling never touches disk.
 * Icons render through lcd_alpha_bitmap_part(), automatically adopting the
 * active theme colours and inverting on the highlighted selection bar.
 ****************************************************************************/
#ifndef TRIMPOD_SVG_H
#define TRIMPOD_SVG_H

#include <stdbool.h>
#include "screen_access.h"

#define TRIMPOD_SVG_ICON_SIZE   56
#define TRIMPOD_SVG_ICON_GAP    12
#define TRIMPOD_SVG_TOGGLE_H    40

struct trimpod_svg_icon
{
    char name[24];
    int width;
    int height;
    unsigned char *alpha;
};

/* Load (and cache) ROCKBOX_DIR/icons/<name>.svg scaled proportionally to
 * `target_h` pixels tall (width rounded up to an even pixel count). */
const struct trimpod_svg_icon *trimpod_svg_load(const char *name,
                                                int target_h);

/* Return a lightweight tag descriptor (e.g. "folder", "add_dir") for GPU UI
 * list rows even when no raster .svg exists on disk. */
const struct trimpod_svg_icon *trimpod_svg_tag(const char *tag);

/* Look up (and lazily rasterize + cache) the SVG icon for a media path or
 * filename.  Returns NULL when no matching <ext>.svg (or folder.svg for
 * directories) exists under ROCKBOX_DIR/icons/. */
const struct trimpod_svg_icon *trimpod_svg_for_path(const char *path,
                                                    bool is_dir);

/* Look up (and lazily rasterize + cache) toggle.svg / toggle_enabled.svg. */
const struct trimpod_svg_icon *trimpod_svg_toggle(bool enabled);

/* Draw a cached SVG icon at viewport-local (x, y) in foreground ink (or
 * inverted ink when `invert` is true on a STYLE_INVERT selection bar). */
void trimpod_svg_draw(struct screen *display,
                      const struct trimpod_svg_icon *icon,
                      int x, int y, bool invert);

#endif /* TRIMPOD_SVG_H */
