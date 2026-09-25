//! The GIF favourites menu: the account's favourite GIFs in a grid, filtered by the search field
//! (the words in their links), sent with a click, taken out with their star.

use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ObjectFit, ParentElement as _,
    Render, SharedString, Styled as _, StyledImage as _, Subscription, Window, div, img, prelude::*, px,
};
use rhein_store::Client;
use rhein_ui::theme::{radius, text};
use rhein_ui::{ActiveTheme as _, Dismiss, Icon, IconButton, InputEvent, TextInput};

const PICKER_W: f32 = 420.;
const PICKER_H: f32 = 440.;
const TILE_H: f32 = 118.;

/// A GIF chosen to send.
pub struct PickedGif(pub String);

pub struct GifPicker {
    client: Entity<Client>,
    search: Entity<TextInput>,
    scroll: gpui::ScrollHandle,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PickedGif> for GifPicker {}
impl EventEmitter<DismissEvent> for GifPicker {}

impl Focusable for GifPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

/// The words a link is found by: its path, split at separators, decoded, lower case.
fn words(url: &str) -> String {
    let path = url.split_once("://").map_or(url, |(_, rest)| rest);
    let decoded = urlencoding::decode(path).map(|c| c.into_owned()).unwrap_or_else(|_| path.to_owned());
    decoded.to_lowercase().replace(['/', '-', '_', '.', '?', '=', '&', '+'], " ")
}

fn matches(url: &str, query: &str) -> bool {
    let haystack = words(url);
    query.split_whitespace().all(|w| haystack.contains(&w.to_lowercase()))
}

impl GifPicker {
    pub fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search your favourite GIFs", cx));
        let subs = vec![
            cx.observe(&client, |_, _, cx| cx.notify()),
            cx.subscribe_in(&search, window, |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Changed => cx.notify(),
                InputEvent::Submit => {
                    if let Some(first) = this.shown(cx).into_iter().next() {
                        cx.emit(PickedGif(first));
                    }
                }
                InputEvent::Cancel => cx.emit(DismissEvent),
                _ => {}
            }),
        ];
        window.focus(&search.focus_handle(cx), cx);
        GifPicker { client, search, scroll: gpui::ScrollHandle::new(), focus: cx.focus_handle(), _subs: subs }
    }

    fn shown(&self, cx: &App) -> Vec<String> {
        let query = self.search.read(cx).text().trim().to_owned();
        self.client.read(cx).gif_favorites().iter().filter(|u| query.is_empty() || matches(u, &query)).cloned().collect()
    }
}

impl Render for GifPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let client = self.client.read(cx);
        let (ready, total) = (client.gif_favorites_ready(), client.gif_favorites().len());
        let shown = self.shown(cx);
        let empty = if !ready {
            Some("Your favourite GIFs are kept encrypted with your account. They open once this device has your keys.")
        } else if total == 0 {
            Some("No favourite GIFs yet. Hover a GIF in the chat and click its star.")
        } else if shown.is_empty() {
            Some("No favourite matches that.")
        } else {
            None
        };
        let tiles = shown.into_iter().enumerate().map(|(i, url)| {
            let (send, unstar) = (url.clone(), url.clone());
            let client = self.client.clone();
            let faint = t.text_faint;
            let name = url.rsplit('/').next().unwrap_or(&url).to_owned();
            div()
                .id(SharedString::from(format!("gif-{i}")))
                .group(SharedString::from(format!("gif-tile-{i}")))
                .relative()
                .w(px((PICKER_W - 36.) / 2.))
                .h(px(TILE_H))
                .rounded(radius::MD)
                .overflow_hidden()
                .bg(t.bg_raised)
                .cursor_pointer()
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(PickedGif(send.clone()))))
                .child(
                    img(url.clone())
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        .with_fallback(move || {
                            div()
                                .size_full()
                                .p_2()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(text::XS)
                                .text_color(faint)
                                .child(name.clone())
                                .into_any_element()
                        }),
                )
                .child(
                    div()
                        .absolute()
                        .top_1()
                        .left_1()
                        .rounded(radius::MD)
                        .bg(t.bg_base)
                        .invisible()
                        .group_hover(SharedString::from(format!("gif-tile-{i}")), |s| s.visible())
                        .child(
                            IconButton::new(SharedString::from(format!("gif-unstar-{i}")), Icon::StarFilled)
                                .active(true)
                                .tooltip("Remove from favourites")
                                .on_click(move |_, _, cx| client.update(cx, |c, cx| c.toggle_gif_favorite(&unstar, cx))),
                        ),
                )
        });
        div()
            .id("gif-picker")
            .occlude()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .track_focus(&self.focus)
            .key_context(crate::views::emoji_picker::PICKER_CONTEXT)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .w(px(PICKER_W))
            .h(px(PICKER_H))
            .flex()
            .flex_col()
            .gap_2()
            .pt_3()
            .rounded(radius::LG)
            .bg(t.bg_raised)
            .border_1()
            .border_color(t.border_strong)
            .shadow_lg()
            .child(div().px_3().child(self.search.clone()))
            .child(rhein_ui::smooth_scroll(
                "gif-glide",
                rhein_ui::ScrollTarget::Handle(self.scroll.clone()),
                div()
                    .id("gif-grid")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px_3()
                    .pb_3()
                    .flex()
                    .flex_wrap()
                    .content_start()
                    .gap_3()
                    .children(empty.map(|note| div().w_full().p_4().text_center().text_size(text::SM).text_color(t.text_faint).child(note)))
                    .children(tiles),
            ))
    }
}
