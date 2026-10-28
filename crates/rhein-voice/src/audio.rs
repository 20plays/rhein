//! Microphone and speakers for calls.
//!
//! cpal moves raw samples between the devices and lock-free rings; everything else happens on
//! one DSP thread, because libwebrtc's audio processing module and resampler must stay on the
//! thread that made them:
//!
//! - capture: device rate → 48 kHz mono, echo cancellation / noise suppression / gain control,
//!   input gain, level, noise gate, mute and push-to-talk, then into the [`NativeAudioSource`]
//!   that calls publish;
//! - playback: every remote voice (48 kHz mono, read from its track by a small task) mixed with
//!   its own volume, handed to the echo canceller as what the speakers play, then resampled to
//!   the device.
//!
//! Speaking is judged like the web client does it: a peak above 1/16 of full scale, held for
//! 250 ms across the gaps in speech.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample as _, SampleFormat, SizedSample};
use futures::StreamExt as _;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::native::apm::AudioProcessingModule;
use livekit::webrtc::native::audio_resampler::AudioResampler;
use livekit::webrtc::prelude::{AudioFrame, AudioSourceOptions, RtcAudioTrack};

/// Calls run at 48 kHz mono in 10 ms frames.
pub const RATE: u32 = 48_000;
const FRAME: usize = (RATE / 100) as usize;
/// A peak over this (1/16 of full scale) counts as speaking.
const SPEAKING_PEAK: i16 = 2048;
const SPEAKING_HANG: Duration = Duration::from_millis(250);
/// How long the gate stays open after the level falls below it.
const GATE_HANG: Duration = Duration::from_millis(200);
/// How much mixed audio waits for the speakers.
const OUTPUT_AHEAD: Duration = Duration::from_millis(40);
/// The most 10 ms frames one turn of the DSP loop handles each way, so that a device that is
/// not paced in real time cannot hold the thread.
const MAX_FRAMES_PER_TURN: usize = 10;
/// A remote voice further behind than this skips ahead.
const VOICE_BACKLOG: usize = FRAME * 10;

/// An input or output device, as cpal names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// Stable across runs; what settings keep.
    pub id: String,
    pub name: String,
}

pub fn input_devices() -> Vec<AudioDevice> {
    devices(true)
}

pub fn output_devices() -> Vec<AudioDevice> {
    devices(false)
}

fn devices(input: bool) -> Vec<AudioDevice> {
    let host = cpal::default_host();
    let list = if input { host.input_devices().map(|d| d.collect::<Vec<_>>()) } else { host.output_devices().map(|d| d.collect()) };
    let all: Vec<AudioDevice> = list
        .unwrap_or_default()
        .into_iter()
        .filter_map(|device| {
            let id = device.id().ok()?.to_string();
            let name = device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| id.clone());
            Some(AudioDevice { id, name })
        })
        .collect();
    let mut usable: Vec<AudioDevice> = all.iter().filter(|d| worth_offering(&d.id)).cloned().collect();
    if usable.is_empty() {
        return all;
    }
    usable.sort_by_key(|d| !d.id.ends_with(":default"));
    // A card appears as `sysdefault:` and `front:` under one name; one entry is enough.
    let mut seen = std::collections::HashSet::new();
    usable.retain(|d| seen.insert(d.name.clone()));
    usable
}

/// ALSA lists its plugins (rate converters, up- and downmixers, the null sink, raw `hw:`
/// handles) as devices; a person picks between the system default, the sound server and the
/// cards.
fn worth_offering(id: &str) -> bool {
    let Some(name) = id.strip_prefix("alsa:") else { return true };
    matches!(name, "default" | "pipewire") || name.starts_with("sysdefault:") || name.starts_with("front:")
}

pub(crate) fn find_device(input: bool, id: Option<&str>) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if let Some(id) = id
        && let Ok(id) = id.parse()
        && let Some(device) = host.device_by_id(&id)
    {
        return Some(device);
    }
    if input { host.default_input_device() } else { host.default_output_device() }
}

