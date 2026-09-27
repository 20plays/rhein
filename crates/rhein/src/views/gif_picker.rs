//! The GIF favourites menu: the account's favourite GIFs in a grid, filtered by the search field
//! (the words in their links), sent with a click, taken out with their star.

use std::rc::Rc;

use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ObjectFit, ParentElement as _,
    Render, ScrollStrategy, SharedString, Styled as _, StyledImage as _, Subscription, UniformListScrollHandle, WeakEntity, Window, div, img,
    prelude::*, px, uniform_list,
};
use rhein_store::Client;
use rhein_store::MediaProbe;
use rhein_store::client::GifShare;
use rhein_store::gif_links;
use rhein_ui::theme::{radius, text};
use rhein_ui::{ActiveTheme as _, Dismiss, Icon, IconButton, InputEvent, TextInput};

const PICKER_W: f32 = 420.;
const PICKER_H: f32 = 440.;
const TILE_H: f32 = 118.;
/// Vertical gap between tile rows (`gap_3`); rows are this much taller than a tile.
const ROW_GAP: f32 = 12.;
const ROW_H: f32 = TILE_H + ROW_GAP;
/// Two tiles per row; only visible rows render, so a large favourites list neither fetches
/// every picture nor resolves every share page at once.
const COLS: usize = 2;
/// Share/media resolutions started per frame; scrolling fast still ramps up gradually.
const MAX_RESOLVE_PER_FRAME: usize = 8;

/// A GIF chosen to send.
pub struct PickedGif(pub String);

pub struct GifPicker {
    client: Entity<Client>,
    search: Entity<TextInput>,
    scroll: UniformListScrollHandle,
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
                InputEvent::Changed => {
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
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
        GifPicker { client, search, scroll: UniformListScrollHandle::new(), focus: cx.focus_handle(), _subs: subs }
    }

    fn shown(&self, cx: &App) -> Vec<String> {
        let query = self.search.read(cx).text().trim().to_owned();
        self.client.read(cx).gif_favorites().iter().filter(|u| query.is_empty() || matches(u, &query)).cloned().collect()
    }
}

