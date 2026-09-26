//! One rich-presence activity as a card: artwork, what and where, a clock or a progress bar,
//! the party and up to two link buttons. Mirrors the web client's `RichActivityCard`.

use gpui::{
    AnyElement, App, FontWeight, IntoElement, ObjectFit, ParentElement as _, SharedString, Styled as _,
    StyledImage as _, div, img, prelude::*, px,
};
use rhein_proto::Activity;
use rhein_ui::theme::{FONT_MONO, radius, text};
use rhein_ui::{Button, Icon, Theme, icon_sized};

const ART: f32 = 60.;
const SMALL_ART: f32 = 20.;

/// `large` and `small` are the artwork URLs already made safe to load (see
/// `Client::artwork_url`), `now_ms` the wall clock for the timer.
pub fn card(id: &str, activity: &Activity, large: Option<String>, small: Option<String>, now_ms: u64, t: &Theme) -> AnyElement {
    let glyph = match activity.kind {
        2 => Icon::Music,
        1 | 3 => Icon::Monitor,
        _ => Icon::Gamepad,
    };
    let art = div()
        .relative()
        .size(px(ART))
        .flex_none()
        .rounded(radius::MD)
        .bg(t.bg_selected)
        .flex()
        .items_center()
        .justify_center()
        .child(match large {
            Some(url) => img(url).size(px(ART)).rounded(radius::MD).object_fit(ObjectFit::Cover).into_any_element(),
            None => icon_sized(glyph, px(24.)).text_color(t.text_faint).into_any_element(),
        })
        .children(small.map(|url| {
            div()
                .absolute()
                .right(px(-4.))
                .bottom(px(-4.))
                .p(px(2.))
                .rounded_full()
                .bg(t.bg_panel)
                .child(img(url).size(px(SMALL_ART)).rounded_full().object_fit(ObjectFit::Cover))
        }));

    let line = |s: &Option<String>| s.clone().map(|s| div().text_size(text::SM).text_color(t.text_muted).truncate().child(s));
    let party = activity.party.as_ref().map(|p| format!("{} of {}", p.size[0], p.size[1]));
    let state = match (&activity.state, party) {
        (Some(s), Some(p)) => Some(format!("{s} ({p})")),
        (Some(s), None) => Some(s.clone()),
        (None, p) => p,
    };
    let clock = activity.timestamps.as_ref().and_then(|ts| match (ts.start, ts.end) {
        // A song: a progress bar with elapsed / total.
        (Some(start), Some(end)) if end > start => {
            let total = end - start;
            let elapsed = now_ms.saturating_sub(start).min(total);
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_family(FONT_MONO)
                    .text_size(text::XS)
                    .text_color(t.text_faint)
                    .child(clock_text(elapsed))
                    .child(
                        div()
                            .flex_1()
                            .h(px(3.))
                            .rounded_full()
                            .bg(t.bg_selected)
                            .child(div().h_full().rounded_full().bg(t.text).w(gpui::relative(elapsed as f32 / total as f32))),
                    )
                    .child(clock_text(total))
                    .into_any_element(),
            )
        }
        (Some(start), _) => Some(
            div()
                .font_family(FONT_MONO)
                .text_size(text::XS)
                .text_color(t.text_faint)
                .child(format!("{} elapsed", clock_text(now_ms.saturating_sub(start))))
                .into_any_element(),
        ),
        (None, Some(end)) => Some(
            div()
                .font_family(FONT_MONO)
                .text_size(text::XS)
                .text_color(t.text_faint)
                .child(format!("{} left", clock_text(end.saturating_sub(now_ms))))
                .into_any_element(),
        ),
        _ => None,
    });

    let buttons = activity.buttons.iter().enumerate().map(|(i, b)| {
        let url = b.url.clone();
        Button::new(SharedString::from(format!("{id}-button-{i}")), b.label.clone())
            .small()
            .on_click(move |_, _, cx: &mut App| cx.open_url(&url))
    });

    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_faint).child(activity.verb().to_lowercase()))
        .child(
            // Everything beside the cover: names, time and buttons.
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(art)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().text_size(text::SM).font_weight(FontWeight::SEMIBOLD).truncate().child(activity.name.clone()))
                        .children(line(&activity.details))
                        .children(line(&state))
                        .children(clock)
                        .when(!activity.buttons.is_empty(), |d| d.child(div().pt_1().flex().flex_wrap().gap_1().children(buttons))),
                ),
        )
        .into_any_element()
}

fn clock_text(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(clock_text(0), "0:00");
        assert_eq!(clock_text(65_000), "1:05");
        assert_eq!(clock_text(3_725_000), "1:02:05");
    }
}
