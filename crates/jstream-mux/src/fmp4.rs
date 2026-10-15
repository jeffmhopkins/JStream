//! Fragmented MP4 (ISO/IEC 14496-12 with 14496-14 and 14496-15): the container recording
//! writes. [`Fmp4Muxer`] turns packets into segments with no I/O; [`Fmp4Writer`] writes
//! them to any [`std::io::Write`].
//!
//! The stream is an **init segment** (`ftyp` and a `moov` with empty sample tables and an
//! `mvex`, so every sample lives in fragments) followed by **media segments**, each a
//! `moof` and its `mdat`. With a video track, a fragment starts at every video keyframe;
//! audio-only streams cut a fragment every second. A file cut short by a crash is
//! readable up to its last complete fragment, which is why recording uses fMP4.
//!
//! Only H.264 (`avc1` with `avcC`) and AAC (`mp4a` with `esds`) are written.

use std::io::Write;

use jstream_media::{Codec, MediaTime, Packet, Rounding, Timebase};

use crate::MuxError;
use crate::aac::AudioSpecificConfig;
use crate::flv::to_ms;
use crate::h264::{AvcDecoderConfig, annexb_to_avcc};

/// The time scale of the video track: 90 kHz, as H.264 timestamps use.
pub const VIDEO_TIMESCALE: u32 = 90_000;
/// The time scale of the movie header, which only carries durations (all zero here).
const MOVIE_TIMESCALE: u32 = 1_000;
/// How much audio an audio-only fragment holds before it is cut, in seconds.
const AUDIO_ONLY_FRAGMENT_SECS: u32 = 1;
/// The duration given to a track's last sample when it is the track's only one: one
/// AAC frame, or 1/30 s of video.
const DEFAULT_AUDIO_DURATION: u32 = 1_024;
const DEFAULT_VIDEO_DURATION: u32 = VIDEO_TIMESCALE / 30;

/// `sample_flags` of a sync sample: `sample_depends_on` = 2 (depends on no other).
const FLAGS_SYNC: u32 = 0x0200_0000;
/// `sample_flags` of a non-sync sample: `sample_depends_on` = 1 and
/// `sample_is_non_sync_sample`.
const FLAGS_NON_SYNC: u32 = 0x0101_0000;

/// `tfhd` flag: offsets are relative to the start of the `moof`.
const TFHD_DEFAULT_BASE_IS_MOOF: u32 = 0x02_0000;
/// `trun` flags: data offset, and per-sample duration, size, flags and composition offset.
const TRUN_DATA_OFFSET: u32 = 0x0001;
const TRUN_DURATION: u32 = 0x0100;
const TRUN_SIZE: u32 = 0x0200;
const TRUN_FLAGS: u32 = 0x0400;
const TRUN_CTS: u32 = 0x0800;

/// The unity transform matrix of `mvhd` and `tkhd`, in 16.16 and 2.30 fixed point.
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

/// The H.264 track.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoTrack {
    /// Display width in pixels, for `tkhd` and the sample entry.
    pub width: u16,
    /// Display height in pixels, for `tkhd` and the sample entry.
    pub height: u16,
    /// The decoder configuration. When `None`, it is built from the SPS and PPS of the
    /// first video packet, which must then be a keyframe carrying them in-band, and the
    /// init segment waits for that packet.
    pub avc: Option<AvcDecoderConfig>,
}

/// The AAC track. Its time scale is the config's sample rate.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrack {
    /// The decoder configuration.
    pub config: AudioSpecificConfig,
    /// Target bitrate in kbit/s, written as the `esds` average and maximum bitrate.
    pub bitrate_kbps: Option<u32>,
}

/// What an fMP4 stream contains.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fmp4Config {
    /// The video track, if any. It is track 1.
    pub video: Option<VideoTrack>,
    /// The audio track, if any. It is track 2, or track 1 without video.
    pub audio: Option<AudioTrack>,
    /// The instant written as decode time 0. `None` means zero: packet times are used as
    /// they are. Packets before the origin are rejected.
    pub origin: Option<MediaTime>,
}

/// One piece of the output, written in the order the muxer yields them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    /// The `ftyp` and `moov`. Always the first segment, and yielded once.
    Init(Vec<u8>),
    /// One `moof` and its `mdat`.
    Media {
        /// The `mfhd` sequence number, counting from 1.
        sequence: u32,
        /// The boxes.
        bytes: Vec<u8>,
    },
}

impl Segment {
    /// The segment's bytes.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Init(bytes) | Self::Media { bytes, .. } => bytes,
        }
    }
}

/// A sample whose duration is not known yet: it is the next sample's dts minus this one's.
#[derive(Debug)]
struct Sample {
    /// Decode time in track ticks from the origin.
    dts: i64,
    /// pts − dts in track ticks.
    cts: u32,
    keyframe: bool,
    /// The payload as MP4 stores it (AVCC for video).
    data: Vec<u8>,
}

/// One track's part of a fragment.
struct Traf<'a> {
    id: u32,
    /// The decode time of the first sample, in track ticks (`tfdt`).
    base: i64,
    /// Whether the samples carry composition offsets (video).
    video: bool,
    samples: Vec<SampleRef<'a>>,
}

/// A sample ready to be written.
struct SampleRef<'a> {
    duration: u32,
    cts: u32,
    keyframe: bool,
    data: &'a [u8],
}

#[derive(Debug)]
struct Track {
    id: u32,
    codec: Codec,
    timescale: u32,
    timebase: Timebase,
    origin: i64,
    /// Samples whose durations are known, in decode order.
    done: Vec<(Sample, u32)>,
    /// The newest sample, waiting for the next one to give it a duration.
    last: Option<Sample>,
    last_duration: Option<u32>,
}

impl Track {
    fn new(
        id: u32,
        codec: Codec,
        timescale: u32,
        origin: Option<MediaTime>,
    ) -> Result<Self, MuxError> {
        let timebase = Timebase::per_second(timescale)?;
        let origin = match origin {
            Some(t) => t.rescale(timebase, Rounding::Nearest)?.ticks(),
            None => 0,
        };
        Ok(Self {
            id,
            codec,
            timescale,
            timebase,
            origin,
            done: Vec::new(),
            last: None,
            last_duration: None,
        })
    }

    fn ticks(&self, t: MediaTime) -> Result<i64, MuxError> {
        Ok(t.rescale(self.timebase, Rounding::Nearest)?
            .ticks()
            .saturating_sub(self.origin))
    }

    fn is_empty(&self) -> bool {
        self.done.is_empty() && self.last.is_none()
    }

    /// The duration the newest sample gets when no successor will come.
    fn closing_duration(&self) -> u32 {
        self.last_duration.unwrap_or(match self.codec {
            Codec::H264 => DEFAULT_VIDEO_DURATION,
            _ => DEFAULT_AUDIO_DURATION,
        })
    }
}

/// Turns packets into fMP4 segments, with no I/O.
///
/// Video payloads are Annex B and are converted to AVCC; audio payloads are raw AAC.
/// Each track's timestamps are its packets' dts and pts rescaled to the track's time
/// scale (rounded to nearest) from the origin, so audio and video stay in sync without
/// an edit list. Decode times must increase within a track, because a sample's duration
/// is the gap to the next one; the newest sample of each track is therefore held until
/// the next arrives or [`Fmp4Muxer::finish`] is called.
///
/// A video keyframe closes the fragment holding the samples before it: all earlier
/// video, and the audio that decodes before the keyframe. The first video packet must be
/// a keyframe.
#[derive(Debug)]
pub struct Fmp4Muxer {
    config: Fmp4Config,
    origin_ms: i64,
    video: Option<Track>,
    audio: Option<Track>,
    init_sent: bool,
    sequence: u32,
}

