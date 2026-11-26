//! The live preview under the canvas: which stream to ask for, and the decisions about
//! each frame that don't need a browser (ADR-0045, `preview.subscribe` in
//! `jstream-protocol`).
//!
//! The canvas opens one preview stream on its session's binary channel: the program
//! mix while it shows the program scene, else the scene it shows (CV-03,
//! `preview.subscribe`'s `scene_id`). Showing another scene closes that stream and opens
//! the next on a new channel number, so a late frame of the old one is never drawn.
//! H.264 goes to WebCodecs' `VideoDecoder` (see `screen.rs`), and JPEG is the
//! fallback when the browser has no `VideoDecoder` or the decoder fails. The program
//! mix is the canvas fitted into the picture, so the picture has the canvas's aspect
//! ratio and lies exactly under the item boxes.
//!
//! Docs: crates/jstream-ui/README.md#canvas

use jstream_control::protocol::{Action, PreviewCodec, PreviewSettings, SceneId};

/// The channel the canvas's first preview stream is opened on. The canvas is the only
/// stream this session opens, so any number does; each later stream takes the next.
pub const CHANNEL: u32 = 1;
/// Pictures per second asked for while the collection's base rate is not known yet: the
/// default base rate (ADR-0119).
pub const FPS: u32 = 30;

/// Pictures per second the canvas asks for at a base rate of `base_fps`: the program's
/// own rate, so the live view is full quality and follows a drag smoothly, within what
/// `preview.subscribe` takes. The engine only encodes when the mix changes, so a still
/// scene costs little.
pub fn live_fps(base_fps: u32) -> u32 {
    if base_fps == 0 {
        return FPS;
    }
    base_fps.clamp(*PreviewSettings::FPS.start(), *PreviewSettings::FPS.end())
}
/// How many chunks may wait in the decoder before the canvas stops feeding it and
/// waits for the next keyframe, so a slow decoder shows a late picture instead of a
/// growing backlog.
///
/// About a fifth of a second at 60 fps. The browser hands chunks to the decoder from
/// the page's main thread, so the queue also grows while that thread is busy (the main
/// window lays out its panels and draws the item boxes there), and drains as soon as
/// it is free. A skip costs every frame up to the next keyframe, up to two seconds of
/// them, so a limit of 3 turned each short stall into a long freeze: the main window's
/// canvas showed 45 of the 56 frames a second it received, where a projector, whose
/// page has little else to do, showed all of them (#1556).
pub const MAX_BACKLOG: u32 = 12;

/// The `preview.subscribe` for a stage `width` × `height` CSS pixels at
/// `pixel_ratio` device pixels per CSS pixel (below 1 when the UI scale is under 100 %,
/// `ui_scale.rs`): a picture as sharp as the stage, kept
/// between 320×180 and the largest the engine sends (1920×1080), so a tiny window still
/// shows something and a large one shows the canvas at full resolution (#1203: at the
/// old 1280×720 cap a window capture's text was unreadable), at `fps` pictures a
/// second. H.264 is asked for first when `h264` (the browser can decode it), with JPEG
/// as the fallback.
pub fn subscribe(width: f32, height: f32, pixel_ratio: f32, h264: bool, fps: u32) -> Action {
    let side = |css: f32, low: u32, high: u32| {
        let device = (css * pixel_ratio).round();
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
        max_width: Some(side(width, 320, *PreviewSettings::WIDTH.end())),
        max_height: Some(side(height, 180, *PreviewSettings::HEIGHT.end())),
        fps: Some(fps.clamp(*PreviewSettings::FPS.start(), *PreviewSettings::FPS.end())),
        codecs,
        scene_id: None,
        source: None,
        source_id: None,
    }
}

/// The stage to size the canvas's stream for: in the desktop app (`local`), where the
/// engine is on the same machine and its pictures cost no network, always the largest
/// the engine sends, so the canvas is as sharp as it can be at any dock size; in a
/// browser, the `measured` stage, so a tablet over Wi-Fi gets only what it shows.
pub fn stage(measured: (f32, f32, f32), local: bool) -> (f32, f32, f32) {
    if local {
        (
            *PreviewSettings::WIDTH.end() as f32,
            *PreviewSettings::HEIGHT.end() as f32,
            1.0,
        )
    } else {
        measured
    }
}

