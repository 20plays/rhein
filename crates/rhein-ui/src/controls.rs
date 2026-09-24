//! Tabs, switches, list rows, section labels and toasts.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div, prelude::*, px,
};

use crate::icon::{Icon, icon};
use crate::theme::{ActiveTheme as _, FONT_MONO, radius, text};

type Click = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type Select = Rc<dyn Fn(usize, &mut Window, &mut App)>;
type Secondary = Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;
type Toggle = Rc<dyn Fn(bool, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Tabs {
    id: SharedString,
    labels: Vec<SharedString>,
    selected: usize,
    on_select: Option<Select>,
}

impl Tabs {
    pub fn new(id: impl Into<SharedString>, labels: impl IntoIterator<Item = impl Into<SharedString>>) -> Self {
        Self { id: id.into(), labels: labels.into_iter().map(Into::into).collect(), selected: 0, on_select: None }
    }

    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Tabs {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        div().flex().gap_1().children(self.labels.into_iter().enumerate().map(|(i, label)| {
            let active = i == self.selected;
            let handler = self.on_select.clone();
            div()
                .id(SharedString::from(format!("{}-{i}", self.id)))
                .px_3()
                .py_1()
                .rounded(radius::MD)
                .text_size(text::MD)
                .cursor_pointer()
                .when(active, |d| d.bg(t.bg_selected).text_color(t.text))
                .when(!active, |d| d.text_color(t.text_muted).hover(|s| s.text_color(t.text)))
                .when_some(handler, |d, h| d.on_click(move |_, window, cx| h(i, window, cx)))
                .child(label)
        }))
    }
}

#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    on: bool,
    on_toggle: Option<Toggle>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, on: bool) -> Self {
        Self { id: id.into(), on, on_toggle: None }
    }

    pub fn on_toggle(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        let on = self.on;
        div()
            .id(self.id)
            .w(px(34.))
            .h(px(20.))
            .flex_none()
            .rounded_full()
            .p(px(2.))
            .bg(if on { t.accent } else { t.bg_selected })
            .flex()
            .when(on, |d| d.justify_end())
            .cursor_pointer()
            .child(div().size(px(16.)).rounded_full().bg(if on { t.bg_base } else { t.text_muted }))
            .when_some(self.on_toggle, |d, h| d.on_click(move |_, window, cx| h(!on, window, cx)))
    }
}

/// One row in a sidebar or settings list: optional icon, a label, optional trailing element.
#[derive(IntoElement)]
pub struct ListRow {
    id: ElementId,
    icon: Option<Icon>,
    leading: Option<AnyElement>,
    label: SharedString,
    selected: bool,
    strong: bool,
    muted: bool,
    trailing: Option<AnyElement>,
    tooltip: Option<SharedString>,
    on_click: Option<Click>,
    on_secondary: Option<Secondary>,
}

impl ListRow {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            icon: None,
            leading: None,
            label: label.into(),
            selected: false,
            strong: false,
            muted: false,
            trailing: None,
            tooltip: None,
            on_click: None,
            on_secondary: None,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Bold, for unread channels.
    pub fn strong(mut self, strong: bool) -> Self {
        self.strong = strong;
        self
    }

    /// Dimmed, for offline members or unavailable channels.
    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// Right click, for context menus.
    pub fn on_secondary(mut self, handler: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_secondary = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ListRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        let color = if self.selected || self.strong { t.text } else { t.text_muted };
        let clickable = self.on_click.is_some();
        div()
            .id(self.id)
            .h(px(32.))
            .px_2()
            .rounded(radius::MD)
            .flex()
            .items_center()
            .gap_2()
            .text_size(text::MD)
            .text_color(color)
            .when(self.strong, |d| d.font_weight(FontWeight::SEMIBOLD))
            .when(self.muted, |d| d.opacity(0.5))
            .when(self.selected, |d| d.bg(t.bg_selected))
            .when(clickable && !self.selected, |d| d.hover(|s| s.bg(t.bg_hover).text_color(t.text)))
            .when(clickable, |d| d.cursor_pointer())
            .when_some(self.icon, |d, i| d.child(icon(i).text_color(t.text_faint)))
            .children(self.leading)
            .child(div().flex_1().min_w_0().truncate().child(self.label))
            .children(self.trailing)
            .when_some(self.tooltip, |d, text| d.tooltip(crate::tooltip::tooltip(text)))
            .when_some(self.on_click, |d, h| d.on_click(move |e, window, cx| h(e, window, cx)))
            .when_some(self.on_secondary, |d, h| {
                d.on_mouse_down(MouseButton::Right, move |e, window, cx| h(e, window, cx))
            })
    }
}

/// Small lowercase mono heading above a group of rows.
pub fn section_label(label: impl Into<SharedString>, cx: &App) -> gpui::Div {
    div()
        .px_2()
        .pt_4()
        .pb_1()
        .font_family(FONT_MONO)
        .text_size(text::XS)
        .text_color(cx.theme().text_faint)
        .child(label.into().to_lowercase())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Error,
}

/// A transient message pinned to the bottom of the window.
#[derive(IntoElement)]
pub struct Toast {
    text: SharedString,
    kind: ToastKind,
    on_dismiss: Option<Click>,
}

impl Toast {
    pub fn new(text: impl Into<SharedString>, kind: ToastKind) -> Self {
        Self { text: text.into(), kind, on_dismiss: None }
    }

    pub fn on_dismiss(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Toast {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.theme();
        let (glyph, color) = match self.kind {
            ToastKind::Info => (Icon::Info, t.accent),
            ToastKind::Error => (Icon::CircleAlert, t.danger),
        };
        div()
            .id("toast")
            .max_w(px(560.))
            .px_3()
            .py_2()
            .rounded(radius::LG)
            .bg(t.bg_raised)
            .border_1()
            .border_color(t.border_strong)
            .shadow_lg()
            .flex()
            .items_center()
            .gap_2()
            .text_size(text::MD)
            .text_color(t.text)
            .child(icon(glyph).text_color(color))
            .child(div().flex_1().child(self.text))
            .when_some(self.on_dismiss, |d, h| {
                d.child(
                    div()
                        .id("toast-close")
                        .cursor_pointer()
                        .text_color(t.text_faint)
                        .hover(|s| s.text_color(t.text))
                        .on_click(move |e, window, cx| h(e, window, cx))
                        .child(icon(Icon::X)),
                )
            })
    }
}
