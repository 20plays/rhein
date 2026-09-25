//! Context menus and dropdowns: a focused list that closes on selection, Escape or a click outside.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Anchor, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    IntoElement, ParentElement as _, Pixels, Point, Render, SharedString, Styled as _, Subscription, Task, Window,
    anchored, deferred, div, prelude::*, px,
};

use crate::Dismiss;
use crate::icon::{Icon, icon};
use crate::motion;
use crate::theme::{ActiveTheme as _, radius, text};

pub const MENU_CONTEXT: &str = "Menu";

type Action = Rc<dyn Fn(&mut Window, &mut App)>;
/// Builds a piece of menu content each time the menu renders.
pub type Build = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

pub enum MenuEntry {
    Item {
        label: SharedString,
        icon: Option<Icon>,
        /// Drawn instead of the icon (a presence dot, an avatar).
        leading: Option<Build>,
        /// A check at the right, for the current choice.
        checked: bool,
        danger: bool,
        action: Action,
    },
    Separator,
    Label(SharedString),
    /// Content that is not an action, such as a card heading the menu.
    Custom(Build),
}

impl MenuEntry {
    pub fn item(label: impl Into<SharedString>, action: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        MenuEntry::Item { label: label.into(), icon: None, leading: None, checked: false, danger: false, action: Rc::new(action) }
    }

    pub fn custom(build: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        MenuEntry::Custom(Rc::new(build))
    }

    pub fn with_leading(mut self, build: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        if let MenuEntry::Item { leading, .. } = &mut self {
            *leading = Some(Rc::new(build));
        }
        self
    }

    pub fn checked(mut self, on: bool) -> Self {
        if let MenuEntry::Item { checked, .. } = &mut self {
            *checked = on;
        }
        self
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
    width: Option<Pixels>,
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
        Self { entries, focus: cx.focus_handle(), width: None, closing: false }
    }
}

impl Render for Menu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .when_some(self.width, |d, w| d.w(w))
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
                MenuEntry::Custom(build) => list.child(build(window, cx)),
                MenuEntry::Item { label, icon: glyph, leading, checked, danger, action } => {
                    let action = action.clone();
                    let color = if *danger { t.danger } else { t.text };
                    let hover_bg = if *danger { t.danger_soft } else { t.bg_selected };
                    let hover_fg = if *danger { t.danger } else { t.text };
                    let row = div()
                        .id(("menu-item", i))
                        .px_2()
                        .py_1p5()
                        .rounded(radius::SM)
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_color(color)
                        .map(|d| match (leading, glyph) {
                            (Some(build), _) => d.child(build(window, cx)),
                            (None, Some(g)) => d.child(icon(*g).text_color(color)),
                            (None, None) => d,
                        })
                        .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                        .when(*checked, |d| d.child(icon(Icon::Check).text_color(t.text_muted)));
                    // A closing menu is only an afterimage: no hover, no clicks.
                    if closing {
                        list.child(row)
                    } else {
                        list.child(
                            row.cursor_pointer()
                                .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
                                .on_click(cx.listener(move |_, _, window, cx| {
                                    cx.emit(DismissEvent);
                                    action(window, cx);
                                })),
                        )
                    }
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
    open: Option<(Place, Entity<Menu>, Subscription)>,
    /// The menu that just closed, fading out.
    closing: Option<(Place, Entity<Menu>, Task<()>)>,
}

/// Where a menu opens: the point and which of its corners sits there.
#[derive(Clone, Copy)]
struct Place {
    at: Point<Pixels>,
    corner: Anchor,
}

impl MenuHost {
    /// Opens a menu with its top-left corner at `at` (a click position).
    pub fn show<V: 'static>(
        &mut self,
        at: Point<Pixels>,
        entries: Vec<MenuEntry>,
        host: fn(&mut V) -> &mut MenuHost,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        self.open_menu(Place { at, corner: Anchor::TopLeft }, entries, None, host, window, cx);
    }

    /// Opens a menu of a fixed width with the given corner at `at`, e.g. upward from a bar at
    /// the bottom of the window.
    #[allow(clippy::too_many_arguments)]
    pub fn show_anchored<V: 'static>(
        &mut self,
        at: Point<Pixels>,
        corner: Anchor,
        width: Pixels,
        entries: Vec<MenuEntry>,
        host: fn(&mut V) -> &mut MenuHost,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        self.open_menu(Place { at, corner }, entries, Some(width), host, window, cx);
    }

    fn open_menu<V: 'static>(
        &mut self,
        place: Place,
        entries: Vec<MenuEntry>,
        width: Option<Pixels>,
        host: fn(&mut V) -> &mut MenuHost,
        window: &mut Window,
        cx: &mut Context<V>,
    ) {
        let menu = cx.new(|cx| {
            let mut menu = Menu::new(entries, cx);
            menu.width = width;
            menu
        });
        let sub = cx.subscribe_in(&menu, window, move |view, _, _: &DismissEvent, _, cx| {
            if let Some((place, menu, _)) = host(view).open.take() {
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
                host(view).closing = Some((place, menu, done));
            }
            cx.notify();
        });
        window.focus(&menu.focus_handle(cx), cx);
        self.open = Some((place, menu, sub));
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// The open menu and the afterimage of one that just closed, if any.
    pub fn element(&self) -> Vec<AnyElement> {
        let closing = self.closing.as_ref().map(|(place, menu, _)| (*place, menu));
        let open = self.open.as_ref().map(|(place, menu, _)| (*place, menu));
        closing
            .into_iter()
            .chain(open)
            .map(|(place, menu)| {
                deferred(anchored().position(place.at).anchor(place.corner).snap_to_window_with_margin(px(8.)).child(menu.clone()))
                    // Above everything else that floats, a profile card or a picker included.
                    .with_priority(3)
                    .into_any_element()
            })
            .collect()
    }
}
