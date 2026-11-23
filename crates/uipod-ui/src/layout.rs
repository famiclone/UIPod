//! Responsive Flexbox screen layouts powered by `taffy`, preserving exact `ui.figcraft`
//! coordinates at `1024x768` (`4:3`) while adapting cleanly to widescreen (`16:9`, `16:10`,
//! `3:2`), square (`1:1`), and portrait displays at native HiDPI resolution.

use taffy::prelude::*;

#[inline]
fn px<T: FromLength>(val: f32) -> T {
    length(val)
}

#[inline]
fn pct<T: FromPercent>(val: f32) -> T {
    percent(val)
}

pub const SCREEN_W: f32 = 1024.0;
pub const SCREEN_H: f32 = 768.0;
pub const LIST_VISIBLE_ROWS: usize = 8;

/// Declarative layout spec for `VolumeBarPopup` (`#3214`) & `Volume Popup` (`#10977` in `ui.figcraft`).
pub mod volume_popup_spec {
    pub const WIDTH: f32 = 527.0;
    pub const HEIGHT: f32 = 72.0;
    pub const CORNER_RADIUS: f32 = 16.0;
    pub const STROKE_WIDTH: f32 = 4.0;
    pub const PADDING: f32 = 16.0;
    /// Total inset from popup outer border to inner `VolumeBar` (`strokes_included: true`).
    pub const INNER_INSET: f32 = STROKE_WIDTH + PADDING;
    pub const BAR_WIDTH: f32 = WIDTH - INNER_INSET * 2.0;
    pub const BAR_HEIGHT: f32 = 32.0;
    pub const ICON_SIZE: f32 = 32.0;
    pub const TRACK_WIDTH: f32 = 360.0;
    pub const TRACK_HEIGHT: f32 = 16.0;
    pub const TRACK_OFFSET_X: f32 = (BAR_WIDTH - TRACK_WIDTH) * 0.5;
    pub const TRACK_OFFSET_Y: f32 = (BAR_HEIGHT - TRACK_HEIGHT) * 0.5;
    pub const LOUD_ICON_OFFSET_X: f32 = BAR_WIDTH - ICON_SIZE;
    pub const REST_Y: f32 = 22.0;
    pub const HIDDEN_Y: f32 = -(HEIGHT + 8.0);
}

/// Declarative layout spec for floating HUD toast tooltips (reusing `volume_popup_spec` container & motion).
pub mod hud_toast_spec {
    use super::volume_popup_spec as vp;

    pub const HEIGHT: f32 = vp::HEIGHT;
    pub const CORNER_RADIUS: f32 = vp::CORNER_RADIUS;
    pub const STROKE_WIDTH: f32 = vp::STROKE_WIDTH;
    pub const PAD_X: f32 = 24.0;
    pub const INNER_INSET_X: f32 = STROKE_WIDTH + PAD_X;
    pub const MIN_WIDTH: f32 = 220.0;
    pub const SCREEN_MARGIN_X: f32 = 64.0;
    pub const FONT_SIZE: f32 = 36.0;
    pub const REST_Y: f32 = vp::REST_Y;
    pub const HIDDEN_Y: f32 = vp::HIDDEN_Y;
    pub const SLIDE_IN_MS: f32 = 180.0;
    pub const HOLD_MS: u64 = 2000;
    pub const SLIDE_OUT_MS: f32 = 200.0;
}

/// Declarative layout spec for the Unified Stacked HUD System (`Volume`, `Toast`, `Progress`, `Confirmation`).
pub mod hud_stack_spec {
    use super::hud_toast_spec as ht;
    use super::volume_popup_spec as vp;

    // Shared stack container & motion choreography
    pub const REST_Y: f32 = vp::REST_Y;
    pub const CARD_GAP_Y: f32 = 12.0;
    pub const MAX_VISIBLE_CARDS: usize = 4;
    pub const CORNER_RADIUS: f32 = vp::CORNER_RADIUS;
    pub const STROKE_WIDTH: f32 = vp::STROKE_WIDTH;
    pub const SCREEN_MARGIN_X: f32 = ht::SCREEN_MARGIN_X;

    pub const SLIDE_IN_MS: f32 = ht::SLIDE_IN_MS;
    pub const DEFAULT_HOLD_MS: u64 = ht::HOLD_MS;
    pub const EXIT_TOP_MS: f32 = ht::SLIDE_OUT_MS;
    pub const EXIT_FADE_MS: f32 = 180.0;
    pub const REFLOW_TAU_MS: f32 = 65.0;
    pub const HIDDEN_TOP_PAD: f32 = 8.0;

    // Two-line Toast card variant
    pub const TOAST_TWO_LINE_HEIGHT: f32 = 108.0;
    pub const TOAST_LINE1_CENTER_Y: f32 = 36.0;
    pub const TOAST_LINE2_CENTER_Y: f32 = 74.0;
    pub const TOAST_LINE2_FONT_SIZE: f32 = 28.0;

    // Progress card (`begin` / `update` / `end`)
    pub const PROGRESS_MIN_WIDTH: f32 = vp::WIDTH;
    pub const PROGRESS_HEIGHT: f32 = 104.0;
    pub const PROGRESS_PAD_X: f32 = 20.0;
    pub const PROGRESS_INNER_INSET_X: f32 = STROKE_WIDTH + PROGRESS_PAD_X;
    pub const PROGRESS_LABEL_CENTER_Y: f32 = 36.0;
    pub const PROGRESS_TITLE_FONT_SIZE: f32 = 30.0;
    pub const PROGRESS_DETAIL_FONT_SIZE: f32 = 26.0;
    pub const PROGRESS_LABEL_GAP_X: f32 = 16.0;
    pub const PROGRESS_TRACK_Y: f32 = 66.0;
    pub const PROGRESS_TRACK_HEIGHT: f32 = vp::TRACK_HEIGHT;
    pub const INDETERMINATE_SHUTTLE_RATIO: f32 = 0.28;
    pub const INDETERMINATE_PERIOD_MS: f32 = 1350.0;

