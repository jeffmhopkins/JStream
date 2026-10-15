//! The `media` source: a local video file, demuxed and decoded by the platform
//! (ADR-0031: OS media APIs, the Media Foundation Source Reader on Windows), played on
//! the media clock.
//!
//! A decode thread reads frames ahead ([`player`]); the source maps its clock time onto
//! the file's timeline, by `speed` and from where it last started, and shows the latest
//! frame the timeline has reached. Audio comes later, through the engine's mixer.

mod player;

use crate::color::rgba_frame;
use crate::describe::{asset, kind, param};
use crate::source::{self, changed_non_live, setting};
use crate::{Context, Limits, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame};
use jstream_platform::{HardwarePreference, MediaDecoderConfig, PlatformError};
use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use player::{Player, micros};
use serde_json::{Value, json};
use std::sync::Arc;

/// The slowest and fastest `speed`.
const MIN_SPEED: f64 = 0.25;
const MAX_SPEED: f64 = 4.0;
/// Frames the decode thread keeps ahead at most, when the memory limit allows.
const QUEUE: usize = 8;
/// How far the frame shown may fall behind the timeline before the source seeks
/// instead of waiting for the decoder: after the source was hidden for a while, or a
/// decoder that can't keep up. In microseconds.
const CATCH_UP_US: i64 = 1_000_000;
/// How soon to look again when the decoder hasn't caught up with the timeline.
const RETRY_US: i64 = 10_000;

/// The `media` kind.
pub(crate) struct MediaKind(KindDescriptor);

impl MediaKind {
    pub(crate) fn new() -> Self {
        let live = |p: ParamDescriptor| ParamDescriptor { live: true, ..p };
        Self(KindDescriptor {
            params: vec![
                ParamDescriptor {
                    description: Some(
                        "A video file the platform can play. Its audio isn't played yet.".into(),
                    ),
                    ..param(
                        "file",
                        "Media file",
                        asset(&["mp4", "mkv", "mov", "webm", "mp3", "wav", "flac", "ogg"]),
                        Value::Null,
                    )
                },
                live(param("loop", "Loop", ParamType::Bool, json!(true))),
                live(ParamDescriptor {
                    description: Some("1 is normal speed.".into()),
                    ..param(
                        "speed",
                        "Speed",
                        ParamType::Float {
                            min: Some(MIN_SPEED),
                            max: Some(MAX_SPEED),
                            step: Some(0.05),
                            unit: None,
                        },
                        json!(1.0),
                    )
                }),
                live(ParamDescriptor {
                    description: Some(
                        "Plays from the start each time the program shows it again.".into(),
                    ),
                    ..param(
                        "restart_on_show",
                        "Restart when shown",
                        ParamType::Bool,
                        json!(true),
                    )
                }),
            ],
            ..kind("media", "Media file", true, true)
        })
    }
}

impl SourceKind for MediaKind {
    fn descriptor(&self) -> &KindDescriptor {
        &self.0
    }

    /// Opens the file and decodes its first frame, so this blocks on I/O and decoding,
    /// then starts the decode thread.
    fn create(&self, settings: &Settings, ctx: &Context) -> Result<Box<dyn Source>, SourceError> {
        let play = Play::from(settings)?;
        let (current, player) = match source::asset(settings, "file")? {
            Some((root, path)) => {
                let media = ctx.media.as_ref().ok_or(SourceError::NoBackend {
                    backend: "media file",
                })?;
                let (hold, host) = ctx.assets.open(root, &path)?;
                let config = MediaDecoderConfig {
                    video_format: PixelFormat::Rgba8,
                    hardware: HardwarePreference::Prefer,
                };
                let decoder = media
                    .open(&host, &config)
                    .map_err(|e| platform_error(&path, e))?;
                // The decoder has the file open now.
                drop(hold);
                let (first, at, decoder) = first_frame(decoder, &path, &ctx.limits)?;
                let capacity = queue_capacity(first.info().size, &path, &ctx.limits)?;
                let size = first.info().size;
                let player =
                    Player::start(decoder, at, size, capacity, play.looping).map_err(|e| {
                        SourceError::Decode {
                            path: path.clone(),
                            format: FORMAT,
                            reason: format!("the decode thread could not start: {e}"),
                        }
                    })?;
                (Arc::new(first), Some(player))
            }
            // No file: one transparent pixel, as the image kind shows.
            None => {
                let size = FrameSize::new(1, 1)?;
                let frame = rgba_frame(size, vec![0; 4], MediaTime::zero(Timebase::MICROS))?;
                (Arc::new(frame), None)
            }
        };
        Ok(Box::new(MediaSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            play,
            player,
            current,
            current_at: 0,
            clock: Anchor::default(),
            last: 0,
        }))
    }
}

