//! Motion tokens and the few transitions Rhein uses. Motion is only there to make changes feel
//! continuous: short, ease-out, never looping on its own.
//!
//! Everything here is built on GPUI's `with_animation`, which honours `App::reduce_motion`: with it
//! set, elements render in their final state and no frames are scheduled.

use std::time::Duration;

use gpui::{Animation, AnimationExt as _, ElementId, IntoElement, Styled, px};

/// Closing things, small state changes.
pub const FAST: Duration = Duration::from_millis(120);
/// Opening overlays, arriving messages.
pub const BASE: Duration = Duration::from_millis(160);
/// Programmatic scrolling.
pub const SCROLL: Duration = Duration::from_millis(180);

/// Cubic ease-out: quick start, soft landing.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    1. - (1. - t).powi(3)
}

fn animation(duration: Duration) -> Animation {
    Animation::new(duration).with_easing(ease_out)
}

/// Fades an element in. The animation restarts whenever `id` changes, so key it by what the
/// element shows (a channel id, say) to fade on every switch.
pub fn fade_in<E>(el: E, id: impl Into<ElementId>, duration: Duration) -> impl IntoElement
where
    E: IntoElement + Styled + 'static,
{
    el.with_animation(id, animation(duration), |el, v| el.opacity(v))
}

/// Fades an element in while it rises `distance` pixels into place. For overlays and new
/// messages. The element must be positioned (`relative` or `absolute`) for the offset to apply.
pub fn rise_in<E>(el: E, id: impl Into<ElementId>, distance: f32, duration: Duration) -> impl IntoElement
where
    E: IntoElement + Styled + 'static,
{
    el.with_animation(id, animation(duration), move |el, v| el.opacity(v).top(px(distance * (1. - v))))
}

/// The opening transition of popovers, menus and dialogs.
pub fn pop_in<E>(el: E, id: impl Into<ElementId>) -> impl IntoElement
where
    E: IntoElement + Styled + 'static,
{
    rise_in(el, id, 4., BASE)
}

/// Fades an element out; used for the brief afterimage of something that just closed.
pub fn fade_out<E>(el: E, id: impl Into<ElementId>) -> impl IntoElement
where
    E: IntoElement + Styled + 'static,
{
    el.with_animation(id, animation(FAST), |el, v| el.opacity(1. - v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_starts_fast_and_lands_on_one() {
        assert_eq!(ease_out(0.), 0.);
        assert_eq!(ease_out(1.), 1.);
        assert!(ease_out(0.25) > 0.5);
        assert_eq!(ease_out(2.), 1.);
    }
}
