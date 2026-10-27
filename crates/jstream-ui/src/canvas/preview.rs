//! The live preview under the canvas: which stream to ask for, and the decisions about
//! each frame that don't need a browser (ADR-0045, `preview.subscribe` in
//! `jstream-protocol`).
//!
//! The canvas opens one preview stream of the program mix on its session's binary
//! channel. H.264 goes to WebCodecs' `VideoDecoder` (see `screen.rs`), and JPEG is the
//! fallback when the browser has no `VideoDecoder` or the decoder fails. The program
//! mix is the canvas fitted into the picture, so the picture has the canvas's aspect
//! ratio and lies exactly under the item boxes.
//!
//! Docs: crates/jstream-ui/README.md#canvas

use jstream_control::protocol::{Action, PreviewCodec, PreviewSettings};

/// The channel the canvas's preview stream is opened on. The canvas is the only
/// stream this session opens, so any fixed number does.
pub const CHANNEL: u32 = 1;
/// Pictures per second asked for: the engine's maximum, so the preview follows a drag
/// smoothly. The engine only encodes when the mix changes, so a still scene costs
/// little.
pub const FPS: u32 = 30;
/// How many chunks may wait in the decoder before the canvas stops feeding it and
/// waits for the next keyframe, so a slow decoder shows a late picture instead of a
/// growing backlog.
pub const MAX_BACKLOG: u32 = 3;

/// The `preview.subscribe` for a stage `width` × `height` CSS pixels at
/// `pixel_ratio` device pixels per CSS pixel: a picture as sharp as the stage, kept
/// between 320×180 and 1280×720 so a tiny window still shows something and a large
/// one doesn't make the engine encode more than an editing view needs. H.264 is asked
/// for first when `h264` (the browser can decode it), with JPEG as the fallback.
pub fn subscribe(width: f32, height: f32, pixel_ratio: f32, h264: bool) -> Action {
    let side = |css: f32, low: u32, high: u32| {
        let device = (css * pixel_ratio.max(1.0)).round();
        if device.is_finite() && device > 0.0 {
            (device as u32).clamp(low, high)
        } else {
            low
        }
    };
    let codecs = if h264 {
        vec![PreviewCodec::H264, PreviewCodec::Jpeg]
    } else {
        vec![PreviewCodec::Jpeg]
    };
    Action::PreviewSubscribe {
        channel: CHANNEL,
        max_width: Some(side(width, 320, 1280).min(*PreviewSettings::WIDTH.end())),
        max_height: Some(side(height, 180, 720).min(*PreviewSettings::HEIGHT.end())),
        fps: Some(FPS),
        codecs,
        scene_id: None,
    }
}

/// The WebCodecs codec string (`avc1.PPCCLL`) for an H.264 access unit, from the
/// profile, constraint flags and level in its sequence parameter set, or `None` when
/// the access unit carries no SPS. The engine sends one in band with every keyframe.
pub fn avc_codec(annex_b: &[u8]) -> Option<String> {
    let mut i = 0;
    while i + 3 <= annex_b.len() {
        // A start code is 00 00 01 (a four-byte 00 00 00 01 ends the same way).
        if annex_b[i..i + 3] == [0, 0, 1] {
            let nal = annex_b.get(i + 3..i + 7)?;
            if nal[0] & 0x1f == 7 {
                return Some(format!("avc1.{:02X}{:02X}{:02X}", nal[1], nal[2], nal[3]));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    None
}

/// What to do with one H.264 frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// (Re)configure the decoder for this codec string, then decode the frame as a
    /// keyframe.
    Configure(String),
    /// Decode the frame.
    Decode {
        /// Whether it is a keyframe.
        key: bool,
    },
    /// Drop the frame: the decoder can't use it.
    Skip,
}

/// Feeds a decoder H.264 frames so that it only ever gets a stream it can decode: it
/// starts at a keyframe, is reconfigured when the stream's profile or level changes,
/// and, once its backlog grows past [`MAX_BACKLOG`], skips to the next keyframe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct H264Feed {
    /// The codec string the decoder is configured with, if it is.
    configured: Option<String>,
    /// Dropping frames until a keyframe.
    waiting: bool,
}

impl H264Feed {
    /// What to do with a frame, given the decoder's current `backlog` (its
    /// `decodeQueueSize`).
    pub fn step(&mut self, keyframe: bool, payload: &[u8], backlog: u32) -> Step {
        if keyframe {
            let Some(codec) = avc_codec(payload) else {
                // A keyframe without its SPS only decodes on a configured decoder.
                return match self.configured {
                    Some(_) => self.decode(true),
                    None => Step::Skip,
                };
            };
            if self.configured.as_ref() != Some(&codec) {
                self.configured = Some(codec.clone());
                self.waiting = false;
                return Step::Configure(codec);
            }
            return self.decode(true);
        }
        if self.configured.is_none() || self.waiting {
            return Step::Skip;
        }
        if backlog > MAX_BACKLOG {
            self.waiting = true;
            return Step::Skip;
        }
        Step::Decode { key: false }
    }

    fn decode(&mut self, key: bool) -> Step {
        self.waiting = false;
        Step::Decode { key }
    }

    /// The decoder was reset or failed: start again at the next keyframe with SPS.
    pub fn reset(&mut self) {
        *self = H264Feed::default();
    }
}

/// The JPEG decode slot: one picture decodes at a time, and while it does only the
/// newest that arrives waits, so a slow decode shows the latest picture instead of a
/// backlog, and the last picture is never lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JpegSlot<T> {
    busy: bool,
    waiting: Option<T>,
    closed: bool,
}

