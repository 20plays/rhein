//! The emoji picker: search, recent, the guild's custom emoji, then the Unicode groups, with a
//! skin-tone choice. Emits the text to insert: the character itself, or `<name:ULID>` for a
//! custom emoji (the web client's wire token).

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, IntoElement, ParentElement as _, Render, ScrollStrategy, SharedString, Styled as _, Subscription,
    UniformListScrollHandle, WeakEntity, Window, div, img, prelude::*, px, uniform_list,
};
use rhein_store::Client;
use rhein_store::emoji::{self, GROUPS};
use rhein_ui::theme::{FONT_EMOJI, radius, text};
use rhein_ui::{ActiveTheme as _, Dismiss, InputEvent, TextInput, tooltip};

const COLS: usize = 9;
const CELL: f32 = 36.;
const PICKER_W: f32 = COLS as f32 * CELL + 24.;
const PICKER_H: f32 = 380.;

pub const PICKER_CONTEXT: &str = "EmojiPicker";

/// What the picker hands back.
pub struct Picked(pub String);

#[derive(Clone)]
enum Glyph {
    Unicode(String),
    Custom { url: String, name: String },
}

#[derive(Clone)]
struct Cell {
    glyph: Glyph,
    insert: String,
    label: String,
}

#[derive(Clone)]
enum Row {
    Header(String),
    Cells(Vec<Cell>),
}

pub struct EmojiPicker {
    client: Entity<Client>,
    guild_id: Option<String>,
    search: Entity<TextInput>,
    tone: u8,
    scroll: UniformListScrollHandle,
    hovered: Option<(String, String)>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<Picked> for EmojiPicker {}
impl EventEmitter<DismissEvent> for EmojiPicker {}

impl Focusable for EmojiPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }
}

impl EmojiPicker {
    pub fn new(client: Entity<Client>, guild_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextInput::new("Search emoji", cx));
        let subs = vec![cx.subscribe_in(&search, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::Changed => {
                this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
            InputEvent::Submit => {
                // Enter picks the first result.
                let first = this.rows(cx).into_iter().find_map(|r| match r {
                    Row::Cells(cells) => cells.into_iter().next(),
                    Row::Header(_) => None,
                });
                if let Some(cell) = first {
                    this.pick(cell.insert, cx);
                }
            }
            InputEvent::Cancel => cx.emit(DismissEvent),
            _ => {}
        })];
        let tone = rhein_store::prefs::load().skin_tone;
        window.focus(&search.focus_handle(cx), cx);
        Self {
            client,
            guild_id,
            search,
            tone,
            scroll: UniformListScrollHandle::new(),
            hovered: None,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    fn pick(&mut self, insert: String, cx: &mut Context<Self>) {
        self.client.update(cx, |c, _| c.use_emoji(insert.clone()));
        cx.emit(Picked(insert));
    }

    fn set_tone(&mut self, tone: u8, cx: &mut Context<Self>) {
        self.tone = tone;
        let mut prefs = rhein_store::prefs::load();
        prefs.skin_tone = tone;
        rhein_store::prefs::store(&prefs);
        cx.notify();
    }

    /// Custom emoji of the current guild, as cells.
    fn custom_cells(&self, cx: &App, filter: Option<&str>) -> Vec<Cell> {
        let client = self.client.read(cx);
        let Some(gid) = self.guild_id.as_deref() else { return Vec::new() };
        client
            .guild_emoji(gid)
            .iter()
            .filter(|e| filter.is_none_or(|q| e.name.to_lowercase().contains(q)))
            .filter_map(|e| {
                let url = client.emoji_url(Some(gid), &e.id, None)?;
                Some(Cell {
                    glyph: Glyph::Custom { url, name: e.name.clone() },
                    insert: format!("<{}:{}>", e.name, e.id),
                    label: format!(":{}:", e.name),
                })
            })
            .collect()
    }

    fn unicode_cell(&self, e: &'static emoji::Emoji) -> Cell {
        let toned = emoji::with_tone(e, self.tone);
        Cell {
            glyph: Glyph::Unicode(toned.as_str().to_owned()),
            insert: toned.as_str().to_owned(),
            label: e.shortcode().map(|s| format!(":{s}:")).unwrap_or_else(|| e.name().to_owned()),
        }
    }

    /// The picker's content as rows of equal height: section headers and rows of cells.
    fn rows(&self, cx: &App) -> Vec<Row> {
        let query = self.search.read(cx).text().trim().to_lowercase();
        let mut rows = Vec::new();
        let push = |rows: &mut Vec<Row>, title: String, cells: Vec<Cell>| {
            if cells.is_empty() {
                return;
            }
            rows.push(Row::Header(title));
            for chunk in cells.chunks(COLS) {
                rows.push(Row::Cells(chunk.to_vec()));
            }
        };
        if !query.is_empty() {
            let mut cells = self.custom_cells(cx, Some(&query));
            cells.extend(emoji::search(&query, 120).into_iter().map(|e| self.unicode_cell(e)));
            push(&mut rows, "Search results".into(), cells);
            return rows;
        }
        let client = self.client.read(cx);
        let recent: Vec<Cell> = client
            .recent_emoji
            .iter()
            .take(COLS * 2)
            .filter_map(|value| {
                if value.starts_with('<') {
                    self.custom_cells(cx, None).into_iter().find(|c| &c.insert == value)
                } else {
                    emoji::get(value).map(|e| Cell {
                        glyph: Glyph::Unicode(value.clone()),
                        insert: value.clone(),
                        label: e.shortcode().map(|s| format!(":{s}:")).unwrap_or_else(|| e.name().to_owned()),
                    })
                }
            })
            .collect();
        push(&mut rows, "Recently used".into(), recent);
        let guild_name = self
            .guild_id
            .as_deref()
            .and_then(|g| client.guild(g))
            .map(|g| g.guild.name.clone())
            .unwrap_or_else(|| "This guild".into());
        push(&mut rows, guild_name, self.custom_cells(cx, None));
        for (group, label, _) in GROUPS {
            push(&mut rows, label.into(), emoji::group(group).map(|e| self.unicode_cell(e)).collect());
        }
        rows
    }
}

impl Render for EmojiPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let rows = Rc::new(self.rows(cx));
        let searching = !self.search.read(cx).text().trim().is_empty();

