//! Flexbox screen layouts powered by `taffy`, matching the Auto-Layout hierarchy of `ui.figcraft`.

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

    // Header [271]: width=fill (960), height=80, padding_bottom=16, spaceBetween, center
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

/// Computes standalone `Header` layout at `(32, 32, 960, 80)` inside `1024x768`.
pub fn compute_header_layout() -> HeaderBoxes {
    let mut tree = TaffyTree::new();
    let h_nodes = build_header_nodes(&mut tree);
    let root = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                size: Size {
                    width: px(SCREEN_W),
                    height: px(SCREEN_H),
                },
                padding: Rect {
                    left: px(32.0),
                    right: px(32.0),
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
            width: AvailableSpace::Definite(SCREEN_W),
            height: AvailableSpace::Definite(SCREEN_H),
        },
    )
    .unwrap();

    extract_header_boxes(&tree, h_nodes, 0.0, 0.0)
}

/// Computes the `Main` / `Source` list screen layout (`[215]` / `[646]`).
pub fn compute_list_layout(row_count: usize, show_scrollbar: bool) -> ListScreenBoxes {
    let mut tree = TaffyTree::new();
    let h_nodes = build_header_nodes(&mut tree);

    let capped_rows = row_count.min(LIST_VISIBLE_ROWS);
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
        // Scrollbar [312]: width = 16.0, stretch vertically inside Frame 3 (624 - 32 = 592)
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

    // Frame 3 [228]: width=fill (960), flex_grow=1 (624), padding_top=32, horizontal gap=16
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
                    width: px(SCREEN_W),
                    height: px(SCREEN_H),
                },
                padding: Rect {
                    left: px(32.0),
                    right: px(32.0),
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
            width: AvailableSpace::Definite(SCREEN_W),
            height: AvailableSpace::Definite(SCREEN_H),
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

/// Computes the `Now Playing` screen layout (`[233]`).
pub fn compute_now_playing_layout() -> NowPlayingBoxes {
    let mut tree = TaffyTree::new();
    let h_nodes = build_header_nodes(&mut tree);

    // 1. Sub-header row Frame 16 [247]: 960 x 40
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

    // 2. Art + Metadata row Frame 5 [251]: height = 256, gap = 32, align_items = Center
    let art_node = tree
        .new_leaf(Style {
            size: Size {
                width: px(256.0),
                height: px(256.0),
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
                    width: auto(),
                    height: px(202.0),
                },
                flex_grow: 1.0,
                flex_shrink: 1.0,
                ..Default::default()
            },
            &[title_node, artist_node, album_node, specs_node],
        )
        .unwrap();

    let art_meta_row = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Row,
                align_items: Some(AlignItems::Center),
                gap: Size {
                    width: px(32.0),
                    height: px(0.0),
                },
                size: Size {
                    width: pct(1.0),
                    height: px(256.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            &[art_node, meta_col],
        )
        .unwrap();

    // 3. Seekbar [640]: 960 x 74, vertical gap = 10 -> track 960x24 + labels 960x40
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

    // Frame 3 [246]: padding_top = 32, vertical gap = 72
    let content_frame = tree
        .new_with_children(
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
            },
            &[subheader, art_meta_row, seekbar],
        )
        .unwrap();

    let root = tree
        .new_with_children(
            Style {
                display: Display::Flex,
                flex_direction: FlexDirection::Column,
                size: Size {
                    width: px(SCREEN_W),
                    height: px(SCREEN_H),
                },
                padding: Rect {
                    left: px(32.0),
                    right: px(32.0),
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
            width: AvailableSpace::Definite(SCREEN_W),
            height: AvailableSpace::Definite(SCREEN_H),
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

#[derive(Debug, Clone, Copy)]
pub struct NowPlayingAltBoxes {
    pub art_box: LayoutBox,
    pub seekbar_track: LayoutBox,
    pub seekbar_labels: LayoutBox,
}

/// Computes `Now Playing (Alt)` layout (`[1805]`: `534x534` cover at `(245, 64)` + `Seekbar` at `(32, 670)`).
pub fn compute_now_playing_alt_layout() -> NowPlayingAltBoxes {
    NowPlayingAltBoxes {
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
    }
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
        assert_eq!(l.rows[7].y, 144.0 + 7.0 * 76.0); // 676.0 -> ends at 736.0
        let sb = l.scrollbar.unwrap();
        assert_eq!((sb.x, sb.y, sb.w, sb.h), (976.0, 144.0, 16.0, 592.0));

        // Without scrollbar (total_items <= 8): MenuItems stretch to full 960px width
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
}
