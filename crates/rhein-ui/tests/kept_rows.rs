//! Rows of a list drawn as cached views that keep their size (`ViewElement::keep_size`, from
//! Rhein's GPUI patch in vendor/gpui): a row renders again when it changes, not when its
//! neighbours, the view holding the list or the scroll position do.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ListAlignment, ListState, ParentElement as _, Render, StyleRefinement, Styled as _,
    TestAppContext, VisualTestContext, Window, div, list, px,
};

struct Row {
    height: f32,
    renders: Rc<Cell<usize>>,
}

impl Render for Row {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        div().w_full().h(px(self.height)).child("row")
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
    list: ListState,
}

fn setup(cx: &mut TestAppContext) -> (Fixture, &mut VisualTestContext) {
    let renders: Vec<_> = (0..50).map(|_| Rc::new(Cell::new(0))).collect();
    let list = ListState::new(50, ListAlignment::Top, px(0.));
    let (rows, host) = cx.update(|cx| {
        let rows: Vec<_> = renders.iter().map(|r| cx.new(|_| Row { height: 20., renders: r.clone() })).collect();
        let host = cx.new(|_| Host { list: list.clone(), rows: rows.clone() });
        (rows, host)
    });
    let root_host = host.clone();
    let (_, cx) = cx.add_window_view(move |_, _| Root { host: root_host });
    let fixture = Fixture { host, rows, renders, list };
    frame(cx);
    (fixture, cx)
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