impl<T> Default for JpegSlot<T> {
    fn default() -> Self {
        JpegSlot {
            busy: false,
            waiting: None,
            closed: false,
        }
    }
}

impl<T> JpegSlot<T> {
    /// A picture arrived. Returns it if it should be decoded now; otherwise keeps it as
    /// the newest waiting one, replacing any older.
    pub fn offer(&mut self, picture: T) -> Option<T> {
        if self.closed {
            return None;
        }
        if self.busy {
            self.waiting = Some(picture);
            return None;
        }
        Some(picture)
    }

    /// The decode of a picture `offer` or `settled` returned has started.
    pub fn started(&mut self) {
        self.busy = true;
    }

    /// The decode finished, drawn or not. Returns the newest waiting picture, to be
    /// decoded now.
    pub fn settled(&mut self) -> Option<T> {
        self.busy = false;
        if self.closed {
            return None;
        }
        self.waiting.take()
    }

    /// The screen is gone: nothing more is decoded.
    pub fn close(&mut self) {
        self.closed = true;
        self.waiting = None;
    }
}

/// Where the preview stands, for the canvas's placeholder text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewState {
    /// Subscribed; no picture shown yet.
    Starting,
    /// Pictures are being shown.
    Live,
    /// No preview: the engine refused the stream, ended it, or the browser can't show
    /// it. Carries why.
    Unavailable(String),
}