    // Confirmation card (`[B] Cancel` / `[A] Confirm`)
    pub const CONFIRM_MIN_WIDTH: f32 = vp::WIDTH;
    pub const CONFIRM_HEIGHT_SINGLE: f32 = 148.0;
    pub const CONFIRM_HEIGHT_DETAIL: f32 = 184.0;
    pub const CONFIRM_PAD_X: f32 = 24.0;
    pub const CONFIRM_INNER_INSET_X: f32 = STROKE_WIDTH + CONFIRM_PAD_X;
    pub const CONFIRM_DETAIL_CENTER_Y: f32 = 36.0;
    pub const CONFIRM_DETAIL_FONT_SIZE: f32 = 24.0;
    pub const CONFIRM_QUESTION_SINGLE_CENTER_Y: f32 = 46.0;
    pub const CONFIRM_QUESTION_DETAIL_CENTER_Y: f32 = 74.0;
    pub const CONFIRM_QUESTION_FONT_SIZE: f32 = 32.0;
    pub const CONFIRM_BTN_BOTTOM_INSET: f32 = 20.0;
    pub const CONFIRM_BTN_HEIGHT: f32 = 44.0;
    pub const CONFIRM_BTN_MIN_WIDTH: f32 = 196.0;
    pub const CONFIRM_BTN_GAP_X: f32 = 16.0;
    pub const CONFIRM_BTN_PAD_X: f32 = 16.0;
    pub const CONFIRM_BTN_RADIUS: f32 = 10.0;
    pub const CONFIRM_BTN_STROKE: f32 = 2.5;
    pub const CONFIRM_BTN_FONT_SIZE: f32 = 25.0;
    pub const CONFIRM_KEY_BADGE_SIZE: f32 = 26.0;
    pub const CONFIRM_KEY_BADGE_RADIUS: f32 = 6.0;
    pub const CONFIRM_KEY_BADGE_FONT_SIZE: f32 = 19.0;
    pub const CONFIRM_KEY_GAP_X: f32 = 10.0;
}

/// Declarative layout spec (`256x256` normalized coordinates, scaled uniformly to `art_box`)
/// for procedural 4-shade Famicom HVC cartridge artwork (`Now Playing` & `Now Playing Alt`).
pub mod famicom_cart_spec {
    pub const REF_SIZE: f32 = 256.0;
    pub const FRAME_RADIUS: f32 = 8.0;
    pub const FRAME_STROKE: f32 = 4.0;

    // Horizontal Famicom HVC cassette body
    pub const CART_X: f32 = 16.0;
    pub const CART_Y: f32 = 30.0;
    pub const CART_W: f32 = 224.0;
    pub const CART_H: f32 = 168.0;
    pub const CART_RADIUS: f32 = 10.0;
    pub const CART_STROKE: f32 = 3.5;

    // Stepped 60-pin bottom connector skirt
    pub const SKIRT_X: f32 = 34.0;
    pub const SKIRT_Y: f32 = 194.0;
    pub const SKIRT_W: f32 = 188.0;
    pub const SKIRT_H: f32 = 32.0;
    pub const SKIRT_RADIUS: f32 = 5.0;
    pub const PIN_COUNT: usize = 11;
    pub const PIN_W: f32 = 8.0;
    pub const PIN_H: f32 = 14.0;
    pub const PIN_GAP: f32 = 6.0;
    pub const PIN_TOP_INSET: f32 = 11.0;

    // Top grip ribs & molded insertion arrow
    pub const GRIP_X: f32 = 56.0;
    pub const GRIP_Y0: f32 = 42.0;
    pub const GRIP_W: f32 = 166.0;
    pub const GRIP_RIB_H: f32 = 3.5;
    pub const GRIP_RIB_STEP: f32 = 7.0;
    pub const GRIP_RIB_COUNT: usize = 3;
    pub const ARROW_CX: f32 = 36.0;
    pub const ARROW_TOP_Y: f32 = 42.0;
    pub const ARROW_HALF_W: f32 = 9.0;
    pub const ARROW_H: f32 = 15.0;

    // Front recessed sticker label (`LabelFrame`)
    pub const LABEL_X: f32 = 28.0;
    pub const LABEL_Y: f32 = 68.0;
    pub const LABEL_W: f32 = 200.0;
    pub const LABEL_H: f32 = 116.0;
    pub const LABEL_RADIUS: f32 = 6.0;
    pub const LABEL_STROKE: f32 = 2.5;
    pub const LABEL_BAND_H: f32 = 28.0;
    pub const LABEL_PAD_X: f32 = 10.0;

    // Corner expansion chip badge (`2A03` / `VRC6` / `MMC5` / `FDS` / `N163` / `5B` / `VRC7`)
    pub const BADGE_H: f32 = 20.0;
    pub const BADGE_PAD_X: f32 = 7.0;
    pub const BADGE_RADIUS: f32 = 4.0;
    pub const BADGE_MARGIN_R: f32 = 6.0;
    pub const BADGE_FONT_SIZE: f32 = 13.5;
    pub const HEADER_FONT_SIZE: f32 = 12.5;

