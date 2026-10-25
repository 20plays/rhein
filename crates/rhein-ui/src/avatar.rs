use gpui::{
    App, FontWeight, IntoElement, ObjectFit, ParentElement as _, Pixels, RenderOnce, SharedString, Styled as _,
    StyledImage as _, Window, div, img, px,
};

use crate::theme::{ActiveTheme as _, user_color};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Online,
    Idle,
    Dnd,
    Offline,
}

impl Presence {
    pub fn parse(status: &str) -> Self {
        match status {
            "online" => Presence::Online,
            "idle" => Presence::Idle,
            "dnd" => Presence::Dnd,
            _ => Presence::Offline,
        }
    }
}

/// A round picture, or the first letter of the name on a colour derived from the user id.
#[derive(IntoElement)]
pub struct Avatar {
    user_id: SharedString,
    name: SharedString,
    size: Pixels,
    image: Option<SharedString>,
    presence: Option<Presence>,
}

impl Avatar {
    pub fn new(user_id: impl Into<SharedString>, name: impl Into<SharedString>) -> Self {
        Self { user_id: user_id.into(), name: name.into(), size: px(32.), image: None, presence: None }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn image(mut self, url: Option<SharedString>) -> Self {
        self.image = url;
        self
    }

    pub fn presence(mut self, presence: Option<Presence>) -> Self {
        self.presence = presence;
        self
    }
}

impl RenderOnce for Avatar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        let size = self.size;
        let letter = self
            .name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into());
        let fallback = div()
            .size(size)
            .rounded_full()
            .bg(user_color(&self.user_id))
            .flex()
            .items_center()
            .justify_center()
            .text_color(t.text_on_accent)
            .text_size(size * 0.42)
            .font_weight(FontWeight::SEMIBOLD)
            .child(letter);
        let face = match self.image {
            Some(url) => img(crate::images::scaled(url, size, window))
                .size(size)
                .rounded_full()
                .object_fit(ObjectFit::Cover)
                .with_fallback({
                    let fallback_user = self.user_id.clone();
                    let fallback_name = self.name.clone();
                    move || Avatar::new(fallback_user.clone(), fallback_name.clone()).size(size).into_any_element()
                })
                .into_any_element(),
            None => fallback.into_any_element(),
        };
        let dot = self.presence.map(|p| {
            let color = match p {
                Presence::Online => t.success,
                Presence::Idle => t.warning,
                Presence::Dnd => t.danger,
                Presence::Offline => t.text_faint,
            };
            let d = (size * 0.32).max(px(8.));
            div()
                .absolute()
                .bottom(px(-1.))
                .right(px(-1.))
                .size(d)
                .rounded_full()
                .bg(color)
                .border_2()
                .border_color(t.bg_panel)
        });
        div().relative().flex_none().size(size).child(face).children(dot)
    }
}
