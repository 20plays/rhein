//! The GIF favourites menu: the account's favourite GIFs in a grid, filtered by the search field
//! (the words in their links), sent with a click, taken out with their star.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement as _,
    Render, ScrollHandle, SharedString, Styled as _, StyledImage as _, Subscription, WeakEntity, Window, div, img, point, prelude::*, px,
};
use rhein_store::Client;
use rhein_store::MediaProbe;
use rhein_store::client::GifShare;
use rhein_store::gif_links;
use rhein_ui::images::{self, Fitted};
use rhein_ui::theme::{radius, text};
use rhein_ui::{ActiveTheme as _, Dismiss, Icon, IconButton, InputEvent, TextInput, Theme};

const PICKER_H: f32 = 440.;
/// Every tile is this wide; its picture takes the height its aspect ratio gives, uncropped.
const TILE_W: f32 = 192.;
/// Height of a tile whose picture isn't there (yet): loading, a share link, a video.
const TILE_H: f32 = 118.;
/// Padding around the grid and gap between tiles (`px_3` / `gap_3`).
const GAP: f32 = 12.;
/// Two columns, or four in the wide picker.
const COLS: usize = 2;
const WIDE_COLS: usize = 4;
/// How far above and below the view tiles are still rendered (and their pictures loaded).
const OVERSCAN: f32 = PICKER_H;
/// Share/media resolutions started per frame; a large favourites list ramps up gradually.
const MAX_RESOLVE_PER_FRAME: usize = 8;

fn picker_w(cols: usize) -> f32 {
    cols as f32 * TILE_W + (cols - 1) as f32 * GAP + 2. * GAP
}

/// A GIF chosen to send.
pub struct PickedGif(pub String);