    // Scissor-clipped Game Title & Artist typography inside `LabelFrame`
    pub const TITLE_CENTER_Y: f32 = 124.0;
    pub const TITLE_FONT_SIZE: f32 = 21.0;
    pub const ARTIST_CENTER_Y: f32 = 156.0;
    pub const ARTIST_FONT_SIZE: f32 = 14.5;
    pub const ACCENT_RULE_Y: f32 = 172.0;
    pub const ACCENT_RULE_H: f32 = 2.0;
}

/// Declarative layout spec for Now Playing chiptune widgets (`LoopTick`, `EndlessBadge`, `ApuStrip`).
pub mod nsf_now_playing_spec {
    // Seekbar loop marker tick (`loop_point_ms`)
    pub const LOOP_TICK_W: f32 = 4.0;
    pub const LOOP_TICK_OVERHANG_Y: f32 = 4.0;
    pub const LOOP_TICK_CAP_W: f32 = 10.0;
    pub const LOOP_TICK_CAP_H: f32 = 3.0;

    // `[∞]` Endless Loop pill beside remaining timestamp
    pub const ENDLESS_BADGE_W: f32 = 52.0;
    pub const ENDLESS_BADGE_H: f32 = 30.0;
    pub const ENDLESS_BADGE_RADIUS: f32 = 6.0;
    pub const ENDLESS_BADGE_STROKE: f32 = 2.5;
    pub const ENDLESS_BADGE_GAP_X: f32 = 14.0;
    pub const INFINITY_LOBE_R: f32 = 5.8;
    pub const INFINITY_LOBE_DX: f32 = 6.4;
    pub const INFINITY_STROKE_W: f32 = 2.8;

    // Compact 4-shade hardware APU channel strip (`y = 532` between `art_box` and `seekbar_box`)
    pub const STRIP_TOP_OFFSET_FROM_ART: f32 = 20.0;
    pub const CHIP_H: f32 = 34.0;
    pub const CHIP_MIN_W: f32 = 54.0;
    pub const CHIP_PAD_X: f32 = 12.0;
    pub const CHIP_GAP_X: f32 = 10.0;
    pub const CHIP_RADIUS: f32 = 6.0;
    pub const CHIP_STROKE: f32 = 2.0;
    pub const CHIP_FONT_SIZE: f32 = 22.0;
    pub const CHIP_METER_H: f32 = 4.0;
    pub const CHIP_METER_INSET_X: f32 = 6.0;
    pub const CHIP_METER_BOTTOM_INSET: f32 = 3.0;
    pub const MUTED_STRIKE_H: f32 = 2.5;
}

/// Declarative layout spec for the Now Playing 4-shade Smart DJ transition telemetry pill
/// (`[DJ SYNC · 124→126 BPM]` / `[DJ WASH › NEXT]`).
pub mod dj_status_spec {
    pub const PILL_H: f32 = 34.0;
    pub const PILL_RADIUS: f32 = 4.0;
    pub const PILL_STROKE: f32 = 2.0;
    pub const PILL_PAD_X: f32 = 12.0;
    pub const PILL_GAP_X: f32 = 12.0;
    pub const FONT_SIZE: f32 = 22.0;
    pub const PROGRESS_BAR_H: f32 = 3.0;
    pub const PROGRESS_INSET_X: f32 = 4.0;
    pub const PROGRESS_BOTTOM_INSET: f32 = 2.0;
    pub const ALT_TOP_Y: f32 = 18.0;
}

/// Declarative layout spec for the `Now Playing -> Channels` live APU toggle screen.
pub mod nsf_channels_spec {
    pub const ROW_HEIGHT: f32 = 60.0;
    pub const ROW_GAP: f32 = 12.0;
    pub const ROW_RADIUS: f32 = 8.0;
    pub const ROW_PAD_X: f32 = 16.0;

    pub const CODE_BADGE_W: f32 = 72.0;
    pub const CODE_BADGE_H: f32 = 36.0;
    pub const CODE_BADGE_RADIUS: f32 = 6.0;
    pub const CODE_BADGE_STROKE: f32 = 2.0;
    pub const CODE_FONT_SIZE: f32 = 22.0;
    pub const CODE_GAP_RIGHT: f32 = 16.0;

    pub const NAME_FONT_SIZE: f32 = 42.0;
    pub const SOLO_FONT_SIZE: f32 = 20.0;
    pub const SOLO_BADGE_H: f32 = 28.0;
    pub const SOLO_BADGE_PAD_X: f32 = 8.0;
    pub const SOLO_GAP_X: f32 = 12.0;

    // Live `ApuFrameTelemetry` activity meter bar next to channel name
    pub const METER_W: f32 = 148.0;
    pub const METER_H: f32 = 18.0;
    pub const METER_RADIUS: f32 = 9.0;
    pub const METER_STROKE: f32 = 2.5;
    pub const METER_INSET: f32 = 3.0;
    pub const METER_GAP_X: f32 = 20.0;

    // Right-hand `[ON]`/`[OFF]` pill + `toggle` switch (`64x32`)
    pub const STATE_TEXT_FONT_SIZE: f32 = 28.0;
    pub const STATE_TEXT_GAP_X: f32 = 12.0;
    pub const TOGGLE_W: f32 = 64.0;
    pub const TOGGLE_H: f32 = 32.0;

    pub const FOOTER_H: f32 = 32.0;
    pub const FOOTER_FONT_SIZE: f32 = 24.0;
    pub const VISIBLE_ROWS_DEFAULT: usize = 8;
}