        // Category tabs jump to their header row.
        let header_index = |title: &str| rows.iter().position(|r| matches!(r, Row::Header(h) if h == title));
        let mut tabs = div().flex().gap_0p5().px_2();
        if !searching {
            for (i, (_, label, icon)) in GROUPS.iter().enumerate() {
                let Some(ix) = header_index(label) else { continue };
                let scroll = self.scroll.clone();
                tabs = tabs.child(
                    div()
                        .id(("emoji-tab", i))
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(radius::MD)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.bg_hover))
                        .tooltip(tooltip(*label))
                        .on_click(move |_, _, _| scroll.scroll_to_item(ix, ScrollStrategy::Top))
                        .child(div().font_family(FONT_EMOJI).child(*icon)),
                );
            }
        }

        let view: WeakEntity<Self> = cx.weak_entity();
        let list_rows = rows.clone();
        let grid = uniform_list("emoji-grid", rows.len(), move |range, _window, cx| {
            let t = cx.theme().clone();
            range
                .map(|ix| match &list_rows[ix] {
                    Row::Header(title) => div()
                        .h(px(CELL))
                        .px_1()
                        .flex()
                        .items_end()
                        .pb_1()
                        .text_size(text::XS)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.text_faint)
                        .font_family(rhein_ui::theme::FONT_MONO)
                        .child(title.to_lowercase())
                        .into_any_element(),
                    Row::Cells(cells) => div()
                        .h(px(CELL))
                        .flex()
                        .children(cells.iter().enumerate().map(|(i, cell)| {
                            let (pick, hover) = (view.clone(), view.clone());
                            let insert = cell.insert.clone();
                            let preview = (glyph_text(&cell.glyph), cell.label.clone());
                            div()
                                .id(SharedString::from(format!("emoji-{ix}-{i}")))
                                .size(px(CELL))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(radius::MD)
                                .cursor_pointer()
                                .hover(|s| s.bg(t.bg_hover))
                                .text_size(px(22.))
                                .on_hover(move |hovered, _, cx| {
                                    if *hovered {
                                        let preview = preview.clone();
                                        hover.update(cx, |v, cx| {
                                            v.hovered = Some(preview);
                                            cx.notify();
                                        })
                                        .ok();
                                    }
                                })
                                .on_click(move |_, _, cx| {
                                    let insert = insert.clone();
                                    pick.update(cx, |v, cx| v.pick(insert, cx)).ok();
                                })
                                .child(glyph(&cell.glyph))
                        }))
                        .into_any_element(),
                })
                .collect::<Vec<AnyElement>>()
        })
        .track_scroll(&self.scroll)
        .flex_1();

        let tones = div().flex().gap_0p5().children(emoji::TONES.iter().enumerate().map(|(i, hand)| {
            let active = self.tone as usize == i;
            div()
                .id(("tone", i))
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(radius::SM)
                .cursor_pointer()
                .when(active, |d| d.bg(t.bg_selected))
                .hover(|s| s.bg(t.bg_hover))
                .on_click(cx.listener(move |this, _, _, cx| this.set_tone(i as u8, cx)))
                .child(div().font_family(FONT_EMOJI).child(*hand))
        }));

        let footer = div()
            .h(px(40.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(t.border)
            .children(self.hovered.clone().map(|(g, label)| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_size(px(20.)).font_family(FONT_EMOJI).child(g))
                    .child(div().text_size(text::SM).text_color(t.text_muted).child(label))
            }))
            .child(div().flex_1())
            .child(tones);

        div()
            .id("emoji-picker")
            .occlude()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .track_focus(&self.focus)
            .key_context(PICKER_CONTEXT)
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
            .child(tabs)
            .child(div().flex_1().min_h_0().px_2().flex().flex_col().child(grid))
            .child(footer)
    }
}

fn glyph_text(g: &Glyph) -> String {
    match g {
        Glyph::Unicode(s) => s.clone(),
        Glyph::Custom { name, .. } => format!(":{name}:"),
    }
}

fn glyph(g: &Glyph) -> AnyElement {
    match g {
        Glyph::Unicode(s) => div().font_family(FONT_EMOJI).child(s.clone()).into_any_element(),
        Glyph::Custom { url, name } => {
            let name = name.clone();
            img(url.clone())
                .size(px(24.))
                .with_fallback(move || div().text_size(text::XS).child(format!(":{name}:")).into_any_element())
                .into_any_element()
        }
    }
}
