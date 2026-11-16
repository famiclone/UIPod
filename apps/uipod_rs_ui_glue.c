/***************************************************************************
 * UIPod: Thin Strangler-Fig C glue bridging Rockbox runtime state to the
 * pure-Rust GPU UI engine (`crates/uipod-ui`).
 ****************************************************************************/
#include "config.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include "action.h"
#include "audio.h"
#include "button.h"
#include "file.h"
#include "gui/list.h"
#include "gui/statusbar-skinned.h"
#include "gui/viewport.h"
#include "gui/wps.h"
#include "lang.h"
#include "lcd.h"
#include "misc.h"
#include "power.h"
#include "powermgmt.h"
#include "rbpaths.h"
#include "screen_access.h"
#include "settings.h"
#include "status.h"
#include "string-extra.h"
#include "uipod_rs_ui.h"
#include "uipod_svg.h"
#include "uipod_ui.h"
#include "window-sdl.h"

bool uipod_gpu_ui_ensure_ready(void)
{
    /* Never touch the SDL GLES context while projectM owns it on the viz thread */
    if (uipod_viz_active || !sdl_gl_get_context())
        return false;

    sdl_gl_make_current();

    if (uipod_rs_ui_is_ready())
        return true;

    return uipod_rs_ui_init(sdl_gl_get_proc_address,
                              ROCKBOX_DIR "/fonts/PixelMplus12-Regular.ttf");
}

void uipod_gpu_fill_header(struct UIPodRsHeader *out,
                             const char *override_title)
{
    if (!out)
        return;

    memset(out, 0, sizeof(*out));

    const char *title = override_title;
    if (!title || !title[0])
        title = sb_get_title(SCREEN_MAIN);
    if (!title || !title[0])
        title = "UIPod Mini";

    out->title  = title;
    out->legend = uipod_get_header_legend();

    int pm = current_playmode();
    struct wps_state *wstate = get_wps_state();
    if (pm == STATUS_PLAY &&
        (((audio_status() & AUDIO_STATUS_PAUSE) != 0) ||
         (wstate && wstate->paused)))
    {
        pm = STATUS_PAUSE;
    }

    switch (pm)
    {
        case STATUS_PLAY:         out->play_mode = 1; break;
        case STATUS_PAUSE:        out->play_mode = 2; break;
        case STATUS_FASTFORWARD:  out->play_mode = 3; break;
        case STATUS_FASTBACKWARD: out->play_mode = 4; break;
        default:                  out->play_mode = 0; break;
    }

    int pct = battery_level_display();
    if (pct < 0)
        pct = 0;
    else if (pct > 100)
        pct = 100;
    out->battery_percent = (uint8_t)pct;

    out->is_charging = charging_state();
#ifdef HAS_BUTTON_HOLD
    out->is_hold = button_hold();
#else
    out->is_hold = is_keys_locked();
#endif
}

bool uipod_gpu_render_header(void)
{
    if (get_current_activity() == ACTIVITY_USBSCREEN)
        return false;

    if (!uipod_gpu_ui_ensure_ready())
        return false;

    struct UIPodRsHeader hdr;
    uipod_gpu_fill_header(&hdr, NULL);

    struct viewport *prev_vp = lcd_set_viewport(NULL);
    fb_data *fb = FBADDR(0, 0);
    bool ok = uipod_rs_ui_render_header((uint32_t)global_settings.bg_color,
                                          (uint32_t)global_settings.fg_color,
                                          &hdr,
                                          (uint8_t *)fb);
    lcd_set_viewport(prev_vp);

    if (ok)
        lcd_update_rect(0, 0, LCD_WIDTH, UIPOD_RS_HEADER_BAND_ROWS);
    return ok;
}