/// Responsive logical canvas metrics derived from physical drawable pixels `(phys_w, phys_h)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalMetrics {
    pub phys_w: u32,
    pub phys_h: u32,
    pub screen_w: f32,
    pub screen_h: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub margin_x: f32,
    pub visible_rows: usize,
}

impl LogicalMetrics {
    /// Computes responsive logical dimensions and HiDPI scale factors for any screen size.
    ///
    /// - Landscape (`aspect >= 1.2`, covering `4:3`, `3:2`, `16:10`, `16:9`, `21:9`):
    ///   locks `screen_h = 768.0` and expands `screen_w = round(768.0 * aspect)` (`1024.0` at `4:3`,
    ///   `1365.0` at `16:9` `1920x1080` with `scale = 1.40625x`).
    /// - Square & Portrait (`aspect < 1.2`, e.g. `1:1` `720x720` or `9:16` `1080x1920`):
    ///   locks `screen_w = 920.0` (so header badges and metadata never crowd) and expands
    ///   `screen_h = round(920.0 / aspect)`, dynamically increasing `visible_rows`.
    pub fn from_physical(phys_w: u32, phys_h: u32) -> Self {
        let pw = phys_w.max(1);
        let ph = phys_h.max(1);
        let aspect = (pw as f32) / (ph as f32);

        let (screen_w, screen_h) = if (aspect - (4.0 / 3.0)).abs() < 0.01 {
            (SCREEN_W, SCREEN_H)
        } else if aspect >= 1.2 {
            let w = (SCREEN_H * aspect).round().max(920.0);
            (w, SCREEN_H)
        } else {
            let base_w = 920.0_f32;
            let h = (base_w / aspect.max(0.35)).round().max(SCREEN_H);
            (base_w, h)
        };

        let margin_x = horizontal_margin(screen_w);
        let visible_rows = compute_visible_list_rows(screen_h);

        Self {
            phys_w: pw,
            phys_h: ph,
            screen_w,
            screen_h,
            scale_x: (pw as f32) / screen_w,
            scale_y: (ph as f32) / screen_h,
            margin_x,
            visible_rows,
        }
    }
}

#[inline]
pub fn horizontal_margin(screen_w: f32) -> f32 {
    if screen_w <= 1040.0 {
        32.0
    } else {
        (32.0 + (screen_w - 1024.0) * 0.045).clamp(32.0, 56.0).round()
    }
}

#[inline]
pub fn compute_visible_list_rows(screen_h: f32) -> usize {
    if (screen_h - SCREEN_H).abs() < 1.0 {
        return LIST_VISIBLE_ROWS;
    }
    // Header + top/bottom 32px margins + 32px content top pad leaves `screen_h - 176.0`
    // for `N * 60 + (N - 1) * 16` (`76 * N - 16 <= avail_h`).
    let avail_h = (screen_h - 176.0).max(300.0);
    (((avail_h + 16.0) / 76.0).floor() as usize).clamp(5, 20)
}

