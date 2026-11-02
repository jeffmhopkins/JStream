//! Where the output thread sends the encoded packets: FLV tags to a live publish
//! target, or fragmented MP4 to a recording file.
//!
//! A recording is opened by `launch`, on the thread running `output.start`, so a path
//! that can't be written (a missing directory, a file already there) fails the start
//! with its reason. Until the start succeeds the recording is provisional: if the start
//! fails after the file was opened, the `.part` file is removed rather than left behind
//! to block the next start. Once it has started, the `.part` file is kept whatever
//! happens, as `Recorder` documents, and a stop renames it to the destination.

use std::borrow::Cow;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use jstream_media::{Codec, FrameSize, MediaTime, Packet};
use jstream_mux::MuxError;
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
    /// The video codec. H.264 goes in legacy FLV tags and MP4; HEVC and AV1 go in
    /// enhanced RTMP tags, and can't be recorded yet. Either way the muxer takes the
    /// decoder configuration from the parameter sets in the first keyframe.
    pub(crate) codec: Codec,
    pub(crate) size: FrameSize,
    pub(crate) origin: MediaTime,
    pub(crate) frame_rate: f64,
    pub(crate) video_kbps: u32,
    pub(crate) audio_kbps: u32,
    pub(crate) sample_rate: u32,
}

impl StreamInfo {
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
        /// The muxer took an H.264 keyframe, so it has its decoder configuration.
        avc_known: bool,
    },
    Record(RecordSink),
}