/// What the error names the file as.
const FORMAT: &str = "a media file";

fn platform_error(path: &str, e: PlatformError) -> SourceError {
    match e {
        PlatformError::NotFound { .. } => SourceError::AssetRead {
            path: path.to_owned(),
            kind: std::io::ErrorKind::NotFound,
        },
        e => SourceError::Decode {
            path: path.to_owned(),
            format: FORMAT,
            reason: e.to_string(),
        },
    }
}

/// The file's first frame and its time, checked against the limits.
fn first_frame(
    mut decoder: Box<dyn jstream_platform::MediaDecoder>,
    path: &str,
    limits: &Limits,
) -> Result<(VideoFrame, i64, Box<dyn jstream_platform::MediaDecoder>), SourceError> {
    let no_video = || SourceError::Decode {
        path: path.to_owned(),
        format: FORMAT,
        reason: "it has no video to show (audio-only files play once the audio path is \
                 built)"
            .into(),
    };
    let too_large = |size: FrameSize| {
        let (w, h) = (size.width(), size.height());
        (w > limits.max_side || h > limits.max_side).then(|| SourceError::TooLarge {
            path: path.to_owned(),
            what: format!("{w}x{h} is over {} px a side", limits.max_side),
        })
    };
    let video = decoder.info().video.ok_or_else(no_video)?;
    // Checked before decoding anything, and again on the frame itself: every later
    // frame must have its size (`Player`).
    if let Some(err) = too_large(video.size) {
        return Err(err);
    }
    let frame = decoder
        .next_video()
        .map_err(|e| platform_error(path, e))?
        .ok_or_else(no_video)?;
    if let Some(err) = too_large(frame.info().size) {
        return Err(err);
    }
    if frame.info().format != PixelFormat::Rgba8 {
        return Err(SourceError::Decode {
            path: path.to_owned(),
            format: FORMAT,
            reason: format!("the decoder gave {:?} frames", frame.info().format),
        });
    }
    let at = micros(frame.info().pts).unwrap_or(0);
    Ok((frame, at, decoder))
}

/// How many frames to decode ahead: [`QUEUE`], or fewer when the frames queued, the one
/// shown and the two the decoder and the program mix may hold would pass
/// `max_decoded_bytes`.
fn queue_capacity(size: FrameSize, path: &str, limits: &Limits) -> Result<usize, SourceError> {
    let frame = u64::from(size.width()) * u64::from(size.height()) * 4;
    let fits = limits.max_decoded_bytes / frame.max(1);
    let capacity = usize::try_from(fits.saturating_sub(3))
        .unwrap_or(QUEUE)
        .min(QUEUE);
    if capacity == 0 {
        return Err(SourceError::TooLarge {
            path: path.to_owned(),
            what: format!(
                "decoding {}x{} frames takes over {} bytes",
                size.width(),
                size.height(),
                limits.max_decoded_bytes
            ),
        });
    }
    Ok(capacity)
}

/// The live settings that shape playback.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Play {
    looping: bool,
    speed: f64,
    restart_on_show: bool,
}

impl Play {
    fn from(settings: &Settings) -> Result<Self, SourceError> {
        let flag = |id: &str| {
            setting(settings, id)?
                .as_bool()
                .ok_or_else(|| bad(id, "must be true or false"))
        };
        let speed = setting(settings, "speed")?
            .as_f64()
            .filter(|s| (MIN_SPEED..=MAX_SPEED).contains(s))
            .ok_or_else(|| {
                bad(
                    "speed",
                    &format!("must be a number from {MIN_SPEED} to {MAX_SPEED}"),
                )
            })?;
        Ok(Self {
            looping: flag("loop")?,
            speed,
            restart_on_show: flag("restart_on_show")?,
        })
    }
}