pub struct GifPicker {
    client: Entity<Client>,
    search: Entity<TextInput>,
    scroll: ScrollHandle,
    /// Four columns instead of two; shared with the chat so reopening keeps the choice.
    wide: Rc<Cell<bool>>,
    /// The height of each favourite's tile once its picture has loaded; others count as
    /// `TILE_H`, the height of their placeholder.
    heights: HashMap<String, f32>,
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
    pub fn new(client: Entity<Client>, wide: Rc<Cell<bool>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search your favourite GIFs", cx));
        let subs = vec![
            cx.observe(&client, |_, _, cx| cx.notify()),
            cx.subscribe_in(&search, window, |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Changed => {
                    this.scroll.set_offset(point(px(0.), px(0.)));
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
        GifPicker { client, search, scroll: ScrollHandle::new(), wide, heights: HashMap::new(), focus: cx.focus_handle(), _subs: subs }
    }

    fn shown(&self, cx: &App) -> Vec<String> {
        let query = self.search.read(cx).text().trim().to_owned();
        self.client.read(cx).gif_favorites().iter().filter(|u| query.is_empty() || matches(u, &query)).cloned().collect()
    }
}

/// What a tile shows. Share pages are HTML and Telium media links say what they are only when
/// asked: neither is fed to `img()` straight, or GPUI fetches the page (or a video) and falls
/// back to its SVG parser, spamming `Usvg(ParsingFailed(MalformedEntityReference))` per tile.
enum Preview {
    Image(String),
    /// A picture fitted to the tile width, smoothed when shrunk.
    Fitted(Fitted),
    Waiting,
    ShareLink(String),
    Still,
}

impl Preview {
    /// The preview for `url`, noting a share or media kind still to be resolved.
    fn of(url: &str, client: &Client, shares_missing: &mut Vec<String>, probes_missing: &mut Vec<String>) -> Self {
        if let Some(share) = gif_links::share(url) {
            match client.gif_share(url) {
                Some(GifShare::Ready(media)) => Preview::Image(media),
                Some(GifShare::Loading) => Preview::Waiting,
                Some(GifShare::Failed) => Preview::ShareLink(share.provider().to_owned()),
                None => {
                    shares_missing.push(url.to_owned());
                    Preview::Waiting
                }
            }
        } else if gif_links::is_telium_media(url) {
            match client.media_kind(url) {
                Some(MediaProbe::Known(kind)) if kind == gif_links::MediaKind::Gif || kind == gif_links::MediaKind::Image => {
                    Preview::Image(url.to_owned())
                }
                Some(MediaProbe::Known(_)) => Preview::Still,
                Some(MediaProbe::Loading) => Preview::Waiting,
                None => {
                    probes_missing.push(url.to_owned());
                    Preview::Waiting
                }
            }
        } else if gif_links::is_gif(url) {
            Preview::Image(url.to_owned())
        } else {
            Preview::Still
        }
    }
}

/// A placeholder tile's content: a line of faint text, centred.
fn note(label: impl Into<SharedString>, faint: gpui::Rgba) -> AnyElement {
    div()
        .w_full()
        .h(px(TILE_H))
        .p_2()
        .flex()
        .items_center()
        .justify_center()
        .text_center()
        .text_size(text::XS)
        .text_color(faint)
        .child(label.into())
        .into_any_element()
}

/// One favourite: its picture at the full height the tile width gives it, sent with a click,
/// taken out with the star shown on hover.
fn tile(idx: usize, url: String, preview: Preview, t: &Theme, view: WeakEntity<GifPicker>, client: Entity<Client>) -> AnyElement {
    let faint = t.text_faint;
    let name = url.rsplit('/').next().unwrap_or(&url).to_owned();
    let picture = match preview {
        // Not loaded yet: only a width, GPUI's img takes the height from the aspect ratio.
        Preview::Image(media) => img(images::source(media))
            .id("pic")
            .w(px(TILE_W))
            .with_loading(move || note("Loading…", faint))
            .with_fallback(move || note(name.clone(), faint))
            .into_any_element(),
        Preview::Fitted(Fitted { source, size }) => img(source)
            .id("pic")
            .w(px(TILE_W))
            // A smaller picture is drawn up to the tile width, so its height is scaled with it.
            .when_some(size, |i, s| i.h(px(TILE_W * f32::from(s.height) / f32::from(s.width))))
            .with_loading(move || note("Loading…", faint))
            .with_fallback(move || note(name.clone(), faint))
            .into_any_element(),
        Preview::Waiting => note("Loading…", faint),
        Preview::ShareLink(provider) => note(format!("Open GIF on {provider}"), faint),
        Preview::Still => note(name, faint),
    };
    let group = SharedString::from(format!("gif-tile-{idx}"));
    let send = url.clone();
    div()
        .id(SharedString::from(format!("gif-{idx}")))
        .group(group.clone())
        .relative()
        .w(px(TILE_W))
        .flex_none()
        .rounded(radius::MD)
        .overflow_hidden()
        .bg(t.bg_base)
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            view.update(cx, |_, cx| cx.emit(PickedGif(send.clone()))).ok();
        })
        .child(picture)
        .child(
            div().absolute().top_1().left_1().rounded(radius::MD).bg(t.bg_base).invisible().group_hover(group, |s| s.visible()).child(
                IconButton::new(SharedString::from(format!("gif-unstar-{idx}")), Icon::StarFilled)
                    .active(true)
                    .tooltip("Remove from favourites")
                    .on_click(move |_, _, cx| {
                        client.update(cx, |c, cx| c.toggle_gif_favorite(&url, cx));
                    }),
            ),
        )
        .into_any_element()
}

impl Render for GifPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let wide = self.wide.get();
        let cols = if wide { WIDE_COLS } else { COLS };
        let (ready, total, shown) = {
            let client = self.client.read(cx);
            (client.gif_favorites_ready(), client.gif_favorites().len(), self.shown(cx))
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
            None => {
                // Masonry: favourite `i` goes to column `i % cols`, and each column stacks its
                // pictures at their own heights. Aspect ratios are only known once a picture
                // loads, so the columns are filled in order rather than shortest-first.
                //
                // Only tiles near the view are rendered, and only their pictures loaded: a
                // decoded GIF holds every frame, so loading hundreds of favourites at once runs
                // a machine out of memory. The rest of a column is a spacer of the heights its
                // tiles are known (or assumed) to have.
                let top = -f32::from(self.scroll.offset().y);
                let view_h = f32::from(self.scroll.bounds().size.height).max(PICKER_H);
                let (lo, hi) = (top - OVERSCAN, top + view_h + OVERSCAN);
                let view: WeakEntity<Self> = cx.weak_entity();
                let mut shares_missing: Vec<String> = Vec::new();
                let mut probes_missing: Vec<String> = Vec::new();
                let mut columns: Vec<Vec<AnyElement>> = (0..cols).map(|_| Vec::new()).collect();
                let mut ys = vec![GAP; cols];
                let mut gaps = vec![0f32; cols];
                for (idx, url) in shown.into_iter().enumerate() {
                    let col = idx % cols;
                    let h = self.heights.get(&url).copied().unwrap_or(TILE_H);
                    let y = ys[col];
                    ys[col] += h + GAP;
                    if y + h < lo || y > hi {
                        gaps[col] += h + GAP;
                        continue;
                    }
                    if gaps[col] > 0. {
                        columns[col].push(div().flex_none().h(px(std::mem::take(&mut gaps[col]))).into_any_element());
                    }
                    let preview = Preview::of(&url, self.client.read(cx), &mut shares_missing, &mut probes_missing);
                    let preview = match preview {
                        Preview::Image(media) => {
                            let bound = gpui::size(px(TILE_W), px(f32::MAX));
                            match images::fit(media.clone(), bound, window, cx) {
                                fitted @ Fitted { size: Some(_), .. } => Preview::Fitted(fitted),
                                _ => Preview::Image(media),
                            }
                        }
                        other => other,
                    };
                    let known = match &preview {
                        Preview::Fitted(Fitted { size: Some(s), .. }) => TILE_W * f32::from(s.height) / f32::from(s.width),
                        _ => TILE_H,
                    };
                    self.heights.insert(url.clone(), known);
                    columns[col].push(div().flex_none().pb(px(GAP)).child(tile(idx, url, preview, &t, view.clone(), self.client.clone())).into_any_element());
                }
                for (col, gap) in gaps.into_iter().enumerate() {
                    if gap > 0. {
                        columns[col].push(div().flex_none().h(px(gap)).into_any_element());
                    }
                }
                // Capped per frame: each resolution notifies the client and renders again, so a
                // large list ramps up instead of bursting every provider and hitting rate limits.
                shares_missing.truncate(MAX_RESOLVE_PER_FRAME);
                probes_missing.truncate(MAX_RESOLVE_PER_FRAME.saturating_sub(shares_missing.len()));
                if !shares_missing.is_empty() || !probes_missing.is_empty() {
                    let client = self.client.clone();
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
                div()
                    .id("gif-grid")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    // Scrolling brings other tiles into view: render again.
                    .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_3()
                            .px_3()
                            .pt_3()
                            .children(columns.into_iter().map(|tiles| div().flex().flex_col().children(tiles))),
                    )
                    .into_any_element()
            }
        };
        let toggle = IconButton::new("gif-expand", Icon::Maximize)
            .active(wide)
            .tooltip(if wide { "Two columns" } else { "Four columns" })
            .on_click(cx.listener(|this, _, _, cx| {
                this.wide.set(!this.wide.get());
                cx.notify();
            }));
        div()
            .id("gif-picker")
            .occlude()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .track_focus(&self.focus)
            .key_context(crate::views::emoji_picker::PICKER_CONTEXT)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .w(px(picker_w(cols)))
            .h(px(PICKER_H))
            .relative()
            .flex()
            .flex_col()
            .pt_3()
            .rounded(radius::LG)
            .bg(t.bg_raised)
            .border_1()
            .border_color(t.border_strong)
            .shadow_lg()
            .child(div().px_3().pb_2().child(self.search.clone()))
            .child(body)
            .child(
                div()
                    .absolute()
                    .bottom_2()
                    .left_2()
                    .rounded(radius::MD)
                    .bg(t.bg_base)
                    .border_1()
                    .border_color(t.border_strong)
                    .shadow_md()
                    .child(toggle),
            )
    }
}