/// The scene the canvas's stream shows: `None`, the program mix, while the canvas shows
/// the program scene (whose picture then has its transitions) or no scene, else the
/// scene it shows.
pub fn stream_scene(shown: Option<&SceneId>, program: Option<&SceneId>) -> Option<SceneId> {
    shown.filter(|s| Some(*s) != program).cloned()
}

/// How long what the canvas's stream should show must hold before the canvas switches
/// streams. A switch from this page moves the program with the selection (`Studio::act`),
/// but whenever the selection runs ahead of the program, the canvas wants the new scene on
/// its own for a moment; within this time it wants the program again, and the program
/// stream, with its transition, plays on (#1205).
pub const SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

/// The stream to switch to once `want`, wanted [`SETTLE`] ago, has settled: `None` while
/// what is wanted `now` has moved on (a later change settles on its own) or `current`
/// already shows it.
pub fn settled_switch(
    current: &Stream,
    want: &Option<SceneId>,
    now: &Option<SceneId>,
) -> Option<Stream> {
    (want == now && current.scene != *want).then(|| current.showing(want.clone()))
}

/// The canvas's current stream: its channel and what it shows, and how to ask for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    /// The channel its frames carry.
    pub channel: u32,
    /// The scene it shows, or `None` for the program mix.
    pub scene: Option<SceneId>,
    /// The stage's size in CSS pixels, and device pixels per CSS pixel.
    pub stage: (f32, f32, f32),
    /// Whether H.264 is still asked for.
    pub h264: bool,
    /// Pictures per second asked for ([`live_fps`]).
    pub fps: u32,
}

impl Stream {
    /// The first stream: the program mix on [`CHANNEL`].
    pub fn first(stage: (f32, f32, f32), h264: bool, fps: u32) -> Stream {
        Stream {
            channel: CHANNEL,
            scene: None,
            stage,
            h264,
            fps,
        }
    }

    /// The same stream at `fps`, on the next channel, or `None` when it is at `fps`
    /// already: a new base rate opens a new stream.
    pub fn at_fps(&self, fps: u32) -> Option<Stream> {
        (self.fps != fps).then(|| Stream {
            channel: self.channel.wrapping_add(1).max(1),
            fps,
            ..self.clone()
        })
    }

    /// The same stream for a stage now `stage`, on the next channel, or `None` while the
    /// picture it asks for is within a quarter of the one it has: a stage that grew (a
    /// window maximized, a dock resized) gets a sharper picture, and one that shrank a
    /// cheaper one, without a new stream for every pixel of a drag.
    pub fn at_stage(&self, stage: (f32, f32, f32)) -> Option<Stream> {
        let size = |(w, h, ratio): (f32, f32, f32)| match subscribe(w, h, ratio, false, FPS) {
            Action::PreviewSubscribe {
                max_width: Some(w),
                max_height: Some(h),
                ..
            } => (w as f32, h as f32),
            _ => (0.0, 0.0),
        };
        let (now_w, now_h) = size(self.stage);
        let (next_w, next_h) = size(stage);
        let off = |now: f32, next: f32| now <= 0.0 || (next - now).abs() / now >= 0.25;
        (off(now_w, next_w) || off(now_h, next_h)).then(|| Stream {
            channel: self.channel.wrapping_add(1).max(1),
            stage,
            ..self.clone()
        })
    }

    /// The same stream showing `scene`, on the same channel: the engine moves it there
    /// with `preview.show` ([`Stream::show`]), so the decoder and the last picture carry
    /// on with no new stream to open (#1205).
    pub fn showing(&self, scene: Option<SceneId>) -> Stream {
        Stream {
            scene,
            ..self.clone()
        }
    }

