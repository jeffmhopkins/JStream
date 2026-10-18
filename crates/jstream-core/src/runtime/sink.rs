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

use jstream_media::{Codec, FrameSize, MediaTime, Packet};
use jstream_mux::aac::{AOT_AAC_LC, AudioSpecificConfig};
use jstream_mux::flv::{self, ExVideoConfig, FlvConfig, FlvTag, FlvTagger, MetadataStyle};
use jstream_mux::fmp4::{self, Fmp4Config};
use jstream_mux::h264::AvcDecoderConfig;
use jstream_outputs::record::{RecordError, Recorder, temp_path_for};

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
    /// The video codec. H.264 goes in legacy FLV tags and MP4 with the parameter sets
    /// below; HEVC and AV1 go in enhanced RTMP tags, whose decoder configuration the
    /// tagger takes from the first keyframe, and can't be recorded yet.
    pub(crate) codec: Codec,
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
                let avc = match info.codec {
                    Codec::H264 => Some(info.avc()?),
                    _ => None,
                };
                let config = FlvConfig {
                    video: Some(flv::VideoTrack {
                        width: info.size.width(),
                        height: info.size.height(),
                        frame_rate: Some(info.frame_rate),
                        bitrate_kbps: Some(info.video_kbps),
                        avc,
                    }),
                    audio: Some(flv::AudioTrack {
                        config: info.aac()?,
                        bitrate_kbps: Some(info.audio_kbps),
                    }),
                    origin: Some(info.origin),
                    metadata_style: MetadataStyle::File,
                    encoder: Some("jstream".into()),
                };
                let tagger = match info.codec {
                    Codec::H264 => FlvTagger::new(config),
                    Codec::Hevc => {
                        FlvTagger::with_enhanced_video(config, ExVideoConfig::Hevc(None))
                    }
                    Codec::Av1 => FlvTagger::with_enhanced_video(config, ExVideoConfig::Av1(None)),
                    other => return Err(format!("FLV can't carry {other:?} video")),
                }
                .map_err(|err| format!("FLV tagger: {err}"))?;
                Ok(Self::Flv { tagger, publish })
            }
            Destination::Record(path) => {
                if info.codec != Codec::H264 {
                    return Err(format!(
                        "recording {:?} video is not supported yet; record in H.264",
                        info.codec
                    ));
                }
                // The collection's check is the same on every platform, so `/show.mp4`
                // passes it on Windows and `C:\show.mp4` on Linux, where it would be a
                // relative name in the working directory.
                if !path.is_absolute() {
                    return Err(format!(
                        "the recording path `{}` is not absolute on this machine",
                        path.display()
                    ));
                }
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
                let recorder = Recorder::create(&path, config).map_err(|err| {
                    // A failed init-segment write leaves the `.part` file behind, which
                    // would block the next start; it holds nothing worth keeping.
                    if matches!(err, RecordError::DiskFull { .. } | RecordError::Write { .. })
                        && let Some(temp) = temp_path_for(&path)
                        && let Err(remove) = fs::remove_file(&temp)
                    {
                        tracing::warn!(error = %remove, part = %temp.display(), "removing a failed start's recording");
                    }
                    format!("opening the recording: {err}")
                })?;
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    use jstream_media::{Codec, FrameSize, MediaTime, Packet, Timebase};
    use jstream_mux::flv::TagType;

    use super::{Destination, Sink, StreamInfo};
    use crate::runtime::RecordingPublish;

    fn info(codec: Codec) -> StreamInfo {
        StreamInfo {
            codec,
            size: FrameSize::new(1280, 720).expect("size"),
            origin: MediaTime::zero(Timebase::MPEG_90K),
            frame_rate: 30.0,
            video_kbps: 3000,
            audio_kbps: 160,
            sample_rate: 48_000,
            sps: &[0x67, 0x42, 0xC0, 0x1E, 0xDA],
            pps: &[0x68, 0xCE, 0x3C, 0x80],
        }
    }

    /// An HEVC IDR access unit with its VPS, SPS and PPS in-band.
    fn hevc_keyframe() -> Vec<u8> {
        const VPS: [u8; 24] = [
            0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00,
            0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x1E, 0x91, 0x30, 0x24,
        ];
        const SPS: [u8; 41] = [
            0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x1E, 0xA0, 0x20, 0x81, 0x05, 0x96, 0x44, 0xE4, 0x93, 0x2B, 0xC0,
            0x5A, 0x02, 0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x3C, 0x10,
        ];
        const PPS: [u8; 7] = [0x44, 0x01, 0xC1, 0x72, 0xB4, 0x22, 0x40];
        let mut au = Vec::new();
        for nalu in [&VPS[..], &SPS[..], &PPS[..], &[0x28, 0x01, 0xAF, 0x08]] {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(nalu);
        }
        au
    }

    /// An AV1 keyframe temporal unit: temporal delimiter, sequence header and a frame.
    fn av1_keyframe() -> Vec<u8> {
        let mut tu = vec![0x12, 0x00];
        tu.extend_from_slice(&[
            0x0A, 0x0D, 0x20, 0x00, 0x00, 0x02, 0xAF, 0xFF, 0x9B, 0x5F, 0x24, 0x04, 0x34, 0x00,
            0x80,
        ]);
        tu.extend_from_slice(&[0x30, 0xAA, 0xBB]);
        tu
    }

    /// HEVC and AV1 go out in enhanced RTMP video tags, named by FourCC, with the
    /// decoder configuration taken from the first keyframe.
    #[test]
    fn publishes_hevc_and_av1_as_enhanced_rtmp() {
        for (codec, keyframe, fourcc) in [
            (Codec::Hevc, hevc_keyframe(), b"hvc1"),
            (Codec::Av1, av1_keyframe(), b"av01"),
        ] {
            let publish = RecordingPublish::new();
            let mut sink = Sink::open(
                Destination::Publish(Box::new(publish.clone())),
                &info(codec),
                Arc::new(AtomicBool::new(true)),
            )
            .expect("open");
            sink.start().expect("start");
            let pts = MediaTime::zero(Timebase::MPEG_90K);
            let packet = Packet::new(codec, true, pts, pts, keyframe).expect("packet");
            let drops = AtomicU64::new(0);
            sink.forward(&packet, &drops).expect("forward");
            assert_eq!(
                drops.into_inner(),
                0,
                "{codec:?}: the muxer took the keyframe"
            );
            let video: Vec<_> = publish
                .tags()
                .into_iter()
                .filter(|t| t.tag_type == TagType::Video)
                .collect();
            // The sequence start, then the frame.
            assert_eq!(video.len(), 2, "{codec:?}");
            for tag in &video {
                assert_ne!(tag.body[0] & 0x80, 0, "{codec:?}: an enhanced RTMP header");
                assert_eq!(&tag.body[1..5], fourcc, "{codec:?}");
            }
        }
    }

    /// MP4 recording is H.264 only for now, so another codec fails the start with a
    /// reason, before any file is opened.
    #[test]
    fn refuses_to_record_hevc_or_av1() {
        let path = PathBuf::from(if cfg!(windows) {
            r"C:\no-such-dir\show.mp4"
        } else {
            "/no-such-dir/show.mp4"
        });
        for codec in [Codec::Hevc, Codec::Av1] {
            let err = Sink::open(
                Destination::Record(path.clone()),
                &info(codec),
                Arc::new(AtomicBool::new(false)),
            )
            .err()
            .expect("refused");
            assert!(err.contains("not supported yet"), "{err}");
        }
    }
}