impl PreviewState {
    /// The text the canvas shows over the background, if any.
    pub fn message(&self) -> Option<String> {
        match self {
            PreviewState::Starting => Some("Starting the preview…".to_owned()),
            PreviewState::Live => None,
            PreviewState::Unavailable(reason) => Some(format!("No preview: {reason}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An access unit as the engine sends a keyframe: AUD, SPS, PPS, IDR slice.
    fn keyframe(profile: u8, level: u8) -> Vec<u8> {
        let mut au = vec![0, 0, 0, 1, 0x09, 0xf0];
        au.extend([0, 0, 0, 1, 0x67, profile, 0xc0, level, 0xda]);
        au.extend([0, 0, 1, 0x68, 0xce, 0x3c, 0x80]);
        au.extend([0, 0, 1, 0x65, 0x88, 0x84]);
        au
    }

    #[test]
    fn the_codec_string_comes_from_the_sps() {
        assert_eq!(
            avc_codec(&keyframe(0x42, 0x1e)).as_deref(),
            Some("avc1.42C01E")
        );
        assert_eq!(
            avc_codec(&keyframe(0x64, 0x28)).as_deref(),
            Some("avc1.64C028")
        );
        assert_eq!(avc_codec(&[0, 0, 1, 0x41, 0x9a]), None);
        assert_eq!(avc_codec(&[0, 0, 1, 0x67]), None, "a cut-off SPS");
        assert_eq!(avc_codec(&[]), None);
    }

    #[test]
    fn the_feed_starts_at_a_keyframe_and_reconfigures_on_a_new_profile() {
        let mut feed = H264Feed::default();
        let delta = [0, 0, 1, 0x41, 0x9a];
        assert_eq!(feed.step(false, &delta, 0), Step::Skip);
        assert_eq!(
            feed.step(true, &keyframe(0x42, 0x1e), 0),
            Step::Configure("avc1.42C01E".into())
        );
        assert_eq!(feed.step(false, &delta, 0), Step::Decode { key: false });
        assert_eq!(
            feed.step(true, &keyframe(0x42, 0x1e), 0),
            Step::Decode { key: true }
        );
        assert_eq!(
            feed.step(true, &keyframe(0x42, 0x1f), 0),
            Step::Configure("avc1.42C01F".into())
        );
    }

    #[test]
    fn a_backlog_skips_to_the_next_keyframe() {
        let mut feed = H264Feed::default();
        let delta = [0, 0, 1, 0x41, 0x9a];
        feed.step(true, &keyframe(0x42, 0x1e), 0);
        assert_eq!(feed.step(false, &delta, MAX_BACKLOG + 1), Step::Skip);
        // Still waiting, even once the decoder has caught up.
        assert_eq!(feed.step(false, &delta, 0), Step::Skip);
        assert_eq!(
            feed.step(true, &keyframe(0x42, 0x1e), 0),
            Step::Decode { key: true }
        );
        assert_eq!(feed.step(false, &delta, 0), Step::Decode { key: false });
        feed.reset();
        assert_eq!(feed.step(false, &delta, 0), Step::Skip);
    }

    #[test]
    fn the_newest_jpeg_waits_and_none_is_lost() {
        let mut slot = JpegSlot::default();
        assert_eq!(slot.offer(1), Some(1));
        slot.started();
        assert_eq!(slot.offer(2), None);
        assert_eq!(slot.offer(3), None, "replaces 2");
        assert_eq!(slot.settled(), Some(3));
        slot.started();
        assert_eq!(slot.settled(), None, "nothing waiting");
        assert_eq!(slot.offer(4), Some(4), "idle again");
        slot.started();
        assert_eq!(slot.offer(5), None);
        slot.close();
        assert_eq!(slot.settled(), None, "nothing restarts after close");
        assert_eq!(slot.offer(6), None);
    }

    #[test]
    fn the_request_follows_the_stage_within_bounds() {
        let Action::PreviewSubscribe {
            channel,
            max_width,
            max_height,
            fps,
            codecs,
            scene_id,
        } = subscribe(800.0, 450.0, 2.0, true)
        else {
            panic!("a subscribe");
        };
        assert_eq!(channel, CHANNEL);
        assert_eq!(scene_id, None, "the program mix");
        assert_eq!((max_width, max_height), (Some(1280), Some(720)));
        assert_eq!(fps, Some(FPS));
        assert_eq!(codecs, vec![PreviewCodec::H264, PreviewCodec::Jpeg]);
        let Action::PreviewSubscribe {
            max_width,
            max_height,
            codecs,
            ..
        } = subscribe(100.0, f32::NAN, 1.0, false)
        else {
            panic!("a subscribe");
        };
        assert_eq!((max_width, max_height), (Some(320), Some(180)));
        assert_eq!(codecs, vec![PreviewCodec::Jpeg]);
        // Each request resolves on the engine's side.
        for action in [
            subscribe(1.0, 1.0, 1.0, true),
            subscribe(9e9, 9e9, 3.0, true),
        ] {
            let Action::PreviewSubscribe {
                max_width,
                max_height,
                fps,
                codecs,
                ..
            } = action
            else {
                panic!("a subscribe");
            };
            assert!(PreviewSettings::resolve(max_width, max_height, fps, &codecs).is_ok());
        }
        assert_eq!(
            PreviewState::Unavailable("x".into()).message().as_deref(),
            Some("No preview: x")
        );
        assert_eq!(PreviewState::Live.message(), None);
    }
}
