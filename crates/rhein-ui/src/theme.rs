//! Colour, type and spacing tokens. Views and components never write a literal colour or size;
//! they read `cx.theme()` or one of the constants here.
//!
//! The look: near-black surfaces separated by small tonal steps rather than borders, three text
//! levels, and a single accent.

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
            bg_base: rgb(0x0e0f11),
            bg_panel: rgb(0x141518),
            bg_main: rgb(0x18191c),
            bg_raised: rgb(0x1f2125),
            bg_hover: rgb(0x23252a),
            bg_selected: rgb(0x2a2d33),
            border: rgb(0x23252a),
            border_strong: rgb(0x33363d),

            text: rgb(0xe8e9ec),
            text_muted: rgb(0xa2a6ad),
            text_faint: rgb(0x6c707a),
            text_on_accent: rgb(0xffffff),

            accent: rgb(0x7c86ff),
            accent_hover: rgb(0x8f98ff),
            accent_soft: rgba(0x7c86ff26),
            danger: rgb(0xf0565c),
            danger_soft: rgba(0xf0565c26),
            success: rgb(0x3fcf8e),
            warning: rgb(0xf2b448),

            selection: rgba(0x7c86ff55),
            overlay: rgba(0x000000b3),
            shadow: gpui::hsla(0., 0., 0., 0.5),
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
    pub const RAIL: Pixels = px(64.);
    pub const SIDEBAR: Pixels = px(240.);
    pub const PANEL: Pixels = px(260.);
    pub const HEADER: Pixels = px(48.);
    pub const ICON: Pixels = px(16.);
    pub const ICON_BUTTON: Pixels = px(28.);
}

pub mod radius {
    use super::*;
    pub const SM: Pixels = px(4.);
    pub const MD: Pixels = px(6.);
    pub const LG: Pixels = px(10.);
}

pub const FONT_UI: &str = "Inter";
pub const FONT_MONO: &str = "JetBrains Mono";
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
