use std::rc::Rc;

use gpui::{
    AnyElement, App, FocusHandle, FontWeight, IntoElement, ParentElement as _, Pixels, RenderOnce, SharedString,
    Styled as _, Window, div, prelude::*, px,
};

use crate::{Dismiss, motion};
use crate::theme::{ActiveTheme as _, radius, text};

pub const MODAL_CONTEXT: &str = "Modal";

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A centred card over a dimmed backdrop. Escape and a click on the backdrop both call
/// `on_dismiss`; give it a focus handle (or focus an input inside) so Escape reaches it.
#[derive(IntoElement)]
pub struct Modal {
    id: SharedString,
    title: Option<SharedString>,
    width: Pixels,
    focus: Option<FocusHandle>,
    body: Vec<AnyElement>,
    footer: Vec<AnyElement>,
    on_dismiss: Option<Handler>,
}

impl Modal {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self { id: id.into(), title: None, width: px(440.), focus: None, body: Vec::new(), footer: Vec::new(), on_dismiss: None }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    pub fn focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.body.push(child.into_any_element());
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = impl IntoElement>) -> Self {
        self.body.extend(children.into_iter().map(IntoElement::into_any_element));
        self
    }

    pub fn footer(mut self, child: impl IntoElement) -> Self {
        self.footer.push(child.into_any_element());
        self
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Modal {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        let backdrop_dismiss = self.on_dismiss.clone();
        let key_dismiss = self.on_dismiss;
        let card_id = SharedString::from(format!("{}-card", self.id));
        let card = div()
            .id(card_id.clone())
            .relative()
            .key_context(MODAL_CONTEXT)
            .when_some(self.focus, |d, f| d.track_focus(&f))
            .when_some(key_dismiss, |d, h| d.on_action(move |_: &Dismiss, window, cx| h(window, cx)))
            .on_click(|_, _, cx| cx.stop_propagation())
            .w(self.width)
            .max_w_full()
            .p_5()
            .rounded(radius::LG)
            .bg(t.bg_panel)
            .border_1()
            .border_color(t.border_strong)
            .shadow_lg()
            .text_color(t.text)
            .flex()
            .flex_col()
            .gap_4()
            .when_some(self.title, |d, title| {
                d.child(div().text_size(text::XL).font_weight(FontWeight::SEMIBOLD).child(title))
            })
            .children(self.body)
            .when(!self.footer.is_empty(), |d| d.child(div().flex().justify_end().gap_2().children(self.footer)));
        let fade_id = SharedString::from(format!("{}-fade", self.id));
        let backdrop = div()
            .id(self.id)
            .absolute()
            .inset_0()
            .bg(t.overlay)
            .flex()
            .items_center()
            .justify_center()
            .when_some(backdrop_dismiss, |d, h| d.on_click(move |_, window, cx| h(window, cx)))
            .child(motion::pop_in(card, SharedString::from(format!("{card_id}-in"))));
        motion::fade_in(backdrop, fade_id, motion::FAST)
    }
}