impl Render for GifPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let (ready, total, shown) = {
            let client = self.client.read(cx);
            let query = self.search.read(cx).text().trim().to_owned();
            let shown: Vec<String> =
                client.gif_favorites().iter().filter(|u| query.is_empty() || matches(u, &query)).cloned().collect();
            (client.gif_favorites_ready(), client.gif_favorites().len(), shown)
        };
        let empty: Option<SharedString> = if !ready {
            Some("Your favourite GIFs are kept encrypted with your account. They open once this device has your keys.".into())
        } else if total == 0 {
            Some("No favourite GIFs yet. Hover a GIF in the chat and click its star.".into())
        } else if shown.is_empty() {
            Some("No favourite matches that.".into())
        } else {
            None
        };
        // Two flexible columns per uniform row: `flex_1` shares whatever width the scrollbar
        // leaves, instead of a pixel width that stops fitting once the scrollbar appears and
        // collapses to a single column. Only visible rows render, so pictures resolve lazily.
        let shown: Rc<Vec<String>> = Rc::new(shown);
        let rows = shown.len().div_ceil(COLS);
        let client = self.client.clone();
        let view: WeakEntity<Self> = cx.weak_entity();
        let grid = uniform_list("gif-grid", rows, move |range, _window, cx| {
            let t = cx.theme().clone();
            let stored = client.read(cx);
            let mut shares_missing: Vec<String> = Vec::new();
            let mut probes_missing: Vec<String> = Vec::new();
            let mut elements: Vec<gpui::AnyElement> = Vec::with_capacity(range.len());
            for row in range {
                let mut tiles: Vec<gpui::AnyElement> = Vec::with_capacity(COLS);
                for col in 0..COLS {
                    let Some(url) = shown.get(row * COLS + col).cloned() else {
                        tiles.push(div().flex_1().into_any_element());
                        continue;
                    };
                    // Share pages are HTML and Telium media links say what they are only when
                    // asked: neither is fed to `img()` straight, or GPUI fetches the page (or
                    // a video) and falls back to its SVG parser, spamming
                    // `Usvg(ParsingFailed(MalformedEntityReference))` for every tile.
                    enum Preview {
                        Image(String),
                        Waiting,
                        ShareLink(String),
                        Still,
                    }
                    let preview = if let Some(share) = gif_links::share(&url) {
                        match stored.gif_share(&url) {
                            Some(GifShare::Ready(media)) => Preview::Image(media),
                            Some(GifShare::Loading) => Preview::Waiting,
                            Some(GifShare::Failed) => Preview::ShareLink(share.provider().to_owned()),
                            None => {
                                shares_missing.push(url.clone());
                                Preview::Waiting
                            }
                        }
                    } else if gif_links::is_telium_media(&url) {
                        match stored.media_kind(&url) {
                            Some(MediaProbe::Known(kind))
                                if kind == gif_links::MediaKind::Gif || kind == gif_links::MediaKind::Image =>
                            {
                                Preview::Image(url.clone())
                            }
                            Some(MediaProbe::Known(_)) => Preview::Still,
                            Some(MediaProbe::Loading) => Preview::Waiting,
                            None => {
                                probes_missing.push(url.clone());
                                Preview::Waiting
                            }
                        }
                    } else if gif_links::is_gif(&url) {
                        Preview::Image(url.clone())
                    } else {
                        Preview::Still
                    };
                    let idx = row * COLS + col;
                    let faint = t.text_faint;
                    let name = url.rsplit('/').next().unwrap_or(&url).to_owned();
                    let picture = match preview {
                        Preview::Image(media) => img(media)
                            .id("pic")
                            .size_full()
                            .object_fit(ObjectFit::Cover)
                            .with_fallback({
                                let (faint, name) = (faint, name.clone());
                                move || {
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
                                }
                            })
                            .into_any_element(),
                        Preview::Waiting => div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(text::XS)
                            .text_color(faint)
                            .child("Loading…")
                            .into_any_element(),
                        Preview::ShareLink(provider) => div()
                            .size_full()
                            .p_2()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(text::XS)
                            .text_color(faint)
                            .child(format!("Open GIF on {provider}"))
                            .into_any_element(),
                        Preview::Still => div()
                            .size_full()
                            .p_2()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(text::XS)
                            .text_color(faint)
                            .child(name.clone())
                            .into_any_element(),
                    };
                    let (send, unstar) = (url.clone(), url.clone());
                    let (pick, unstar_client) = (view.clone(), client.clone());
                    tiles.push(
                        div()
                            .id(SharedString::from(format!("gif-{idx}")))
                            .group(SharedString::from(format!("gif-tile-{idx}")))
                            .relative()
                            .flex_1()
                            .h(px(TILE_H))
                            .rounded(radius::MD)
                            .overflow_hidden()
                            .bg(t.bg_raised)
                            .cursor_pointer()
                            .on_click(move |_, _, cx| {
                                pick.update(cx, |_, cx| cx.emit(PickedGif(send.clone()))).ok();
                            })
                            .child(picture)
                            .child(
                                div()
                                    .absolute()
                                    .top_1()
                                    .left_1()
                                    .rounded(radius::MD)
                                    .bg(t.bg_base)
                                    .invisible()
                                    .group_hover(SharedString::from(format!("gif-tile-{idx}")), |s| s.visible())
                                    .child(
                                        IconButton::new(SharedString::from(format!("gif-unstar-{idx}")), Icon::StarFilled)
                                            .active(true)
                                            .tooltip("Remove from favourites")
                                            .on_click(move |_, _, cx| {
                                                unstar_client.update(cx, |c, cx| c.toggle_gif_favorite(&unstar, cx));
                                            }),
                                    ),
                            )
                            .into_any_element(),
                    );
                }
                elements.push(
                    div().h(px(ROW_H)).w_full().flex().gap_3().px_3().pb_3().children(tiles).into_any_element(),
                );
            }
            // Visible rows only, capped per frame: fast scrolling ramps up instead of bursting
            // every provider at once and hitting rate limits.
            shares_missing.truncate(MAX_RESOLVE_PER_FRAME);
            probes_missing.truncate(MAX_RESOLVE_PER_FRAME.saturating_sub(shares_missing.len()));
            if !shares_missing.is_empty() || !probes_missing.is_empty() {
                let client = client.clone();
                cx.defer(move |cx| {
                    client.update(cx, |c, cx| {
                        for url in &shares_missing {
                            c.request_gif_share(url, cx);
                        }
                        for url in &probes_missing {
                            c.request_media_kind(url, cx);
                        }
                    });
                });
            }
            elements
        })
        .track_scroll(&self.scroll)
        .flex_1();
        let body = match empty {
            Some(note) => div()
                .flex_1()
                .min_h_0()
                .p_4()
                .flex()
                .items_start()
                .justify_center()
                .text_center()
                .text_size(text::SM)
                .text_color(t.text_faint)
                .child(note)
                .into_any_element(),
            None => div().flex_1().min_h_0().flex().flex_col().pt_3().child(grid).into_any_element(),
        };
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
            .child(body)
    }
}
