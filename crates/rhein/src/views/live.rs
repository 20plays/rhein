//! Pieces of a call drawn as cached views of their own, so what changes many times a second
//! redraws only itself and not the stage, the chat or anything else that reads the client. Being
//! cached, each draws again only when notified: each watches what it shows.

use gpui::{
    Context, Entity, IntoElement, ObjectFit, ParentElement as _, Pixels, Render, Styled as _, StyledImage as _, Subscription, Window,
    div, img,
};
use rhein_store::{CallSpeaking, CallVideo, Slot};
use rhein_ui::{ActiveTheme as _, Avatar};

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
