//! Playing a sound file from a message: decoded with symphonia as it plays (a long file is never
//! held decoded in memory), resampled to the output device and played through cpal. One thread
//! decodes into a ring buffer the output callback drains; pause, seek and stop are flags it
//! reads.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use crate::buffered::Buffered;

/// The file extensions (and so attachments) this can play.
pub const PLAYABLE: &[&str] = &["mp3", "ogg", "oga", "flac", "wav", "m4a", "aac", "mp4"];

/// Whether a file of this name and MIME type can be played here.
pub fn playable(filename: &str, mime: &str) -> bool {
    let ext = filename.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    // Opus has no decoder here; an .mp4 is only sound when it says so.
    if mime.contains("opus") || ext == "opus" {
        return false;
    }
    if ext == "mp4" {
        return mime.starts_with("audio/");
    }
    mime.starts_with("audio/") || PLAYABLE.contains(&ext.as_str())
}

#[derive(Default)]
struct Control {
    paused: AtomicBool,
    stop: AtomicBool,
    /// Asked for by the UI, taken by the decoder.
    seek: Mutex<Option<f64>>,
    /// Set by the decoder after a seek: the output drops what it still holds.
    flush: AtomicBool,
    /// Where playback stood at the last seek, in seconds (f64 bits).
    base: AtomicU64,
    /// Device frames played since the last seek.
    played: AtomicU64,
    device_rate: AtomicU64,
    /// The file's length in seconds (f64 bits), 0 when unknown.
    duration: AtomicU64,
    decoded_all: AtomicBool,
    ended: AtomicBool,
    error: Mutex<Option<String>>,
}

/// A sound file playing (or paused). Dropping it stops it.
pub struct Playback {
    control: Arc<Control>,
}

impl Playback {
    /// Starts playing `bytes`. `extension` helps recognise the format; `device` is a cpal output
    /// id (`None` for the default, `Some("alsa:null")` for silence).
    pub fn start(bytes: Vec<u8>, extension: Option<&str>, device: Option<String>) -> Result<Playback> {
        Playback::start_buffered(Arc::new(Buffered::full(bytes)), extension, device)
    }

    /// Starts playing a file that may still be arriving: it waits for the bytes it needs.
    pub fn start_buffered(bytes: Arc<Buffered>, extension: Option<&str>, device: Option<String>) -> Result<Playback> {
        let control = Arc::new(Control::default());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let extension = extension.map(str::to_owned);
        {
            let control = control.clone();
            std::thread::Builder::new()
                .name("rhein-playback".into())
                .spawn(move || {
                    if let Err(err) = run(bytes, extension, device, &control, &ready_tx) {
                        let message = format!("{err:#}");
                        *control.error.lock().unwrap() = Some(message.clone());
                        control.ended.store(true, Ordering::Relaxed);
                        let _ = ready_tx.send(Err(anyhow!(message)));
                    }
                })
                .context("cannot start playback")?;
        }
        ready_rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow!("the file did not start playing"))??;
        Ok(Playback { control })
    }

    pub fn set_paused(&self, paused: bool) {
        self.control.paused.store(paused, Ordering::Relaxed);
    }

    pub fn paused(&self) -> bool {
        self.control.paused.load(Ordering::Relaxed)
    }

    /// Jumps to `seconds` from the start.
    pub fn seek(&self, seconds: f64) {
        *self.control.seek.lock().unwrap() = Some(seconds.max(0.));
        self.control.ended.store(false, Ordering::Relaxed);
    }

    /// How far it has played, in seconds (where a seek goes, until the decoder has made it).
    pub fn position(&self) -> f64 {
        if let Some(to) = *self.control.seek.lock().unwrap() {
            return to;
        }
        let base = f64::from_bits(self.control.base.load(Ordering::Relaxed));
        let rate = self.control.device_rate.load(Ordering::Relaxed).max(1) as f64;
        let at = base + self.control.played.load(Ordering::Relaxed) as f64 / rate;
        match self.duration() {
            Some(d) => at.min(d),
            None => at,
        }
    }

    pub fn duration(&self) -> Option<f64> {
        let d = f64::from_bits(self.control.duration.load(Ordering::Relaxed));
        (d > 0.).then_some(d)
    }

    /// Played to the end (or failed; see [`Playback::error`]).
    pub fn ended(&self) -> bool {
        self.control.ended.load(Ordering::Relaxed)
    }

    pub fn error(&self) -> Option<String> {
        self.control.error.lock().unwrap().clone()
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::Relaxed);
    }
}