/// Computed absolute screen-space rectangle `(x, y, width, height)`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LayoutBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl LayoutBox {
    #[inline]
    pub fn center_x(self) -> f32 {
        self.x + self.w * 0.5
    }

    #[inline]
    pub fn center_y(self) -> f32 {
        self.y + self.h * 0.5
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HeaderBoxes {
    pub container: LayoutBox,
    pub left_slot: LayoutBox,
    pub title_slot: LayoutBox,
    pub right_slot: LayoutBox,
    pub border_y: f32,
}

#[derive(Debug, Clone)]
pub struct ListScreenBoxes {
    pub header: HeaderBoxes,
    pub content_frame: LayoutBox,
    pub list_column: LayoutBox,
    pub rows: Vec<LayoutBox>,
    pub scrollbar: Option<LayoutBox>,
}

#[derive(Debug, Clone, Copy)]
pub struct NowPlayingBoxes {
    pub header: HeaderBoxes,
    pub content_frame: LayoutBox,
    pub subheader_row: LayoutBox,
    pub art_box: LayoutBox,
    pub meta_col: LayoutBox,
    pub title_box: LayoutBox,
    pub artist_box: LayoutBox,
    pub album_box: LayoutBox,
    pub specs_row: LayoutBox,
    pub seekbar_box: LayoutBox,
    pub seekbar_track: LayoutBox,
    pub seekbar_labels: LayoutBox,
}

fn abs_box(tree: &TaffyTree, node: NodeId, parent_x: f32, parent_y: f32) -> LayoutBox {
    let l = tree.layout(node).expect("computed layout");
    LayoutBox {
        x: parent_x + l.location.x,
        y: parent_y + l.location.y,
        w: l.size.width,
        h: l.size.height,
    }
}

fn build_header_nodes(tree: &mut TaffyTree) -> (NodeId, NodeId, NodeId, NodeId) {
    // Frame 4 [1572]: width=85, height=48 (holds 48x48 play-mode icon)
    let left_slot = tree
        .new_leaf(Style {
            size: Size {
                width: px(85.0),
                height: px(48.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .unwrap();

    let title_slot = tree
        .new_leaf(Style {
            size: Size {
                width: auto(),
                height: px(60.0),
            },
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..Default::default()
        })
        .unwrap();

    // Battery [1564]: width=64, height=64
    let right_slot = tree
        .new_leaf(Style {
            size: Size {
                width: px(64.0),
                height: px(64.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .unwrap();

    // Header [271]: width=fill, height=80, padding_bottom=16, spaceBetween, center
    let header = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                justify_content: Some(JustifyContent::SpaceBetween),
                align_items: Some(AlignItems::Center),
                size: Size {
                    width: pct(1.0),
                    height: px(80.0),
                },
                padding: Rect {
                    left: px(0.0),
                    right: px(0.0),
                    top: px(0.0),
                    bottom: px(16.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            &[left_slot, title_slot, right_slot],
        )
        .unwrap();

    (header, left_slot, title_slot, right_slot)
}

fn extract_header_boxes(
    tree: &TaffyTree,
    nodes: (NodeId, NodeId, NodeId, NodeId),
    root_x: f32,
    root_y: f32,
) -> HeaderBoxes {
    let (header, left_slot, title_slot, right_slot) = nodes;
    let container = abs_box(tree, header, root_x, root_y);
    let left = abs_box(tree, left_slot, container.x, container.y);
    let title = abs_box(tree, title_slot, container.x, container.y);
    let right = abs_box(tree, right_slot, container.x, container.y);
    HeaderBoxes {
        container,
        left_slot: left,
        title_slot: title,
        right_slot: right,
        // 4px outside bottom stroke starts right at container.y + container.h (= 112.0)
        border_y: container.y + container.h,
    }
}

/// Computes standalone `Header` layout at `(margin_x, 32, screen_w - 2*margin_x, 80)`.
pub fn compute_header_layout_sized(screen_w: f32, screen_h: f32) -> HeaderBoxes {
    let mx = horizontal_margin(screen_w);
    let mut tree = TaffyTree::new();
    let h_nodes = build_header_nodes(&mut tree);
    let root = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                size: Size {
                    width: px(screen_w),
                    height: px(screen_h),
                },
                padding: Rect {
                    left: px(mx),
                    right: px(mx),
                    top: px(32.0),
                    bottom: px(32.0),
                },
                ..Default::default()
            },
            &[h_nodes.0],
        )
        .unwrap();

    tree.compute_layout(
        root,
        Size {
            width: AvailableSpace::Definite(screen_w),
            height: AvailableSpace::Definite(screen_h),
        },
    )
    .unwrap();

    extract_header_boxes(&tree, h_nodes, 0.0, 0.0)
}

/// Computes standalone `Header` layout at `(32, 32, 960, 80)` inside `1024x768`.
pub fn compute_header_layout() -> HeaderBoxes {
    compute_header_layout_sized(SCREEN_W, SCREEN_H)
}

/// Computes responsive `Main` / `Source` list screen layout for `(screen_w, screen_h)`.
pub fn compute_list_layout_sized(
    screen_w: f32,
    screen_h: f32,
    row_count: usize,
    max_visible_rows: usize,
    show_scrollbar: bool,
) -> ListScreenBoxes {
    let mx = horizontal_margin(screen_w);
    let mut tree = TaffyTree::new();
    let h_nodes = build_header_nodes(&mut tree);

    let capped_rows = row_count.min(max_visible_rows.max(1));
    let mut row_nodes = Vec::with_capacity(capped_rows);
    for _ in 0..capped_rows {
        let row = tree
            .new_leaf(Style {
                size: Size {
                    width: pct(1.0),
                    height: px(60.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        row_nodes.push(row);
    }

    // Frame 17 [340]: vertical column of MenuItems, gap = 16.0, flex_grow = 1.0
    let list_col = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                gap: Size {
                    width: px(0.0),
                    height: px(16.0),
                },
                flex_grow: 1.0,
                flex_shrink: 1.0,
                ..Default::default()
            },
            &row_nodes,
        )
        .unwrap();

    let mut content_children = vec![list_col];
    let sb_node = if show_scrollbar {
        let sb = tree
            .new_leaf(Style {
                size: Size {
                    width: px(16.0),
                    height: auto(),
                },
                align_self: Some(AlignSelf::Stretch),
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        content_children.push(sb);
        Some(sb)
    } else {
        None
    };

    let content_frame = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: Some(AlignItems::FlexStart),
                gap: Size {
                    width: px(16.0),
                    height: px(0.0),
                },
                padding: Rect {
                    left: px(0.0),
                    right: px(0.0),
                    top: px(32.0),
                    bottom: px(0.0),
                },
                size: Size {
                    width: pct(1.0),
                    height: auto(),
                },
                min_size: Size {
                    width: auto(),
                    height: px(0.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            },
            &content_children,
        )
        .unwrap();

    let root = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                size: Size {
                    width: px(screen_w),
                    height: px(screen_h),
                },
                padding: Rect {
                    left: px(mx),
                    right: px(mx),
                    top: px(32.0),
                    bottom: px(32.0),
                },
                ..Default::default()
            },
            &[h_nodes.0, content_frame],
        )
        .unwrap();

    tree.compute_layout(
        root,
        Size {
            width: AvailableSpace::Definite(screen_w),
            height: AvailableSpace::Definite(screen_h),
        },
    )
    .unwrap();

    let header = extract_header_boxes(&tree, h_nodes, 0.0, 0.0);
    let cf_box = abs_box(&tree, content_frame, 0.0, 0.0);
    let lc_box = abs_box(&tree, list_col, cf_box.x, cf_box.y);
    let rows = row_nodes
        .into_iter()
        .map(|rn| abs_box(&tree, rn, lc_box.x, lc_box.y))
        .collect();
    let scrollbar = sb_node.map(|sn| abs_box(&tree, sn, cf_box.x, cf_box.y));

    ListScreenBoxes {
        header,
        content_frame: cf_box,
        list_column: lc_box,
        rows,
        scrollbar,
    }
}

/// Computes the canonical `1024x768` `Main` / `Source` list screen layout (`[215]` / `[646]`).
pub fn compute_list_layout(row_count: usize, show_scrollbar: bool) -> ListScreenBoxes {
    compute_list_layout_sized(SCREEN_W, SCREEN_H, row_count, LIST_VISIBLE_ROWS, show_scrollbar)
}

/// Computes responsive `Now Playing` screen layout for `(screen_w, screen_h)`.
pub fn compute_now_playing_layout_sized(screen_w: f32, screen_h: f32) -> NowPlayingBoxes {
    let mx = horizontal_margin(screen_w);
    let is_exact_43 = (screen_w - SCREEN_W).abs() < 0.5 && (screen_h - SCREEN_H).abs() < 0.5;
    let is_portrait = screen_h > screen_w * 1.12;

    let mut tree = TaffyTree::new();
    let h_nodes = build_header_nodes(&mut tree);

    // 1. Sub-header row Frame 16 [247]: height = 40
    let subheader = tree
        .new_leaf(Style {
            size: Size {
                width: pct(1.0),
                height: px(40.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .unwrap();

    // 2. Art + Metadata block
    let art_size = if is_exact_43 {
        256.0
    } else if is_portrait {
        (screen_w * 0.54).clamp(240.0, 420.0).round()
    } else {
        (256.0 + (screen_w - SCREEN_W).max(0.0) * 0.095)
            .clamp(256.0, 304.0)
            .round()
    };

    let art_node = tree
        .new_leaf(Style {
            size: Size {
                width: px(art_size),
                height: px(art_size),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .unwrap();

    let title_node = tree
        .new_leaf(Style {
            size: Size {
                width: pct(1.0),
                height: px(60.0),
            },
            ..Default::default()
        })
        .unwrap();
    let artist_node = tree
        .new_leaf(Style {
            size: Size {
                width: pct(1.0),
                height: px(45.0),
            },
            ..Default::default()
        })
        .unwrap();
    let album_node = tree
        .new_leaf(Style {
            size: Size {
                width: pct(1.0),
                height: px(63.0),
            },
            ..Default::default()
        })
        .unwrap();
    let specs_node = tree
        .new_leaf(Style {
            size: Size {
                width: pct(1.0),
                height: px(34.0),
            },
            ..Default::default()
        })
        .unwrap();

    let meta_col = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                size: Size {
                    width: if is_portrait { pct(1.0) } else { auto() },
                    height: px(202.0),
                },
                flex_grow: if is_portrait { 0.0 } else { 1.0 },
                flex_shrink: 1.0,
                ..Default::default()
            },
            &[title_node, artist_node, album_node, specs_node],
        )
        .unwrap();

    let art_gap = if is_exact_43 {
        32.0
    } else {
        (32.0 + (screen_w - SCREEN_W).max(0.0) * 0.04).clamp(32.0, 48.0).round()
    };

    let art_meta_row = if is_portrait {
        tree.new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                align_items: Some(AlignItems::Center),
                gap: Size {
                    width: px(0.0),
                    height: px(36.0),
                },
                size: Size {
                    width: pct(1.0),
                    height: auto(),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            &[art_node, meta_col],
        )
        .unwrap()
    } else {
        tree.new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: Some(AlignItems::Center),
                gap: Size {
                    width: px(art_gap),
                    height: px(0.0),
                },
                size: Size {
                    width: pct(1.0),
                    height: px(art_size),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            &[art_node, meta_col],
        )
        .unwrap()
    };

    // 3. Seekbar [640]: height = 74 -> track 24 + gap 10 + labels 40
    let seek_track = tree
        .new_leaf(Style {
            size: Size {
                width: pct(1.0),
                height: px(24.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .unwrap();
    let seek_labels = tree
        .new_leaf(Style {
            size: Size {
                width: pct(1.0),
                height: px(40.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        })
        .unwrap();
    let seekbar = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                gap: Size {
                    width: px(0.0),
                    height: px(10.0),
                },
                size: Size {
                    width: pct(1.0),
                    height: px(74.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            &[seek_track, seek_labels],
        )
        .unwrap();

    // At 1024x768, preserve exact 72px vertical gap; on dynamic sizes, distribute cleanly
    let content_style = if is_exact_43 {
        Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            gap: Size {
                width: px(0.0),
                height: px(72.0),
            },
            padding: Rect {
                left: px(0.0),
                right: px(0.0),
                top: px(32.0),
                bottom: px(0.0),
            },
            size: Size {
                width: pct(1.0),
                height: auto(),
            },
            min_size: Size {
                width: auto(),
                height: px(0.0),
            },
            flex_grow: 1.0,
            ..Default::default()
        }
    } else {
        Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            justify_content: Some(JustifyContent::SpaceBetween),
            padding: Rect {
                left: px(0.0),
                right: px(0.0),
                top: px(32.0),
                bottom: px(24.0),
            },
            size: Size {
                width: pct(1.0),
                height: auto(),
            },
            min_size: Size {
                width: auto(),
                height: px(0.0),
            },
            flex_grow: 1.0,
            ..Default::default()
        }
    };

    let content_frame = tree
        .new_with_children(content_style, &[subheader, art_meta_row, seekbar])
        .unwrap();

    let root = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                size: Size {
                    width: px(screen_w),
                    height: px(screen_h),
                },
                padding: Rect {
                    left: px(mx),
                    right: px(mx),
                    top: px(32.0),
                    bottom: px(32.0),
                },
                ..Default::default()
            },
            &[h_nodes.0, content_frame],
        )
        .unwrap();

    tree.compute_layout(
        root,
        Size {
            width: AvailableSpace::Definite(screen_w),
            height: AvailableSpace::Definite(screen_h),
        },
    )
    .unwrap();

    let header = extract_header_boxes(&tree, h_nodes, 0.0, 0.0);
    let cf = abs_box(&tree, content_frame, 0.0, 0.0);
    let sh = abs_box(&tree, subheader, cf.x, cf.y);
    let am = abs_box(&tree, art_meta_row, cf.x, cf.y);
    let art = abs_box(&tree, art_node, am.x, am.y);
    let mc = abs_box(&tree, meta_col, am.x, am.y);
    let tb = abs_box(&tree, title_node, mc.x, mc.y);
    let arb = abs_box(&tree, artist_node, mc.x, mc.y);
    let alb = abs_box(&tree, album_node, mc.x, mc.y);
    let spb = abs_box(&tree, specs_node, mc.x, mc.y);
    let sb = abs_box(&tree, seekbar, cf.x, cf.y);
    let sbt = abs_box(&tree, seek_track, sb.x, sb.y);
    let sbl = abs_box(&tree, seek_labels, sb.x, sb.y);

    NowPlayingBoxes {
        header,
        content_frame: cf,
        subheader_row: sh,
        art_box: art,
        meta_col: mc,
        title_box: tb,
        artist_box: arb,
        album_box: alb,
        specs_row: spb,
        seekbar_box: sb,
        seekbar_track: sbt,
        seekbar_labels: sbl,
    }
}

/// Computes the canonical `1024x768` `Now Playing` screen layout (`[233]`).
pub fn compute_now_playing_layout() -> NowPlayingBoxes {
    compute_now_playing_layout_sized(SCREEN_W, SCREEN_H)
}

#[derive(Debug, Clone, Copy)]
pub struct NowPlayingAltBoxes {
    pub art_box: LayoutBox,
    pub seekbar_track: LayoutBox,
    pub seekbar_labels: LayoutBox,
}

/// Computes responsive `Now Playing (Alt)` layout for `(screen_w, screen_h)`.
pub fn compute_now_playing_alt_layout_sized(screen_w: f32, screen_h: f32) -> NowPlayingAltBoxes {
    if (screen_w - SCREEN_W).abs() < 0.5 && (screen_h - SCREEN_H).abs() < 0.5 {
        return NowPlayingAltBoxes {
            art_box: LayoutBox {
                x: 245.0,
                y: 64.0,
                w: 534.0,
                h: 534.0,
            },
            seekbar_track: LayoutBox {
                x: 32.0,
                y: 670.0,
                w: 960.0,
                h: 24.0,
            },
            seekbar_labels: LayoutBox {
                x: 32.0,
                y: 704.0,
                w: 960.0,
                h: 40.0,
            },
        };
    }

    let mx = horizontal_margin(screen_w);
    let inner_w = (screen_w - mx * 2.0).max(200.0);
    let seek_track_y = (screen_h - 98.0).max(280.0);
    let seek_labels_y = seek_track_y + 34.0;
    let avail_art_h = (seek_track_y - 96.0).max(200.0);
    let art_size = avail_art_h.min(inner_w).clamp(200.0, 680.0).round();
    let art_x = ((screen_w - art_size) * 0.5).round();
    let art_y = ((seek_track_y - art_size) * 0.5).max(32.0).round();

    NowPlayingAltBoxes {
        art_box: LayoutBox {
            x: art_x,
            y: art_y,
            w: art_size,
            h: art_size,
        },
        seekbar_track: LayoutBox {
            x: mx,
            y: seek_track_y,
            w: inner_w,
            h: 24.0,
        },
        seekbar_labels: LayoutBox {
            x: mx,
            y: seek_labels_y,
            w: inner_w,
            h: 40.0,
        },
    }
}

/// Computes `Now Playing (Alt)` layout (`[1805]`: `534x534` cover at `(245, 64)` + `Seekbar` at `(32, 670)`).
pub fn compute_now_playing_alt_layout() -> NowPlayingAltBoxes {
    compute_now_playing_alt_layout_sized(SCREEN_W, SCREEN_H)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_figcraft_list_coordinates() {
        let l = compute_list_layout(8, true);
        assert_eq!((l.header.container.x, l.header.container.y), (32.0, 32.0));
        assert_eq!((l.header.container.w, l.header.container.h), (960.0, 80.0));
        assert_eq!(l.header.border_y, 112.0);
        assert_eq!((l.list_column.x, l.list_column.y), (32.0, 144.0));
        assert_eq!(l.list_column.w, 928.0);
        assert_eq!(l.rows.len(), 8);
        assert_eq!(
            (l.rows[0].x, l.rows[0].y, l.rows[0].w, l.rows[0].h),
            (32.0, 144.0, 928.0, 60.0)
        );
        assert_eq!(l.rows[7].y, 144.0 + 7.0 * 76.0);
        let sb = l.scrollbar.unwrap();
        assert_eq!((sb.x, sb.y, sb.w, sb.h), (976.0, 144.0, 16.0, 592.0));

        let l_no_sb = compute_list_layout(8, false);
        assert!(l_no_sb.scrollbar.is_none());
        assert_eq!(l_no_sb.list_column.w, 960.0);
        assert_eq!(
            (
                l_no_sb.rows[0].x,
                l_no_sb.rows[0].y,
                l_no_sb.rows[0].w,
                l_no_sb.rows[0].h
            ),
            (32.0, 144.0, 960.0, 60.0)
        );
    }

    #[test]
    fn verify_figcraft_now_playing_coordinates() {
        let np = compute_now_playing_layout();
        assert_eq!(
            (
                np.subheader_row.x,
                np.subheader_row.y,
                np.subheader_row.w,
                np.subheader_row.h
            ),
            (32.0, 144.0, 960.0, 40.0)
        );
        assert_eq!(
            (np.art_box.x, np.art_box.y, np.art_box.w, np.art_box.h),
            (32.0, 256.0, 256.0, 256.0)
        );
        assert_eq!(
            (np.meta_col.x, np.meta_col.y, np.meta_col.w, np.meta_col.h),
            (320.0, 283.0, 672.0, 202.0)
        );
        assert_eq!(
            (
                np.seekbar_box.x,
                np.seekbar_box.y,
                np.seekbar_box.w,
                np.seekbar_box.h
            ),
            (32.0, 584.0, 960.0, 74.0)
        );
        assert_eq!(
            (
                np.seekbar_track.x,
                np.seekbar_track.y,
                np.seekbar_track.w,
                np.seekbar_track.h
            ),
            (32.0, 584.0, 960.0, 24.0)
        );
    }

    #[test]
    fn verify_responsive_metrics_across_screen_shapes() {
        // 16:9 Retroid Pocket 6 (1920x1080)
        let rp6 = LogicalMetrics::from_physical(1920, 1080);
        assert_eq!(rp6.screen_h, 768.0);
        assert_eq!(rp6.screen_w, 1365.0);
        assert_eq!(rp6.visible_rows, 8);
        let l169 = compute_list_layout_sized(rp6.screen_w, rp6.screen_h, 8, rp6.visible_rows, true);
        assert!(l169.rows[0].w > 1200.0);

        // 1:1 Square Handheld (720x720)
        let sq = LogicalMetrics::from_physical(720, 720);
        assert_eq!(sq.screen_w, 920.0);
        assert_eq!(sq.screen_h, 920.0);
        assert!(sq.visible_rows >= 9);

        // 9:16 Portrait (1080x1920)
        let port = LogicalMetrics::from_physical(1080, 1920);
        assert_eq!(port.screen_w, 920.0);
        assert!(port.screen_h > 1500.0);
        assert!(port.visible_rows >= 16);
        let np_port = compute_now_playing_layout_sized(port.screen_w, port.screen_h);
        assert!(np_port.meta_col.y > np_port.art_box.y + np_port.art_box.h);
    }

    #[test]
    fn verify_nsf_phase3_layout_specs_fit_1024x768_without_overlap() {
        let np = compute_now_playing_layout();

        // 1. Compact APU strip sits cleanly between art/meta and seekbar with >= 18px clearance
        let art_bottom = np.art_box.y + np.art_box.h;
        let meta_bottom = np.meta_col.y + np.meta_col.h;
        let strip_top = art_bottom + nsf_now_playing_spec::STRIP_TOP_OFFSET_FROM_ART;
        let strip_bottom = strip_top + nsf_now_playing_spec::CHIP_H;
        assert!(strip_top >= art_bottom + 18.0);
        assert!(strip_top >= meta_bottom + 18.0);
        assert!(strip_bottom + 18.0 <= np.seekbar_box.y);

        // 8 expansion pills (`2A03` + `VRC6`/`MMC5`) fit strictly inside 960px content width
        let eight_pills_w =
            8.0 * 88.0 + 7.0 * nsf_now_playing_spec::CHIP_GAP_X;
        assert!(eight_pills_w <= np.seekbar_box.w);

        // 2. Famicom cartridge geometry fits strictly inside the 256x256 reference unit box
        assert!(famicom_cart_spec::CART_X >= 0.0);
        assert!(famicom_cart_spec::CART_X + famicom_cart_spec::CART_W <= famicom_cart_spec::REF_SIZE);
        assert!(famicom_cart_spec::SKIRT_Y + famicom_cart_spec::SKIRT_H <= famicom_cart_spec::REF_SIZE);
        assert!(famicom_cart_spec::LABEL_X >= famicom_cart_spec::CART_X);
        assert!(famicom_cart_spec::LABEL_X + famicom_cart_spec::LABEL_W <= famicom_cart_spec::CART_X + famicom_cart_spec::CART_W);
        assert!(famicom_cart_spec::LABEL_Y + famicom_cart_spec::LABEL_H <= famicom_cart_spec::SKIRT_Y);

        // 3. NSF Channels row controls leave >= 400px unclipped label room even with scrollbar visible
        let list_with_sb = compute_list_layout(7, true);
        let row_w = list_with_sb.rows[0].w;
        let left_cluster_w = nsf_channels_spec::ROW_PAD_X
            + nsf_channels_spec::CODE_BADGE_W
            + nsf_channels_spec::CODE_GAP_RIGHT;
        let right_cluster_w = nsf_channels_spec::ROW_PAD_X
            + nsf_channels_spec::TOGGLE_W
            + nsf_channels_spec::STATE_TEXT_GAP_X
            + 72.0 // "[OFF]" text width upper bound
            + nsf_channels_spec::METER_GAP_X
            + nsf_channels_spec::METER_W;
        let clear_label_w = row_w - left_cluster_w - right_cluster_w;
        assert!(clear_label_w >= 400.0, "clear_label_w was {clear_label_w}");
    }
}
