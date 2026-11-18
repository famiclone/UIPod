/***************************************************************************
 * UIPod: lightweight file-type / item icon tag descriptors for GPU UI lists.
 *
 * Maps paths/extensions ("mp3", "flac", "mp4", "folder", "add_dir", "uipod")
 * to static tag descriptors consumed by `crates/uipod-ui/src/icons.rs`.
 ****************************************************************************/
#ifndef UIPOD_SVG_H
#define UIPOD_SVG_H

#include <stdbool.h>

struct uipod_svg_icon
{
    char name[24];
};

const struct uipod_svg_icon *uipod_svg_tag(const char *tag);
const struct uipod_svg_icon *uipod_svg_for_path(const char *path, bool is_dir);

#endif /* UIPOD_SVG_H */
