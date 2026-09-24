use gpui::{App, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div, px};

use crate::theme::{ActiveTheme as _, FONT_MONO, text};

/// A small dot for "something unread here".
#[derive(IntoElement)]
pub struct UnreadDot;

impl RenderOnce for UnreadDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        div().size(px(8.)).rounded_full().bg(cx.theme().text)
    }
}

/// A pill with a count, for mentions.
#[derive(IntoElement)]
pub struct CountBadge(pub u32);

impl RenderOnce for CountBadge {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        let label = if self.0 > 99 { "99+".to_owned() } else { self.0.to_string() };
        div()
            .h(px(16.))
            .min_w(px(16.))
            .px_1()
            .rounded_full()
            .bg(t.danger)
            .flex()
            .items_center()
            .justify_center()
            .font_family(FONT_MONO)
            .text_size(text::XS)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(gpui::white())
            .child(label)
    }
}

/// A small inline tag such as "BOT".
#[derive(IntoElement)]
pub struct Tag(pub &'static str);

impl RenderOnce for Tag {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        div()
            .px_1()
            .rounded(px(3.))
            .border_1()
            .border_color(t.border_strong)
            .text_color(t.text_muted)
            .font_family(FONT_MONO)
            .text_size(text::XS)
            .font_weight(FontWeight::MEDIUM)
            .child(self.0.to_lowercase())
    }
}
