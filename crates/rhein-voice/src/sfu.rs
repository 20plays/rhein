//! SFU calls, as the web client's `SfuCall` makes them: the join token is spent at the Telium
//! SFU's `/session` for a LiveKit url and token, the room is joined with `autoSubscribe` off,
//! everything but screen shares is subscribed to (screens only once watched), and the
//! microphone goes out from the [`AudioEngine`](crate::audio::AudioEngine)'s source.

use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, Result, anyhow};
use livekit::e2ee::key_provider::KeyProvider;
use livekit::options::TrackPublishOptions;
use livekit::id::TrackSid;
use livekit::track::{LocalAudioTrack, LocalTrack, LocalVideoTrack, RemoteTrack, TrackSource};
use livekit::webrtc::video_source::RtcVideoSource;
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::media_stream_track::MediaStreamTrack;
use livekit::webrtc::prelude::RtcAudioSource;
use livekit::{Room, RoomEvent, RoomOptions};
use rhein_proto::SfuSession;
use tokio::sync::mpsc;

use crate::{CallEvent, Slot};

/// Spends a guild (or DM) join token at `{endpoint}/session`.
pub async fn session(http: &reqwest::Client, endpoint: &str, join_token: &str) -> Result<SfuSession> {
    let url = format!("{}/session", endpoint.trim_end_matches('/'));
    let response = http.post(&url).json(&serde_json::json!({ "token": join_token })).send().await.with_context(|| format!("POST {url}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow!("the SFU refused the session ({status}): {body}"));
    }
    let session: SfuSession = response.json().await.context("the SFU's /session answer")?;
    if session.url.is_empty() || session.token.is_empty() {
        return Err(anyhow!("the SFU answered without a url or token"));
    }
    Ok(session)
}

fn slot_of(source: TrackSource) -> Option<Slot> {
    match source {
        TrackSource::Microphone => Some(Slot::Mic),
        TrackSource::Camera => Some(Slot::Camera),
        TrackSource::Screenshare => Some(Slot::Screen),
        TrackSource::ScreenshareAudio => Some(Slot::ScreenAudio),
        _ => None,
    }
}

fn is_screen(source: TrackSource) -> bool {
    matches!(source, TrackSource::Screenshare | TrackSource::ScreenshareAudio)
}

/// One participant's side of an SFU call.
pub struct SfuCall {
    room: Room,
    session: SfuSession,
    watching: HashSet<String>,
    published: HashMap<Slot, TrackSid>,
    /// The key slot our media is sent under (`epoch % 16`); LiveKit itself leaves every sender
    /// at slot 0, where a receiver keyed by epoch has nothing.
    send_slot: Option<i32>,
}

impl SfuCall {
    /// Joins the room of `session`, publishing `microphone`. With `e2ee` (DM calls) every frame
    /// is encrypted from the first one on.
    pub async fn connect(session: SfuSession, microphone: NativeAudioSource, e2ee: Option<KeyProvider>) -> Result<(SfuCall, mpsc::UnboundedReceiver<CallEvent>)> {
        let slot = e2ee.as_ref().map(|p| p.get_latest_key_index());
        let mut options = RoomOptions::default();
        options.auto_subscribe = false;
        options.adaptive_stream = true;
        options.dynacast = true;
        if let Some(provider) = e2ee {
            crate::e2ee::encrypt(&mut options, provider);
        }
        let (room, room_events) = Room::connect(&session.url, &session.token, options).await.context("joining the LiveKit room")?;
        let (events, rx) = mpsc::unbounded_channel();
        let mut call = SfuCall { room, session, watching: HashSet::new(), published: HashMap::new(), send_slot: None };
        // What was already in the room before we came.
        for participant in call.room.remote_participants().values() {
            let _ = events.send(CallEvent::Link { user: participant.identity().to_string(), up: true });
            for publication in participant.track_publications().values() {
                publication.set_subscribed(!is_screen(publication.source()));
            }
        }
        tokio::spawn(pump(room_events, events));

        let track = LocalAudioTrack::create_audio_track("microphone", RtcAudioSource::Native(microphone));
        call.room
            .local_participant()
            .publish_track(LocalTrack::Audio(track), TrackPublishOptions { source: TrackSource::Microphone, ..Default::default() })
            .await
            .context("publishing the microphone")?;
        call.send_slot = slot;
        call.aim_senders();
        Ok((call, rx))
    }

    pub fn session(&self) -> &SfuSession {
        &self.session
    }

    /// Subscribes to (or drops) one participant's screen share and its sound.
    pub fn watch(&mut self, user: &str, on: bool) {
        if on {
            self.watching.insert(user.to_owned());
        } else {
            self.watching.remove(user);
        }
        for participant in self.room.remote_participants().values() {
            if participant.identity().as_str() != user {
                continue;
            }
            for publication in participant.track_publications().values() {
                if is_screen(publication.source()) {
                    publication.set_subscribed(on);
                }
            }
        }
    }

    /// Publishes a camera or a screen (simulcast, so each viewer gets the layer it can take).
    pub async fn publish_video(&mut self, slot: Slot, source: NativeVideoSource) -> Result<()> {
        self.unpublish(slot).await;
        let source_kind = match slot {
            Slot::Camera => TrackSource::Camera,
            Slot::Screen => TrackSource::Screenshare,
            _ => return Err(anyhow!("{slot:?} is not a video slot")),
        };
        let track = LocalVideoTrack::create_video_track(if slot == Slot::Screen { "screen" } else { "camera" }, RtcVideoSource::Native(source));
        let publication = self
            .room
            .local_participant()
            .publish_track(LocalTrack::Video(track), TrackPublishOptions { source: source_kind, simulcast: true, ..Default::default() })
            .await
            .context("publishing video")?;
        self.published.insert(slot, publication.sid());
        self.aim_senders();
        Ok(())
    }

    /// Installs a new epoch's key and sends under it from now on. The earlier keys stay in the
    /// ring, so a peer a commit behind is still heard.
    pub fn rekey(&mut self, provider: &KeyProvider, key: &[u8], epoch: u64) {
        crate::e2ee::set_epoch_key(provider, key, epoch);
        self.send_slot = Some(crate::e2ee::key_slot(epoch));
        self.aim_senders();
    }

    /// Points our own tracks' frame cryptors at the current key slot.
    fn aim_senders(&self) {
        let Some(slot) = self.send_slot else { return };
        let me = self.room.local_participant().identity();
        for ((who, _), cryptor) in self.room.e2ee_manager().frame_cryptors() {
            if who == me {
                cryptor.set_key_index(slot);
            }
        }
    }

    /// Takes a published camera or screen down again.
    pub async fn unpublish(&mut self, slot: Slot) {
        if let Some(sid) = self.published.remove(&slot) {
            let _ = self.room.local_participant().unpublish_track(&sid).await;
        }
    }

    pub async fn close(self) {
        let _ = self.room.close().await;
    }
}

/// Turns the room's events into call events, and subscribes to what arrives later.
async fn pump(mut room_events: mpsc::UnboundedReceiver<RoomEvent>, events: mpsc::UnboundedSender<CallEvent>) {
    while let Some(event) = room_events.recv().await {
        let out = match event {
            RoomEvent::ParticipantConnected(p) => Some(CallEvent::Link { user: p.identity().to_string(), up: true }),
            RoomEvent::ParticipantDisconnected(p) => Some(CallEvent::Link { user: p.identity().to_string(), up: false }),
            RoomEvent::TrackPublished { publication, .. } => {
                // Screens wait for Watch; everything else is heard and seen.
                publication.set_subscribed(!is_screen(publication.source()));
                None
            }
            RoomEvent::TrackSubscribed { track, publication, participant } => slot_of(publication.source()).map(|slot| {
                let track: MediaStreamTrack = match track {
                    RemoteTrack::Audio(t) => t.rtc_track().into(),
                    RemoteTrack::Video(t) => t.rtc_track().into(),
                };
                CallEvent::Track { from: participant.identity().to_string(), slot, track }
            }),
            RoomEvent::TrackUnsubscribed { publication, participant, .. } => {
                slot_of(publication.source()).map(|slot| CallEvent::TrackEnded { from: participant.identity().to_string(), slot })
            }
            RoomEvent::TrackSubscriptionFailed { participant, error, .. } => Some(CallEvent::Error(format!("{}: {error}", participant.identity()))),
            RoomEvent::Disconnected { reason } => Some(CallEvent::Error(format!("the SFU disconnected: {reason:?}"))),
            _ => None,
        };
        if let Some(out) = out
            && events.send(out).is_err()
        {
            return;
        }
    }
}