impl Fmp4Muxer {
    /// A muxer for `config`.
    ///
    /// # Errors
    /// [`MuxError::Media`] when the origin can't be expressed in a track's time scale or
    /// in milliseconds.
    pub fn new(config: Fmp4Config) -> Result<Self, MuxError> {
        let origin_ms = match config.origin {
            Some(t) => to_ms(t)?,
            None => 0,
        };
        let video = match config.video {
            Some(_) => Some(Track::new(1, Codec::H264, VIDEO_TIMESCALE, config.origin)?),
            None => None,
        };
        let audio = match &config.audio {
            Some(a) => Some(Track::new(
                if video.is_some() { 2 } else { 1 },
                Codec::Aac,
                a.config.sample_rate(),
                config.origin,
            )?),
            None => None,
        };
        Ok(Self {
            config,
            origin_ms,
            video,
            audio,
            init_sent: false,
            sequence: 0,
        })
    }

    /// The configuration, including an AVC configuration taken from the stream.
    pub fn config(&self) -> &Fmp4Config {
        &self.config
    }

    /// The init segment, if every decoder configuration is known and it wasn't yielded
    /// yet. Without a configured AVC record it comes with the first video packet instead.
    ///
    /// # Errors
    /// [`MuxError::TooLong`] when a box is too large for its size field.
    pub fn start(&mut self) -> Result<Option<Segment>, MuxError> {
        if self.init_sent || self.config.video.as_ref().is_some_and(|v| v.avc.is_none()) {
            return Ok(None);
        }
        let init = init_segment(&self.config)?;
        self.init_sent = true;
        Ok(Some(Segment::Init(init)))
    }

    /// The segments one packet completes: the init segment if not yet yielded, then a
    /// media segment when the packet is a video keyframe that ends a fragment (or, for
    /// audio-only streams, when a second of audio is buffered). Often nothing.
    ///
    /// # Errors
    /// [`MuxError::NoTrack`] for a codec with no configured track,
    /// [`MuxError::NegativeTimestamp`] for a dts before the origin,
    /// [`MuxError::DtsNotIncreasing`] when a track's dts doesn't increase,
    /// [`MuxError::TimestampOutOfRange`] for a composition offset or duration beyond 32
    /// bits, [`MuxError::MissingAvcConfig`], [`MuxError::FirstVideoNotKeyframe`],
    /// [`MuxError::NotAnnexB`], [`MuxError::TooLong`] and [`MuxError::Media`]. A rejected
    /// packet leaves the muxer unchanged.
    pub fn push(&mut self, packet: &Packet) -> Result<Vec<Segment>, MuxError> {
        let codec = packet.codec();
        let track = match codec {
            Codec::H264 => self.video.as_ref(),
            Codec::Aac => self.audio.as_ref(),
            _ => None,
        }
        .ok_or(MuxError::NoTrack { codec })?;

        let dts = track.ticks(packet.dts())?;
        if dts < 0 {
            return Err(MuxError::NegativeTimestamp {
                ms: to_ms(packet.dts())?.saturating_sub(self.origin_ms),
            });
        }
        // Rounding is monotonic and dts <= pts, so this is never negative.
        let offset = track.ticks(packet.pts())? - dts;
        let cts = u32::try_from(offset).map_err(|_| MuxError::TimestampOutOfRange {
            what: "composition offset",
            ticks: offset,
        })?;
        let duration =
            match &track.last {
                Some(last) if dts <= last.dts => {
                    return Err(MuxError::DtsNotIncreasing {
                        codec,
                        previous: last.dts,
                        current: dts,
                        timescale: track.timescale,
                    });
                }
                Some(last) => Some(u32::try_from(dts - last.dts).map_err(|_| {
                    MuxError::TimestampOutOfRange {
                        what: "sample duration",
                        ticks: dts - last.dts,
                    }
                })?),
                None => None,
            };

        let mut new_avc = None;
        let data = if codec == Codec::H264 {
            let first = track.is_empty();
            if self.config.video.as_ref().is_some_and(|v| v.avc.is_none()) {
                new_avc = Some(avc_from_packet(packet)?);
            } else if first && !packet.is_keyframe() {
                return Err(MuxError::FirstVideoNotKeyframe);
            }
            annexb_to_avcc(packet.data())?
        } else {
            packet.data().to_vec()
        };
        let sample = Sample {
            dts,
            cts,
            keyframe: packet.is_keyframe(),
            data,
        };

        // Build every segment before changing any state, so a failure leaves the muxer
        // as it was.
        let init = if self.init_sent {
            None
        } else if let Some(avc) = &new_avc {
            let mut config = self.config.clone();
            if let Some(video) = &mut config.video {
                video.avc = Some(avc.clone());
            }
            Some(init_segment(&config)?)
        } else if self.config.video.as_ref().is_some_and(|v| v.avc.is_none()) {
            None
        } else {
            Some(init_segment(&self.config)?)
        };

        let cut = self.cut_for(codec, &sample, duration);
        let fragment = match &cut {
            Some(cut) => Some(self.fragment(cut, codec, duration)?),
            None => None,
        };

        // Commit.
        if let Some(avc) = new_avc
            && let Some(video) = &mut self.config.video
        {
            video.avc = Some(avc);
        }
        let mut out = Vec::new();
        if let Some(init) = init {
            self.init_sent = true;
            out.push(Segment::Init(init));
        }
        let track = match codec {
            Codec::H264 => self.video.as_mut(),
            _ => self.audio.as_mut(),
        }
        .ok_or(MuxError::NoTrack { codec })?;
        if let (Some(last), Some(d)) = (track.last.take(), duration) {
            track.done.push((last, d));
            track.last_duration = Some(d);
        }
        track.last = Some(sample);
        if let (Some(cut), Some(bytes)) = (cut, fragment) {
            self.commit_cut(&cut);
            out.push(Segment::Media {
                sequence: self.sequence,
                bytes,
            });
        }
        Ok(out)
    }

