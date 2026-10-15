//! Where the output thread sends the encoded packets: FLV tags to a live publish
//! target, or fragmented MP4 to a recording file.
//!
//! A recording is opened by `launch`, on the thread running `output.start`, so a path
//! that can't be written (a missing directory, a file already there) fails the start
//! with its reason. Until the start succeeds the recording is provisional: if the start
//! fails after the file was opened, the `.part` file is removed rather than left behind
//! to block the next start. Once it has started, the `.part` file is kept whatever
//! happens, as `Recorder` documents, and a stop renames it to the destination.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use jstream_media::{FrameSize, MediaTime, Packet};
use jstream_mux::aac::{AOT_AAC_LC, AudioSpecificConfig};
use jstream_mux::flv::{self, FlvConfig, FlvTag, FlvTagger, MetadataStyle};
use jstream_mux::fmp4::{self, Fmp4Config};
use jstream_mux::h264::AvcDecoderConfig;
use jstream_outputs::record::{RecordError, Recorder};

use crate::runtime::{LinkState, LivePublish};

/// Where a start sends its output, before the pipeline knows the frame size.
pub(crate) enum Destination {
    /// A live publish target, sent FLV tags.
    Publish(Box<dyn LivePublish>),
    /// A recording to this path, in MP4.
    Record(PathBuf),
}

/// What the stream carries, which both containers describe in their headers.
pub(crate) struct StreamInfo {
    pub(crate) size: FrameSize,
    pub(crate) origin: MediaTime,
    pub(crate) frame_rate: f64,
    pub(crate) video_kbps: u32,
    pub(crate) audio_kbps: u32,
    pub(crate) sample_rate: u32,
    /// The H.264 parameter sets, for encoders that don't send them in-band.
    pub(crate) sps: &'static [u8],
    pub(crate) pps: &'static [u8],
}

impl StreamInfo {
    fn avc(&self) -> Result<AvcDecoderConfig, String> {
        AvcDecoderConfig::new(vec![self.sps.to_vec()], vec![self.pps.to_vec()])
            .map_err(|err| format!("AVC decoder config: {err}"))
    }

    fn aac(&self) -> Result<AudioSpecificConfig, String> {
        AudioSpecificConfig::new(AOT_AAC_LC, self.sample_rate, 2)
            .map_err(|err| format!("AAC config: {err}"))
    }
}

/// The output thread's end of the pipeline.
pub(crate) enum Sink {
    Flv {
        tagger: FlvTagger,
        publish: Box<dyn LivePublish>,
    },
    Record(RecordSink),
}

impl Sink {
    /// Builds the muxer for `destination`. A recording opens its `.part` file here and
    /// writes the init segment; it stays provisional until `keep` is set.
    pub(crate) fn open(
        destination: Destination,
        info: &StreamInfo,
        keep: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        match destination {
            Destination::Publish(publish) => {
                let config = FlvConfig {
                    video: Some(flv::VideoTrack {
                        width: info.size.width(),
                        height: info.size.height(),
                        frame_rate: Some(info.frame_rate),
                        bitrate_kbps: Some(info.video_kbps),
                        avc: Some(info.avc()?),
                    }),
                    audio: Some(flv::AudioTrack {
                        config: info.aac()?,
                        bitrate_kbps: Some(info.audio_kbps),
                    }),
                    origin: Some(info.origin),
                    metadata_style: MetadataStyle::File,
                    encoder: Some("jstream".into()),
                };
                let tagger = FlvTagger::new(config).map_err(|err| format!("FLV tagger: {err}"))?;
                Ok(Self::Flv { tagger, publish })
            }
            Destination::Record(path) => {
                let too_big = || {
                    format!(
                        "the frame size {}x{} is too large for MP4",
                        info.size.width(),
                        info.size.height()
                    )
                };
                let config = Fmp4Config {
                    video: Some(fmp4::VideoTrack {
                        width: u16::try_from(info.size.width()).map_err(|_| too_big())?,
                        height: u16::try_from(info.size.height()).map_err(|_| too_big())?,
                        avc: Some(info.avc()?),
                    }),
                    audio: Some(fmp4::AudioTrack {
                        config: info.aac()?,
                        bitrate_kbps: Some(info.audio_kbps),
                    }),
                    origin: Some(info.origin),
                };
                let recorder = Recorder::create(&path, config)
                    .map_err(|err| format!("opening the recording: {err}"))?;
                tracing::info!(path = %recorder.temp_path().display(), "recording");
                Ok(Self::Record(RecordSink {
                    recorder: Some(recorder),
                    keep,
                }))
            }
        }
    }

