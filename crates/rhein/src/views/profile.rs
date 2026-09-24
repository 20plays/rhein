//! What a profile card looks like: the banner (a picture or the accent colour) with the face
//! overlapping it, and the optional two-colour theme around the card. Shared by the profile card
//! and the preview in settings, following the web client's card.

use gpui::{
    AnyElement, Div, IntoElement, ObjectFit, ParentElement as _, Rgba, Styled as _, StyledImage as _, div, img,
    linear_color_stop, linear_gradient, px,
};
use rhein_ui::Theme;
use rhein_ui::theme::radius;

/// The web client's curated accents; each reads well behind a white name.
pub const ACCENTS: [(&str, u32); 11] = [
    ("Lilac", 0xa98bff),
    ("Violet", 0x7c5cff),
    ("Indigo", 0x5865f2),
    ("Azure", 0x3e8ff0),
    ("Teal", 0x33c2c2),
    ("Mint", 0x3fbf8f),
    ("Amber", 0xe0a03a),
    ("Coral", 0xe8705b),
    ("Rose", 0xe0558c),
    ("Magenta", 0xb44cd9),
    ("Slate", 0x7a8194),
];

pub const CARD_W: f32 = 300.;
const BANNER_H: f32 = 105.;
const FACE: f32 = 80.;
const RING: f32 = 5.;
/// How strongly a theme tints the card's background, as in the web client.
const THEME_TINT: f32 = 0.14;

#[derive(Clone, Default)]
pub struct Look {
    /// A resolved URL for the banner picture.
    pub banner: Option<String>,
    pub accent: Option<u32>,
    pub theme: Option<[u32; 2]>,
}

pub fn rgb(hex: u32) -> Rgba {
    gpui::rgb(hex)
}

fn mix(base: Rgba, tint: Rgba, amount: f32) -> Rgba {
    Rgba {
        r: base.r + (tint.r - base.r) * amount,
        g: base.g + (tint.g - base.g) * amount,
        b: base.b + (tint.b - base.b) * amount,
        a: 1.,
    }
}

/// The card container: plain with a hairline, or wrapped in the theme's gradient border with a
/// faint gradient fill. `body` is everything below the frame's edge.
pub fn frame(look: &Look, t: &Theme, body: impl IntoElement) -> Div {
    match look.theme {
        Some([top, bottom]) => {
            let (top, bottom) = (rgb(top), rgb(bottom));
            div().w(px(CARD_W)).p(px(2.)).rounded(radius::LG).bg(linear_gradient(180., linear_color_stop(top, 0.), linear_color_stop(bottom, 1.))).child(
                div()
                    .size_full()
                    .rounded(radius::LG - px(2.))
                    .overflow_hidden()
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(mix(t.bg_panel, top, THEME_TINT), 0.),
                        linear_color_stop(mix(t.bg_panel, bottom, THEME_TINT), 1.),
                    ))
                    .child(body),
            )
        }
        None => div()
            .w(px(CARD_W))
            .rounded(radius::LG)
            .overflow_hidden()
            .bg(t.bg_panel)
            .border_1()
            .border_color(t.border_strong)
            .child(body),
    }
}

/// The banner band with the face overlapping its lower edge. The face gets a ring in the card's
/// own colour, so it looks cut out of the banner.
pub fn header(look: &Look, face: AnyElement, t: &Theme) -> AnyElement {
    header_sized(look, face, FACE, BANNER_H, t)
}

/// [`header`] at another scale, e.g. the smaller card heading the account menu.
pub fn header_sized(look: &Look, face: AnyElement, face_size: f32, banner_h: f32, t: &Theme) -> AnyElement {
    let fill = look.accent.map(rgb).unwrap_or(t.bg_selected);
    let ring = match look.theme {
        Some([top, _]) => mix(t.bg_panel, rgb(top), THEME_TINT),
        None => t.bg_panel,
    };
    div()
        .relative()
        .w_full()
        .h(px(banner_h + face_size / 2. + RING))
        .child(
            div()
                .w_full()
                .h(px(banner_h))
                .bg(fill)
                .overflow_hidden()
                .children(look.banner.clone().map(|url| img(url).size_full().object_fit(ObjectFit::Cover))),
        )
        .child(
            div()
                .absolute()
                .left(px(16. - RING))
                .top(px(banner_h - face_size / 2. - RING))
                .p(px(RING))
                .rounded_full()
                .bg(ring)
                .child(face),
        )
        .into_any_element()
}

pub const FACE_SIZE: f32 = FACE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tint_mixes_towards_the_theme() {
        let base = rgb(0x000000);
        let tint = rgb(0xffffff);
        let m = mix(base, tint, THEME_TINT);
        assert!((m.r - THEME_TINT).abs() < 1e-6);
        assert_eq!(mix(base, tint, 0.).r, 0.);
    }
}
