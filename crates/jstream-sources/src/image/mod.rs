//! The `image` source: a PNG, JPEG, WebP or GIF file, animated when the file is.

mod decode;

use crate::color::rgba_frame;
use crate::describe::{asset, kind, param};
use crate::source::{self, at_millis, changed_non_live, millis};
use crate::{Context, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, VideoFrame};
use jstream_protocol::{KindDescriptor, Settings};
use serde_json::Value;
use std::sync::Arc;

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
    /// blocks on I/O and decoding.
    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let animation = match source::asset(settings, "file")? {
            Some((root, path)) => {
                let bytes = ctx.assets.read(root, &path, ctx.limits.max_file_bytes)?;
                Animation::new(decode::decode(&bytes, &path, &ctx.limits)?)?
            }
            None => Animation::empty()?,
        };
        Ok(Box::new(ImageSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            animation,
        }))
    }
}

/// A running `image` source.
struct ImageSource {
    descriptor: KindDescriptor,
    settings: Settings,
    animation: Animation,
}

impl Source for ImageSource {
    fn frame(&mut self, at: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        Ok(self.animation.frame_at(millis(at)))
    }

    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        self.animation.next_change(millis(at)).map(at_millis)
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

    fn frame_at(&self, t: u64) -> Arc<VideoFrame> {
        Arc::clone(&self.frames[self.index(t)])
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