/// Which devices to open and what to run the microphone through.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    /// `None` for the system default.
    pub input: Option<String>,
    pub output: Option<String>,
    pub echo_cancellation: bool,
    pub noise_suppression: bool,
    pub auto_gain: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig { input: None, output: None, echo_cancellation: true, noise_suppression: true, auto_gain: true }
    }
}

/// What the UI reads and flips without going through the DSP thread.
#[derive(Default)]
struct Shared {
    /// The microphone to your own speakers, for hearing how you sound (the settings' mic test).
    loopback: AtomicBool,
    muted: AtomicBool,
    deafened: AtomicBool,
    push_to_talk: AtomicBool,
    talking: AtomicBool,
    /// Input gain as f32 bits (1.0 = unchanged).
    gain: AtomicU32,
    /// The gate's threshold in dBFS as f32 bits; NaN when the gate is off.
    gate: AtomicU32,
    /// The microphone's last peak, 0..1, as f32 bits.
    level: AtomicU32,
    /// When each source (`""` for this microphone) was last loud.
    loud: Mutex<HashMap<String, Instant>>,
    errors: Mutex<Vec<String>>,
}

impl Shared {
    fn new() -> Shared {
        let shared = Shared::default();
        shared.gain.store(1f32.to_bits(), Ordering::Relaxed);
        shared.gate.store(f32::NAN.to_bits(), Ordering::Relaxed);
        shared
    }

    fn f32(cell: &AtomicU32) -> f32 {
        f32::from_bits(cell.load(Ordering::Relaxed))
    }

    fn transmitting(&self) -> bool {
        !self.muted.load(Ordering::Relaxed)
            && !self.deafened.load(Ordering::Relaxed)
            && (!self.push_to_talk.load(Ordering::Relaxed) || self.talking.load(Ordering::Relaxed))
    }

    fn mark_loud(&self, key: &str) {
        self.loud.lock().unwrap().insert(key.to_owned(), Instant::now());
    }

    fn report(&self, error: String) {
        log::warn!("audio: {error}");
        self.errors.lock().unwrap().push(error);
    }
}

enum Command {
    AddVoice { key: String, samples: rtrb::Consumer<i16> },
    RemoveVoice(String),
    Volume { key: String, gain: f32 },
    Devices { input: Option<String>, output: Option<String> },
    Capture(Vec<i16>),
    Tap(mpsc::Sender<Vec<i16>>),
    Stop,
}

