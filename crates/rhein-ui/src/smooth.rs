//! Smooth wheel scrolling. GPUI scrolls a list or a scrolling div by the whole wheel step at
//! once; with a mouse wheel that is a jump of a few lines per notch. `smooth_scroll` wraps such a
//! container and, for wheel steps counted in lines, glides the same distance over a few frames
//! instead (an exponential ease, settled in about 120 ms; notches in quick succession add up).
//! Touchpads already report pixels as the fingers move and are left alone, and with reduce
//! motion the step is taken at once, as before.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Element, ElementId, GlobalElementId, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    ListState, Pixels, Point, ScrollDelta, ScrollHandle, ScrollWheelEvent, Window, px,
};

/// What gets scrolled.
#[derive(Clone)]
pub enum ScrollTarget {
    List(ListState),
    Handle(ScrollHandle),
}

/// The time constant of the glide: 63% of the way after this long.
const TAU_SECS: f32 = 0.04;
/// How far a line of wheel travels in a list (what GPUI's list uses).
const LIST_LINE: f32 = 20.;

/// The rest of a glide in progress.
#[doc(hidden)]
#[derive(Default)]
pub struct Glide {
    /// Pixels still to scroll; positive moves the content up (towards later items).
    remaining: f32,
    last: Option<Instant>,
}

pub struct SmoothScroll {
    id: ElementId,
    target: ScrollTarget,
    child: Option<AnyElement>,
}

/// Wraps `child`, the scrolling container `target` belongs to.
pub fn smooth_scroll(id: impl Into<ElementId>, target: ScrollTarget, child: impl IntoElement) -> SmoothScroll {
    SmoothScroll { id: id.into(), target, child: Some(child.into_any_element()) }
}

impl IntoElement for SmoothScroll {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

fn apply(target: &ScrollTarget, distance: f32) {
    if distance == 0. {
        return;
    }
    match target {
        // Through the scrollbar's setter, which measures from the viewport's top as the wheel
        // does (`scroll_by` counts from the end of the content while a bottom-aligned list sits
        // at its end, and the list snaps straight back), and takes up following the tail again
        // at the bottom.
        ScrollTarget::List(list) => {
            let y = list.scroll_px_offset_for_scrollbar().y - px(distance);
            list.set_offset_from_scrollbar(Point::new(px(0.), y));
        }
        ScrollTarget::Handle(handle) => {
            let max = handle.max_offset();
            let offset = handle.offset();
            let y = (offset.y - px(distance)).clamp(-max.y, px(0.));
            handle.set_offset(Point::new(offset.x, y));
        }
    }
}

impl Element for SmoothScroll {
    type RequestLayoutState = (AnyElement, Rc<RefCell<Glide>>);
    type PrepaintState = Option<gpui::Hitbox>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, id: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
        let glide = window.with_element_state(id.expect("smooth_scroll has an id"), |state: Option<Rc<RefCell<Glide>>>, _| {
            let glide = state.unwrap_or_default();
            (glide.clone(), glide)
        });
        // One step of the glide per frame, as far as the time since the last frame says.
        {
            let mut g = glide.borrow_mut();
            if g.remaining != 0. {
                let now = Instant::now();
                let dt = g.last.map_or(1. / 120., |at| now.duration_since(at).as_secs_f32()).min(0.1);
                g.last = Some(now);
                let step = if g.remaining.abs() < 0.5 { g.remaining } else { g.remaining * (1. - (-dt / TAU_SECS).exp()) };
                g.remaining -= step;
                apply(&self.target, step);
                if g.remaining != 0. {
                    window.request_animation_frame();
                } else {
                    g.last = None;
                }
            }
        }
        let mut child = self.child.take().expect("laid out once");
        let layout = child.request_layout(window, cx);
        (layout, (child, glide))
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, state: &mut Self::RequestLayoutState, window: &mut Window, cx: &mut App) -> Self::PrepaintState {
        state.0.prepaint(window, cx);
        Some(window.insert_hitbox(bounds, HitboxBehavior::Normal))
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, state: &mut Self::RequestLayoutState, hitbox: &mut Self::PrepaintState, window: &mut Window, cx: &mut App) {
        state.0.paint(window, cx);
        let Some(hitbox) = hitbox.clone() else { return };
        let (glide, target) = (state.1.clone(), self.target.clone());
        let line = match &target {
            ScrollTarget::List(_) => LIST_LINE,
            ScrollTarget::Handle(_) => f32::from(window.line_height()),
        };
        // The view this sits in: it has to render again for the glide to take its next step
        // (a window refresh alone reuses the cached view, and nothing would move).
        let view = window.current_view();
        // Bubbling goes from the front to the back, and this handler comes after the container's
        // own, so it sees the step first, and the frontmost glide takes it (an overlay over the
        // chat, a picker over the composer).
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                return;
            }
            let ScrollDelta::Lines(lines) = event.delta else { return };
            if lines.y == 0. {
                return;
            }
            let distance = -lines.y * line;
            cx.stop_propagation();
            if cx.reduce_motion() {
                apply(&target, distance);
            } else {
                let mut g = glide.borrow_mut();
                // Turning the wheel the other way drops what was left of the old direction.
                if g.remaining.signum() != distance.signum() {
                    g.remaining = 0.;
                }
                g.remaining += distance;
                g.last = Some(Instant::now());
            }
            cx.notify(view);
            window.refresh();
        });
    }
}
