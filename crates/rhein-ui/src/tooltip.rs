use gpui::{
    AnyView, App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div,
};

use crate::theme::{ActiveTheme as _, radius, text};

pub struct Tooltip {
    text: SharedString,
}

impl Render for Tooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        // GPUI positions tooltips relative to the pointer; a small inset keeps it off the cursor.
        div().pl_2().pt_3().child(
            div()
                .px_2()
                .py_1()
                .rounded(radius::MD)
                .bg(t.bg_raised)
                .border_1()
                .border_color(t.border_strong)
                .shadow_md()
                .text_size(text::SM)
                .text_color(t.text)
                .child(self.text.clone()),
        )
    }
}

/// For `.tooltip(tooltip("Settings"))` on any stateful element.
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tooltip { text: text.clone() }).into()
}
