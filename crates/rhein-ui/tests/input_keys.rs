//! Keyboard behaviour of `TextInput`, driven through GPUI's key dispatch like a real window.

use gpui::{Focusable as _, TestAppContext};
use rhein_ui::{TextInput, Theme};

fn setup(cx: &mut TestAppContext, multiline: bool) -> (gpui::Entity<TextInput>, &mut gpui::VisualTestContext) {
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.bind_keys(rhein_ui::input::bindings());
    });
    let (input, cx) = cx.add_window_view(|_, cx| {
        let i = TextInput::new("type here", cx);
        if multiline { i.multiline(5) } else { i }
    });
    cx.update(|window, cx| window.focus(&input.focus_handle(cx), cx));
    (input, cx)
}

#[gpui::test]
fn home_and_end_single_line(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx, false);
    cx.simulate_input("hello");
    cx.simulate_keystrokes("home");
    assert_eq!(input.read_with(cx, |i, _| i.cursor()), 0, "home");
    cx.simulate_keystrokes("end");
    assert_eq!(input.read_with(cx, |i, _| i.cursor()), 5, "end");
}

#[gpui::test]
fn shift_selection_single_line(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx, false);
    cx.simulate_input("hello");
    cx.simulate_keystrokes("shift-left shift-left");
    cx.simulate_input("X");
    assert_eq!(input.read_with(cx, |i, _| i.text().to_owned()), "helX");
}

#[gpui::test]
fn home_end_and_selection_multiline(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx, true);
    cx.simulate_input("hello");
    cx.simulate_keystrokes("home");
    assert_eq!(input.read_with(cx, |i, _| i.cursor()), 0, "home");
    cx.simulate_keystrokes("end");
    assert_eq!(input.read_with(cx, |i, _| i.cursor()), 5, "end");
    cx.simulate_keystrokes("shift-left shift-left");
    cx.simulate_input("X");
    assert_eq!(input.read_with(cx, |i, _| i.text().to_owned()), "helX");
}

fn text(input: &gpui::Entity<TextInput>, cx: &mut gpui::VisualTestContext) -> String {
    input.read_with(cx, |i, _| i.text().to_owned())
}

#[gpui::test]
fn shift_home_and_end_select(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx, true);
    cx.simulate_input("hello world");
    cx.simulate_keystrokes("shift-home");
    cx.simulate_input("X");
    assert_eq!(text(&input, cx), "X");
    cx.simulate_input(" more");
    cx.simulate_keystrokes("home shift-end");
    cx.simulate_input("Y");
    assert_eq!(text(&input, cx), "Y");
}

#[gpui::test]
fn word_motion_and_deletion(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx, false);
    cx.simulate_input("one two three");
    cx.simulate_keystrokes("ctrl-left");
    assert_eq!(input.read_with(cx, |i, _| i.cursor()), 8, "start of three");
    cx.simulate_keystrokes("ctrl-shift-left");
    cx.simulate_input("2 ");
    assert_eq!(text(&input, cx), "one 2 three");
    cx.simulate_keystrokes("end ctrl-backspace");
    assert_eq!(text(&input, cx), "one 2 ");
}

#[gpui::test]
fn home_end_stay_within_the_paragraph(cx: &mut TestAppContext) {
    let (input, cx) = setup(cx, true);
    cx.simulate_input("first");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("second");
    cx.simulate_keystrokes("home");
    assert_eq!(input.read_with(cx, |i, _| i.cursor()), 6, "start of second line");
    cx.simulate_keystrokes("ctrl-home");
    assert_eq!(input.read_with(cx, |i, _| i.cursor()), 0);
    cx.simulate_keystrokes("end");
    assert_eq!(input.read_with(cx, |i, _| i.cursor()), 5, "end of first line");
    cx.simulate_keystrokes("shift-down");
    cx.simulate_input("!");
    assert!(text(&input, cx).starts_with("first!"), "{}", text(&input, cx));
}