impl Sink {
    /// Builds the muxer for `destination`. A recording opens its `.part` file here and
    /// the muxer writes the init segment with the first keyframe. It stays provisional
    /// until `keep` is set.
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
                        // From the first keyframe, so the sequence header is the
                        // encoder's own (#746).
                        avc: None,
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
                Ok(Self::Flv {
                    tagger,
                    publish,
                    avc_known: false,
                })
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
                        // From the first keyframe: the init segment waits for it (#746).
                        avc: None,
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
                    avc_known: false,
                    keep,
                }))
            }
        }
    }

    /// Sends what goes before the first packet: FLV's header tags. A recording writes
    /// its init segment with the first keyframe.
    pub(crate) fn start(&mut self) -> Result<(), String> {
        match self {
            Self::Flv {
                tagger, publish, ..
            } => match tagger.start() {
                Ok(tags) => publish_tags(publish, &tags),
                Err(err) => Err(format!("FLV start: {err}")),
            },
            Self::Record(_) => Ok(()),
        }
    }

    /// Muxes one packet and sends or writes it. A packet the muxer rejects is dropped
    /// and counted in `output_drops`, and the call returns `false`; `true` means the
    /// muxer took it. An error means the destination is gone and ends the output.
    pub(crate) fn forward(
        &mut self,
        packet: &Packet,
        output_drops: &AtomicU64,
    ) -> Result<bool, String> {
        let rejected = |err: &dyn std::fmt::Display| {
            output_drops.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(error = %err, "dropping a packet the muxer rejected");
        };
        let avc_known = match self {
            Self::Flv { avc_known, .. } => avc_known,
            Self::Record(sink) => &mut sink.avc_known,
        };
        // Only until the muxer has a decoder configuration: a later keyframe's stand-in
        // SPS would replace the encoder's own, which shares its id.
        let packet = if *avc_known {
            Cow::Borrowed(packet)
        } else {
            with_parameter_sets(packet)
        };
        let configures = !*avc_known && packet.codec() == Codec::H264 && packet.is_keyframe();
        let taken = match self {
            Self::Flv {
                tagger, publish, ..
            } => match tagger.push(&packet) {
                Ok(tags) => {
                    publish_tags(publish, &tags)?;
                    true
                }
                Err(err) => {
                    rejected(&err);
                    false
                }
            },
            Self::Record(sink) => match sink.write(&packet) {
                Ok(()) => true,
                Err(err @ RecordError::Packet(_)) => {
                    rejected(&err);
                    false
                }
                Err(err) => return Err(format!("recording failed: {err}")),
            },
        };
        if configures && taken {
            match self {
                Self::Flv { avc_known, .. } => *avc_known = true,
                Self::Record(sink) => sink.avc_known = true,
            }
        }
        Ok(taken)
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

/// Stand-in H.264 parameter sets (Baseline 3.0, no frame size) for a first keyframe
/// that carries none in-band. Only the fake encoder sends such keyframes; every real one
/// sends its SPS and PPS with its first keyframe, and those go in the decoder
/// configuration.
const STAND_IN_SPS: [u8; 5] = [0x67, 0x42, 0xC0, 0x1E, 0xDA];
const STAND_IN_PPS: [u8; 4] = [0x68, 0xCE, 0x3C, 0x80];

/// `packet`, or for an H.264 keyframe with no SPS or PPS in-band, a copy with the
/// stand-in sets in front, so the muxers always find a decoder configuration in the
/// first keyframe. Only for keyframes before the muxer has one (`avc_known`).
fn with_parameter_sets(packet: &Packet) -> Cow<'_, Packet> {
    if packet.codec() != Codec::H264
        || !packet.is_keyframe()
        || !matches!(
            AvcDecoderConfig::from_annexb(packet.data()),
            Err(MuxError::MissingParameterSets { .. })
        )
    {
        return Cow::Borrowed(packet);
    }
    let mut data = Vec::with_capacity(packet.data().len() + 16);
    for set in [&STAND_IN_SPS[..], &STAND_IN_PPS[..]] {
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(set);
    }
    data.extend_from_slice(packet.data());
    // Built from a packet that was valid, so this can't fail; if it did, the muxer
    // would reject the packet as it is.
    Packet::new(packet.codec(), true, packet.pts(), packet.dts(), data)
        .map_or(Cow::Borrowed(packet), Cow::Owned)
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
    /// The muxer took an H.264 keyframe, so it has its decoder configuration.
    avc_known: bool,
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

    use std::fs;

    use jstream_mux::h264::AvcDecoderConfig;

    use super::{Destination, STAND_IN_PPS, STAND_IN_SPS, Sink, StreamInfo};
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

    /// A High-profile SPS (1280x720) and its PPS, as `jstream-mux`'s tests use.
    const SPS: [u8; 25] = [
        0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01, 0x10, 0x00, 0x00, 0x03,
        0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x83, 0x19,
    ];
    const PPS: [u8; 6] = [0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];

    /// An H.264 IDR access unit, with the SPS and PPS in-band as a real encoder sends
    /// them, or bare as the fake encoder does.
    fn h264_keyframe(in_band: bool) -> Packet {
        let mut au = Vec::new();
        let sets = [&SPS[..], &PPS[..]];
        for nalu in sets
            .iter()
            .filter(|_| in_band)
            .chain([&&[0x65, 0x88, 0x84][..]])
        {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(nalu);
        }
        let pts = MediaTime::zero(Timebase::MPEG_90K);
        Packet::new(Codec::H264, true, pts, pts, au).expect("packet")
    }

    /// The AVC sequence header the FLV sink publishes for `keyframe`: the
    /// `AVCDecoderConfigurationRecord` after the 5-byte video tag header.
    fn flv_avc_record(keyframe: &Packet) -> Vec<u8> {
        let publish = RecordingPublish::new();
        let mut sink = Sink::open(
            Destination::Publish(Box::new(publish.clone())),
            &info(Codec::H264),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("open");
        sink.start().expect("start");
        let drops = AtomicU64::new(0);
        sink.forward(keyframe, &drops).expect("forward");
        assert_eq!(drops.into_inner(), 0, "the muxer took the keyframe");
        let video: Vec<_> = publish
            .tags()
            .into_iter()
            .filter(|t| t.tag_type == TagType::Video)
            .collect();
        assert_eq!(video.len(), 2, "the sequence header, then the frame");
        assert_eq!(video[0].body[..2], [0x17, 0], "an AVC sequence header");
        video[0].body[5..].to_vec()
    }

    /// The `avcC` box's payload in an MP4 file.
    fn avcc_payload(file: &[u8]) -> &[u8] {
        let at = file
            .windows(4)
            .position(|w| w == b"avcC")
            .expect("an avcC box");
        let size = u32::from_be_bytes(file[at - 4..at].try_into().unwrap()) as usize;
        &file[at + 4..at - 4 + size]
    }

    /// #746: the decoder configuration a real encoder's stream goes out with is the
    /// one in its first keyframe, in the RTMP sequence header and in the recording's
    /// `avcC`, not a stand-in Baseline 3.0 set that Windows and YouTube reject.
    #[test]
    fn h264_decoder_configuration_comes_from_the_first_keyframe() {
        let own = AvcDecoderConfig::new(vec![SPS.to_vec()], vec![PPS.to_vec()])
            .expect("config")
            .to_bytes();
        assert_eq!(flv_avc_record(&h264_keyframe(true)), own, "RTMP");

        let dir = std::env::temp_dir().join(format!("jstream-sink-avcc-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("show.mp4");
        let _ = fs::remove_file(&path);
        let mut sink = Sink::open(
            Destination::Record(path.clone()),
            &info(Codec::H264),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("open");
        let drops = AtomicU64::new(0);
        sink.forward(&h264_keyframe(true), &drops).expect("forward");
        sink.finish();
        let file = fs::read(&path).expect("recording");
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(drops.into_inner(), 0);
        assert_eq!(avcc_payload(&file), own, "MP4");
    }

    /// The fake encoder's keyframes carry no parameter sets, so the stand-in ones fill
    /// in for them.
    #[test]
    fn a_keyframe_without_parameter_sets_gets_the_stand_in_ones() {
        let stand_in =
            AvcDecoderConfig::new(vec![STAND_IN_SPS.to_vec()], vec![STAND_IN_PPS.to_vec()])
                .expect("config")
                .to_bytes();
        assert_eq!(flv_avc_record(&h264_keyframe(false)), stand_in);
    }

    /// Once the encoder's own sets configured the muxer, a later keyframe without
    /// in-band sets goes out as it is: a stand-in SPS there would replace the encoder's,
    /// which shares its id.
    #[test]
    fn a_later_bare_keyframe_gets_no_stand_in_sets() {
        let publish = RecordingPublish::new();
        let mut sink = Sink::open(
            Destination::Publish(Box::new(publish.clone())),
            &info(Codec::H264),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("open");
        sink.start().expect("start");
        let drops = AtomicU64::new(0);
        sink.forward(&h264_keyframe(true), &drops).expect("first");
        let bare = h264_keyframe(false);
        let later = MediaTime::new(3_000, Timebase::MPEG_90K);
        let later =
            Packet::new(Codec::H264, true, later, later, bare.data().to_vec()).expect("packet");
        sink.forward(&later, &drops).expect("later");
        assert_eq!(drops.into_inner(), 0);
        let last = publish.tags().pop().expect("a tag");
        // The frame tag: 5-byte header, then one length-prefixed IDR slice.
        assert_eq!(last.body[..2], [0x17, 1]);
        assert_eq!(last.body[5..], [0, 0, 0, 3, 0x65, 0x88, 0x84]);
    }

    /// The same for a recording: the later bare keyframe reaches the file as it is.
    #[test]
    fn a_later_bare_keyframe_is_recorded_without_stand_in_sets() {
        let dir = std::env::temp_dir().join(format!("jstream-sink-bare-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("show.mp4");
        let _ = fs::remove_file(&path);
        let mut sink = Sink::open(
            Destination::Record(path.clone()),
            &info(Codec::H264),
            Arc::new(AtomicBool::new(true)),
        )
        .expect("open");
        let drops = AtomicU64::new(0);
        sink.forward(&h264_keyframe(true), &drops).expect("first");
        let later = MediaTime::new(3_000, Timebase::MPEG_90K);
        let bare = h264_keyframe(false).data().to_vec();
        let later = Packet::new(Codec::H264, true, later, later, bare).expect("packet");
        sink.forward(&later, &drops).expect("later");
        sink.finish();
        let file = fs::read(&path).expect("recording");
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(drops.into_inner(), 0);
        assert!(
            !file.windows(STAND_IN_SPS.len()).any(|w| w == STAND_IN_SPS),
            "no stand-in SPS anywhere in the file"
        );
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