/// The call's audio: one microphone going out, any number of voices coming in.
pub struct AudioEngine {
    shared: Arc<Shared>,
    commands: mpsc::Sender<Command>,
    microphone: NativeAudioSource,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioEngine {
    /// Opens the devices and starts the DSP thread. A missing microphone or speaker is not an
    /// error: a call without one is still worth being in, and [`AudioEngine::errors`] says why.
    pub fn start(config: AudioConfig) -> Result<AudioEngine> {
        Self::start_with(config, true)
    }

    /// The same without touching any device: capture comes from [`AudioEngine::feed`] and
    /// playback goes nowhere (still paced, still mixed and metered). For tests.
    pub fn headless() -> Result<AudioEngine> {
        Self::start_with(AudioConfig { echo_cancellation: false, noise_suppression: false, auto_gain: false, ..Default::default() }, false)
    }

    fn start_with(config: AudioConfig, devices: bool) -> Result<AudioEngine> {
        let shared = Arc::new(Shared::new());
        // No queue: the DSP thread hands over exact 10 ms frames.
        let microphone = NativeAudioSource::new(AudioSourceOptions::default(), RATE, 1, 0);
        let (commands, rx) = mpsc::channel();
        let (started, ready) = mpsc::channel();
        let thread = {
            let (shared, microphone) = (shared.clone(), microphone.clone());
            std::thread::Builder::new()
                .name("rhein-audio".into())
                .spawn(move || {
                    let mut dsp = Dsp::new(&config, shared, microphone);
                    if devices {
                        dsp.open_input(config.input.as_deref());
                        dsp.open_output(config.output.as_deref());
                    }
                    let _ = started.send(());
                    dsp.run(rx);
                })
                .context("cannot start the audio thread")?
        };
        ready.recv().map_err(|_| anyhow!("the audio thread did not start"))?;
        Ok(AudioEngine { shared, commands, microphone, thread: Some(thread) })
    }

    /// The source calls publish as their microphone track.
    pub fn microphone(&self) -> NativeAudioSource {
        self.microphone.clone()
    }

    /// Plays a remote track under `key` (for example the user id, or `user#screen` for the sound
    /// of a screen share). Must be called inside a tokio runtime.
    pub fn add_voice(&self, key: &str, track: RtcAudioTrack) {
        let (mut producer, consumer) = rtrb::RingBuffer::new(RATE as usize / 5);
        let _ = self.commands.send(Command::AddVoice { key: key.to_owned(), samples: consumer });
        tokio::spawn(async move {
            let mut stream = NativeAudioStream::new(track, RATE as i32, 1);
            while let Some(frame) = stream.next().await {
                if producer.is_abandoned() {
                    break;
                }
                for &sample in frame.data.iter() {
                    // Full means the DSP thread is behind; it skips ahead on its own.
                    let _ = producer.push(sample);
                }
            }
        });
    }

    pub fn remove_voice(&self, key: &str) {
        let _ = self.commands.send(Command::RemoveVoice(key.to_owned()));
        self.shared.loud.lock().unwrap().remove(key);
    }

    /// A local volume for one voice, 0..200 percent.
    pub fn set_volume(&self, key: &str, percent: u32) {
        let _ = self.commands.send(Command::Volume { key: key.to_owned(), gain: percent.min(200) as f32 / 100. });
    }

    pub fn set_devices(&self, input: Option<String>, output: Option<String>) {
        let _ = self.commands.send(Command::Devices { input, output });
    }

    /// Plays the microphone, as processed for sending (suppression, gain, gate), back through
    /// your own speakers, muted or deafened or not: the mic test.
    pub fn set_loopback(&self, on: bool) {
        self.shared.loopback.store(on, Ordering::Relaxed);
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }

    pub fn muted(&self) -> bool {
        self.shared.muted.load(Ordering::Relaxed)
    }

    /// Deafened: nothing is played, and nothing is sent either.
    pub fn set_deafened(&self, deafened: bool) {
        self.shared.deafened.store(deafened, Ordering::Relaxed);
    }

    pub fn deafened(&self) -> bool {
        self.shared.deafened.load(Ordering::Relaxed)
    }

    /// With push-to-talk on, the microphone only goes out while [`AudioEngine::set_talking`].
    pub fn set_push_to_talk(&self, on: bool) {
        self.shared.push_to_talk.store(on, Ordering::Relaxed);
    }

    pub fn set_talking(&self, held: bool) {
        self.shared.talking.store(held, Ordering::Relaxed);
    }

    /// Input gain, 0..200 percent.
    pub fn set_input_gain(&self, percent: u32) {
        self.shared.gain.store((percent.min(200) as f32 / 100.).to_bits(), Ordering::Relaxed);
    }

    /// The noise gate's threshold in dBFS (for example -50), or `None` for no gate.
    pub fn set_gate(&self, threshold_db: Option<f32>) {
        self.shared.gate.store(threshold_db.unwrap_or(f32::NAN).to_bits(), Ordering::Relaxed);
    }

    /// The microphone's level right now, 0..1 (peak, after gain, before the gate).
    pub fn input_level(&self) -> f32 {
        Shared::f32(&self.shared.level)
    }

    /// Whether this microphone is audible to the call right now.
    pub fn self_speaking(&self) -> bool {
        self.speaking().iter().any(|k| k.is_empty())
    }

    /// The keys that are audible right now; `""` is this microphone.
    pub fn speaking(&self) -> Vec<String> {
        let now = Instant::now();
        self.shared.loud.lock().unwrap().iter().filter(|(_, at)| now.duration_since(**at) < SPEAKING_HANG).map(|(k, _)| k.clone()).collect()
    }

    /// Device problems since the last call, oldest first.
    pub fn errors(&self) -> Vec<String> {
        std::mem::take(&mut *self.shared.errors.lock().unwrap())
    }

    /// 48 kHz mono as if the microphone had captured it (whole 10 ms frames). For headless
    /// engines and tests.
    pub fn feed(&self, samples: &[i16]) {
        let _ = self.commands.send(Command::Capture(samples.to_vec()));
    }

    /// Every mixed 10 ms frame from now on, as the speakers get it. For tests.
    pub fn tap(&self) -> mpsc::Receiver<Vec<i16>> {
        let (tx, rx) = mpsc::channel();
        let _ = self.commands.send(Command::Tap(tx));
        rx
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Voice {
    samples: rtrb::Consumer<i16>,
    gain: f32,
}

struct Input {
    _stream: cpal::Stream,
    samples: rtrb::Consumer<f32>,
    rate: u32,
    channels: u16,
}

struct Output {
    _stream: cpal::Stream,
    samples: rtrb::Producer<f32>,
    capacity: usize,
    rate: u32,
    channels: u16,
}

/// The DSP thread's state.
struct Dsp {
    shared: Arc<Shared>,
    microphone: NativeAudioSource,
    apm: Option<AudioProcessingModule>,
    capture_resampler: AudioResampler,
    output_resampler: AudioResampler,
    input: Option<Input>,
    output: Option<Output>,
    voices: HashMap<String, Voice>,
    gate_open_until: Option<Instant>,
    tap: Option<mpsc::Sender<Vec<i16>>>,
    /// Processed microphone frames waiting to be heard in the mic test.
    loopback: std::collections::VecDeque<i16>,
    /// Where playback is paced by the clock (no output device).
    next_render: Instant,
}

impl Dsp {
    fn new(config: &AudioConfig, shared: Arc<Shared>, microphone: NativeAudioSource) -> Dsp {
        let processing = config.echo_cancellation || config.noise_suppression || config.auto_gain;
        let mut apm = processing.then(|| AudioProcessingModule::new(config.echo_cancellation, config.auto_gain, true, config.noise_suppression));
        if let Some(apm) = apm.as_mut() {
            // What is buffered on the way to the speakers, plus a typical device.
            let _ = apm.set_stream_delay_ms(OUTPUT_AHEAD.as_millis() as i32 + 20);
        }
        Dsp {
            shared,
            microphone,
            apm,
            capture_resampler: AudioResampler::default(),
            output_resampler: AudioResampler::default(),
            input: None,
            output: None,
            voices: HashMap::new(),
            gate_open_until: None,
            tap: None,
            loopback: std::collections::VecDeque::new(),
            next_render: Instant::now(),
        }
    }

    fn open_input(&mut self, id: Option<&str>) {
        self.input = None;
        match open_input(id) {
            Ok(input) => self.input = Some(input),
            Err(err) => self.shared.report(format!("no microphone: {err:#}")),
        }
    }

    fn open_output(&mut self, id: Option<&str>) {
        self.output = None;
        match open_output(id) {
            Ok(output) => self.output = Some(output),
            Err(err) => self.shared.report(format!("no speakers: {err:#}")),
        }
        self.next_render = Instant::now();
    }

    fn run(&mut self, commands: mpsc::Receiver<Command>) {
        loop {
            loop {
                match commands.try_recv() {
                    Ok(Command::AddVoice { key, samples }) => {
                        self.voices.insert(key, Voice { samples, gain: 1. });
                    }
                    Ok(Command::RemoveVoice(key)) => {
                        self.voices.remove(&key);
                    }
                    Ok(Command::Volume { key, gain }) => {
                        if let Some(voice) = self.voices.get_mut(&key) {
                            voice.gain = gain;
                        }
                    }
                    Ok(Command::Devices { input, output }) => {
                        self.open_input(input.as_deref());
                        self.open_output(output.as_deref());
                    }
                    Ok(Command::Capture(samples)) => {
                        for chunk in samples.chunks(FRAME) {
                            let mut frame = chunk.to_vec();
                            frame.resize(FRAME, 0);
                            self.process_capture(&mut frame);
                        }
                    }
                    Ok(Command::Tap(tx)) => self.tap = Some(tx),
                    Ok(Command::Stop) | Err(mpsc::TryRecvError::Disconnected) => return,
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            self.capture();
            self.render();
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    /// Everything the microphone captured, in 10 ms frames.
    fn capture(&mut self) {
        let Some(mut input) = self.input.take() else { return };
        let chunk = (input.rate / 100) as usize * input.channels as usize;
        let mut raw = vec![0i16; chunk];
        // A capture that has run far ahead (a device that is not paced in real time, a stall
        // here) loses its oldest audio rather than making the call lag behind it.
        let backlog = chunk * 20;
        while input.samples.slots() > backlog {
            let _ = input.samples.pop();
        }
        for _ in 0..MAX_FRAMES_PER_TURN {
            if input.samples.slots() < chunk {
                break;
            }
            for sample in raw.iter_mut() {
                *sample = i16::from_sample(input.samples.pop().unwrap_or(0.));
            }
            let (rate, channels) = (input.rate, input.channels as u32);
            let mut frame = if rate == RATE && channels == 1 {
                raw.clone()
            } else {
                self.capture_resampler.remix_and_resample(&raw, rate / 100, channels, rate, 1, RATE).to_vec()
            };
            frame.resize(FRAME, 0);
            self.process_capture(&mut frame);
        }
        self.input = Some(input);
    }

    fn process_capture(&mut self, frame: &mut [i16]) {
        if let Some(apm) = self.apm.as_mut() {
            let _ = apm.process_stream(frame, RATE as i32, 1);
        }
        let gain = Shared::f32(&self.shared.gain);
        if (gain - 1.).abs() > f32::EPSILON {
            for sample in frame.iter_mut() {
                *sample = (*sample as f32 * gain).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            }
        }
        let peak = peak(frame);
        self.shared.level.store((peak as f32 / i16::MAX as f32).to_bits(), Ordering::Relaxed);

        let now = Instant::now();
        let threshold = Shared::f32(&self.shared.gate);
        let gated = if threshold.is_nan() {
            false
        } else {
            if db(peak) >= threshold {
                self.gate_open_until = Some(now + GATE_HANG);
            }
            self.gate_open_until.is_none_or(|until| now >= until)
        };
        // The mic test hears what the others would, before mute and deafen silence it.
        if self.shared.loopback.load(Ordering::Relaxed) {
            if gated {
                self.loopback.extend(std::iter::repeat_n(0, frame.len()));
            } else {
                self.loopback.extend(frame.iter().copied());
            }
            // Never more than 100 ms behind.
            while self.loopback.len() > FRAME * 10 {
                self.loopback.pop_front();
            }
        } else {
            self.loopback.clear();
        }
        let live = self.shared.transmitting() && !gated;
        if !live {
            frame.fill(0);
        } else if peak > SPEAKING_PEAK {
            self.shared.mark_loud("");
        }
        let frame = AudioFrame { data: (&*frame).into(), sample_rate: RATE, num_channels: 1, samples_per_channel: FRAME as u32 };
        let _ = futures::executor::block_on(self.microphone.capture_frame(&frame));
    }

    /// Mixes as much as the speakers need (or, without speakers, as much as time has passed).
    fn render(&mut self) {
        for _ in 0..MAX_FRAMES_PER_TURN {
            match self.output.as_ref() {
                Some(output) => {
                    let need = (output.rate / 100) as usize * output.channels as usize;
                    let queued = output.capacity - output.samples.slots();
                    let ahead = (OUTPUT_AHEAD.as_millis() as usize * output.rate as usize / 1000) * output.channels as usize;
                    if queued >= ahead || output.samples.slots() < need * 2 {
                        return;
                    }
                }
                None => {
                    if Instant::now() < self.next_render {
                        return;
                    }
                    self.next_render += Duration::from_millis(10);
                }
            }
            let mut mix = self.mix();
            if let Some(tap) = &self.tap
                && tap.send(mix.clone()).is_err()
            {
                self.tap = None;
            }
            if let Some(apm) = self.apm.as_mut() {
                let mut reference = mix.clone();
                let _ = apm.process_reverse_stream(&mut reference, RATE as i32, 1);
            }
            let Some(output) = self.output.as_mut() else { continue };
            let played = if output.rate == RATE && output.channels == 1 {
                std::mem::take(&mut mix)
            } else {
                self.output_resampler.remix_and_resample(&mix, FRAME as u32, 1, RATE, output.channels as u32, output.rate).to_vec()
            };
            for sample in played {
                let _ = output.samples.push(f32::from_sample(sample));
            }
        }
    }

    /// 10 ms of every voice together.
    fn mix(&mut self) -> Vec<i16> {
        let mut sum = [0i32; FRAME];
        let deafened = self.shared.deafened.load(Ordering::Relaxed);
        let mut loud = Vec::new();
        for (key, voice) in self.voices.iter_mut() {
            // Fallen behind (a stall, a burst): drop to the last 100 ms.
            while voice.samples.slots() > VOICE_BACKLOG {
                let _ = voice.samples.pop();
            }
            if voice.samples.slots() < FRAME {
                continue;
            }
            let mut frame_peak = 0i16;
            for slot in sum.iter_mut() {
                let sample = voice.samples.pop().unwrap_or(0);
                frame_peak = frame_peak.max(sample.saturating_abs());
                *slot += (sample as f32 * voice.gain) as i32;
            }
            if frame_peak > SPEAKING_PEAK && voice.gain > 0. {
                loud.push(key.clone());
            }
        }
        for key in loud {
            self.shared.mark_loud(&key);
        }
        if deafened {
            sum = [0i32; FRAME];
        }
        // The mic test, heard even while deafened (the call's voices are not).
        if self.loopback.len() >= FRAME {
            for slot in sum.iter_mut() {
                *slot += self.loopback.pop_front().unwrap_or(0) as i32;
            }
        }
        sum.iter().map(|&s| s.clamp(i16::MIN as i32, i16::MAX as i32) as i16).collect()
    }
}

fn peak(frame: &[i16]) -> i16 {
    frame.iter().map(|s| s.saturating_abs()).max().unwrap_or(0)
}

fn db(peak: i16) -> f32 {
    20. * (peak.max(1) as f32 / i16::MAX as f32).log10()
}

/// The device's config, at 48 kHz when it can do that.
pub(crate) fn pick_config(device: &cpal::Device, input: bool) -> Result<cpal::SupportedStreamConfig> {
    let ranges: Vec<_> = if input {
        device.supported_input_configs().map(|c| c.collect()).unwrap_or_default()
    } else {
        device.supported_output_configs().map(|c| c.collect()).unwrap_or_default()
    };
    let usable = |f: SampleFormat| matches!(f, SampleFormat::F32 | SampleFormat::I16);
    for format in [SampleFormat::F32, SampleFormat::I16] {
        if let Some(range) = ranges.iter().find(|r| r.sample_format() == format && r.channels() <= 2)
            && let Some(config) = (*range).try_with_sample_rate(RATE)
        {
            return Ok(config);
        }
    }
    let default = if input { device.default_input_config() } else { device.default_output_config() }.map_err(|e| anyhow!("{e}"))?;
    if usable(default.sample_format()) {
        Ok(default)
    } else {
        Err(anyhow!("unsupported sample format {}", default.sample_format()))
    }
}

fn open_input(id: Option<&str>) -> Result<Input> {
    let device = find_device(true, id).context("no input device")?;
    let config = pick_config(&device, true)?;
    let (rate, channels) = (config.sample_rate(), config.channels());
    let (producer, samples) = rtrb::RingBuffer::new(rate as usize * channels as usize / 2);
    let stream = match config.sample_format() {
        SampleFormat::I16 => input_stream::<i16>(&device, &config.config(), producer)?,
        _ => input_stream::<f32>(&device, &config.config(), producer)?,
    };
    stream.play().map_err(|e| anyhow!("{e}"))?;
    Ok(Input { _stream: stream, samples, rate, channels })
}

fn input_stream<T>(device: &cpal::Device, config: &cpal::StreamConfig, mut producer: rtrb::Producer<f32>) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                for &sample in data {
                    let _ = producer.push(f32::from_sample(sample));
                }
            },
            |err| log::warn!("audio input: {err}"),
            None,
        )
        .map_err(|e| anyhow!("{e}"))
}

fn open_output(id: Option<&str>) -> Result<Output> {
    let device = find_device(false, id).context("no output device")?;
    let config = pick_config(&device, false)?;
    let (rate, channels) = (config.sample_rate(), config.channels());
    let capacity = rate as usize * channels as usize / 4;
    let (samples, consumer) = rtrb::RingBuffer::new(capacity);
    let stream = match config.sample_format() {
        SampleFormat::I16 => output_stream::<i16>(&device, &config.config(), consumer)?,
        _ => output_stream::<f32>(&device, &config.config(), consumer)?,
    };
    stream.play().map_err(|e| anyhow!("{e}"))?;
    Ok(Output { _stream: stream, samples, capacity, rate, channels })
}

fn output_stream<T>(device: &cpal::Device, config: &cpal::StreamConfig, mut consumer: rtrb::Consumer<f32>) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                for slot in data.iter_mut() {
                    *slot = T::from_sample(consumer.pop().unwrap_or(0.));
                }
            },
            |err| log::warn!("audio output: {err}"),
            None,
        )
        .map_err(|e| anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, amplitude: f32, frames: usize) -> Vec<i16> {
        (0..FRAME * frames).map(|i| ((i as f32 * 2. * std::f32::consts::PI * hz / RATE as f32).sin() * amplitude) as i16).collect()
    }

    #[test]
    fn the_gate_mute_and_push_to_talk_decide_what_goes_out() {
        let engine = AudioEngine::headless().unwrap();
        let wait = || std::thread::sleep(Duration::from_millis(60));
        engine.feed(&tone(440., 8000., 3));
        wait();
        assert!(engine.input_level() > 0.2, "level {}", engine.input_level());
        assert!(engine.self_speaking());

        // Quiet input under a gate at -30 dBFS does not count as speaking.
        engine.shared.loud.lock().unwrap().clear();
        engine.set_gate(Some(-30.));
        engine.feed(&tone(440., 300., 3));
        wait();
        assert!(!engine.self_speaking());
        engine.set_gate(None);

        engine.set_muted(true);
        engine.feed(&tone(440., 8000., 3));
        wait();
        assert!(!engine.self_speaking(), "muted");
        engine.set_muted(false);

        engine.set_push_to_talk(true);
        engine.feed(&tone(440., 8000., 3));
        wait();
        assert!(!engine.self_speaking(), "push-to-talk not held");
        engine.set_talking(true);
        engine.feed(&tone(440., 8000., 3));
        wait();
        assert!(engine.self_speaking(), "push-to-talk held");
    }

    /// The cpal paths, against ALSA's null device (silent both ways).
    #[test]
    fn null_devices_open_and_keep_time() {
        let null = "alsa:null".parse().ok().and_then(|id| cpal::default_host().device_by_id(&id));
        if null.is_none() {
            eprintln!("no alsa:null; skipping");
            return;
        }
        let config = AudioConfig { input: Some("alsa:null".into()), output: Some("alsa:null".into()), ..Default::default() };
        let engine = AudioEngine::start(config).unwrap();
        let tap = engine.tap();
        // 20 frames are 200 ms of sound. A busy CI runner can take a while to start the null device,
        // so the test waits for them instead of counting what came in a fixed time.
        let start = Instant::now();
        let mut frames = 0;
        while frames < 20 && start.elapsed() < Duration::from_secs(5) {
            frames += tap.recv_timeout(Duration::from_millis(100)).into_iter().count();
        }
        assert_eq!(engine.errors(), Vec::<String>::new());
        assert!(frames >= 20, "the speakers were fed ({frames} frames in {:?})", start.elapsed());
        assert!(engine.input_level() < 0.01, "null captures silence");
    }

    #[test]
    fn db_of_full_scale_is_zero() {
        assert!(db(i16::MAX).abs() < 0.01);
        assert!((db(i16::MAX / 10) + 20.).abs() < 0.1);
    }
}
