//! What a guild call may send, as the web client works it out (`web/src/lib/screenShare.js`
//! `publishBudget` / `simulcastLadder`). Every participant has a publish ceiling (the channel's
//! bitrate, clamped by the server and the SFU); the SFU adds up the bitrates a participant's video
//! tracks *declare* and mutes the largest until they fit. LiveKit's own presets for a full-HD
//! screen declare more than the usual 2.5 Mbit/s, so a monitor share went out muted, black. Here
//! the ceiling is split into audio, a camera slice and a screen slice, and each video gets a
//! simulcast ladder whose layers add up to its slice.

use livekit::options::{VideoEncoding, VideoPreset};

const AUDIO_BITRATE: u64 = 48_000;
const AUDIO_FLOOR: u64 = 6_000;
const CAMERA_MAX: u64 = 1_500_000;
/// Below this a layer is not worth sending; the ladder loses a rung instead.
const LAYER_FLOOR: u64 = 80_000;

/// A ceiling split between what one participant sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub total: u64,
    pub audio: u64,
    pub camera: u64,
    pub screen: u64,
}

pub fn budget(ceiling: u64) -> Option<Budget> {
    if ceiling == 0 {
        return None;
    }
    let audio = (ceiling * 15 / 100).clamp(AUDIO_FLOOR, AUDIO_BITRATE);
    let video = ceiling.saturating_sub(audio * 2);
    let camera = (video / 5).min(CAMERA_MAX);
    Some(Budget { total: ceiling, audio, camera, screen: video - camera })
}

/// How a video is sent: the top encoding and the lower simulcast layers (smallest first).
#[derive(Debug, Clone)]
pub struct Ladder {
    pub top: VideoEncoding,
    pub layers: Vec<VideoPreset>,
    /// What the layers add up to, as the SFU will count it.
    pub declared: u64,
}

/// A ladder for a `width`×`height` picture that fits `slice`. `screen` uses the web client's
/// "detail" weighting (sharp text: most of the bits in the top layer, 15 frames a second), a
/// camera its default one.
pub fn ladder(width: u32, height: u32, fps: f64, slice: u64, screen: bool) -> Ladder {
    let size = width.max(height);
    let mut count = if size >= 960 {
        3
    } else if size >= 480 {
        2
    } else {
        1
    };
    let shares = |count: usize| -> &'static [f64] {
        match (screen, count) {
            (true, 3) => &[1. / 32., 1. / 8., 1.],
            (true, 2) => &[1. / 8., 1.],
            (false, 3) => &[1. / 16., 1. / 4., 1.],
            (false, 2) => &[1. / 4., 1.],
            _ => &[1.],
        }
    };
    let slice = slice.max(1);
    loop {
        let ratios = shares(count);
        let top = (slice as f64 / ratios.iter().sum::<f64>()).floor();
        let bitrates: Vec<u64> = ratios.iter().map(|r| (top * r).floor() as u64).collect();
        if count == 1 || bitrates[0] >= LAYER_FLOOR {
            let layers = bitrates[..count - 1]
                .iter()
                .enumerate()
                .map(|(i, &max_bitrate)| {
                    let scale = 1u32 << (count - 1 - i);
                    VideoPreset { width: width / scale, height: height / scale, encoding: VideoEncoding { max_bitrate, max_framerate: fps } }
                })
                .collect();
            return Ladder { top: VideoEncoding { max_bitrate: bitrates[count - 1], max_framerate: fps }, layers, declared: bitrates.iter().sum() };
        }
        count -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ceiling_splits_as_the_web_client_splits_it() {
        let b = budget(2_500_000).unwrap();
        assert_eq!(b.audio, 48_000);
        assert_eq!(b.camera, 480_800);
        assert_eq!(b.screen, 1_923_200);
        assert!(budget(0).is_none());
        // A tight channel still leaves the voice its floor.
        assert_eq!(budget(64_000).unwrap().audio, 9_600);
    }

    #[test]
    fn a_full_hd_screen_fits_its_slice() {
        let slice = budget(2_500_000).unwrap().screen;
        // The bottom of three rungs would get under 80 kbit/s: two layers, the full picture and
        // half of it, as the web client makes them.
        let l = ladder(1920, 1080, 15., slice, true);
        assert_eq!(l.layers.len(), 1);
        assert_eq!((l.layers[0].width, l.layers[0].height), (960, 540));
        assert!(l.declared <= slice, "{} > {slice}", l.declared);
        // With more room, three.
        let wide = ladder(1920, 1080, 15., 8_000_000, true);
        assert_eq!((wide.layers.len(), wide.layers[0].width), (2, 480));
        assert!(wide.declared <= 8_000_000);
        // Too little for two: one layer, never over.
        let thin = ladder(1920, 1080, 15., 600_000, true);
        assert!(thin.layers.is_empty() && thin.declared <= 600_000);
        assert_eq!(ladder(400, 300, 15., 500_000, true).layers.len(), 0);
    }
}
