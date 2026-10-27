//! Rows of a list drawn as cached views that keep their size (`ViewElement::keep_size`, from
//! Rhein's GPUI patch in vendor/gpui): a row renders again when it changes, not when its
//! neighbours, the view holding the list or the scroll position do.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ListAlignment, ListState, Modifiers, ParentElement as _, Render,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, TestAppContext, VisualTestContext, Window, div, list, point, px, size,
};

struct Row {
    ix: usize,
    height: f32,
    renders: Rc<Cell<usize>>,
    clicks: Rc<RefCell<Vec<usize>>>,
}

impl Render for Row {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        let (ix, clicks) = (self.ix, self.clicks.clone());
        div().id("row").w_full().h(px(self.height)).on_click(move |_, _, _| clicks.borrow_mut().push(ix)).child("row")
    }
}

/// Holds the list, cached like Rhein's history part.
struct Host {
    list: ListState,
    rows: Vec<Entity<Row>>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.clone();
        list(self.list.clone(), move |ix, _, _| {
            rows[ix].clone().cached(StyleRefinement::default().w_full()).keep_size().into_any_element()
        })
        .size_full()
    }
}

struct Root {
    host: Entity<Host>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.host.clone().cached(StyleRefinement::default().size_full()))
    }
}

struct Fixture {
    host: Entity<Host>,
    rows: Vec<Entity<Row>>,
    renders: Vec<Rc<Cell<usize>>>,
    clicks: Rc<RefCell<Vec<usize>>>,
    list: ListState,
}

fn setup(cx: &mut TestAppContext) -> (Fixture, &mut VisualTestContext) {
    let renders: Vec<_> = (0..50).map(|_| Rc::new(Cell::new(0))).collect();
    let list = ListState::new(50, ListAlignment::Top, px(0.));
    let clicks = Rc::new(RefCell::new(Vec::new()));
    let (rows, host) = cx.update(|cx| {
        let rows: Vec<_> = renders
            .iter()
            .enumerate()
            .map(|(ix, r)| cx.new(|_| Row { ix, height: 20., renders: r.clone(), clicks: clicks.clone() }))
            .collect();
        let host = cx.new(|_| Host { list: list.clone(), rows: rows.clone() });
        (rows, host)
    });
    let root_host = host.clone();
    let (_, cx) = cx.add_window_view(move |_, _| Root { host: root_host });
    let fixture = Fixture { host, rows, renders, clicks, list };
    // 15 rows of 20 px in view, with the pointer away from them.
    cx.simulate_resize(size(px(400.), px(300.)));
    cx.simulate_mouse_move(point(px(1000.), px(1000.)), None, Modifiers::none());
    frame(cx);
    frame(cx);
    (fixture, cx)
}

/// Scrolls the list as its wheel handler does: moving it and notifying the view drawing it.
fn scroll(f: &Fixture, by: f32, cx: &mut VisualTestContext) {
    f.list.scroll_by(px(by));
    f.host.update(cx, |_, cx| cx.notify());
    frame(cx);
}

fn frame(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
}

fn counts(f: &Fixture) -> Vec<usize> {
    f.renders.iter().map(|r| r.get()).collect()
}

#[gpui::test]
fn a_row_draws_again_only_when_it_is_notified(cx: &mut TestAppContext) {
    let (f, cx) = setup(cx);
    let before = counts(&f);
    assert!(before[0] >= 1, "the first rows are drawn");

    f.rows[3].update(cx, |_, cx| cx.notify());
    frame(cx);
    let after = counts(&f);
    assert_eq!(after[3], before[3] + 1, "the notified row draws again");
    for (ix, (a, b)) in after.iter().zip(&before).enumerate().filter(|(ix, _)| *ix != 3) {
        assert_eq!(a, b, "row {ix} is reused");
    }
}

#[gpui::test]
fn rows_are_reused_while_the_view_holding_them_draws_again(cx: &mut TestAppContext) {
    let (f, cx) = setup(cx);
    let before = counts(&f);
    f.host.update(cx, |_, cx| cx.notify());
    frame(cx);
    assert_eq!(counts(&f), before);
}

