use gpui::{App, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div, px};

use crate::theme::{ActiveTheme as _, text};

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
            .text_size(text::XS)
            .font_weight(FontWeight::BOLD)
            .text_color(t.text_on_accent)
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
            .bg(t.accent_soft)
            .text_color(t.accent)
            .text_size(text::XS)
            .font_weight(FontWeight::BOLD)
            .child(self.0)
    }
}
