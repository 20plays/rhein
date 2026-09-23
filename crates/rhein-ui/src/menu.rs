//! Context menus and dropdowns: a focused list that closes on selection, Escape or a click outside.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement as _, Pixels, Point, Render, SharedString, Styled as _, Subscription, Window,
    anchored, deferred, div, prelude::*, px,
};

use crate::Dismiss;
use crate::icon::{Icon, icon};
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
}

impl EventEmitter<DismissEvent> for Menu {}

impl Focusable for Menu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Menu {
    pub fn new(entries: Vec<MenuEntry>, cx: &mut Context<Self>) -> Self {
        Self { entries, focus: cx.focus_handle() }
    }
}

impl Render for Menu {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let mut list = div()
            .id("menu")
            .track_focus(&self.focus)
            .key_context(MENU_CONTEXT)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
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
                    let hover_bg = if *danger { t.danger } else { t.accent };
                    let hover_fg = t.text_on_accent;
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
        list
    }
}

/// Keeps at most one open menu for a view. Put it in a field, call `show` from a click handler
/// and render `element()` last in the view so the menu floats above everything.
#[derive(Default)]
pub struct MenuHost {
    open: Option<(Point<Pixels>, Entity<Menu>, Subscription)>,
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
            host(view).open = None;
            cx.notify();
        });
        window.focus(&menu.focus_handle(cx), cx);
        self.open = Some((at, menu, sub));
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    pub fn element(&self) -> Option<AnyElement> {
        let (at, menu, _) = self.open.as_ref()?;
        Some(
            deferred(anchored().position(*at).snap_to_window_with_margin(px(8.)).child(menu.clone()))
                .with_priority(1)
                .into_any_element(),
        )
    }
}
