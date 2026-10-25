//! Pieces of a call drawn as cached views of their own, so what changes many times a second
//! redraws only itself and not the stage, the chat or anything else that reads the client. Being
//! cached, each draws again only when notified: each watches what it shows.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, Context, Entity, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, Window, canvas, div, img, prelude::FluentBuilder as _,
    px, relative,
};
use rhein_store::{CallSpeaking, CallVideo, Client, MoviePlayer, Slot};
use rhein_ui::theme::{FONT_MONO, text};
use rhein_ui::{ActiveTheme as _, Avatar, Icon, IconButton};

/// One video of a call (a camera or a screen), whose pictures arrive up to thirty times a
/// second.
pub struct LiveVideo {
    video: Entity<CallVideo>,
    user_id: String,
    slot: Slot,
    /// Filling its box (a camera tile), else whole inside it (a screen).
    cover: bool,
    _watch: Subscription,
}

impl LiveVideo {
    pub fn new(video: Entity<CallVideo>, user_id: String, slot: Slot, cover: bool, cx: &mut Context<Self>) -> Self {
        let watch = cx.observe(&video, |_, _, cx| cx.notify());
        Self { video, user_id, slot, cover, _watch: watch }
    }
}

impl Render for LiveVideo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let picture = self.video.read(cx).picture(&self.user_id, self.slot);
        div().size_full().children(picture.map(|picture| img(picture).size_full().object_fit(if self.cover { ObjectFit::Cover } else { ObjectFit::Contain })))
    }
}

/// Someone's face in a call, ringed while they speak. Its own cached view, watching who speaks:
/// a ring coming and going redraws the face, not the chat around it.
pub struct SpeakingFace {
    speaking: Entity<CallSpeaking>,
    pub user_id: String,
    pub name: String,
    pub avatar: Option<String>,
    size: Pixels,
    _watch: Subscription,
}

impl SpeakingFace {
    pub fn new(speaking: Entity<CallSpeaking>, user_id: String, size: Pixels, cx: &mut Context<Self>) -> Self {
        let watch = cx.observe(&speaking, |_, _, cx| cx.notify());
        Self { speaking, user_id, name: String::new(), avatar: None, size, _watch: watch }
    }

    /// The name and picture to show, redrawn only when they change.
    pub fn set_look(&mut self, name: String, avatar: Option<String>, cx: &mut Context<Self>) {
        if self.name != name || self.avatar != avatar {
            (self.name, self.avatar) = (name, avatar);
            cx.notify();
        }
    }
}

impl Render for SpeakingFace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let speaking = self.speaking.read(cx).is_speaking(&self.user_id);
        div()
            .rounded_full()
            .border_2()
            .border_color(if speaking { cx.theme().success } else { gpui::transparent_black().into() })
            .child(Avatar::new(self.user_id.clone(), self.name.clone()).size(self.size).image(self.avatar.clone().map(Into::into)))
    }
}

/// The video playing from a message: its picture and its controls. The chat has one, shown in
/// the row of whichever video is loaded; it watches the player, so a new picture redraws it
/// alone.
pub struct LiveMovie {
    client: Entity<Client>,
    player: Entity<MoviePlayer>,
    _watch: Subscription,
}

impl LiveMovie {
    pub fn new(client: Entity<Client>, cx: &mut Context<Self>) -> Self {
        let player = client.read(cx).movie_player.clone();
        let watch = cx.observe(&player, |_, _, cx| cx.notify());
        Self { client, player, _watch: watch }
    }
}

impl Render for LiveMovie {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let player = self.player.read(cx);
        let (Some(key), Some(state)) = (player.key().map(str::to_owned), player.view()) else { return div().size_full() };
        let picture = player.picture();
        let fraction = state.duration.filter(|d| *d > 0.).map_or(0., |d| (state.position / d).clamp(0., 1.)) as f32;
        let clock = |secs: f64| format!("{}:{:02}", (secs / 60.) as u64, secs as u64 % 60);
        let times = match state.duration {
            Some(d) => format!("{} / {}", clock(state.position), clock(d)),
            None => String::new(),
        };
        let (c1, c2, c3) = (self.client.clone(), self.client.clone(), self.client.clone());
        // Where the bar is on screen, for turning a click into a position.
        let bar = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let measured = bar.clone();
        let message = if state.loading {
            Some("Loading…".to_owned())
        } else {
            state.error.clone().map(|e| format!("This video can't be played here: {e}"))
        };
        div()
            .size_full()
            .relative()
            .bg(gpui::black())
            .child(
                div()
                    .id("movie-picture")
                    .size_full()
                    .cursor_pointer()
                    .on_click(move |_, _, cx| c1.update(cx, |c, cx| c.play_pause_movie(cx)))
                    .children(picture.map(|p| img(p).size_full().object_fit(ObjectFit::Contain))),
            )
            .children(message.map(|m| {
                div()
                    .absolute()
                    .inset_0()
                    .p_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_center()
                    .text_size(text::SM)
                    .text_color(gpui::white())
                    .child(m)
            }))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .px_2()
                    .py_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .bg(gpui::Hsla::from(t.bg_base).opacity(0.85))
                    .child(
                        IconButton::new("movie-play", if state.playing { Icon::Pause } else { Icon::Play })
                            .tooltip(if state.playing { "Pause" } else { "Play" })
                            .on_click(move |_, _, cx| c2.update(cx, |c, cx| c.play_pause_movie(cx))),
                    )
                    .child(
                        div()
                            .id("movie-bar")
                            .relative()
                            .flex_1()
                            .h(px(6.))
                            .rounded_full()
                            .bg(t.bg_selected)
                            .when(state.duration.is_some(), |d| d.cursor_pointer())
                            .child(div().absolute().left_0().top_0().h_full().rounded_full().bg(t.accent).w(relative(fraction)))
                            .child(canvas(move |bounds, _, _| measured.set(Some(bounds)), |_, _, _, _| {}).absolute().inset_0())
                            .on_mouse_down(gpui::MouseButton::Left, move |e, _, cx| {
                                cx.stop_propagation();
                                let Some(bounds) = bar.get() else { return };
                                let fraction = ((e.position.x - bounds.left()) / bounds.size.width) as f64;
                                c3.update(cx, |c, cx| c.seek_movie(&key, fraction, cx));
                            }),
                    )
                    .child(div().flex_none().font_family(FONT_MONO).text_size(text::XS).text_color(t.text_muted).child(SharedString::from(times))),
            )
    }
}