    /// The same stream on the next channel: opened anew with `preview.subscribe`, when
    /// the engine couldn't move this one.
    pub fn on_next_channel(&self) -> Stream {
        Stream {
            channel: self.channel.wrapping_add(1).max(1),
            ..self.clone()
        }
    }

    /// Its `preview.show`: what an open stream on its channel shows becomes its scene.
    pub fn show(&self) -> Action {
        Action::PreviewShow {
            channel: self.channel,
            scene_id: self.scene.clone(),
        }
    }

    /// Its `preview.request_keyframe`.
    pub fn keyframe(&self) -> Action {
        Action::PreviewRequestKeyframe {
            channel: self.channel,
        }
    }

    /// Its `preview.subscribe`.
    pub fn subscribe(&self) -> Action {
        let (width, height, ratio) = self.stage;
        match subscribe(width, height, ratio, self.h264, self.fps) {
            Action::PreviewSubscribe {
                max_width,
                max_height,
                fps,
                codecs,
                ..
            } => Action::PreviewSubscribe {
                channel: self.channel,
                max_width,
                max_height,
                fps,
                codecs,
                scene_id: self.scene.clone(),
                source: None,
                source_id: None,
            },
            other => other,
        }
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
/// and, once its backlog grows past [`MAX_BACKLOG`], skips to the next keyframe, which
/// it asks the engine for at once ([`H264Feed::take_keyframe_ask`], `preview.request_keyframe`)
/// so the skip lasts a frame or two rather than up to the stream's two seconds
/// between keyframes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct H264Feed {
    /// The codec string the decoder is configured with, if it is.
    configured: Option<String>,
    /// Dropping frames until a keyframe.
    waiting: bool,
    /// A keyframe to ask the engine for, once.
    ask: bool,
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
            self.ask = true;
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

    /// Whether to ask the engine for a keyframe now: once each time a backlog starts a
    /// skip, never again until the next one.
    pub fn take_keyframe_ask(&mut self) -> bool {
        std::mem::take(&mut self.ask)
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
    /// Waiting pictures a newer one replaced, since [`JpegSlot::take_replaced`].
    replaced: u64,
}

impl<T> Default for JpegSlot<T> {
    fn default() -> Self {
        JpegSlot {
            busy: false,
            waiting: None,
            closed: false,
            replaced: 0,
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
            if self.waiting.replace(picture).is_some() {
                self.replaced = self.replaced.saturating_add(1);
            }
            return None;
        }
        Some(picture)
    }

    /// How many waiting pictures newer ones replaced since the last call: pictures the
    /// view skipped.
    pub fn take_replaced(&mut self) -> u64 {
        std::mem::take(&mut self.replaced)
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

/// The text the canvas shows while its live preview is off.
pub const OFF_MESSAGE: &str = "Live preview off · right-click to enable it";

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
    /// The person turned the live preview off (the canvas menu's "Disable live
    /// preview"): no stream is open, so the engine neither draws nor encodes a picture
    /// for the canvas, and the item boxes are all the canvas shows.
    Off,
}

impl PreviewState {
    /// The state once the canvas subscribes to another scene's stream: `Live` stays
    /// `Live`, so the last picture stays on the canvas until the new stream draws
    /// (#1205: a scene switch re-subscribes, and showing the "Starting" placeholder
    /// each time flashed grey boxes), and anything else starts again.
    pub fn resubscribed(&self) -> PreviewState {
        match self {
            PreviewState::Live => PreviewState::Live,
            _ => PreviewState::Starting,
        }
    }

    /// The text the canvas shows over the background, if any.
    pub fn message(&self) -> Option<String> {
        match self {
            PreviewState::Starting => Some("Starting the preview…".to_owned()),
            PreviewState::Live => None,
            PreviewState::Unavailable(reason) => Some(format!("No preview: {reason}")),
            PreviewState::Off => Some(OFF_MESSAGE.to_owned()),
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
        assert!(!feed.take_keyframe_ask(), "nothing to ask for yet");
        assert_eq!(feed.step(false, &delta, MAX_BACKLOG + 1), Step::Skip);
        assert!(feed.take_keyframe_ask(), "the skip asks for a keyframe");
        assert!(!feed.take_keyframe_ask(), "once");
        // Still waiting, even once the decoder has caught up, and asking no more.
        assert_eq!(feed.step(false, &delta, 0), Step::Skip);
        assert!(!feed.take_keyframe_ask());
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
        assert_eq!(slot.take_replaced(), 1);
        assert_eq!(slot.take_replaced(), 0);
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
    fn the_stream_follows_the_shown_scene_on_its_channel() {
        let (main, other) = (SceneId::new("main"), SceneId::new("other"));
        assert_eq!(
            stream_scene(Some(&main), Some(&main)),
            None,
            "the program mix"
        );
        assert_eq!(stream_scene(None, Some(&main)), None);
        assert_eq!(stream_scene(Some(&other), Some(&main)), Some(other.clone()));
        assert_eq!(stream_scene(Some(&other), None), Some(other.clone()));
        let first = Stream::first((800.0, 450.0, 1.0), true, FPS);
        let next = first.showing(Some(other.clone()));
        assert_eq!((next.channel, next.scene.as_ref()), (CHANNEL, Some(&other)));
        assert_eq!(
            next.show(),
            Action::PreviewShow {
                channel: CHANNEL,
                scene_id: Some(other.clone()),
            }
        );
        assert_eq!(
            next.keyframe(),
            Action::PreviewRequestKeyframe { channel: CHANNEL }
        );
        // When the engine can't move it, the same picture on a new stream.
        let reopened = next.on_next_channel();
        let Action::PreviewSubscribe {
            channel, scene_id, ..
        } = reopened.subscribe()
        else {
            panic!("a subscribe");
        };
        assert_eq!((channel, scene_id), (CHANNEL + 1, Some(other)));
        let wrapped = Stream {
            channel: u32::MAX,
            ..first
        };
        assert_eq!(wrapped.on_next_channel().channel, 1, "0 is skipped");
    }

    /// A collection at 60 fps gets a 60 fps live view, on a new stream of the same
    /// scene; one already at its rate stays.
    #[test]
    fn the_live_view_follows_the_base_rate() {
        assert_eq!(live_fps(30), 30);
        assert_eq!(live_fps(60), 60);
        assert_eq!(live_fps(0), FPS, "not known yet");
        assert_eq!(live_fps(240), *PreviewSettings::FPS.end());
        let scene = Some(SceneId::new("a"));
        let first = Stream::first((800.0, 450.0, 1.0), true, live_fps(30)).showing(scene.clone());
        assert_eq!(first.at_fps(30), None);
        let fast = first.at_fps(live_fps(60)).expect("a new stream");
        assert_eq!((fast.scene.clone(), fast.fps), (scene, 60));
        assert_ne!(fast.channel, first.channel);
        let Action::PreviewSubscribe { fps, .. } = fast.subscribe() else {
            panic!("a subscribe");
        };
        assert_eq!(fps, Some(60));
    }

    /// #1205: a scene switch's moment of wanting the new scene on its own, before the
    /// program follows, doesn't switch streams; a change that holds does, once.
    #[test]
    fn only_a_settled_change_switches_streams() {
        let program = Stream::first((800.0, 450.0, 1.0), true, FPS);
        let b = Some(SceneId::new("b"));
        // Wanted B, then the program again before B settled: the program stream stays.
        assert_eq!(settled_switch(&program, &b, &None), None);
        // Wanted the program all along: nothing to do.
        assert_eq!(settled_switch(&program, &None, &None), None);
        // B held: the stream switches to it, on its own channel.
        let next = settled_switch(&program, &b, &b).expect("a switch");
        assert_eq!(next.scene, b);
        assert_eq!(next.channel, program.channel);
        // B shown already: a second settle of the same change does nothing.
        assert_eq!(settled_switch(&next, &b, &b), None);
    }

    #[test]
    fn the_desktop_app_always_asks_for_the_full_picture() {
        let small = (400.0, 225.0, 1.0);
        let big = (1700.0, 956.0, 1.0);
        assert_eq!(stage(small, false), small);
        let local = Stream::first(stage(small, true), true, FPS);
        assert!(matches!(
            local.subscribe(),
            Action::PreviewSubscribe {
                max_width: Some(1920),
                max_height: Some(1080),
                ..
            }
        ));
        // A resized dock never restarts the desktop's stream.
        assert_eq!(local.at_stage(stage(big, true)), None);
    }

    #[test]
    fn a_stage_that_grows_or_shrinks_a_quarter_gets_a_new_stream() {
        // The docked canvas asked once, at the size it opened at, so maximizing the
        // window kept the small picture while a pop-out was sharp.
        let first = Stream::first((640.0, 360.0, 1.0), true, FPS);
        let bigger = first
            .at_stage((1600.0, 900.0, 1.0))
            .expect("a sharper stream");
        assert_ne!(bigger.channel, first.channel);
        let Action::PreviewSubscribe { max_width, .. } = bigger.subscribe() else {
            panic!("a subscribe");
        };
        assert_eq!(max_width, Some(1600));
        // A few pixels of a drag: the same stream.
        assert_eq!(bigger.at_stage((1580.0, 890.0, 1.0)), None);
        // Past the engine's largest picture nothing changes either.
        let full = Stream::first((1920.0, 1080.0, 1.0), true, FPS);
        assert_eq!(full.at_stage((3840.0, 2160.0, 1.0)), None);
        // Much smaller: a cheaper stream.
        assert!(bigger.at_stage((800.0, 450.0, 1.0)).is_some());
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
            ..
        } = subscribe(800.0, 450.0, 2.0, true, FPS)
        else {
            panic!("a subscribe");
        };
        assert_eq!(channel, CHANNEL);
        assert_eq!(scene_id, None, "the program mix");
        assert_eq!((max_width, max_height), (Some(1600), Some(900)));
        assert_eq!(fps, Some(FPS));
        assert_eq!(codecs, vec![PreviewCodec::H264, PreviewCodec::Jpeg]);
        let Action::PreviewSubscribe {
            max_width,
            max_height,
            codecs,
            ..
        } = subscribe(100.0, f32::NAN, 1.0, false, FPS)
        else {
            panic!("a subscribe");
        };
        assert_eq!((max_width, max_height), (Some(320), Some(180)));
        assert_eq!(codecs, vec![PreviewCodec::Jpeg]);
        // #1203: a large stage gets the full 1080p picture, not a 720p one.
        let Action::PreviewSubscribe {
            max_width,
            max_height,
            ..
        } = subscribe(1600.0, 900.0, 1.5, true, 60)
        else {
            panic!("a subscribe");
        };
        assert_eq!((max_width, max_height), (Some(1920), Some(1080)));
        // At a 50 % UI scale the stage's CSS pixels are half a device pixel each.
        let Action::PreviewSubscribe {
            max_width,
            max_height,
            ..
        } = subscribe(1600.0, 900.0, 0.5, true, 60)
        else {
            panic!("a subscribe");
        };
        assert_eq!((max_width, max_height), (Some(800), Some(450)));
        // Each request resolves on the engine's side.
        for action in [
            subscribe(1.0, 1.0, 1.0, true, 0),
            subscribe(9e9, 9e9, 3.0, true, 999),
            subscribe(800.0, 450.0, 1.0, true, live_fps(60)),
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
        assert_eq!(PreviewState::Off.message().as_deref(), Some(OFF_MESSAGE));
        // #1205: a re-subscribe keeps a live picture, and starts again otherwise.
        assert_eq!(PreviewState::Live.resubscribed(), PreviewState::Live);
        assert_eq!(
            PreviewState::Starting.resubscribed(),
            PreviewState::Starting
        );
        assert_eq!(
            PreviewState::Unavailable("x".into()).resubscribed(),
            PreviewState::Starting
        );
    }
}
