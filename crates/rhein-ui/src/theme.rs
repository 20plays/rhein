//! Colour, type and spacing tokens. Views and components never write a literal colour or size;
//! they read `cx.theme()` or one of the constants here.
//!
//! The look: pure black surfaces separated by 1px lines, three grey text levels, white as the only
//! accent, and colour reserved for status (success, warning, danger).

use gpui::{App, Global, Hsla, Pixels, Rgba, px};

#[derive(Clone, Debug)]
pub struct Theme {
    /// The rail and anything behind everything else.
    pub bg_base: Rgba,
    /// Sidebars and panels.
    pub bg_panel: Rgba,
    /// The main content area.
    pub bg_main: Rgba,
    /// Raised things on top of a surface: inputs, cards, menus, modals.
    pub bg_raised: Rgba,
    pub bg_hover: Rgba,
    pub bg_selected: Rgba,
    pub border: Rgba,
    pub border_strong: Rgba,

    pub text: Rgba,
    pub text_muted: Rgba,
    pub text_faint: Rgba,
    pub text_on_accent: Rgba,

    pub accent: Rgba,
    pub accent_hover: Rgba,
    pub accent_soft: Rgba,
    pub danger: Rgba,
    pub danger_soft: Rgba,
    pub success: Rgba,
    pub warning: Rgba,

    pub selection: Rgba,
    pub overlay: Rgba,
    pub shadow: Hsla,
}

impl Global for Theme {}

impl Theme {
    pub fn dark() -> Self {
        Self {
            bg_base: rgb(0x000000),
            bg_panel: rgb(0x000000),
            bg_main: rgb(0x000000),
            bg_raised: rgb(0x0a0a0a),
            bg_hover: rgb(0x111111),
            bg_selected: rgb(0x1a1a1a),
            border: rgb(0x1f1f1f),
            border_strong: rgb(0x2e2e2e),

            text: rgb(0xededed),
            text_muted: rgb(0xa1a1a1),
            text_faint: rgb(0x666666),
            text_on_accent: rgb(0x000000),

            accent: rgb(0xededed),
            accent_hover: rgb(0xffffff),
            accent_soft: rgba(0xffffff14),
            danger: rgb(0xee4444),
            danger_soft: rgba(0xee444426),
            success: rgb(0x3ecf8e),
            warning: rgb(0xf5a524),

            selection: rgba(0xffffff33),
            overlay: rgba(0x000000b3),
            shadow: gpui::hsla(0., 0., 0., 0.6),
        }
    }
}

pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

/// A stable colour per user, for avatars without a picture.
pub fn user_color(user_id: &str) -> Hsla {
    let mut h: u32 = 2166136261;
    for b in user_id.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    gpui::hsla((h % 360) as f32 / 360.0, 0.42, 0.46, 1.0)
}

/// Type scale.
pub mod text {
    use super::*;
    pub const XS: Pixels = px(11.);
    pub const SM: Pixels = px(12.5);
    pub const MD: Pixels = px(14.);
    pub const LG: Pixels = px(16.);
    pub const XL: Pixels = px(20.);
    pub const XXL: Pixels = px(26.);
}

/// Fixed layout sizes.
pub mod size {
    use super::*;
    pub const SIDEBAR: Pixels = px(240.);
    pub const PANEL: Pixels = px(260.);
    pub const HEADER: Pixels = px(48.);
    /// The status line along the bottom of the window.
    pub const STATUS: Pixels = px(34.);
    /// Height shared by text fields and the buttons that sit beside them.
    pub const CONTROL: Pixels = px(36.);
    /// Buttons inside dense rows (list items, inline notices).
    pub const CONTROL_SM: Pixels = px(28.);
    pub const ICON: Pixels = px(16.);
    pub const ICON_BUTTON: Pixels = px(28.);
    /// Icon buttons in headers and next to the composer.
    pub const ICON_BUTTON_LG: Pixels = px(32.);
}

pub mod radius {
    use super::*;
    pub const SM: Pixels = px(4.);
    pub const MD: Pixels = px(6.);
    pub const LG: Pixels = px(8.);
}

pub const FONT_UI: &str = "Geist";
pub const FONT_MONO: &str = "Geist Mono";
/// Colour emoji; forced for emoji ranges because UI fonts carry monochrome glyphs for some of them.
pub const FONT_EMOJI: &str = "Noto Color Emoji";

const fn rgb(hex: u32) -> Rgba {
    rgba((hex << 8) | 0xff)
}

const fn rgba(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 24) & 0xff) as f32 / 255.0,
        g: ((hex >> 16) & 0xff) as f32 / 255.0,
        b: ((hex >> 8) & 0xff) as f32 / 255.0,
        a: (hex & 0xff) as f32 / 255.0,
    }
}