#[gpui::test]
fn a_row_that_grows_is_measured_again(cx: &mut TestAppContext) {
    let (f, cx) = setup(cx);
    let next_top = |cx: &mut VisualTestContext| {
        let _ = cx;
        f.list.bounds_for_item(3).map(|b| b.top())
    };
    let top = next_top(cx).expect("row 3 is shown");
    f.rows[2].update(cx, |row, cx| {
        row.height = 50.;
        cx.notify();
    });
    frame(cx);
    frame(cx);
    assert_eq!(next_top(cx), Some(top + px(30.)), "the row under the grown one moves down");
    assert_eq!(f.list.bounds_for_item(2).map(|b| b.size.height), Some(px(50.)));
}

#[gpui::test]
fn scrolling_moves_rows_without_drawing_them(cx: &mut TestAppContext) {
    let (f, cx) = setup(cx);
    let before = counts(&f);
    scroll(&f, 7., cx);
    let after = counts(&f);
    let drawn: Vec<usize> = (0..50).filter(|&ix| after[ix] != before[ix]).collect();
    // Only the rows the edges cut, before or after, draw again.
    assert!(drawn.len() <= 3, "drawn again: {drawn:?}");
    assert_eq!(f.list.bounds_for_item(5).map(|b| b.top()), Some(px(5. * 20. - 7.)));
}

#[gpui::test]
fn the_row_under_the_pointer_is_drawn_where_it_is(cx: &mut TestAppContext) {
    let (f, cx) = setup(cx);
    scroll(&f, 7., cx);
    scroll(&f, 5., cx);
    let target = f.list.bounds_for_item(6).expect("row 6 in view").center();
    // The pointer comes over the row first, as it does before any click.
    cx.simulate_mouse_move(target, None, Modifiers::none());
    frame(cx);
    cx.simulate_click(target, Modifiers::none());
    assert_eq!(*f.clicks.borrow(), vec![6]);
}

#[gpui::test]
fn a_moved_row_hears_no_click_meant_for_where_it_was(cx: &mut TestAppContext) {
    let (f, cx) = setup(cx);
    // Row 6 was painted at 120..140; moved up 12 px, 120..140 now holds most of row 6's lower
    // neighbour, also moved. Clicking there without the pointer arriving first (no frame between)
    // may reach no row, but never a wrong one.
    scroll(&f, 12., cx);
    let target = point(px(100.), px(130.));
    cx.simulate_click(target, Modifiers::none());
    let clicks = f.clicks.borrow().clone();
    let under = (0..50).find(|&ix| f.list.bounds_for_item(ix).is_some_and(|b| b.contains(&target)));
    assert!(clicks.is_empty() || clicks == under.into_iter().collect::<Vec<_>>(), "clicked {clicks:?}, under {under:?}");
}

/// A view inside a row that changes on its own, as a video playing in a message does.
struct Player {
    renders: Rc<Cell<usize>>,
}

impl Render for Player {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        div().w_full().h(px(10.)).child(format!("frame {}", self.renders.get()))
    }
}

struct PlayerRow {
    player: Entity<Player>,
}

impl Render for PlayerRow {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w_full().h(px(20.)).child(self.player.clone())
    }
}

struct PlayerHost {
    list: ListState,
    rows: Vec<gpui::AnyView>,
}

impl Render for PlayerHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.clone();
        list(self.list.clone(), move |ix, _, _| rows[ix].clone().cached(StyleRefinement::default().w_full()).keep_size().into_any_element()).size_full()
    }
}