    /// Sends what goes before the first packet: FLV's header tags. A recording wrote
    /// its init segment when it was opened.
    pub(crate) fn start(&mut self) -> Result<(), String> {
        match self {
            Self::Flv { tagger, publish } => match tagger.start() {
                Ok(tags) => publish_tags(publish, &tags),
                Err(err) => Err(format!("FLV start: {err}")),
            },
            Self::Record(_) => Ok(()),
        }
    }

    /// Muxes one packet and sends or writes it. A packet the muxer rejects is dropped
    /// and counted in `output_drops`. An error means the destination is gone and ends
    /// the output.
    pub(crate) fn forward(
        &mut self,
        packet: &Packet,
        output_drops: &AtomicU64,
    ) -> Result<(), String> {
        let rejected = |err: &dyn std::fmt::Display| {
            output_drops.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(error = %err, "dropping a packet the muxer rejected");
        };
        match self {
            Self::Flv { tagger, publish } => match tagger.push(packet) {
                Ok(tags) => publish_tags(publish, &tags),
                Err(err) => {
                    rejected(&err);
                    Ok(())
                }
            },
            Self::Record(sink) => match sink.write(packet) {
                Ok(()) => Ok(()),
                Err(err @ RecordError::Packet(_)) => {
                    rejected(&err);
                    Ok(())
                }
                Err(err) => Err(format!("recording failed: {err}")),
            },
        }
    }

    /// Reads what a live destination sent and moves a reconnect along.
    pub(crate) fn poll(&mut self) -> Result<(), String> {
        match self {
            Self::Flv { publish, .. } => publish
                .poll()
                .map_err(|err| format!("publish poll failed: {err}")),
            Self::Record(_) => Ok(()),
        }
    }

    /// The output is stopping; see [`LivePublish::stop_by`]. Writing a file has no peer
    /// to wait on, so a recording ignores it.
    pub(crate) fn stop_by(&mut self, deadline: Instant) {
        if let Self::Flv { publish, .. } = self {
            publish.stop_by(deadline);
        }
    }

    /// Whether a live link is up. A recording has none to lose.
    pub(crate) fn link_state(&self) -> LinkState {
        match self {
            Self::Flv { publish, .. } => publish.link_state(),
            Self::Record(_) => LinkState::Active,
        }
    }

    /// Tags a live destination dropped while its link was down.
    pub(crate) fn dropped(&self) -> u64 {
        match self {
            Self::Flv { publish, .. } => publish.dropped(),
            Self::Record(_) => 0,
        }
    }

    /// Ends the session, or finishes the recording and renames it into place. A
    /// failure is logged: the output is already on its way to `stopped` or `failed`.
    pub(crate) fn finish(&mut self) {
        match self {
            Self::Flv { publish, .. } => {
                if let Err(err) = publish.finish() {
                    tracing::error!(error = %err, "publish finish failed");
                }
            }
            Self::Record(sink) => sink.finish(),
        }
    }
}

fn publish_tags(publish: &mut Box<dyn LivePublish>, tags: &[FlvTag]) -> Result<(), String> {
    for tag in tags {
        publish
            .send_tag(tag)
            .map_err(|err| format!("sending an FLV tag: {err}"))?;
    }
    Ok(())
}

/// A recording, and whether its start succeeded (`keep`). Finishing or dropping one
/// whose start failed removes its `.part` file.
pub(crate) struct RecordSink {
    recorder: Option<Recorder>,
    keep: Arc<AtomicBool>,
}

impl RecordSink {
    fn write(&mut self, packet: &Packet) -> Result<(), RecordError> {
        match self.recorder.as_mut() {
            Some(recorder) => recorder.write_packet(packet),
            None => Err(RecordError::Ended),
        }
    }

    fn finish(&mut self) {
        let Some(recorder) = self.recorder.take() else {
            return;
        };
        if !self.keep.load(Ordering::Acquire) {
            discard(recorder);
            return;
        }
        let temp = recorder.temp_path().to_owned();
        match recorder.finish() {
            Ok(path) => tracing::info!(path = %path.display(), "recording finished"),
            Err(err) => tracing::error!(
                error = %err,
                part = %temp.display(),
                "finishing the recording failed; the .part file is kept"
            ),
        }
    }
}

impl Drop for RecordSink {
    /// A recording dropped without [`finish`](Self::finish) keeps its `.part` file, as
    /// a crash would, unless its start failed.
    fn drop(&mut self) {
        if let Some(recorder) = self.recorder.take()
            && !self.keep.load(Ordering::Acquire)
        {
            discard(recorder);
        }
    }
}

/// Closes a recording whose start failed and removes its `.part` file.
fn discard(recorder: Recorder) {
    let temp = recorder.temp_path().to_owned();
    drop(recorder);
    if let Err(err) = fs::remove_file(&temp) {
        tracing::warn!(error = %err, part = %temp.display(), "removing a failed start's recording");
    }
}
