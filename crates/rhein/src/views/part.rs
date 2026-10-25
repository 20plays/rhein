//! Pieces of a big view drawn as cached views of their own.
//!
//! GPUI draws every view that is not cached again on every frame, and a cached view again
//! whenever anything inside it changes. A cursor blinking in the message bar, a hover or a step
//! of a scroll glide redrew the whole sidebar, the member list and the history with it. A part
//! is a slice of its owner's render (one of the owner's methods) in a cached view: it draws again
//! only when the owner or something it watches notifies, or when an element inside it changes (a
//! hover, a scroll). Otherwise the frame reuses what it drew last.
//!
//! A cached view is not redrawn for what it reads, only when notified: a part has to watch
//! every entity whose changes it shows.

use gpui::{AnyElement, App, AppContext as _, Context, Entity, IntoElement, Render, Subscription, WeakEntity, Window, div};

pub type Draw<V> = fn(&mut V, &mut Window, &mut Context<V>) -> AnyElement;

pub struct Part<V: 'static> {
    owner: WeakEntity<V>,
    draw: Draw<V>,
    _watch: Vec<Subscription>,
}

/// A part of `owner` drawn by `draw`, again whenever `owner` notifies.
pub fn part<V: 'static>(owner: &Entity<V>, draw: Draw<V>, cx: &mut App) -> Entity<Part<V>> {
    cx.new(|cx| Part { owner: owner.downgrade(), draw, _watch: vec![cx.observe(owner, |_, _, cx| cx.notify())] })
}

impl<V: 'static> Part<V> {
    /// Draws again whenever `entity` notifies too.
    pub fn watch<T: 'static>(&mut self, entity: &Entity<T>, cx: &mut Context<Self>) {
        self._watch.push(cx.observe(entity, |_, _, cx| cx.notify()));
    }
}

impl<V: 'static> Render for Part<V> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let draw = self.draw;
        self.owner.update(cx, |owner, cx| draw(owner, window, cx)).unwrap_or_else(|_| div().into_any_element())
    }
}

/// Nothing but something to notify: a ticking clock read by a view that should redraw on each
/// tick, without notifying the view itself (and every part watching it).
pub struct Clock;