#[gpui::test]
fn a_view_inside_a_row_draws_on_every_change(cx: &mut TestAppContext) {
    let renders = Rc::new(Cell::new(0));
    let list = ListState::new(10, ListAlignment::Top, px(0.));
    let (player, host) = cx.update(|cx| {
        let player = cx.new(|_| Player { renders: renders.clone() });
        let rows: Vec<gpui::AnyView> = (0..10)
            .map(|ix| {
                if ix == 2 {
                    cx.new(|_| PlayerRow { player: player.clone() }).into()
                } else {
                    cx.new(|_| Row { ix, height: 20., renders: Rc::default(), clicks: Rc::default() }).into()
                }
            })
            .collect();
        (player, cx.new(|_| PlayerHost { list: list.clone(), rows }))
    });
    let root = host.clone();
    let (_, cx) = cx.add_window_view(move |_, _| Root2 { host: root });
    cx.simulate_resize(size(px(400.), px(300.)));
    cx.simulate_mouse_move(point(px(1000.), px(1000.)), None, Modifiers::none());
    frame(cx);
    frame(cx);
    for pointer in [point(px(1000.), px(1000.)), point(px(100.), px(50.))] {
        cx.simulate_mouse_move(pointer, None, Modifiers::none());
        frame(cx);
        for n in 0..5 {
            // Something else draws the list again in between, its rows reused.
            host.update(cx, |_, cx| cx.notify());
            frame(cx);
            let before = renders.get();
            player.update(cx, |_, cx| cx.notify());
            frame(cx);
            assert_eq!(renders.get(), before + 1, "change {n}, pointer at {pointer:?}");
            list.scroll_by(px(1.));
            host.update(cx, |_, cx| cx.notify());
            frame(cx);
            let before = renders.get();
            player.update(cx, |_, cx| cx.notify());
            frame(cx);
            assert_eq!(renders.get(), before + 1, "change {n} after a scroll, pointer at {pointer:?}");
        }
    }
}

struct Root2 {
    host: Entity<PlayerHost>,
}

impl Render for Root2 {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.host.clone().cached(StyleRefinement::default().size_full()))
    }
}

/// A row that says where it was painted (`debug_bounds`).
struct Marked {
    name: &'static str,
}

impl Render for Marked {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let name = self.name;
        div().w_full().h(px(20.)).debug_selector(move || name.to_owned()).child(name)
    }
}

/// Two channels' rows in one list, as the history shows one channel and then another.
struct Channels {
    list: ListState,
    a: Vec<Entity<Marked>>,
    b: Vec<Entity<Marked>>,
    showing_a: bool,
}

impl Render for Channels {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let rows = if self.showing_a { self.a.clone() } else { self.b.clone() };
        list(self.list.clone(), move |ix, _, _| rows[ix].clone().cached(StyleRefinement::default().w_full()).keep_size().into_any_element()).size_full()
    }
}

struct Root3 {
    host: Entity<Channels>,
}

impl Render for Root3 {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.host.clone().cached(StyleRefinement::default().size_full()))
    }
}

#[gpui::test]
fn a_channel_shown_again_draws_its_rows(cx: &mut TestAppContext) {
    channel_switches(cx, ListAlignment::Top, false);
}

#[gpui::test]
fn a_channel_shown_again_draws_its_rows_at_the_bottom(cx: &mut TestAppContext) {
    channel_switches(cx, ListAlignment::Bottom, false);
}

#[gpui::test]
fn a_channel_shown_again_draws_its_rows_in_its_own_list(cx: &mut TestAppContext) {
    channel_switches(cx, ListAlignment::Bottom, true);
}

/// Switches between two channels' rows, the list started over (or, with `own_lists`, each
/// channel's list kept and put back), as the history does.
fn channel_switches(cx: &mut TestAppContext, alignment: ListAlignment, own_lists: bool) {
    const A: [&str; 5] = ["a0", "a1", "a2", "a3", "a4"];
    const B: [&str; 5] = ["b0", "b1", "b2", "b3", "b4"];
    let new_list = || {
        let list = ListState::new(5, alignment, px(600.));
        list.set_follow_mode(gpui::FollowMode::Tail);
        list
    };
    let list = new_list();
    let mut other = Some(new_list());
    let host = cx.update(|cx| {
        let a = A.iter().map(|&name| cx.new(|_| Marked { name })).collect();
        let b = B.iter().map(|&name| cx.new(|_| Marked { name })).collect();
        cx.new(|_| Channels { list: list.clone(), a, b, showing_a: true })
    });
    let root = host.clone();
    let (_, cx) = cx.add_window_view(move |_, _| Root3 { host: root });
    cx.simulate_resize(size(px(400.), px(300.)));
    cx.simulate_mouse_move(point(px(1000.), px(1000.)), None, Modifiers::none());
    frame(cx);
    frame(cx);
    assert!(cx.debug_bounds("a2").is_some(), "channel a is drawn");
    for (showing_a, name) in [(false, "b2"), (true, "a2"), (false, "b2"), (true, "a2")] {
        host.update(cx, |h, cx| {
            h.showing_a = showing_a;
            match (own_lists, other.take()) {
                (true, Some(kept)) => other = Some(std::mem::replace(&mut h.list, kept)),
                _ => {
                    h.list.reset(5);
                    other = Some(new_list());
                }
            }
            cx.notify();
        });
        frame(cx);
        assert!(cx.debug_bounds(name).is_some(), "{name} is drawn after switching to it");
        frame(cx);
        assert!(cx.debug_bounds(name).is_some(), "{name} is still drawn a frame later");
    }
}