fn run(bytes: Arc<Buffered>, extension: Option<String>, device: Option<String>, control: &Arc<Control>, ready: &std::sync::mpsc::Sender<Result<()>>) -> Result<()> {
    // The file first, so a format we cannot read is said before any device is opened. Waiting for
    // bytes still coming stops for a stop, or a seek (which may not need them).
    let reader = {
        let control = control.clone();
        bytes.reader(move || control.stop.load(Ordering::Relaxed) || control.seek.lock().unwrap().is_some())
    };
    let source = MediaSourceStream::new(Box::new(reader), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = &extension {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, source, &FormatOptions { enable_gapless: true, ..Default::default() }, &MetadataOptions::default())
        .map_err(|e| anyhow!("this sound file cannot be read: {e}"))?;
    let mut format = probed.format;
    // The first track with a codec symphonia knows: a video's sound sits next to its pictures.
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).ok_or_else(|| anyhow!("this file has no sound in it"))?.clone();
    let track_id = track.id;
    let source_rate = track.codec_params.sample_rate.ok_or_else(|| anyhow!("this file does not say its sample rate"))?;
    if let (Some(frames), Some(tb)) = (track.codec_params.n_frames, track.codec_params.time_base) {
        let t = tb.calc_time(frames);
        control.duration.store((t.seconds as f64 + t.frac).to_bits(), Ordering::Relaxed);
    }
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| anyhow!("this sound format is not supported: {e}"))?;

    let device = crate::audio::find_device(false, device.as_deref()).context("no output device")?;
    let config = crate::audio::pick_config(&device, false)?;
    let (device_rate, channels) = (config.sample_rate(), config.channels() as usize);
    control.device_rate.store(device_rate as u64, Ordering::Relaxed);
    // A second of sound ahead of the speakers.
    let (mut producer, consumer) = rtrb::RingBuffer::<f32>::new(device_rate as usize * channels);
    let stream = match config.sample_format() {
        SampleFormat::I16 => output::<i16>(&device, &config.config(), consumer, control.clone(), channels)?,
        _ => output::<f32>(&device, &config.config(), consumer, control.clone(), channels)?,
    };
    stream.play().map_err(|e| anyhow!("{e}"))?;
    let _ = ready.send(Ok(()));

    // Linear resampling from the file's rate to the device's, carried across packets.
    let step = source_rate as f64 / device_rate as f64;
    let mut pending: Vec<f32> = Vec::new(); // source-rate frames, already in the device's channel layout
    let mut pos = 0f64;
    let mut buffer: Option<SampleBuffer<f32>> = None;
    loop {
        if control.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(to) = control.seek.lock().unwrap().take() {
            let time = Time { seconds: to.trunc() as u64, frac: to.fract() };
            if format.seek(SeekMode::Accurate, SeekTo::Time { time, track_id: Some(track_id) }).is_ok() {
                decoder.reset();
                pending.clear();
                pos = 0.;
                control.base.store(to.to_bits(), Ordering::Relaxed);
                control.played.store(0, Ordering::Relaxed);
                control.decoded_all.store(false, Ordering::Relaxed);
                control.flush.store(true, Ordering::Relaxed);
            }
        }
        if control.decoded_all.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(_) => {
                // The end of the file (or of what can be read of it).
                control.decoded_all.store(true, Ordering::Relaxed);
                continue;
            }
        };
        if packet.track_id() != track_id {
            continue;
        }
        let Ok(decoded) = decoder.decode(&packet) else { continue };
        let spec = *decoded.spec();
        let buf = buffer.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, spec));
        if buf.capacity() < decoded.capacity() {
            *buf = SampleBuffer::new(decoded.capacity() as u64, spec);
        }
        buf.copy_interleaved_ref(decoded);
        let src_channels = spec.channels.count().max(1);
        for frame in buf.samples().chunks(src_channels) {
            for c in 0..channels {
                // Mono to every speaker; more channels than the device takes are folded in order.
                pending.push(frame[c.min(src_channels - 1)]);
            }
        }
        // Out at the device's rate.
        let frames = pending.len() / channels;
        let mut out = Vec::new();
        while pos + 1. < frames as f64 {
            let i = pos as usize;
            let t = (pos - i as f64) as f32;
            for c in 0..channels {
                let (a, b) = (pending[i * channels + c], pending[(i + 1) * channels + c]);
                out.push(a + (b - a) * t);
            }
            pos += step;
        }
        let keep_from = (pos as usize).min(frames.saturating_sub(1));
        pending.drain(..keep_from * channels);
        pos -= keep_from as f64;
        let mut written = 0;
        while written < out.len() {
            if control.stop.load(Ordering::Relaxed) || control.seek.lock().unwrap().is_some() {
                break;
            }
            match producer.push(out[written]) {
                Ok(()) => written += 1,
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

fn output<T>(device: &cpal::Device, config: &cpal::StreamConfig, mut consumer: rtrb::Consumer<f32>, control: Arc<Control>, channels: usize) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                if control.flush.swap(false, Ordering::Relaxed) {
                    while consumer.pop().is_ok() {}
                }
                if control.paused.load(Ordering::Relaxed) || control.stop.load(Ordering::Relaxed) {
                    data.iter_mut().for_each(|s| *s = T::from_sample(0f32));
                    return;
                }
                let mut frames = 0u64;
                for (n, slot) in data.iter_mut().enumerate() {
                    match consumer.pop() {
                        Ok(sample) => {
                            *slot = T::from_sample(sample);
                            if n % channels == 0 {
                                frames += 1;
                            }
                        }
                        Err(_) => *slot = T::from_sample(0f32),
                    }
                }
                control.played.fetch_add(frames, Ordering::Relaxed);
                if frames == 0 && control.decoded_all.load(Ordering::Relaxed) {
                    control.ended.store(true, Ordering::Relaxed);
                }
            },
            |err| log::warn!("playback: {err}"),
            None,
        )
        .map_err(|e| anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mono 16-bit WAV of a 440 Hz tone.
    fn wav(seconds: f32, rate: u32) -> Vec<u8> {
        let n = (seconds * rate as f32) as u32;
        let mut out = Vec::new();
        let data_len = n * 2;
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..n {
            let s = ((i as f32 * 2. * std::f32::consts::PI * 440. / rate as f32).sin() * 8000.) as i16;
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    fn null() -> Option<String> {
        let id = "alsa:null".to_owned();
        let found = id.parse().ok().and_then(|id| cpal::traits::HostTrait::device_by_id(&cpal::default_host(), &id));
        found.map(|_| id)
    }

    #[test]
    fn a_wav_plays_seeks_and_ends() {
        let Some(device) = null() else {
            eprintln!("no alsa:null; skipping");
            return;
        };
        let playback = Playback::start(wav(0.6, 44_100), Some("wav"), Some(device)).unwrap();
        let duration = playback.duration().expect("a WAV says how long it is");
        assert!((duration - 0.6).abs() < 0.01, "{duration}");
        std::thread::sleep(Duration::from_millis(150));
        assert!(playback.position() > 0.05, "it plays ({})", playback.position());
        playback.set_paused(true);
        let held = playback.position();
        std::thread::sleep(Duration::from_millis(100));
        assert!((playback.position() - held).abs() < 0.02, "paused holds still");
        playback.set_paused(false);
        playback.seek(0.5);
        std::thread::sleep(Duration::from_millis(400));
        assert!(playback.ended(), "it ends after the seek ({} s)", playback.position());
    }

    #[test]
    fn what_is_not_sound_says_so() {
        assert!(Playback::start(b"not a sound file at all".to_vec(), Some("mp3"), null()).is_err());
        assert!(playable("song.mp3", "application/octet-stream"));
        assert!(playable("x", "audio/ogg"));
        assert!(!playable("clip.mp4", "video/mp4"));
        assert!(!playable("voice.opus", "audio/opus"));
    }
}
