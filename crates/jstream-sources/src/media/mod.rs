//! The `media` source: a local video file, demuxed and decoded by the platform
//! (ADR-0031: OS media APIs, the Media Foundation Source Reader on Windows), played on
//! the media clock.
//!
//! A decode thread reads frames and audio ahead ([`player`]); the source maps its clock
//! time onto the file's timeline, by `speed` and from where it last started, and shows
//! the latest frame the timeline has reached. Its audio goes to the engine's mixer
//! ([`audio`]), on the same timeline. A file with no video shows nothing and plays its
//! audio.

mod audio;
mod player;

pub use audio::MediaAudio;

use crate::color::rgba_frame;
use crate::describe::{asset, kind, param};
use crate::source::{self, changed_non_live, setting};
use crate::{Context, Limits, Source, SourceError, SourceKind};
use jstream_media::{FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame};
use jstream_platform::{HardwarePreference, MediaDecoder, MediaDecoderConfig, PlatformError};
use jstream_protocol::{KindDescriptor, ParamDescriptor, ParamType, Settings};
use player::{Player, Start, Timing, micros};
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
                        "A video or audio file the platform can play. Its sound plays while the \
                         program shows it, at speed 1."
                            .into(),
                    ),
                    ..param(
                        "file",
                        "Media file",
                        asset(&[
                            "mp4", "mkv", "mov", "webm", "mp3", "m4a", "wav", "flac", "ogg",
                        ]),
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
        let timing = Timing {
            clock: 0,
            timeline: 0,
            speed: play.speed,
        };
        let (current, player, audio) = match source::asset(settings, "file")? {
            Some((root, path)) => {
                let media = ctx.media.as_ref().ok_or(SourceError::NoBackend {
                    backend: "media file",
                })?;
                let (hold, host) = ctx.assets.open(root, &path)?;
                let config = MediaDecoderConfig {
                    video_format: PixelFormat::Rgba8,
                    hardware: HardwarePreference::Prefer,
                    audio: true,
                };
                let decoder = media
                    .open(&host, &config)
                    .map_err(|e| platform_error(&path, e))?;
                // The decoder has the file open now.
                drop(hold);
                let sound = decoder.info().audio.map(|a| a.format);
                let (current, first, decoder) = match first_frame(decoder, &path, &ctx.limits)? {
                    (Some((frame, at)), decoder) => {
                        let size = frame.info().size;
                        (Arc::new(frame), Some((at, size)), decoder)
                    }
                    // Audio only: nothing to see.
                    (None, decoder) => (blank()?, None, decoder),
                };
                let capacity = match first {
                    Some((_, size)) => queue_capacity(size, &path, &ctx.limits)?,
                    None => 0,
                };
                let player = Player::start(Start {
                    decoder,
                    first,
                    capacity,
                    looping: play.looping,
                    timing,
                })
                .map_err(|e| SourceError::Decode {
                    path: path.clone(),
                    format: FORMAT,
                    reason: format!("the decode thread could not start: {e}"),
                })?;
                let audio = sound.map(|format| {
                    MediaAudio::new(Arc::clone(player.shared()), format, first.is_some())
                });
                (current, Some((player, first.is_some())), audio)
            }
            // No file: one transparent pixel, as the image kind shows.
            None => (blank()?, None, None),
        };
        let (player, video) = match player {
            Some((player, video)) => (Some(player), video),
            None => (None, false),
        };
        Ok(Box::new(MediaSource {
            descriptor: self.0.clone(),
            settings: settings.clone(),
            play,
            player,
            video,
            audio,
            current,
            current_at: 0,
            timing,
            last: 0,
        }))
    }
}

/// One transparent pixel: what a source with nothing to see shows.
fn blank() -> Result<Arc<VideoFrame>, SourceError> {
    let size = FrameSize::new(1, 1)?;
    let frame = rgba_frame(size, vec![0; 4], MediaTime::zero(Timebase::MICROS))?;
    Ok(Arc::new(frame))
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

/// A file's first frame and its time in microseconds.
type FirstFrame = (VideoFrame, i64);

/// The file's first frame and its time, checked against the limits; no frame for a
/// file with only audio.
fn first_frame(
    mut decoder: Box<dyn MediaDecoder>,
    path: &str,
    limits: &Limits,
) -> Result<(Option<FirstFrame>, Box<dyn MediaDecoder>), SourceError> {
    let nothing = || SourceError::Decode {
        path: path.to_owned(),
        format: FORMAT,
        reason: "it has no video or audio the platform can play".into(),
    };
    let too_large = |size: FrameSize| {
        let (w, h) = (size.width(), size.height());
        (w > limits.max_side || h > limits.max_side).then(|| SourceError::TooLarge {
            path: path.to_owned(),
            what: format!("{w}x{h} is over {} px a side", limits.max_side),
        })
    };
    let Some(video) = decoder.info().video else {
        return match decoder.info().audio {
            Some(_) => Ok((None, decoder)),
            None => Err(nothing()),
        };
    };
    // Checked before decoding anything, and again on the frame itself: every later
    // frame must have its size (`Player`).
    if let Some(err) = too_large(video.size) {
        return Err(err);
    }
    let frame = decoder
        .next_video()
        .map_err(|e| platform_error(path, e))?
        .ok_or_else(nothing)?;
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
    Ok((Some((frame, at)), decoder))
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

/// A running `media` source.
struct MediaSource {
    descriptor: KindDescriptor,
    settings: Settings,
    play: Play,
    /// `None` with no file.
    player: Option<Player>,
    /// Whether the file has video.
    video: bool,
    /// The file's audio, when it has some.
    audio: Option<MediaAudio>,
    /// The frame shown and its timeline position.
    current: Arc<VideoFrame>,
    current_at: i64,
    /// How the source's clock maps onto the timeline.
    timing: Timing,
    /// The latest clock time asked about, so a speed change keeps the position.
    last: i64,
}

impl MediaSource {
    /// The timeline position at clock time `at`, in microseconds.
    fn position(&self, at: i64) -> i64 {
        self.timing.position(at)
    }

    /// Maps the clock onto the timeline from now on, for the frames and the audio.
    fn set_timing(&mut self, timing: Timing) {
        self.timing = timing;
        if let Some(player) = &self.player {
            player.set_timing(timing);
        }
    }

    /// Starts the timeline again from `timeline` at clock time `at`.
    fn restart(&mut self, at: i64, timeline: i64) {
        self.set_timing(Timing {
            clock: at,
            timeline,
            speed: self.play.speed,
        });
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
        // With no video, the audio feeder catches the timeline up instead.
        let behind = self.video
            && state.queue.is_empty()
            && !state.ended
            && !state.seeking
            && !state.video_over
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
        let player = self.player.as_ref().filter(|_| self.video)?;
        let at = micros(at).unwrap_or(0).max(0);
        let state = player.state();
        let next = match state.queue.front() {
            Some(next) => self.timing.clock_at(next.at).max(at + 1),
            None if state.ended || (state.video_over && !self.play.looping) => return None,
            // The picture is over but the audio plays on: a looping file's next frame
            // comes once the queued audio has played out.
            None => match state.audio.back().filter(|_| state.video_over) {
                Some(last) => self.timing.clock_at(last.end()).max(at + RETRY_US),
                None => at + RETRY_US,
            },
        };
        Some(MediaTime::new(next, Timebase::MICROS))
    }

    fn sound(&self) -> Option<MediaAudio> {
        self.audio.clone()
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
            self.set_timing(Timing {
                clock: self.last,
                timeline,
                speed: play.speed,
            });
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