fn bad(param: &str, reason: &str) -> SourceError {
    SourceError::Setting {
        param: param.to_owned(),
        reason: reason.to_owned(),
    }
}

/// Where the timeline was at a clock time: the timeline runs on from there at `speed`.
/// Both in microseconds; the clock is the source's, from when it started.
#[derive(Clone, Copy, Debug, Default)]
struct Anchor {
    clock: i64,
    timeline: i64,
}

/// A running `media` source.
struct MediaSource {
    descriptor: KindDescriptor,
    settings: Settings,
    play: Play,
    /// `None` with no file.
    player: Option<Player>,
    /// The frame shown and its timeline position.
    current: Arc<VideoFrame>,
    current_at: i64,
    clock: Anchor,
    /// The latest clock time asked about, so a speed change keeps the position.
    last: i64,
}

impl MediaSource {
    /// The timeline position at clock time `at`, in microseconds.
    fn position(&self, at: i64) -> i64 {
        let elapsed = (at - self.clock.clock).max(0) as f64 * self.play.speed;
        self.clock.timeline.saturating_add(elapsed as i64)
    }

    /// The first clock time at which the timeline reaches `to`.
    fn clock_at(&self, to: i64) -> i64 {
        let ahead = (to - self.clock.timeline).max(0) as f64 / self.play.speed;
        self.clock.clock.saturating_add(ahead.ceil() as i64)
    }

    /// Starts the timeline again from `timeline` at clock time `at`.
    fn restart(&mut self, at: i64, timeline: i64) {
        self.clock = Anchor {
            clock: at,
            timeline,
        };
        self.current_at = timeline;
        if let Some(player) = &self.player {
            player.seek(timeline);
        }
    }
}

impl Source for MediaSource {
    fn frame(&mut self, at: MediaTime) -> Result<Arc<VideoFrame>, SourceError> {
        let at = micros(at).unwrap_or(0).max(0);
        self.last = at;
        let position = self.position(at);
        let Some(player) = &self.player else {
            return Ok(Arc::clone(&self.current));
        };
        let mut state = player.state();
        if let Some(reason) = &state.failure {
            return Err(SourceError::Decode {
                path: self.file_path(),
                format: FORMAT,
                reason: reason.clone(),
            });
        }
        let mut taken = false;
        while let Some(next) = state.queue.front().filter(|d| d.at <= position) {
            self.current_at = next.at;
            if let Some(next) = state.queue.pop_front() {
                self.current = next.frame;
            }
            taken = true;
        }
        let behind = state.queue.is_empty()
            && !state.ended
            && !state.seeking
            && position - self.current_at > CATCH_UP_US;
        drop(state);
        if taken {
            player.taken();
        }
        if behind {
            player.seek(position);
        }
        Ok(Arc::clone(&self.current))
    }

    fn next_change(&self, at: MediaTime) -> Option<MediaTime> {
        let player = self.player.as_ref()?;
        let at = micros(at).unwrap_or(0).max(0);
        let state = player.state();
        let next = match state.queue.front() {
            Some(next) => self.clock_at(next.at).max(at + 1),
            None if state.ended => return None,
            None => at + RETRY_US,
        };
        Some(MediaTime::new(next, Timebase::MICROS))
    }

    fn show(&mut self, at: MediaTime) {
        if self.play.restart_on_show && self.player.is_some() {
            self.restart(micros(at).unwrap_or(0).max(0), 0);
        }
    }

    fn update(&mut self, settings: &Settings) -> Result<(), SourceError> {
        if let Some(param) = changed_non_live(&self.descriptor, &self.settings, settings) {
            return Err(SourceError::NotLive {
                param: param.to_owned(),
            });
        }
        let play = Play::from(settings)?;
        if play.speed != self.play.speed {
            // Keep the position: the new speed applies from the latest time asked about.
            let timeline = self.position(self.last);
            self.clock = Anchor {
                clock: self.last,
                timeline,
            };
        }
        if let Some(player) = &self.player {
            player.set_looping(play.looping, self.position(self.last));
        }
        self.play = play;
        self.settings = settings.clone();
        Ok(())
    }
}

impl MediaSource {
    /// The `file` setting's path, for errors.
    fn file_path(&self) -> String {
        source::asset(&self.settings, "file")
            .ok()
            .flatten()
            .map(|(_, path)| path)
            .unwrap_or_default()
    }
}
