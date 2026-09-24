//! Ctrl+K: jump to any guild, channel or loaded thread by typing part of its name.

use gpui::{
    AppContext as _, Context, DismissEvent, Entity, EventEmitter, Focusable as _, FontWeight, IntoElement, ParentElement as _,
    Render, Styled as _, Subscription, Window, div, prelude::*, px,
};
use rhein_store::{Client, nav};
use rhein_ui::theme::{radius, text};
use rhein_ui::{ActiveTheme as _, Icon, InputEvent, Nav, TextInput, icon};

const LIMIT: usize = 12;

#[derive(Clone)]
enum Target {
    Guild(String),
    Channel(String),
}

#[derive(Clone)]
struct Item {
    target: Target,
    label: String,
    detail: String,
    glyph: Icon,
    unread: bool,
}

pub struct QuickSwitcher {
    client: Entity<Client>,
    input: Entity<TextInput>,
    selected: usize,
    _subs: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for QuickSwitcher {}

impl QuickSwitcher {
    pub fn new(client: Entity<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut i = TextInput::new("Where to? Type a channel or guild name", cx);
            i.capture_navigation(true);
            i
        });
        let subs = vec![cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::Changed => {
                this.selected = 0;
                cx.notify();
            }
            InputEvent::Navigate(Nav::Up) => {
                this.selected = this.selected.saturating_sub(1);
                cx.notify();
            }
            InputEvent::Navigate(Nav::Down) => {
                let n = this.items(cx).len();
                this.selected = (this.selected + 1).min(n.saturating_sub(1));
                cx.notify();
            }
            InputEvent::Navigate(Nav::Accept) => {
                let ix = this.selected;
                this.go(ix, cx);
            }
            InputEvent::Navigate(Nav::Close) | InputEvent::Cancel => cx.emit(DismissEvent),
            _ => {}
        })];
        window.focus(&input.focus_handle(cx), cx);
        Self { client, input, selected: 0, _subs: subs }
    }

    fn items(&self, cx: &gpui::App) -> Vec<Item> {
        let client = self.client.read(cx);
        let query = self.input.read(cx).text().trim().to_owned();
        let mut all = Vec::new();
        for entry in &client.guilds {
            let (unread, _) = client.guild_unread(&entry.guild.id);
            all.push(Item {
                target: Target::Guild(entry.guild.id.clone()),
                label: entry.guild.name.clone(),
                detail: entry.server.clone(),
                glyph: Icon::Server,
                unread,
            });
            for ch in entry.guild.channels.iter().filter(|c| c.is_textual()) {
                let parent = ch
                    .parent_id
                    .as_deref()
                    .filter(|_| ch.is_thread())
                    .and_then(|p| client.channel(p))
                    .map(|p| format!(" › #{}", p.name))
                    .unwrap_or_default();
                all.push(Item {
                    target: Target::Channel(ch.id.clone()),
                    label: ch.name.clone(),
                    detail: format!("{}{parent}", entry.guild.name),
                    glyph: if ch.is_thread() { Icon::MessagesSquare } else if ch.kind == "announcement" { Icon::Megaphone } else { Icon::Hash },
                    unread: client.read_mark(&ch.id).is_unread(),
                });
            }
        }
        if query.is_empty() {
            // Recently visited channels first, then unread ones.
            let mut out: Vec<Item> = client
                .visited
                .iter()
                .skip(1)
                .filter_map(|id| all.iter().find(|i| matches!(&i.target, Target::Channel(c) if c == id)).cloned())
                .collect();
            for item in all.iter().filter(|i| i.unread) {
                if !out.iter().any(|o| same(&o.target, &item.target)) {
                    out.push(item.clone());
                }
            }
            out.truncate(LIMIT);
            return out;
        }
        let mut scored: Vec<(i32, Item)> =
            all.into_iter().filter_map(|i| nav::fuzzy(&query, &i.label).map(|s| (s + if i.unread { 50 } else { 0 }, i))).collect();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        scored.into_iter().take(LIMIT).map(|(_, i)| i).collect()
    }

    fn go(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(item) = self.items(cx).into_iter().nth(ix) else { return };
        self.client.update(cx, |c, cx| match item.target {
            Target::Guild(id) => c.select_guild(id, cx),
            Target::Channel(id) => c.select_channel(id, cx),
        });
        cx.emit(DismissEvent);
    }
}

fn same(a: &Target, b: &Target) -> bool {
    match (a, b) {
        (Target::Guild(x), Target::Guild(y)) | (Target::Channel(x), Target::Channel(y)) => x == y,
        _ => false,
    }
}

impl Render for QuickSwitcher {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let items = self.items(cx);
        let empty_query = self.input.read(cx).text().trim().is_empty();
        div()
            .w(px(560.))
            .p_3()
            .rounded(radius::LG)
            .bg(t.bg_panel)
            .border_1()
            .border_color(t.border_strong)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_2()
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(self.input.clone())
            .when(empty_query && !items.is_empty(), |d| {
                d.child(div().px_1().font_family(rhein_ui::theme::FONT_MONO).text_size(text::XS).text_color(t.text_faint).child("recent and unread"))
            })
            .children(items.iter().enumerate().map(|(i, item)| {
                let selected = i == self.selected;
                div()
                    .id(("switch", i))
                    .px_2()
                    .py_1p5()
                    .rounded(radius::MD)
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(t.bg_selected))
                    .hover(|s| s.bg(t.bg_hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.go(i, cx)))
                    .child(icon(item.glyph).text_color(t.text_faint))
                    .child(div().when(item.unread, |d| d.font_weight(FontWeight::SEMIBOLD)).child(item.label.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_size(text::SM).text_color(t.text_faint).child(item.detail.clone()))
            }))
            .when(items.is_empty(), |d| d.child(div().p_2().text_size(text::SM).text_color(t.text_faint).child("Nothing matches.")))
    }
}
