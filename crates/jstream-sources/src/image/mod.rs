//! The `image` source: a PNG, JPEG, WebP or GIF file, animated when the file is.

pub(crate) mod decode;

use crate::color::rgba_frame;
use crate::describe::{asset, kind, param};
use crate::source::{self, at_millis, changed_non_live, millis};
use crate::{Context, Painter, Source, SourceError, SourceKind, Still};
use jstream_media::{FrameSize, MediaTime, VideoFrame};
use jstream_protocol::{KindDescriptor, Settings};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The `image` kind.
pub(crate) struct ImageKind(KindDescriptor);

impl ImageKind {
    pub(crate) fn new() -> Self {
        Self(KindDescriptor {
            params: vec![param(
                "file",
                "Image file",
                asset(&["png", "jpg", "jpeg", "webp", "gif"]),
                Value::Null,
            )],
            ..kind("image", "Image", true, false)
        })
    }
}

impl SourceKind for ImageKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    /// Reads and decodes the whole file, every animation frame included, so this
    /// blocks on I/O and decoding. A still image keeps the file to decode again once
    /// its pixels are let go of ([`Still`]).
    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let file = source::asset(settings, "file")?;
        let load = {
            let (assets, limits) = (ctx.assets.clone(), ctx.limits);
            let file = file.clone();
            move || match &file {
                Some((root, path)) => {
                    let bytes = assets.read(*root, path, limits.max_file_bytes)?;
                    Animation::new(decode::decode(&bytes, path, &limits)?)
                }
                None => Animation::empty(),
            }
        };
        let animation = load()?;
        let shows = if animation.animated() {
            Shows::Animated(Arc::new(Shown {
                animation,
                index: AtomicUsize::new(0),
            }))
        } else {
            let path = file.map(|(_, path)| path).unwrap_or_default();
            Shows::Still(Arc::new(Still::new(
                Arc::new(animation.into_last()),
                path,
                move || load().map(Animation::into_last),
            )))
        };
        Ok(Box::new(ImageSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            shows,
        }))
    }
}

/// A running `image` source.
struct ImageSource {
    descriptor: KindDescriptor,
    settings: Settings,
    shows: Shows,
}

/// What an image source shows: its frames, stepped on, or one still frame, which it
/// holds only while the host does.
enum Shows {
    Animated(Arc<Shown>),
    Still(Arc<Still>),
}

/// An image's frames and the one shown, which an animated image's [`Painter`] moves
/// on, so the host's own thread can step it without the source (ADR-0117 §3).
#[derive(Debug)]
struct Shown {
    animation: Animation,
    /// The frame shown last: the one [`Painter::paint`] or [`Source::frame`] picked.
    index: AtomicUsize,
}

impl Painter for Shown {
    /// Picks the frame to show at `at`: no drawing, the frames are decoded already.
    fn paint(&self, at: MediaTime) {
        let index = self.animation.index(millis(at));
        self.index.store(index, Ordering::Relaxed);
    }

    fn frame(&self) -> Arc<VideoFrame> {
        let index = self.index.load(Ordering::Relaxed);
        self.animation.frame(index)
    }

    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        self.animation.next_change(millis(at)).map(at_millis)
    }
}

impl Source for ImageSource {
    /// A still image's frame is decoded from the file again when no frame it handed
    /// out is held any more ([`Still::frame`]): a host that doesn't hold one asks
    /// [`still`](Source::still) instead.
    fn frame(&mut self, at: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        match &self.shows {
            Shows::Animated(shown) => {
                shown.paint(at);
                Ok(shown.frame())
            }
            Shows::Still(still) => still.frame(),
        }
    }

    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        match &self.shows {
            Shows::Animated(shown) => shown.next_change(at),
            Shows::Still(_) => None,
        }
    }

    /// An animated image's: the host steps it on its own thread and takes each new
    /// frame without a plan. A still image has none.
    fn painter(&self) -> Option<Arc<dyn Painter>> {
        match &self.shows {
            Shows::Animated(shown) => {
                let painter: Arc<dyn Painter> = shown.clone();
                Some(painter)
            }
            Shows::Still(_) => None,
        }
    }

    fn still(&self) -> Option<Arc<Still>> {
        match &self.shows {
            Shows::Still(still) => Some(Arc::clone(still)),
            Shows::Animated(_) => None,
        }
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        match changed_non_live(&self.descriptor, &self.settings, settings) {
            Some(param) => Err(SourceError::NotLive {
                param: param.to_owned(),
            }),
            None => {
                self.settings = settings.clone();
                Ok(())
            }
        }
    }
}

/// An image's frames and their timing. A still image is one frame.
#[derive(Debug)]
pub(crate) struct Animation {
    frames: Vec<Arc<VideoFrame>>,
    /// When each frame starts within one play, in milliseconds; the first is 0.
    starts: Vec<u64>,
    /// One play's length in milliseconds.
    play_ms: u64,
    /// How many plays; `None` is forever. After the last, the last frame stays.
    plays: Option<u32>,
}