bool uipod_gpu_render_synclist(struct gui_synclist *list)
{
    if (!list || !list->callback_get_item_name || list->callback_draw_item != NULL)
        return false;

    struct viewport *parent = list->parent[SCREEN_MAIN];
    if (parent && (parent->width < 800 || parent->height < 500))
        return false;

    if (!uipod_gpu_ui_ensure_ready())
        return false;

    /* Stop any legacy bitmap scroll-engine marquees on SCREEN_MAIN */
    screens[SCREEN_MAIN].scroll_stop();

    /* Keep statusbar-skinned title state in sync for standalone header refreshes */
    const char *resolved_title = NULL;
    if (list->title)
    {
        if (P2ID((const unsigned char *)list->title) <= VOICEONLY_DELIMITER)
            resolved_title = (const char *)P2STR((const unsigned char *)list->title);
    }
    sb_set_title_text(resolved_title, list->title_icon, SCREEN_MAIN);

    int total = (list->nb_items > 0) ? list->nb_items : 0;
    int start = list->start_item[SCREEN_MAIN];
    if (start < 0)
        start = 0;
    if (start > total)
        start = total;

    char label_bufs[UIPOD_RS_LIST_VISIBLE_ROWS][MAX_PATH];
    char rval_bufs[UIPOD_RS_LIST_VISIBLE_ROWS][128];
    char ftag_bufs[UIPOD_RS_LIST_VISIBLE_ROWS][32];
    struct UIPodRsListRow rows[UIPOD_RS_LIST_VISIBLE_ROWS];
    size_t row_count = 0;

    bool cursor_visible = (list->cursor_style != SYNCLIST_CURSOR_NOSTYLE);
    int sel_begin = list->selected_item;
    int sel_end   = sel_begin + ((list->selected_size > 0) ? list->selected_size : 1);

    for (int r = 0; r < UIPOD_RS_LIST_VISIBLE_ROWS && (start + r) < total; r++)
    {
        int idx = start + r;
        char scratch[MAX_PATH];
        const char *raw = list->callback_get_item_name(idx, list->data,
                                                       scratch, sizeof(scratch));
        const char *entry_str = "";
        if (raw && P2ID((const unsigned char *)raw) <= VOICEONLY_DELIMITER)
            entry_str = (const char *)P2STR((const unsigned char *)raw);
        while (*entry_str == '\t')
            entry_str++;

        strlcpy(label_bufs[r], entry_str, sizeof(label_bufs[r]));
        rval_bufs[r][0] = '\0';
        ftag_bufs[r][0] = '\0';

        const struct uipod_svg_icon *svg_icon =
            list->callback_get_item_svg_icon
                ? list->callback_get_item_svg_icon(idx, list->data)
                : NULL;
        if (svg_icon && svg_icon->name[0])
            strlcpy(ftag_bufs[r], svg_icon->name, sizeof(ftag_bufs[r]));

        const char *indic =
            list->callback_get_item_indicator
                ? list->callback_get_item_indicator(idx, list->data)
                : NULL;
        bool chev =
            list->callback_get_item_chevron
                ? list->callback_get_item_chevron(idx, list->data)
                : false;

        uint8_t right_kind = 0;
        if (indic && indic[0])
        {
            if (indic[0] == '[' && indic[1] && indic[2] == ']' && indic[3] == '\0')
            {
                right_kind = (indic[1] != ' ') ? 3 : 2;
            }
            else if (indic[0] == '>' && indic[1] == '\0')
            {
                right_kind = 1;
            }
            else
            {
                strlcpy(rval_bufs[r], indic, sizeof(rval_bufs[r]));
                right_kind = 4;
            }
        }
        else if (chev)
        {
            right_kind = 1;
        }

        rows[row_count].label       = label_bufs[r];
        rows[row_count].right_value = rval_bufs[r];
        rows[row_count].file_tag    = ftag_bufs[r];
        rows[row_count].selected    = cursor_visible &&
                                      (idx >= sel_begin && idx < sel_end);
        rows[row_count].right_kind  = right_kind;
        row_count++;
    }

    struct UIPodRsListScreen sc;
    memset(&sc, 0, sizeof(sc));
    sc.bg_rgb        = (uint32_t)global_settings.bg_color;
    sc.fg_rgb        = (uint32_t)global_settings.fg_color;
    uipod_gpu_fill_header(&sc.header, resolved_title);
    sc.rows          = rows;
    sc.row_count     = row_count;
    sc.total_items   = (size_t)total;
    sc.start_item    = (size_t)start;
    sc.visible_items = UIPOD_RS_LIST_VISIBLE_ROWS;

    struct viewport *prev_vp = lcd_set_viewport(NULL);
    fb_data *fb = FBADDR(0, 0);
    bool ok = uipod_rs_ui_render_list(&sc, (uint8_t *)fb);
    lcd_set_viewport(prev_vp);

    if (ok)
        lcd_update();
    return ok;
}
