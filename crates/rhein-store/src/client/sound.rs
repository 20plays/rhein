//! Sound files in messages: one plays at a time, fetched (and in DMs decrypted) when first
//! played, on the speakers the voice settings chose.

use std::sync::Arc;

use rhein_voice::playback::Playback;

use super::*;
use crate::conversations::Engine;

/// Where a sound file's bytes come from.
#[derive(Clone)]
pub enum SoundSource {
    /// A guild attachment.
    Url(String),
    /// An encrypted DM attachment: the conversation, the message, its index.
    Dm { conversation: String, message: String, index: usize },
}

/// What the row of a sound file shows.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundView {
    pub loading: bool,
    pub playing: bool,
    pub position: f64,
    pub duration: Option<f64>,
    pub error: Option<String>,
}

/// Ticks a few times a second while a sound file plays, for the views showing where it is: they
/// watch this instead of the whole client.
pub struct SoundTick;

pub struct Sound {
    key: String,
    loading: bool,
    error: Option<String>,
    playback: Option<Arc<Playback>>,
    _ticker: Option<Task<()>>,
}

impl Client {
    /// The state of the sound file `key` (message id and attachment index), if it is the one
    /// loaded.
    pub fn sound(&self, key: &str) -> Option<SoundView> {
        let sound = self.sound.as_ref().filter(|s| s.key == key)?;
        let playback = sound.playback.as_ref();
        Some(SoundView {
            loading: sound.loading,
            playing: playback.is_some_and(|p| !p.paused() && !p.ended()),
            position: playback.map_or(0., |p| p.position()),
            duration: playback.and_then(|p| p.duration()),
            error: sound.error.clone().or_else(|| playback.and_then(|p| p.error())),
        })
    }

    /// Plays or pauses the sound file `key`; another one playing stops.
    pub fn toggle_sound(&mut self, key: String, source: SoundSource, filename: String, cx: &mut Context<Self>) {
        if let Some(sound) = self.sound.as_ref().filter(|s| s.key == key) {
            if let Some(playback) = &sound.playback {
                if playback.ended() {
                    playback.seek(0.);
                    playback.set_paused(false);
                } else {
                    playback.set_paused(!playback.paused());
                }
                self.tick_sound(cx);
                cx.notify();
            }
            return;
        }
        let device = self.voice_prefs().null_devices.then(|| "alsa:null".to_owned()).or_else(|| self.prefs.voice.output.clone());
        let (io, http) = (self.io.clone(), self.http.clone());
        let (engine, api) = (self.dm_engine.clone(), self.home.as_ref().map(|h| h.api.clone()));
        let extension = filename.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        let wanted = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let started = join(io.spawn(async move {
                let bytes = match source {
                    SoundSource::Url(url) => http.get(&url).send().await?.error_for_status()?.bytes().await?.to_vec(),
                    SoundSource::Dm { conversation, message, index } => {
                        let (Some(engine), Some(api)) = (engine, api) else { anyhow::bail!("encrypted messages are not open on this device") };
                        let attachment = {
                            let engine = engine.lock().await;
                            engine
                                .conversation(&conversation)
                                .and_then(|c| c.messages.iter().find(|m| m.id == message))
                                .and_then(|m| m.body.attachments.get(index).cloned())
                                .ok_or_else(|| anyhow!("that sound file is gone"))?
                        };
                        Engine::fetch_attachment(&api, &attachment).await?
                    }
                };
                let playback = tokio::task::spawn_blocking(move || Playback::start(bytes, extension.as_deref(), device)).await??;
                Ok(Arc::new(playback))
            }))
            .await;
            this.update(cx, |this, cx| {
                let Some(sound) = this.sound.as_mut().filter(|s| s.key == wanted) else { return };
                sound.loading = false;
                match started {
                    Ok(playback) => sound.playback = Some(playback),
                    Err(err) => sound.error = Some(friendly(&err)),
                }
                this.tick_sound(cx);
                cx.notify();
            })
            .ok();
        });
        self.sound = Some(Sound { key, loading: true, error: None, playback: None, _ticker: Some(task) });
        cx.notify();
    }

    /// Jumps to `fraction` (0..1) of the sound file `key`.
    pub fn seek_sound(&mut self, key: &str, fraction: f64, cx: &mut Context<Self>) {
        let Some(playback) = self.sound.as_ref().filter(|s| s.key == key).and_then(|s| s.playback.clone()) else { return };
        if let Some(duration) = playback.duration() {
            playback.seek(duration * fraction.clamp(0., 1.));
            self.tick_sound(cx);
            cx.notify();
        }
    }

    /// Ticks `sound_tick` a few times a second while a sound plays.
    fn tick_sound(&mut self, cx: &mut Context<Self>) {
        let Some(sound) = self.sound.as_mut() else { return };
        let key = sound.key.clone();
        let tick = self.sound_tick.clone();
        sound._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(200)).await;
                let Ok(going) = this.update(cx, |this, cx| {
                    tick.update(cx, |_, cx| cx.notify());
                    this.sound(&key).is_some_and(|s| s.playing || s.loading)
                }) else {
                    return;
                };
                if !going {
                    return;
                }
            }
        }));
    }
}