impl Animation {
    fn new(decoded: decode::Decoded) -> Result<Animation, SourceError> {
        let size = FrameSize::new(decoded.width, decoded.height)?;
        let mut frames = Vec::with_capacity(decoded.frames.len());
        let mut starts = Vec::with_capacity(decoded.frames.len());
        let mut play_ms = 0u64;
        for (rgba, delay_ms) in decoded.frames {
            starts.push(play_ms);
            frames.push(Arc::new(rgba_frame(size, rgba, at_millis(play_ms))?));
            play_ms += u64::from(delay_ms);
        }
        Ok(Animation {
            frames,
            starts,
            play_ms,
            plays: decoded.plays,
        })
    }

    /// What an image source with no file shows: one transparent pixel.
    fn empty() -> Result<Animation, SourceError> {
        Animation::new(decode::Decoded {
            width: 1,
            height: 1,
            frames: vec![(vec![0; 4], 0)],
            plays: Some(1),
        })
    }

    /// The index of the frame shown `t` ms after the start.
    fn index(&self, t: u64) -> usize {
        let last = self.frames.len() - 1;
        if last == 0 || self.play_ms == 0 || self.finished(t) {
            return last;
        }
        let pos = t % self.play_ms;
        self.starts.partition_point(|&s| s <= pos) - 1
    }

    fn finished(&self, t: u64) -> bool {
        self.plays
            .is_some_and(|plays| t >= self.play_ms.saturating_mul(u64::from(plays)))
    }

    /// Frame `index`, or the last one past the end.
    fn frame(&self, index: usize) -> Arc<VideoFrame> {
        let last = self.frames.len() - 1;
        Arc::clone(&self.frames[index.min(last)])
    }

    /// The frame a still shows, its last as [`Self::index`] picks, without the rest.
    fn into_last(mut self) -> VideoFrame {
        let last = self
            .frames
            .pop()
            .expect("invariant: an animation has a frame");
        Arc::try_unwrap(last).unwrap_or_else(|shared| (*shared).clone())
    }

    /// Whether it has more than one frame to show.
    fn animated(&self) -> bool {
        self.frames.len() > 1 && self.play_ms > 0
    }

    /// When, in ms after the start, the frame shown at `t` is next replaced.
    fn next_change(&self, t: u64) -> Option<u64> {
        let last = self.frames.len() - 1;
        if last == 0 || self.play_ms == 0 || self.finished(t) {
            return None;
        }
        let index = self.index(t);
        let play = t / self.play_ms;
        if index == last && self.plays.is_some_and(|plays| play + 1 >= u64::from(plays)) {
            // The last frame of the last play stays.
            return None;
        }
        let end = self.starts.get(index + 1).copied().unwrap_or(self.play_ms);
        Some(play * self.play_ms + end)
    }
}

/// One still image: straight sRGB RGBA8, row by row with no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StillImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// Decodes `bytes`, read from the asset at `path` (named in errors), as a PNG, JPEG,
/// WebP or GIF file within `limits`, keeping its first frame: the image a filter reads
/// (a mask, a LUT laid out as tiles), which never animates.
///
/// # Errors
/// As the `image` kind's decoding: [`SourceError::UnsupportedFormat`],
/// [`SourceError::Decode`] and [`SourceError::TooLarge`].
pub fn decode_still(
    bytes: &[u8],
    path: &str,
    limits: &crate::Limits,
) -> Result<StillImage, SourceError> {
    let decoded = decode::decode(bytes, path, limits)?;
    let (rgba, _) = decoded
        .frames
        .into_iter()
        .next()
        .ok_or_else(|| SourceError::Decode {
            path: path.to_owned(),
            format: "an image",
            reason: "it has no frames".into(),
        })?;
    Ok(StillImage {
        width: decoded.width,
        height: decoded.height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::Animation;
    use super::decode::Decoded;

    /// Three 1x1 frames of 100, 200 and 300 ms.
    fn three(plays: Option<u32>) -> Animation {
        let frames = [100, 200, 300].map(|ms| (vec![0; 4], ms)).into();
        Animation::new(Decoded {
            width: 1,
            height: 1,
            frames,
            plays,
        })
        .expect("an animation")
    }

    #[test]
    fn frames_follow_their_delays_and_loop() {
        let a = three(None);
        let at = |t| a.index(t);
        assert_eq!(
            [at(0), at(99), at(100), at(299), at(300), at(599)],
            [0, 0, 1, 1, 2, 2]
        );
        assert_eq!([at(600), at(700), at(6000 + 350)], [0, 1, 2]);
        assert_eq!(a.next_change(0), Some(100));
        assert_eq!(a.next_change(350), Some(600));
        assert_eq!(a.next_change(650), Some(700));
    }

    #[test]
    fn a_finite_animation_stops_on_its_last_frame() {
        let a = three(Some(2));
        assert_eq!(a.index(700), 1);
        assert_eq!(a.next_change(650), Some(700));
        assert_eq!(a.next_change(500), Some(600), "the first play ends");
        assert_eq!(a.index(1199), 2);
        assert_eq!(a.next_change(1000), None, "the last frame of the last play");
        assert_eq!(a.index(1200), 2);
        assert_eq!(a.index(u64::MAX), 2);
        assert_eq!(a.next_change(5000), None);
    }

    #[test]
    fn a_still_image_never_changes() {
        let a = Animation::empty().expect("a frame");
        assert_eq!(a.index(12345), 0);
        assert_eq!(a.next_change(0), None);
    }
}