    /// The final media segment, holding every buffered sample. A track's newest sample
    /// gets the duration of the one before it (one AAC frame or 1/30 s if it is alone).
    /// Yields nothing when nothing is buffered.
    ///
    /// # Errors
    /// [`MuxError::MissingAvcConfig`] when samples are buffered but no video keyframe
    /// ever gave the init segment its AVC record, and [`MuxError::TooLong`].
    pub fn finish(&mut self) -> Result<Vec<Segment>, MuxError> {
        let buffered = [&self.video, &self.audio]
            .into_iter()
            .flatten()
            .any(|t| !t.is_empty());
        if !buffered {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        if let Some(init) = self.start()? {
            out.push(init);
        }
        if !self.init_sent {
            return Err(MuxError::MissingAvcConfig);
        }
        let tracks: Vec<Traf<'_>> = [&self.video, &self.audio]
            .into_iter()
            .flatten()
            .filter(|t| !t.is_empty())
            .map(|t| pending_samples(t, usize::MAX, Some(t.closing_duration())))
            .collect();
        let sequence = self.sequence + 1;
        let bytes = media_segment(sequence, &tracks)?;
        for track in [&mut self.video, &mut self.audio].into_iter().flatten() {
            track.done.clear();
            track.last = None;
        }
        self.sequence = sequence;
        out.push(Segment::Media { sequence, bytes });
        Ok(out)
    }

    /// Whether `sample` (arriving on `codec`'s track, completing its predecessor with
    /// `duration`) closes a fragment, and what goes in it.
    fn cut_for(&self, codec: Codec, sample: &Sample, duration: Option<u32>) -> Option<Cut> {
        match (&self.video, &self.audio) {
            (Some(video), audio) if codec == Codec::H264 => {
                // A keyframe ends the fragment of everything before it, if there is any.
                if !sample.keyframe || video.last.is_none() {
                    return None;
                }
                let audio_count = audio.as_ref().map_or(0, |a| {
                    a.done
                        .iter()
                        .take_while(|(s, _)| decodes_before(s.dts, a.timescale, sample.dts))
                        .count()
                });
                Some(Cut {
                    video: true,
                    audio_count,
                })
            }
            (None, Some(audio)) => {
                let buffered: u64 = audio
                    .done
                    .iter()
                    .map(|(_, d)| u64::from(*d))
                    .chain(duration.map(u64::from))
                    .sum();
                (buffered >= u64::from(audio.timescale) * u64::from(AUDIO_ONLY_FRAGMENT_SECS))
                    .then_some(Cut {
                        video: false,
                        audio_count: audio.done.len() + usize::from(duration.is_some()),
                    })
            }
            _ => None,
        }
    }

    /// The media segment for `cut`, which `codec`'s newest sample (completed with
    /// `duration`) belongs to.
    fn fragment(
        &self,
        cut: &Cut,
        codec: Codec,
        duration: Option<u32>,
    ) -> Result<Vec<u8>, MuxError> {
        // Only the track the packet is on has its newest sample completed by it.
        let completing = |track: &Track| if track.codec == codec { duration } else { None };
        let mut tracks = Vec::with_capacity(2);
        if cut.video
            && let Some(video) = &self.video
        {
            tracks.push(pending_samples(video, usize::MAX, completing(video)));
        }
        if cut.audio_count > 0
            && let Some(audio) = &self.audio
        {
            tracks.push(pending_samples(audio, cut.audio_count, completing(audio)));
        }
        media_segment(self.sequence + 1, &tracks)
    }

    fn commit_cut(&mut self, cut: &Cut) {
        self.sequence += 1;
        if cut.video
            && let Some(video) = &mut self.video
        {
            // The new keyframe is `last`; everything before it was written.
            video.done.clear();
        }
        if let Some(audio) = &mut self.audio {
            let n = cut.audio_count.min(audio.done.len());
            audio.done.drain(..n);
        }
    }
}

/// What a fragment takes: all completed video (when `video`), and the first
/// `audio_count` completed audio samples (the newest audio sample counts when the
/// packet being pushed completes it).
struct Cut {
    video: bool,
    audio_count: usize,
}

fn sample_ref(s: &Sample, duration: u32) -> SampleRef<'_> {
    SampleRef {
        duration,
        cts: s.cts,
        keyframe: s.keyframe,
        data: &s.data,
    }
}

/// The first `count` buffered samples of `track`, its newest one included (with
/// `last_duration`) when that is `Some` and the count allows.
fn pending_samples(track: &Track, count: usize, last_duration: Option<u32>) -> Traf<'_> {
    let mut samples: Vec<SampleRef<'_>> = track
        .done
        .iter()
        .take(count)
        .map(|(s, d)| sample_ref(s, *d))
        .collect();
    if samples.len() < count
        && let (Some(last), Some(d)) = (&track.last, last_duration)
    {
        samples.push(sample_ref(last, d));
    }
    let first = track.done.first().map(|(s, _)| s).or(track.last.as_ref());
    Traf {
        id: track.id,
        base: first.map_or(0, |s| s.dts),
        video: track.codec == Codec::H264,
        samples,
    }
}

/// Whether `dts` ticks at `timescale` come before `video_dts` at [`VIDEO_TIMESCALE`].
fn decodes_before(dts: i64, timescale: u32, video_dts: i64) -> bool {
    i128::from(dts) * i128::from(VIDEO_TIMESCALE) < i128::from(video_dts) * i128::from(timescale)
}

fn avc_from_packet(packet: &Packet) -> Result<AvcDecoderConfig, MuxError> {
    if !packet.is_keyframe() {
        return Err(MuxError::MissingAvcConfig);
    }
    match AvcDecoderConfig::from_annexb(packet.data()) {
        Err(MuxError::MissingParameterSets { .. }) => Err(MuxError::MissingAvcConfig),
        other => other,
    }
}

/// Writes an fMP4 file (or stream) to any [`Write`]: the init segment as soon as it is
/// known, a media segment per fragment, and the last fragment on [`Fmp4Writer::finish`].
#[derive(Debug)]
pub struct Fmp4Writer<W: Write> {
    out: W,
    muxer: Fmp4Muxer,
}

impl<W: Write> Fmp4Writer<W> {
    /// Writes the init segment to `out` if every decoder configuration is known.
    /// An invalid configuration is rejected before anything is written.
    ///
    /// # Errors
    /// As [`Fmp4Muxer::new`] and [`Fmp4Muxer::start`], and [`MuxError::Io`].
    pub fn new(mut out: W, config: Fmp4Config) -> Result<Self, MuxError> {
        let mut muxer = Fmp4Muxer::new(config)?;
        if let Some(init) = muxer.start()? {
            out.write_all(init.bytes())?;
        }
        Ok(Self { out, muxer })
    }

    /// Writes the segments one packet completes.
    ///
    /// # Errors
    /// As [`Fmp4Muxer::push`], and [`MuxError::Io`].
    pub fn write_packet(&mut self, packet: &Packet) -> Result<(), MuxError> {
        for segment in self.muxer.push(packet)? {
            self.out.write_all(segment.bytes())?;
        }
        Ok(())
    }

    /// The output.
    pub fn get_ref(&self) -> &W {
        &self.out
    }