/// Holds the list in a box that fades, as the history fades in after a switch.
struct Fading {
    list: ListState,
    rows: Vec<Entity<Row>>,
    opacity: f32,
}

impl Render for Fading {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.clone();
        div()
            .size_full()
            .opacity(self.opacity)
            .child(list(self.list.clone(), move |ix, _, _| rows[ix].clone().cached(StyleRefinement::default().w_full()).keep_size().into_any_element()).size_full())
    }
}

struct Root4 {
    host: Entity<Fading>,
}

impl Render for Root4 {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.host.clone().cached(StyleRefinement::default().size_full()))
    }
}

#[gpui::test]
fn rows_draw_again_as_what_holds_them_fades(cx: &mut TestAppContext) {
    let renders: Vec<_> = (0..10).map(|_| Rc::new(Cell::new(0))).collect();
    let list = ListState::new(10, ListAlignment::Top, px(0.));
    let host = cx.update(|cx| {
        let rows = renders.iter().enumerate().map(|(ix, r)| cx.new(|_| Row { ix, height: 20., renders: r.clone(), clicks: Rc::default() })).collect();
        cx.new(|_| Fading { list: list.clone(), rows, opacity: 0.01 })
    });
    let root = host.clone();
    let (_, cx) = cx.add_window_view(move |_, _| Root4 { host: root });
    cx.simulate_resize(size(px(400.), px(300.)));
    cx.simulate_mouse_move(point(px(1000.), px(1000.)), None, Modifiers::none());
    frame(cx);
    frame(cx);
    for opacity in [0.5, 1.0] {
        let before = renders[3].get();
        host.update(cx, |h, cx| {
            h.opacity = opacity;
            cx.notify();
        });
        frame(cx);
        // Kept as drawn at the faint opacity, the row would stay invisible.
        assert_eq!(renders[3].get(), before + 1, "the row draws again at opacity {opacity}");
    }
    // At a steady opacity it is reused again.
    let before = renders[3].get();
    host.update(cx, |_, cx| cx.notify());
    frame(cx);
    assert_eq!(renders[3].get(), before, "reused while nothing changes");
}

/// Text laid out before the history in the window, as a sidebar or an overlay has: how much of
/// it there is moves where the history's text layouts sit in each frame.
struct Before {
    lines: usize,
    host: Entity<Host>,
}

impl Render for Before {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .children((0..self.lines).map(|n| div().id(n).on_click(|_, _, _| {}).child(format!("line {n}"))))
            .child(self.host.clone().cached(StyleRefinement::default().size_full()))
    }
}

#[gpui::test]
fn rows_reused_after_what_comes_before_them_changed_find_their_text(cx: &mut TestAppContext) {
    let renders: Vec<_> = (0..50).map(|_| Rc::new(Cell::new(0))).collect();
    let list = ListState::new(50, ListAlignment::Top, px(0.));
    let host = cx.update(|cx| {
        let rows = renders.iter().enumerate().map(|(ix, r)| cx.new(|_| Row { ix, height: 20., renders: r.clone(), clicks: Rc::default() })).collect();
        cx.new(|_| Host { list: list.clone(), rows })
    });
    let root_host = host.clone();
    let (root, cx) = cx.add_window_view(move |_, _| Before { lines: 0, host: root_host });
    cx.simulate_resize(size(px(400.), px(300.)));
    cx.simulate_mouse_move(point(px(1000.), px(1000.)), None, Modifiers::none());
    frame(cx);
    frame(cx);
    for lines in [6, 0, 3, 9] {
        // Lines come or go before the history, which is reused whole meanwhile...
        root.update(cx, |r, cx| {
            r.lines = lines;
            cx.notify();
        });
        frame(cx);
        // ...and then draws again, its rows reused where this frame has them.
        host.update(cx, |_, cx| cx.notify());
        frame(cx);
        frame(cx);
    }
}
