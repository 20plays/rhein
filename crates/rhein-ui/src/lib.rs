//! Rhein's design system. Depends only on gpui: it knows nothing about Telium, the store or views.
//!
//! Call [`init`] once at startup; then read tokens with `cx.theme()` and build views from the
//! components exported here.

mod assets;
mod avatar;
mod badge;
mod button;
mod controls;
mod icon;
pub mod input;
mod menu;
mod modal;
pub mod theme;
mod tooltip;

use gpui::{App, KeyBinding, actions};

pub use assets::Assets;
pub use avatar::{Avatar, Presence};
pub use badge::{CountBadge, Tag, UnreadDot};
pub use button::{Button, ButtonKind, IconButton};
pub use controls::{ListRow, Switch, Tabs, Toast, ToastKind, section_label};
pub use icon::{Icon, icon, icon_sized};
pub use input::{InputEvent, TextInput};
pub use menu::{Menu, MenuEntry, MenuHost};
pub use modal::Modal;
pub use theme::{ActiveTheme, Theme};
pub use tooltip::tooltip;

actions!(rhein_ui, [Dismiss]);

/// Installs the theme, the embedded fonts and the component key bindings.
pub fn init(cx: &mut App) {
    cx.set_global(Theme::dark());
    if let Err(err) = Assets::load_fonts(cx) {
        log::error!("loading bundled fonts: {err:#}");
    }
    cx.bind_keys(input::bindings());
    cx.bind_keys([
        KeyBinding::new("escape", Dismiss, Some(modal::MODAL_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(menu::MENU_CONTEXT)),
    ]);
}