    /// Writes the last fragment, flushes, and returns the output.
    ///
    /// # Errors
    /// As [`Fmp4Muxer::finish`], and [`MuxError::Io`].
    pub fn finish(mut self) -> Result<W, MuxError> {
        for segment in self.muxer.finish()? {
            self.out.write_all(segment.bytes())?;
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

// ---- Box writing ----

/// Appends a box: its 32-bit size, its type, and what `body` writes.
fn mp4_box(
    out: &mut Vec<u8>,
    kind: &[u8; 4],
    body: impl FnOnce(&mut Vec<u8>) -> Result<(), MuxError>,
) -> Result<(), MuxError> {
    let start = out.len();
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(kind);
    body(out)?;
    let len = out.len() - start;
    let size = u32::try_from(len).map_err(|_| MuxError::TooLong {
        what: "MP4 box",
        len,
        max: u32::MAX as usize,
    })?;
    out[start..start + 4].copy_from_slice(&size.to_be_bytes());
    Ok(())
}

/// Appends a full box: a box whose body starts with a version byte and 24 bits of flags.
fn full_box(
    out: &mut Vec<u8>,
    kind: &[u8; 4],
    version: u8,
    flags: u32,
    body: impl FnOnce(&mut Vec<u8>) -> Result<(), MuxError>,
) -> Result<(), MuxError> {
    mp4_box(out, kind, |out| {
        let f = flags.to_be_bytes();
        out.extend_from_slice(&[version, f[1], f[2], f[3]]);
        body(out)
    })
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_matrix(out: &mut Vec<u8>) {
    for v in MATRIX {
        put_u32(out, v);
    }
}

/// The init segment: `ftyp` and `moov` for `config`, whose AVC record must be known.
fn init_segment(config: &Fmp4Config) -> Result<Vec<u8>, MuxError> {
    let mut out = Vec::new();
    ftyp(&mut out)?;
    mp4_box(&mut out, b"moov", |out| {
        let tracks = u32::from(config.video.is_some()) + u32::from(config.audio.is_some());
        mvhd(out, tracks + 1)?;
        let mut id = 1;
        if let Some(video) = &config.video {
            let avc = video.avc.as_ref().ok_or(MuxError::MissingAvcConfig)?;
            trak(
                out,
                id,
                VIDEO_TIMESCALE,
                Some((video.width, video.height)),
                |out| avc1(out, video, avc),
            )?;
            id += 1;
        }
        if let Some(audio) = &config.audio {
            trak(out, id, audio.config.sample_rate(), None, |out| {
                mp4a(out, audio)
            })?;
        }
        mp4_box(out, b"mvex", |out| {
            for track_id in 1..=tracks {
                trex(out, track_id)?;
            }
            Ok(())
        })
    })?;
    Ok(out)
}

/// `ftyp`: ISO base media version 6 (which defines `mvex` fragments), compatible with
/// the earlier brands players check for.
fn ftyp(out: &mut Vec<u8>) -> Result<(), MuxError> {
    mp4_box(out, b"ftyp", |out| {
        out.extend_from_slice(b"iso6");
        put_u32(out, 0);
        for brand in [b"iso6", b"isom", b"iso2", b"avc1", b"mp41"] {
            out.extend_from_slice(brand);
        }
        Ok(())
    })
}

/// `mvhd`, version 0, with zero times and duration (the duration lives in fragments).
fn mvhd(out: &mut Vec<u8>, next_track_id: u32) -> Result<(), MuxError> {
    full_box(out, b"mvhd", 0, 0, |out| {
        put_u32(out, 0); // creation_time
        put_u32(out, 0); // modification_time
        put_u32(out, MOVIE_TIMESCALE);
        put_u32(out, 0); // duration
        put_u32(out, 0x0001_0000); // rate 1.0
        put_u16(out, 0x0100); // volume 1.0
        out.extend_from_slice(&[0; 10]); // reserved
        put_matrix(out);
        out.extend_from_slice(&[0; 24]); // pre_defined
        put_u32(out, next_track_id);
        Ok(())
    })
}

/// A `trak` with its `tkhd` and `mdia`; `video_size` is set for the video track, and
/// `sample_entry` writes the `stsd` entry.
fn trak(
    out: &mut Vec<u8>,
    id: u32,
    timescale: u32,
    video_size: Option<(u16, u16)>,
    sample_entry: impl FnOnce(&mut Vec<u8>) -> Result<(), MuxError>,
) -> Result<(), MuxError> {
    mp4_box(out, b"trak", |out| {
        // Flags: track_enabled | track_in_movie.
        full_box(out, b"tkhd", 0, 3, |out| {
            put_u32(out, 0); // creation_time
            put_u32(out, 0); // modification_time
            put_u32(out, id);
            put_u32(out, 0); // reserved
            put_u32(out, 0); // duration
            out.extend_from_slice(&[0; 8]); // reserved
            put_u16(out, 0); // layer
            put_u16(out, 0); // alternate_group
            put_u16(out, if video_size.is_some() { 0 } else { 0x0100 }); // volume
            put_u16(out, 0); // reserved
            put_matrix(out);
            let (w, h) = video_size.unwrap_or((0, 0));
            put_u32(out, u32::from(w) << 16);
            put_u32(out, u32::from(h) << 16);
            Ok(())
        })?;
        mp4_box(out, b"mdia", |out| {
            full_box(out, b"mdhd", 0, 0, |out| {
                put_u32(out, 0); // creation_time
                put_u32(out, 0); // modification_time
                put_u32(out, timescale);
                put_u32(out, 0); // duration
                put_u16(out, 0x55C4); // language "und"
                put_u16(out, 0); // pre_defined
                Ok(())
            })?;
            let (handler, name): (&[u8; 4], &[u8]) = if video_size.is_some() {
                (b"vide", b"VideoHandler\0")
            } else {
                (b"soun", b"SoundHandler\0")
            };
            full_box(out, b"hdlr", 0, 0, |out| {
                put_u32(out, 0); // pre_defined
                out.extend_from_slice(handler);
                out.extend_from_slice(&[0; 12]); // reserved
                out.extend_from_slice(name);
                Ok(())
            })?;
            mp4_box(out, b"minf", |out| {
                if video_size.is_some() {
                    // Flags 1, as the spec requires; graphicsmode copy, opcolor 0.
                    full_box(out, b"vmhd", 0, 1, |out| {
                        out.extend_from_slice(&[0; 8]);
                        Ok(())
                    })?;
                } else {
                    full_box(out, b"smhd", 0, 0, |out| {
                        out.extend_from_slice(&[0; 4]); // balance, reserved
                        Ok(())
                    })?;
                }
                mp4_box(out, b"dinf", |out| {
                    full_box(out, b"dref", 0, 0, |out| {
                        put_u32(out, 1);
                        // Flag 1: the media is in this file.
                        full_box(out, b"url ", 0, 1, |_| Ok(()))
                    })
                })?;
                mp4_box(out, b"stbl", |out| {
                    full_box(out, b"stsd", 0, 0, |out| {
                        put_u32(out, 1);
                        sample_entry(out)
                    })?;
                    // Empty tables: every sample is in a fragment.
                    for kind in [b"stts", b"stsc", b"stco"] {
                        full_box(out, kind, 0, 0, |out| {
                            put_u32(out, 0);
                            Ok(())
                        })?;
                    }
                    full_box(out, b"stsz", 0, 0, |out| {
                        put_u32(out, 0); // sample_size
                        put_u32(out, 0); // sample_count
                        Ok(())
                    })
                })
            })
        })
    })
}

/// The common start of a sample entry: six reserved bytes and `data_reference_index` 1.
fn sample_entry_header(out: &mut Vec<u8>) {
    out.extend_from_slice(&[0; 6]);
    put_u16(out, 1);
}

/// The `avc1` visual sample entry with its `avcC`.
fn avc1(out: &mut Vec<u8>, video: &VideoTrack, avc: &AvcDecoderConfig) -> Result<(), MuxError> {
    mp4_box(out, b"avc1", |out| {
        sample_entry_header(out);
        out.extend_from_slice(&[0; 16]); // pre_defined, reserved, pre_defined
        put_u16(out, video.width);
        put_u16(out, video.height);
        put_u32(out, 0x0048_0000); // 72 dpi
        put_u32(out, 0x0048_0000);
        put_u32(out, 0); // reserved
        put_u16(out, 1); // frame_count
        out.extend_from_slice(&[0; 32]); // compressorname
        put_u16(out, 0x0018); // depth
        put_u16(out, 0xFFFF); // pre_defined = -1
        mp4_box(out, b"avcC", |out| {
            out.extend_from_slice(&avc.to_bytes());
            Ok(())
        })
    })
}

/// The `mp4a` audio sample entry with its `esds`.
fn mp4a(out: &mut Vec<u8>, audio: &AudioTrack) -> Result<(), MuxError> {
    mp4_box(out, b"mp4a", |out| {
        sample_entry_header(out);
        out.extend_from_slice(&[0; 8]); // reserved
        put_u16(out, u16::from(audio.config.channels()));
        put_u16(out, 16); // samplesize
        put_u32(out, 0); // pre_defined, reserved
        // 16.16 fixed point; rates above 65535 Hz don't fit and are left to the esds.
        let rate = audio.config.sample_rate();
        put_u32(out, if rate <= 0xFFFF { rate << 16 } else { 0 });
        esds(out, audio)
    })
}

/// The `esds`: an MPEG-4 `ES_Descriptor` holding the `AudioSpecificConfig`.
fn esds(out: &mut Vec<u8>, audio: &AudioTrack) -> Result<(), MuxError> {
    let bitrate = audio.bitrate_kbps.map_or(0, |k| k.saturating_mul(1_000));
    let asc = audio.config.as_bytes();
    let mut decoder_specific = Vec::new();
    descriptor(&mut decoder_specific, 0x05, asc)?;
    let mut decoder_config = vec![
        0x40, // objectTypeIndication: MPEG-4 Audio
        0x15, // streamType 5 (audio) << 2 | reserved 1
        0, 0, 0, // bufferSizeDB
    ];
    decoder_config.extend_from_slice(&bitrate.to_be_bytes()); // maxBitrate
    decoder_config.extend_from_slice(&bitrate.to_be_bytes()); // avgBitrate
    decoder_config.extend_from_slice(&decoder_specific);
    let mut es = vec![0, 0, 0]; // ES_ID 0, no flags
    descriptor(&mut es, 0x04, &decoder_config)?;
    descriptor(&mut es, 0x06, &[0x02])?; // SLConfigDescriptor: predefined MP4
    full_box(out, b"esds", 0, 0, |out| descriptor(out, 0x03, &es))
}

/// Appends an MPEG-4 descriptor: its tag, its size in 7-bit groups, and `body`.
fn descriptor(out: &mut Vec<u8>, tag: u8, body: &[u8]) -> Result<(), MuxError> {
    let len = body.len();
    if len >= 1 << 28 {
        return Err(MuxError::TooLong {
            what: "MPEG-4 descriptor",
            len,
            max: (1 << 28) - 1,
        });
    }
    out.push(tag);
    let groups = (1..4).take_while(|&i| len >> (7 * i) != 0).count() + 1;
    for i in (0..groups).rev() {
        // Each group holds seven bits; the cast keeps them.
        let bits = ((len >> (7 * i)) & 0x7F) as u8;
        out.push(if i == 0 { bits } else { bits | 0x80 });
    }
    out.extend_from_slice(body);
    Ok(())
}

/// A `trex` with no defaults: every fragment gives each sample's duration, size and
/// flags.
fn trex(out: &mut Vec<u8>, track_id: u32) -> Result<(), MuxError> {
    full_box(out, b"trex", 0, 0, |out| {
        put_u32(out, track_id);
        put_u32(out, 1); // default_sample_description_index
        put_u32(out, 0); // default_sample_duration
        put_u32(out, 0); // default_sample_size
        put_u32(out, 0); // default_sample_flags
        Ok(())
    })
}

/// A media segment: `moof` with one `traf` per track, then the `mdat` holding the
/// tracks' samples in the same order.
fn media_segment(sequence: u32, tracks: &[Traf<'_>]) -> Result<Vec<u8>, MuxError> {
    let mut out = Vec::new();
    // Where each trun's data_offset field sits, to patch once the moof size is known.
    let mut offset_fields = Vec::with_capacity(tracks.len());
    mp4_box(&mut out, b"moof", |out| {
        full_box(out, b"mfhd", 0, 0, |out| {
            put_u32(out, sequence);
            Ok(())
        })?;
        for traf in tracks {
            let samples = &traf.samples;
            mp4_box(out, b"traf", |out| {
                full_box(out, b"tfhd", 0, TFHD_DEFAULT_BASE_IS_MOOF, |out| {
                    put_u32(out, traf.id);
                    Ok(())
                })?;
                full_box(out, b"tfdt", 1, 0, |out| {
                    // Decode times are never negative: earlier packets are rejected.
                    out.extend_from_slice(&u64::try_from(traf.base).unwrap_or(0).to_be_bytes());
                    Ok(())
                })?;
                let mut flags = TRUN_DATA_OFFSET | TRUN_DURATION | TRUN_SIZE | TRUN_FLAGS;
                if traf.video {
                    flags |= TRUN_CTS;
                }
                full_box(out, b"trun", 0, flags, |out| {
                    let count = samples.len();
                    put_u32(
                        out,
                        u32::try_from(count).map_err(|_| MuxError::TooLong {
                            what: "trun sample count",
                            len: count,
                            max: u32::MAX as usize,
                        })?,
                    );
                    offset_fields.push(out.len());
                    put_u32(out, 0); // data_offset, patched below
                    for s in samples {
                        put_u32(out, s.duration);
                        put_u32(out, sample_size(s.data.len())?);
                        put_u32(
                            out,
                            if s.keyframe {
                                FLAGS_SYNC
                            } else {
                                FLAGS_NON_SYNC
                            },
                        );
                        if traf.video {
                            put_u32(out, s.cts);
                        }
                    }
                    Ok(())
                })
            })?;
        }
        Ok(())
    })?;

    let data_len: usize = tracks
        .iter()
        .flat_map(|t| t.samples.iter().map(|s| s.data.len()))
        .sum();
    // A 32-bit size when it fits, else size 1 and a 64-bit largesize.
    let header = if data_len + 8 <= u32::MAX as usize {
        8
    } else {
        16
    };
    let mut data_offset = out.len() + header;
    for (field, traf) in offset_fields.into_iter().zip(tracks) {
        let offset = i32::try_from(data_offset).map_err(|_| MuxError::TooLong {
            what: "fragment",
            len: data_offset,
            max: i32::MAX as usize,
        })?;
        out[field..field + 4].copy_from_slice(&offset.to_be_bytes());
        data_offset += traf.samples.iter().map(|s| s.data.len()).sum::<usize>();
    }

    if header == 8 {
        // Checked above.
        put_u32(&mut out, (data_len + 8) as u32);
        out.extend_from_slice(b"mdat");
    } else {
        put_u32(&mut out, 1);
        out.extend_from_slice(b"mdat");
        out.extend_from_slice(&(data_len as u64 + 16).to_be_bytes());
    }
    for traf in tracks {
        for s in &traf.samples {
            out.extend_from_slice(s.data);
        }
    }
    Ok(out)
}

fn sample_size(len: usize) -> Result<u32, MuxError> {
    u32::try_from(len).map_err(|_| MuxError::TooLong {
        what: "MP4 sample",
        len,
        max: u32::MAX as usize,
    })
}

#[cfg(test)]
mod tests {
    use jstream_media::{Codec, MediaTime, Packet, Timebase};

    use super::*;
    use crate::h264::tests::{PPS, SPS};

    fn k90(t: i64) -> MediaTime {
        MediaTime::new(t, Timebase::MPEG_90K)
    }

    fn k48(t: i64) -> MediaTime {
        MediaTime::new(t, Timebase::per_second(48_000).unwrap())
    }

    fn avc() -> AvcDecoderConfig {
        AvcDecoderConfig::new(vec![SPS.to_vec()], vec![PPS.to_vec()]).unwrap()
    }

    fn video_track(avc: Option<AvcDecoderConfig>) -> VideoTrack {
        VideoTrack {
            width: 1280,
            height: 720,
            avc,
        }
    }

    fn audio_track() -> AudioTrack {
        AudioTrack {
            config: AudioSpecificConfig::aac_lc(48_000, 2).unwrap(),
            bitrate_kbps: Some(128),
        }
    }

    fn av_config(avc: Option<AvcDecoderConfig>) -> Fmp4Config {
        Fmp4Config {
            video: Some(video_track(avc)),
            audio: Some(audio_track()),
            origin: None,
        }
    }

    /// An IDR access unit carrying SPS and PPS in-band, as Media Foundation emits it.
    fn idr_annexb() -> Vec<u8> {
        let mut au = Vec::new();
        for nalu in [
            &SPS[..],
            &PPS[..],
            &[0x65, 0x88, 0x84, 0x00, 0x00, 0x03, 0x01],
        ] {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(nalu);
        }
        au
    }

    fn video(key: bool, pts: i64, dts: i64, data: Vec<u8>) -> Packet {
        Packet::new(Codec::H264, key, k90(pts), k90(dts), data).unwrap()
    }

    fn aac(t: i64, byte: u8) -> Packet {
        Packet::new(Codec::Aac, true, k48(t), k48(t), vec![0x21, byte]).unwrap()
    }

    // ---- A minimal reader for the boxes the muxer writes, used to check structure. ----

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
    }

    /// The top-level boxes of `b` as (type, whole box including header).
    fn boxes(b: &[u8]) -> Vec<(String, &[u8])> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < b.len() {
            let mut size = u32_at(b, pos) as usize;
            if size == 1 {
                size = u64::from_be_bytes(b[pos + 8..pos + 16].try_into().unwrap()) as usize;
            }
            let kind = String::from_utf8(b[pos + 4..pos + 8].to_vec()).unwrap();
            out.push((kind, &b[pos..pos + size]));
            pos += size;
        }
        assert_eq!(pos, b.len(), "boxes fill their parent exactly");
        out
    }

    /// The children of a container box.
    fn children(b: &[u8]) -> Vec<(String, &[u8])> {
        boxes(&b[8..])
    }

    /// The first box of type `kind` along `path` below `b` (a container box).
    fn find<'a>(b: &'a [u8], path: &[&str]) -> &'a [u8] {
        let mut cur = b;
        for kind in path {
            cur = children(cur)
                .into_iter()
                .find(|(k, _)| k == kind)
                .unwrap_or_else(|| panic!("no {kind}"))
                .1;
        }
        cur
    }

    /// One sample as read back from a fragment.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ReadSample {
        dts: i64,
        pts: i64,
        keyframe: bool,
        data: Vec<u8>,
    }

