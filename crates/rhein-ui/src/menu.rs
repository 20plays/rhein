//! Context menus and dropdowns: a focused list that closes on selection, Escape or a click outside.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement as _, Pixels, Point, Render, SharedString, Styled as _, Subscription, Task, Window,
    anchored, deferred, div, prelude::*, px,
};

use crate::Dismiss;
use crate::icon::{Icon, icon};
use crate::motion;
use crate::theme::{ActiveTheme as _, radius, text};

pub const MENU_CONTEXT: &str = "Menu";

type Action = Rc<dyn Fn(&mut Window, &mut App)>;

pub enum MenuEntry {
    Item { label: SharedString, icon: Option<Icon>, danger: bool, action: Action },
    Separator,
    Label(SharedString),
}

impl MenuEntry {
    pub fn item(label: impl Into<SharedString>, action: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        MenuEntry::Item { label: label.into(), icon: None, danger: false, action: Rc::new(action) }
    }

    pub fn with_icon(mut self, with: Icon) -> Self {
        if let MenuEntry::Item { icon, .. } = &mut self {
            *icon = Some(with);
        }
        self
    }

    pub fn danger(mut self) -> Self {
        if let MenuEntry::Item { danger, .. } = &mut self {
            *danger = true;
        }
        self
    }
}

pub struct Menu {
    entries: Vec<MenuEntry>,
    focus: FocusHandle,
    /// Closed and fading out: drawn without any handlers, so input goes to what's beneath.
    closing: bool,
}

impl EventEmitter<DismissEvent> for Menu {}

impl Focusable for Menu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Menu {
    pub fn new(entries: Vec<MenuEntry>, cx: &mut Context<Self>) -> Self {
        Self { entries, focus: cx.focus_handle(), closing: false }
    }
}

impl Render for Menu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let closing = self.closing;
        let mut list = div()
            .id("menu")
            .relative()
            .when(!closing, |d| {
                d.occlude()
                    .track_focus(&self.focus)
                    .key_context(MENU_CONTEXT)
                    .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
                    .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            })
            .min_w(px(180.))
            .p_1()
            .rounded(radius::LG)
            .bg(t.bg_raised)
            .border_1()
            .border_color(t.border_strong)
            .shadow_lg()
            .flex()
            .flex_col()
            .text_size(text::MD);
        for (i, entry) in self.entries.iter().enumerate() {
            list = match entry {
                MenuEntry::Separator => list.child(div().my_1().h(px(1.)).bg(t.border)),
                MenuEntry::Label(label) => list.child(
                    div().px_2().pt_1p5().pb_1().text_size(text::XS).text_color(t.text_faint).child(label.clone()),
                ),
                MenuEntry::Item { label, icon: glyph, danger, action } => {
                    let action = action.clone();
                    let color = if *danger { t.danger } else { t.text };
                    let hover_bg = if *danger { t.danger_soft } else { t.bg_selected };
                    let hover_fg = if *danger { t.danger } else { t.text };
                    if closing {
                        list = list.child(
                            div().px_2().py_1p5().flex().items_center().gap_2().text_color(color).when_some(*glyph, |d, g| d.child(icon(g))).child(label.clone()),
                        );
                        continue;
                    }
                    list.child(
                        div()
                            .id(("menu-item", i))
                            .px_2()
                            .py_1p5()
                            .rounded(radius::SM)
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_color(color)
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
                            .when_some(*glyph, |d, g| d.child(icon(g)))
                            .child(label.clone())
                            .on_click(cx.listener(move |_, _, window, cx| {
                                cx.emit(DismissEvent);
                                action(window, cx);
                            })),
                    )
                }
            };
        }
        if closing { motion::fade_out(list, "menu-out").into_any_element() } else { motion::pop_in(list, "menu-in").into_any_element() }
    }
}

/// Keeps at most one open menu for a view. Put it in a field, call `show` from a click handler
/// and render `element()` last in the view so the menu floats above everything.
#[derive(Default)]
pub struct MenuHost {
    open: Option<(Point<Pixels>, Entity<Menu>, Subscription)>,
    /// The menu that just closed, fading out.
    closing: Option<(Point<Pixels>, Entity<Menu>, Task<()>)>,
}

impl MenuHost {
    pub fn show<V: 'static>(
        &mut self,
        at: Point<Pixels>,
        entries: Vec<MenuEntry>,
        host: fn(&mut V) -> &mut MenuHost,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let menu = cx.new(|cx| Menu::new(entries, cx));
        let sub = cx.subscribe_in(&menu, window, move |view, _, _: &DismissEvent, _, cx| {
            if let Some((at, menu, _)) = host(view).open.take() {
                menu.update(cx, |m, cx| {
                    m.closing = true;
                    cx.notify();
                });
                let done = cx.spawn(async move |view, cx| {
                    cx.background_executor().timer(motion::FAST).await;
                    view.update(cx, |view, cx| {
                        host(view).closing = None;
                        cx.notify();
                    })
                    .ok();
                });
                host(view).closing = Some((at, menu, done));
            }
            cx.notify();
        });
        window.focus(&menu.focus_handle(cx), cx);
        self.open = Some((at, menu, sub));
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The open menu and the afterimage of one that just closed, if any.
    pub fn element(&self) -> Vec<AnyElement> {
        let closing = self.closing.as_ref().map(|(at, menu, _)| (at, menu));
        let open = self.open.as_ref().map(|(at, menu, _)| (at, menu));
        closing
            .into_iter()
            .chain(open)
            .map(|(at, menu)| {
                deferred(anchored().position(*at).snap_to_window_with_margin(px(8.)).child(menu.clone()))
                    .with_priority(1)
                    .into_any_element()
            })
            .collect()
    }
}
