use uipod_ui::ThemeColors;

pub const UIPOD_INK_DARK: u32 = 0x181C18;
pub const UIPOD_INK_LIGHT: u32 = 0xFFFFFF;

#[derive(Debug, Clone, Copy)]
pub struct ThemePreset {
    pub name: &'static str,
    pub colors: ThemeColors,
}

#[inline]
pub const fn lerp_rgb(c0: u32, c1: u32, pct: u32) -> u32 {
    let r0 = (c0 >> 16) & 0xFF;
    let g0 = (c0 >> 8) & 0xFF;
    let b0 = c0 & 0xFF;
    let r1 = (c1 >> 16) & 0xFF;
    let g1 = (c1 >> 8) & 0xFF;
    let b1 = c1 & 0xFF;
    let r = (r0 * (100 - pct) + r1 * pct + 50) / 100;
    let g = (g0 * (100 - pct) + g1 * pct + 50) / 100;
    let b = (b0 * (100 - pct) + b1 * pct + 50) / 100;
    (r << 16) | (g << 8) | b
}

#[inline]
pub const fn derive_4shade(bg: u32, fg: u32) -> ThemeColors {
    ThemeColors {
        bg_rgb: bg,
        fg_rgb: fg,
        fg1_rgb: lerp_rgb(bg, fg, 30),
        fg2_rgb: lerp_rgb(bg, fg, 78),
    }
}

const fn auto(name: &'static str, bg: u32, fg: u32) -> ThemePreset {
    ThemePreset {
        name,
        colors: derive_4shade(bg, fg),
    }
}

/// Exact 36 built-in 4-shade iPod Mini themes from `apps/menus/power_menu.c`.
pub const THEME_PRESETS: &[ThemePreset] = &[
    ThemePreset {
        name: "UIPod Classic",
        colors: ThemeColors {
            bg_rgb: 0xD6D8D5,
            fg1_rgb: 0xA8A8A8,
            fg2_rgb: 0x595959,
            fg_rgb: 0x3C3637,
        },
    },
    ThemePreset {
        name: "UIPod Classic Dark",
        colors: ThemeColors {
            bg_rgb: 0x1E1B1C,
            fg1_rgb: 0x555253,
            fg2_rgb: 0xAEAFAD,
            fg_rgb: 0xD6D8D5,
        },
    },
    auto("Classic Dark", 0xD8DCD0, UIPOD_INK_DARK),
    auto("Classic Light", 0xD8DCD0, UIPOD_INK_LIGHT),
    auto("Classic Sepia", 0xD8DCD0, 0x5D4037),
    auto("Silver Dark", 0xC4C7CC, UIPOD_INK_DARK),
    auto("Silver Light", 0xC4C7CC, UIPOD_INK_LIGHT),
    auto("Silver Slate", 0xC4C7CC, 0x37474F),
    auto("Slate Dark", 0x637D8C, UIPOD_INK_DARK),
    auto("Slate Light", 0x637D8C, UIPOD_INK_LIGHT),
    auto("Slate Gold", 0x637D8C, 0xFFD97D),
    auto("Blue Dark", 0x0094E1, UIPOD_INK_DARK),
    auto("Blue Light", 0x0094E1, UIPOD_INK_LIGHT),
    auto("Blue Navy", 0x0094E1, 0x002B4A),
    auto("Green Dark", 0xA0CB3B, UIPOD_INK_DARK),
    auto("Green Light", 0xA0CB3B, UIPOD_INK_LIGHT),
    auto("Green Forest", 0xA0CB3B, 0x2E4A1C),
    auto("Yellow Dark", 0xFFD400, UIPOD_INK_DARK),
    auto("Yellow Light", 0xFFD400, UIPOD_INK_LIGHT),
    auto("Yellow Navy", 0xFFD400, 0x1D3557),
    auto("Gold Dark", 0xFAB71F, UIPOD_INK_DARK),
    auto("Gold Light", 0xFAB71F, UIPOD_INK_LIGHT),
    auto("Gold Espresso", 0xFAB71F, 0x4E342E),
    auto("Orange Dark", 0xF08A1C, UIPOD_INK_DARK),
    auto("Orange Light", 0xF08A1C, UIPOD_INK_LIGHT),
    auto("Orange Navy", 0xF08A1C, 0x1D3557),
    auto("Red Dark", 0xEA323C, UIPOD_INK_DARK),
    auto("Red Light", 0xEA323C, UIPOD_INK_LIGHT),
    auto("Red Cream", 0xEA323C, 0xFFF3E0),
    auto("Pink Dark", 0xEC5298, UIPOD_INK_DARK),
    auto("Pink Light", 0xEC5298, UIPOD_INK_LIGHT),
    auto("Pink Berry", 0xEC5298, 0x4A0728),
    auto("Purple Dark", 0x9B72B0, UIPOD_INK_DARK),
    auto("Purple Light", 0x9B72B0, UIPOD_INK_LIGHT),
    auto("Purple Plum", 0x9B72B0, 0x2A1438),
    auto("Mono Green Dark", 0xBFCBB0, UIPOD_INK_DARK),
    auto("Mono Green Light", 0xBFCBB0, UIPOD_INK_LIGHT),
    ThemePreset {
        name: "Mono Green Pixel",
        colors: ThemeColors {
            bg_rgb: 0xBFCBB0,
            fg1_rgb: 0x8BAC0F,
            fg2_rgb: 0x306230,
            fg_rgb: 0x0F380F,
        },
    },
];

pub fn parse_hex_rgb(input: &str) -> Option<u32> {
    let cleaned = input.trim().trim_start_matches('#');
    if cleaned.len() != 6 {
        return None;
    }
    u32::from_str_radix(cleaned, 16).ok()
}