    /// Reads a `moof` + `mdat` segment: its sequence number and, per track ID, its
    /// samples with absolute decode times from `tfdt` and the durations.
    fn read_fragment(seg: &[u8]) -> (u32, Vec<(u32, Vec<ReadSample>)>) {
        let top = boxes(seg);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, "moof");
        assert_eq!(top[1].0, "mdat");
        let moof = top[0].1;
        let mfhd = find(moof, &["mfhd"]);
        let sequence = u32_at(mfhd, 12);
        let mut tracks = Vec::new();
        for (kind, traf) in children(moof) {
            if kind != "traf" {
                continue;
            }
            let tfhd = find(traf, &["tfhd"]);
            assert_eq!(u32_at(tfhd, 8) & 0xFF_FFFF, TFHD_DEFAULT_BASE_IS_MOOF);
            let id = u32_at(tfhd, 12);
            let tfdt = find(traf, &["tfdt"]);
            assert_eq!(tfdt[8], 1, "tfdt version 1");
            let mut dts = u64::from_be_bytes(tfdt[12..20].try_into().unwrap()) as i64;
            let trun = find(traf, &["trun"]);
            let flags = u32_at(trun, 8) & 0xFF_FFFF;
            let has_cts = flags & TRUN_CTS != 0;
            let count = u32_at(trun, 12) as usize;
            let mut data_at = u32_at(trun, 16) as usize;
            let mut pos = 20;
            let mut samples = Vec::new();
            for _ in 0..count {
                let duration = i64::from(u32_at(trun, pos));
                let size = u32_at(trun, pos + 4) as usize;
                let sample_flags = u32_at(trun, pos + 8);
                let cts = if has_cts {
                    i64::from(u32_at(trun, pos + 12))
                } else {
                    0
                };
                pos += if has_cts { 16 } else { 12 };
                samples.push(ReadSample {
                    dts,
                    pts: dts + cts,
                    keyframe: sample_flags == FLAGS_SYNC,
                    data: seg[data_at..data_at + size].to_vec(),
                });
                dts += duration;
                data_at += size;
            }
            assert_eq!(pos, trun.len());
            tracks.push((id, samples));
        }
        (sequence, tracks)
    }

    fn media(segments: &[Segment]) -> Vec<&[u8]> {
        segments
            .iter()
            .filter_map(|s| match s {
                Segment::Media { bytes, .. } => Some(&bytes[..]),
                Segment::Init(_) => None,
            })
            .collect()
    }

    #[test]
    fn ftyp_is_byte_exact() {
        let mut out = Vec::new();
        ftyp(&mut out).unwrap();
        let mut expected = vec![0, 0, 0, 0x24];
        expected.extend_from_slice(b"ftypiso6");
        expected.extend_from_slice(&[0, 0, 0, 0]);
        expected.extend_from_slice(b"iso6isomiso2avc1mp41");
        assert_eq!(out, expected);
    }

    #[test]
    fn descriptor_sizes_use_seven_bit_groups() {
        let mut out = Vec::new();
        descriptor(&mut out, 5, &[0xAB; 127]).unwrap();
        assert_eq!(&out[..2], &[5, 127]);
        assert_eq!(out.len(), 2 + 127);

        let mut out = Vec::new();
        descriptor(&mut out, 5, &[0; 128]).unwrap();
        assert_eq!(&out[..3], &[5, 0x81, 0x00]);

        let mut out = Vec::new();
        descriptor(&mut out, 5, &vec![0; 0x4000]).unwrap();
        assert_eq!(&out[..4], &[5, 0x81, 0x80, 0x00]);
    }

    #[test]
    fn esds_is_byte_exact() {
        let mut out = Vec::new();
        esds(&mut out, &audio_track()).unwrap();
        let mut expected = vec![0, 0, 0, 0x27];
        expected.extend_from_slice(b"esds");
        expected.extend_from_slice(&[0, 0, 0, 0]); // version, flags
        expected.extend_from_slice(&[0x03, 0x19, 0, 0, 0]); // ES_Descriptor, ES_ID 0
        expected.extend_from_slice(&[0x04, 0x11, 0x40, 0x15, 0, 0, 0]); // DecoderConfig
        expected.extend_from_slice(&[0x00, 0x01, 0xF4, 0x00]); // maxBitrate 128000
        expected.extend_from_slice(&[0x00, 0x01, 0xF4, 0x00]); // avgBitrate
        expected.extend_from_slice(&[0x05, 0x02, 0x11, 0x90]); // AudioSpecificConfig
        expected.extend_from_slice(&[0x06, 0x01, 0x02]); // SLConfigDescriptor
        assert_eq!(out, expected);
    }

    #[test]
    fn init_segment_structure() {
        let mut muxer = Fmp4Muxer::new(av_config(Some(avc()))).unwrap();
        let Some(Segment::Init(init)) = muxer.start().unwrap() else {
            panic!("init expected");
        };
        assert!(muxer.start().unwrap().is_none(), "init is yielded once");

        let top = boxes(&init);
        let kinds: Vec<_> = top.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["ftyp", "moov"]);
        let moov = top[1].1;
        let kinds: Vec<_> = children(moov).into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, ["mvhd", "trak", "trak", "mvex"]);
        let mvhd = find(moov, &["mvhd"]);
        assert_eq!(mvhd.len(), 108);
        assert_eq!(u32_at(mvhd, 20), MOVIE_TIMESCALE);
        assert_eq!(u32_at(mvhd, 104), 3, "next_track_ID");

        let traks: Vec<_> = children(moov)
            .into_iter()
            .filter(|(k, _)| k == "trak")
            .map(|(_, b)| b)
            .collect();
        // track ID, handler, time scale
        let expected = [(1, *b"vide", VIDEO_TIMESCALE), (2, *b"soun", 48_000)];
        for (trak, (id, handler, timescale)) in traks.iter().zip(expected) {
            let tkhd = find(trak, &["tkhd"]);
            assert_eq!(tkhd.len(), 92);
            assert_eq!(u32_at(tkhd, 20), id);
            assert_eq!(&find(trak, &["mdia", "hdlr"])[16..20], &handler);
            assert_eq!(u32_at(find(trak, &["mdia", "mdhd"]), 20), timescale);
            for empty in ["stts", "stsc", "stco"] {
                let table = find(trak, &["mdia", "minf", "stbl", empty]);
                assert_eq!(table.len(), 16, "{empty} has no entries");
            }
        }
        let video_tkhd = find(traks[0], &["tkhd"]);
        assert_eq!(u32_at(video_tkhd, 84), 1280 << 16);
        assert_eq!(u32_at(video_tkhd, 88), 720 << 16);

        let stsd = find(traks[0], &["mdia", "minf", "stbl", "stsd"]);
        let entries = boxes(&stsd[16..]);
        assert_eq!(entries.len(), 1);
        let (kind, avc1) = &entries[0];
        assert_eq!(kind, "avc1");
        assert_eq!(u16::from_be_bytes([avc1[32], avc1[33]]), 1280);
        assert_eq!(u16::from_be_bytes([avc1[34], avc1[35]]), 720);
        // The avcC follows the 78-byte visual sample entry body.
        let avcc = boxes(&avc1[86..]);
        assert_eq!(avcc[0].0, "avcC");
        assert_eq!(&avcc[0].1[8..], &avc().to_bytes()[..]);

        let stsd = find(traks[1], &["mdia", "minf", "stbl", "stsd"]);
        let (kind, mp4a) = &boxes(&stsd[16..])[0];
        assert_eq!(kind, "mp4a");
        assert_eq!(u16::from_be_bytes([mp4a[24], mp4a[25]]), 2, "channels");
        assert_eq!(u32_at(mp4a, 32), 48_000 << 16);
        let mut esds_box = Vec::new();
        esds(&mut esds_box, &audio_track()).unwrap();
        assert_eq!(&mp4a[36..], &esds_box[..]);

        let trex: Vec<_> = children(find(moov, &["mvex"]))
            .into_iter()
            .map(|(k, b)| (k, u32_at(b, 12), u32_at(b, 16)))
            .collect();
        assert_eq!(trex, [("trex".into(), 1, 1), ("trex".into(), 2, 1)]);
    }

    #[test]
    fn audio_only_is_track_one() {
        let mut muxer = Fmp4Muxer::new(Fmp4Config {
            audio: Some(audio_track()),
            ..Fmp4Config::default()
        })
        .unwrap();
        let init = muxer.start().unwrap().unwrap();
        let moov = boxes(init.bytes())[1].1;
        let kinds: Vec<_> = children(moov).into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, ["mvhd", "trak", "mvex"]);
        assert_eq!(u32_at(find(moov, &["trak", "tkhd"]), 20), 1);
        assert_eq!(u32_at(find(moov, &["mvhd"]), 104), 2);
    }

    #[test]
    fn media_segment_is_byte_exact() {
        let mut muxer = Fmp4Muxer::new(av_config(Some(avc()))).unwrap();
        muxer.start().unwrap();
        assert!(
            muxer
                .push(&video(true, 0, 0, vec![0, 0, 0, 1, 0x65, 0xAA]))
                .unwrap()
                .is_empty()
        );
        assert!(muxer.push(&aac(0, 0x10)).unwrap().is_empty());
        let segments = muxer.finish().unwrap();
        assert_eq!(segments.len(), 1);
        let Segment::Media { sequence, bytes } = &segments[0] else {
            panic!("media segment expected");
        };
        assert_eq!(*sequence, 1);

        let mut e = Vec::new();
        e.extend_from_slice(&[0, 0, 0, 180]);
        e.extend_from_slice(b"moof");
        e.extend_from_slice(&[0, 0, 0, 16]);
        e.extend_from_slice(b"mfhd");
        e.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        // video traf: base-is-moof, tfdt 0, one sync sample of 6 bytes lasting 1/30 s
        e.extend_from_slice(&[0, 0, 0, 80]);
        e.extend_from_slice(b"traf");
        e.extend_from_slice(&[0, 0, 0, 16]);
        e.extend_from_slice(b"tfhd");
        e.extend_from_slice(&[0, 2, 0, 0, 0, 0, 0, 1]);
        e.extend_from_slice(&[0, 0, 0, 20]);
        e.extend_from_slice(b"tfdt");
        e.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        e.extend_from_slice(&[0, 0, 0, 36]);
        e.extend_from_slice(b"trun");
        e.extend_from_slice(&[0, 0, 0x0F, 0x01, 0, 0, 0, 1, 0, 0, 0, 188]);
        e.extend_from_slice(&[0, 0, 0x0B, 0xB8, 0, 0, 0, 6, 2, 0, 0, 0, 0, 0, 0, 0]);
        // audio traf: one sample of 2 bytes lasting one AAC frame, no composition offset
        e.extend_from_slice(&[0, 0, 0, 76]);
        e.extend_from_slice(b"traf");
        e.extend_from_slice(&[0, 0, 0, 16]);
        e.extend_from_slice(b"tfhd");
        e.extend_from_slice(&[0, 2, 0, 0, 0, 0, 0, 2]);
        e.extend_from_slice(&[0, 0, 0, 20]);
        e.extend_from_slice(b"tfdt");
        e.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        e.extend_from_slice(&[0, 0, 0, 32]);
        e.extend_from_slice(b"trun");
        e.extend_from_slice(&[0, 0, 0x07, 0x01, 0, 0, 0, 1, 0, 0, 0, 194]);
        e.extend_from_slice(&[0, 0, 0x04, 0x00, 0, 0, 0, 2, 2, 0, 0, 0]);
        e.extend_from_slice(&[0, 0, 0, 16]);
        e.extend_from_slice(b"mdat");
        e.extend_from_slice(&[0, 0, 0, 2, 0x65, 0xAA, 0x21, 0x10]);
        assert_eq!(bytes, &e);
        assert!(muxer.finish().unwrap().is_empty(), "nothing left");
    }

    /// Two GOPs of I P B B at 30000/1001 fps with B-frame reordering, against AAC at
    /// 48 kHz, from an origin that isn't zero. Every sample must come back with its
    /// times, sync flag and AVCC payload, and each fragment must start at a keyframe.
    ///
    /// The payloads here are not decodable, so only the container is checked. To check a
    /// real stream with real tools, feed encoder output through `Fmp4Writer` and run
    /// `ffprobe -show_streams -show_packets` and `ffmpeg -i out.mp4 -f framemd5 -` on the
    /// file locally. CI does not depend on ffmpeg.
    #[test]
    fn round_trips_packets_through_fragments() {
        let f = 3003; // one frame in 90 kHz ticks
        let origin = 900_000; // 10 s
        let mut config = av_config(None);
        config.origin = Some(k90(origin));
        let mut muxer = Fmp4Muxer::new(config).unwrap();
        assert!(
            muxer.start().unwrap().is_none(),
            "init waits for the first keyframe"
        );

        // (keyframe, pts, dts) in frames, decode order I P B B with one frame of delay.
        let gop = [(true, 1, 0), (false, 4, 1), (false, 2, 2), (false, 3, 3)];
        let mut video_in = Vec::new();
        for g in 0..2 {
            for (key, pts, dts) in gop {
                let base = 4 * g;
                let data = if key {
                    idr_annexb()
                } else {
                    vec![0, 0, 1, 0x41, 0x9A, (base + dts) as u8]
                };
                video_in.push(video(
                    key,
                    origin + (base + pts) * f,
                    origin + (base + dts) * f,
                    data,
                ));
            }
        }
        // 48 kHz audio from the origin over the same span, 1024 samples a packet.
        let audio_origin = origin * 48_000 / 90_000;
        let audio_in: Vec<_> = (0..9)
            .map(|i| aac(audio_origin + i * 1024, i as u8))
            .collect();

        // Interleave by decode time, as the engine delivers them.
        let mut packets: Vec<&Packet> = video_in.iter().chain(&audio_in).collect();
        packets.sort_by_key(|p| p.dts());
        let mut segments = Vec::new();
        for p in packets {
            segments.extend(muxer.push(p).unwrap());
        }
        segments.extend(muxer.finish().unwrap());

        assert!(matches!(segments[0], Segment::Init(_)));
        assert_eq!(
            muxer.config().video.as_ref().unwrap().avc,
            Some(avc()),
            "the record comes from the first keyframe"
        );
        let fragments = media(&segments);
        assert_eq!(fragments.len(), 2, "one fragment per keyframe");

        let mut video_out = Vec::new();
        let mut audio_out = Vec::new();
        for (n, fragment) in fragments.iter().enumerate() {
            let (sequence, tracks) = read_fragment(fragment);
            assert_eq!(sequence as usize, n + 1);
            for (id, samples) in tracks {
                match id {
                    1 => {
                        assert!(samples[0].keyframe, "fragment {n} starts at a keyframe");
                        video_out.extend(samples);
                    }
                    2 => audio_out.extend(samples),
                    other => panic!("unexpected track {other}"),
                }
            }
        }

        let expected_video: Vec<_> = video_in
            .iter()
            .map(|p| ReadSample {
                dts: p.dts().ticks() - origin,
                pts: p.pts().ticks() - origin,
                keyframe: p.is_keyframe(),
                data: annexb_to_avcc(p.data()).unwrap(),
            })
            .collect();
        assert_eq!(video_out, expected_video);
        let expected_audio: Vec<_> = audio_in
            .iter()
            .map(|p| ReadSample {
                dts: p.dts().ticks() - audio_origin,
                pts: p.pts().ticks() - audio_origin,
                keyframe: true,
                data: p.data().to_vec(),
            })
            .collect();
        assert_eq!(audio_out, expected_audio);

        // The first fragment holds the audio that decodes before the second keyframe.
        let (_, first) = read_fragment(fragments[0]);
        let second_key = 4 * f * 48_000 / 90_000;
        assert!(first[1].1.iter().all(|s| s.dts < second_key));
        assert_eq!(first[1].1.len(), 6);
    }

    #[test]
    fn audio_before_the_first_keyframe_waits_for_the_init_segment() {
        let mut muxer = Fmp4Muxer::new(av_config(None)).unwrap();
        assert!(muxer.push(&aac(0, 0)).unwrap().is_empty());
        assert!(muxer.push(&aac(1024, 1)).unwrap().is_empty());
        let segments = muxer.push(&video(true, 3003, 3003, idr_annexb())).unwrap();
        assert_eq!(segments.len(), 1);
        assert!(matches!(segments[0], Segment::Init(_)));
        let segments = muxer.finish().unwrap();
        let (_, tracks) = read_fragment(media(&segments)[0]);
        assert_eq!(tracks[0].1.len(), 1);
        assert_eq!(tracks[0].1[0].dts, 3003);
        assert_eq!(tracks[1].1.len(), 2);
    }

    #[test]
    fn audio_only_cuts_a_fragment_every_second() {
        let mut muxer = Fmp4Muxer::new(Fmp4Config {
            audio: Some(audio_track()),
            ..Fmp4Config::default()
        })
        .unwrap();
        let mut segments = vec![muxer.start().unwrap().unwrap()];
        // 100 packets of 1024 samples: about 2.13 s.
        for i in 0..100 {
            segments.extend(muxer.push(&aac(i * 1024, i as u8)).unwrap());
        }
        segments.extend(muxer.finish().unwrap());
        let counts: Vec<_> = media(&segments)
            .into_iter()
            .map(|f| read_fragment(f).1[0].1.len())
            .collect();
        // 47 frames is the first count reaching 48000 samples (47 * 1024 = 48128).
        assert_eq!(counts, [47, 47, 6]);
    }

    #[test]
    fn rejects_bad_packets_and_stays_unchanged() {
        let mut muxer = Fmp4Muxer::new(Fmp4Config {
            video: Some(video_track(Some(avc()))),
            origin: Some(k90(9_000)),
            ..Fmp4Config::default()
        })
        .unwrap();
        muxer.start().unwrap();
        assert!(matches!(
            muxer.push(&aac(0, 0)),
            Err(MuxError::NoTrack { codec: Codec::Aac })
        ));
        assert!(matches!(
            muxer.push(&video(true, 0, 0, idr_annexb())),
            Err(MuxError::NegativeTimestamp { ms: -100 })
        ));
        assert!(matches!(
            muxer.push(&video(false, 9_000, 9_000, vec![0, 0, 1, 0x41])),
            Err(MuxError::FirstVideoNotKeyframe)
        ));
        assert!(matches!(
            muxer.push(&video(true, 9_000, 9_000, vec![0x41])),
            Err(MuxError::NotAnnexB { .. })
        ));
        muxer
            .push(&video(true, 9_000, 9_000, idr_annexb()))
            .unwrap();
        assert!(matches!(
            muxer.push(&video(false, 9_000, 9_000, vec![0, 0, 1, 0x41])),
            Err(MuxError::DtsNotIncreasing {
                codec: Codec::H264,
                previous: 0,
                current: 0,
                timescale: VIDEO_TIMESCALE
            })
        ));
        let huge = i64::from(u32::MAX) + 1;
        assert!(matches!(
            muxer.push(&video(
                false,
                9_000 + 2 * huge,
                9_000 + huge,
                vec![0, 0, 1, 0x41]
            )),
            Err(MuxError::TimestampOutOfRange {
                what: "composition offset",
                ..
            })
        ));
        assert!(matches!(
            muxer.push(&video(
                false,
                9_000 + huge,
                9_000 + huge,
                vec![0, 0, 1, 0x41]
            )),
            Err(MuxError::TimestampOutOfRange {
                what: "sample duration",
                ..
            })
        ));
        // None of that changed anything: the stream goes on from the keyframe.
        muxer
            .push(&video(false, 12_003, 12_003, vec![0, 0, 1, 0x41]))
            .unwrap();
        let segments = muxer
            .push(&video(true, 15_006, 15_006, idr_annexb()))
            .unwrap();
        let (sequence, tracks) = read_fragment(media(&segments)[0]);
        assert_eq!(sequence, 1);
        let times: Vec<_> = tracks[0].1.iter().map(|s| (s.dts, s.keyframe)).collect();
        assert_eq!(times, [(0, true), (3003, false)]);
    }

    #[test]
    fn missing_parameter_sets_are_an_error() {
        let mut muxer = Fmp4Muxer::new(av_config(None)).unwrap();
        assert!(matches!(
            muxer.push(&video(false, 0, 0, vec![0, 0, 1, 0x41])),
            Err(MuxError::MissingAvcConfig)
        ));
        assert!(matches!(
            muxer.push(&video(true, 0, 0, vec![0, 0, 1, 0x65])),
            Err(MuxError::MissingAvcConfig)
        ));
        // Audio is buffered, but with no keyframe there is no init segment to write.
        muxer.push(&aac(0, 0)).unwrap();
        assert!(matches!(muxer.finish(), Err(MuxError::MissingAvcConfig)));
    }

    #[test]
    fn writer_writes_the_segments_in_order() {
        let packets = [
            video(true, 0, 0, idr_annexb()),
            video(false, 3003, 3003, vec![0, 0, 1, 0x41, 1]),
            video(true, 6006, 6006, idr_annexb()),
        ];
        let mut muxer = Fmp4Muxer::new(av_config(None)).unwrap();
        let mut expected = Vec::new();
        for p in &packets {
            for s in muxer.push(p).unwrap() {
                expected.extend_from_slice(s.bytes());
            }
        }
        for s in muxer.finish().unwrap() {
            expected.extend_from_slice(s.bytes());
        }

        let mut writer = Fmp4Writer::new(Vec::new(), av_config(None)).unwrap();
        assert!(writer.get_ref().is_empty(), "init waits for the keyframe");
        for p in &packets {
            writer.write_packet(p).unwrap();
        }
        let bytes = writer.finish().unwrap();
        assert_eq!(bytes, expected);
        let kinds: Vec<_> = boxes(&bytes).into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, ["ftyp", "moov", "moof", "mdat", "moof", "mdat"]);

        // With the record configured, the init segment is written at once.
        let writer = Fmp4Writer::new(Vec::new(), av_config(Some(avc()))).unwrap();
        let kinds: Vec<_> = boxes(writer.get_ref())
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(kinds, ["ftyp", "moov"]);
        assert_eq!(
            writer.finish().unwrap().len(),
            boxes(&expected)[..2].iter().map(|b| b.1.len()).sum()
        );
    }
}
