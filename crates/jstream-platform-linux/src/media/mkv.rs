//! A Matroska and WebM demuxer (ADR-0072 (d)): the tracks, then every cluster's blocks
//! by position and time, read once when the file opens.
//!
//! It reads what playback needs of the first video track and the first audio track: the
//! codec and its private data, and each frame's position, size, presentation time and
//! whether it is a keyframe. Blocks are walked by their headers alone, so a long file
//! costs a few bytes read per frame, not the frames themselves. Laced blocks give one
//! sample per frame. Header stripping (`ContentCompAlgo` 3) is put back as each sample's
//! prefix; other compression and encryption are refused. A file whose sizes are unknown
//! (a live recording) or that ends early (a recorder that stopped) reads up to where it
//! ends. Matroska has no decode times, so each track's decode times are its
//! presentation times in order, which is all the decoder needs of them.

use std::fs::File;
use std::io::{BufReader, Read, Seek};

use jstream_platform::PlatformError;

use super::demux::{Codec, Demuxed, Sample, Track};

/// The most frames a track may list.
const MAX_SAMPLES: usize = 20_000_000;
/// The largest element read whole (a track's private data, a header).
const MAX_ELEMENT: u64 = 16 << 20;

mod id {
    pub(super) const EBML: u32 = 0x1A45_DFA3;
    pub(super) const DOC_TYPE: u32 = 0x4282;
    pub(super) const SEGMENT: u32 = 0x1853_8067;
    pub(super) const INFO: u32 = 0x1549_A966;
    pub(super) const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
    pub(super) const TRACKS: u32 = 0x1654_AE6B;
    pub(super) const TRACK_ENTRY: u32 = 0xAE;
    pub(super) const TRACK_NUMBER: u32 = 0xD7;
    pub(super) const TRACK_TYPE: u32 = 0x83;
    pub(super) const FLAG_ENABLED: u32 = 0xB9;
    pub(super) const CODEC_ID: u32 = 0x86;
    pub(super) const CODEC_PRIVATE: u32 = 0x63A2;
    pub(super) const DEFAULT_DURATION: u32 = 0x23_E383;
    pub(super) const VIDEO: u32 = 0xE0;
    pub(super) const PIXEL_WIDTH: u32 = 0xB0;
    pub(super) const PIXEL_HEIGHT: u32 = 0xBA;
    pub(super) const AUDIO: u32 = 0xE1;
    pub(super) const SAMPLING_FREQUENCY: u32 = 0xB5;
    pub(super) const CHANNELS: u32 = 0x9F;
    pub(super) const CONTENT_ENCODINGS: u32 = 0x6D80;
    pub(super) const CONTENT_ENCODING: u32 = 0x6240;
    pub(super) const CONTENT_COMPRESSION: u32 = 0x5034;
    pub(super) const CONTENT_COMP_ALGO: u32 = 0x4254;
    pub(super) const CONTENT_COMP_SETTINGS: u32 = 0x4255;
    pub(super) const CONTENT_ENCRYPTION: u32 = 0x5035;
    pub(super) const CLUSTER: u32 = 0x1F43_B675;
    pub(super) const TIMESTAMP: u32 = 0xE7;
    pub(super) const SIMPLE_BLOCK: u32 = 0xA3;
    pub(super) const BLOCK_GROUP: u32 = 0xA0;
    pub(super) const BLOCK: u32 = 0xA1;
    pub(super) const REFERENCE_BLOCK: u32 = 0xFB;
    /// The segment's other children: where an unknown-size cluster ends.
    pub(super) const TOP_LEVEL: [u32; 8] = [
        0x114D_9B74, // SeekHead
        INFO,
        TRACKS,
        CLUSTER,
        0x1C53_BB6B, // Cues
        0x1254_C367, // Tags
        0x1043_A770, // Chapters
        0x1941_A469, // Attachments
    ];
}

fn invalid(what: &str) -> PlatformError {
    PlatformError::Os {
        call: "Matroska demuxer",
        code: None,
        message: format!("a damaged Matroska file: {what}"),
    }
}

fn io_error(err: &std::io::Error) -> PlatformError {
    PlatformError::Os {
        call: "read",
        code: err.raw_os_error().and_then(|c| u32::try_from(c).ok()),
        message: err.to_string(),
    }
}

/// An element's header: its ID, where its body starts, and its size (`None`: unknown,
/// it runs to the end of its parent).
#[derive(Clone, Copy, Debug)]
struct Header {
    id: u32,
    body: u64,
    size: Option<u64>,
}

impl Header {
    /// Where the body ends, `limit` for an unknown size.
    fn end(&self, limit: u64) -> u64 {
        self.size
            .map_or(limit, |s| self.body.saturating_add(s).min(limit))
    }
}

/// A reader that knows where it is.
struct Reader {
    inner: BufReader<File>,
    at: u64,
    len: u64,
}

impl Reader {
    fn byte(&mut self) -> Result<Option<u8>, PlatformError> {
        let mut b = [0_u8];
        match self.inner.read_exact(&mut b) {
            Ok(()) => {
                self.at += 1;
                Ok(Some(b[0]))
            }
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(err) => Err(io_error(&err)),
        }
    }

    fn need(&mut self) -> Result<u8, PlatformError> {
        self.byte()?.ok_or_else(|| invalid("an element ends early"))
    }

    /// An EBML variable-length integer: its value with the marker bit cleared, and
    /// whether every value bit was set (an unknown size).
    fn vint(&mut self) -> Result<Option<(u64, bool)>, PlatformError> {
        let Some(first) = self.byte()? else {
            return Ok(None);
        };
        let extra = first.leading_zeros();
        if extra > 7 {
            return Err(invalid("a variable-length integer longer than eight bytes"));
        }
        let mask = 0xFF_u64 >> (extra + 1);
        let mut value = u64::from(first) & mask;
        let mut all_ones = value == mask;
        for _ in 0..extra {
            let b = self.need()?;
            all_ones &= b == 0xFF;
            value = value << 8 | u64::from(b);
        }
        Ok(Some((value, all_ones)))
    }

    /// The next element's header, or `None` at the end of the file.
    fn header(&mut self) -> Result<Option<Header>, PlatformError> {
        let Some(first) = self.byte()? else {
            return Ok(None);
        };
        let extra = first.leading_zeros();
        if extra > 3 {
            return Err(invalid("an element ID longer than four bytes"));
        }
        let mut id = u32::from(first);
        for _ in 0..extra {
            id = id << 8 | u32::from(self.need()?);
        }
        let Some((size, unknown)) = self.vint()? else {
            return Ok(None);
        };
        Ok(Some(Header {
            id,
            body: self.at,
            size: (!unknown).then_some(size),
        }))
    }

    fn seek(&mut self, to: u64) -> Result<(), PlatformError> {
        let delta = i64::try_from(to)
            .unwrap_or(i64::MAX)
            .saturating_sub(i64::try_from(self.at).unwrap_or(0));
        self.inner.seek_relative(delta).map_err(|e| io_error(&e))?;
        self.at = to;
        Ok(())
    }

    /// A body read whole.
    fn body(&mut self, h: &Header) -> Result<Vec<u8>, PlatformError> {
        let size = h
            .size
            .ok_or_else(|| invalid("an unknown-size leaf element"))?;
        if size > MAX_ELEMENT || h.body.saturating_add(size) > self.len {
            return Err(invalid("an element larger than the file"));
        }
        let mut data = vec![0; usize::try_from(size).unwrap_or(0)];
        self.inner.read_exact(&mut data).map_err(|e| io_error(&e))?;
        self.at += size;
        Ok(data)
    }
}

/// A big-endian unsigned integer element.
fn uint(data: &[u8]) -> u64 {
    data.iter().take(8).fold(0, |v, b| v << 8 | u64::from(*b))
}

/// A float element (4 or 8 bytes).
fn float(data: &[u8]) -> f64 {
    match data.len() {
        4 => f64::from(f32::from_bits(u32::try_from(uint(data)).unwrap_or(0))),
        8 => f64::from_bits(uint(data)),
        _ => 0.0,
    }
}

/// Children of an element body held in memory.
fn children(mut data: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    while let Some((id, rest)) = mem_id(data) {
        let Some((size, rest)) = mem_vint(rest) else {
            break;
        };
        let Some(body) = usize::try_from(size).ok().and_then(|s| rest.get(..s)) else {
            break;
        };
        out.push((id, body));
        data = &rest[body.len()..];
    }
    out
}

fn mem_id(data: &[u8]) -> Option<(u32, &[u8])> {
    let first = *data.first()?;
    let len = usize::try_from(first.leading_zeros()).ok()? + 1;
    if len > 4 {
        return None;
    }
    let bytes = data.get(..len)?;
    Some((
        bytes.iter().fold(0, |v, b| v << 8 | u32::from(*b)),
        &data[len..],
    ))
}

fn mem_vint(data: &[u8]) -> Option<(u64, &[u8])> {
    let first = *data.first()?;
    let len = usize::try_from(first.leading_zeros()).ok()? + 1;
    if len > 8 {
        return None;
    }
    let bytes = data.get(..len)?;
    let mut value = u64::from(first) & (0xFF_u64 >> len);
    for b in &bytes[1..] {
        value = value << 8 | u64::from(*b);
    }
    Some((value, &data[len..]))
}

/// One track as the `Tracks` element describes it.
#[derive(Clone, Debug)]
struct Entry {
    number: u64,
    video: bool,
    codec: Codec,
    size: (u32, u32),
    channels: u16,
    sample_rate: u32,
    /// Nanoseconds per frame, when the file says.
    default_duration: Option<u64>,
    prefix: Vec<u8>,
}

/// The codec a `CodecID` names, from its private data.
fn codec(name: &str, private: Option<&[u8]>) -> Codec {
    match (name, private) {
        ("V_MPEG4/ISO/AVC", Some(record)) => Codec::Avc(record.to_vec()),
        ("A_MPEG/L3", _) => Codec::Mp3,
        ("A_VORBIS", Some(headers)) => Codec::Vorbis(headers.to_vec()),
        ("A_FLAC", Some(private)) => flac_stream_info(private).map_or_else(
            || Codec::Other("FLAC without STREAMINFO".into()),
            Codec::Flac,
        ),
        (name, _) if name.starts_with("A_AAC") => Codec::Other("AAC".into()),
        ("A_OPUS", _) => Codec::Other("Opus".into()),
        (name, _) => Codec::Other(name.to_string()),
    }
}

/// FLAC's `STREAMINFO` block from its private data (`fLaC` and the metadata blocks).
fn flac_stream_info(private: &[u8]) -> Option<Vec<u8>> {
    let mut rest = private.strip_prefix(b"fLaC")?;
    loop {
        let (head, body) = rest.split_at_checked(4)?;
        let len = usize::try_from(uint(&head[1..4])).ok()?;
        if head[0] & 0x7F == 0 {
            return (len == 34).then(|| body.get(..34).map(<[u8]>::to_vec))?;
        }
        if head[0] & 0x80 != 0 {
            return None;
        }
        rest = body.get(len..)?;
    }
}

/// A `TrackEntry`, or `None` for a track that is neither video nor audio, or is off.
fn entry(data: &[u8]) -> Option<Entry> {
    let mut number = None;
    let mut kind = 0;
    let mut enabled = true;
    let mut name = String::new();
    let mut private = None;
    let mut default_duration = None;
    let mut size = (0, 0);
    let mut channels = 1;
    let mut sample_rate = 8000.0;
    let mut prefix = Vec::new();
    let mut refused = None;
    for (id, body) in children(data) {
        match id {
            id::TRACK_NUMBER => number = Some(uint(body)),
            id::TRACK_TYPE => kind = uint(body),
            id::FLAG_ENABLED => enabled = uint(body) != 0,
            id::CODEC_ID => name = String::from_utf8_lossy(body).trim_end_matches('\0').into(),
            id::CODEC_PRIVATE => private = Some(body),
            id::DEFAULT_DURATION => default_duration = Some(uint(body)).filter(|d| *d > 0),
            id::VIDEO => {
                for (id, body) in children(body) {
                    match id {
                        id::PIXEL_WIDTH => size.0 = u32::try_from(uint(body)).unwrap_or(0),
                        id::PIXEL_HEIGHT => size.1 = u32::try_from(uint(body)).unwrap_or(0),
                        _ => {}
                    }
                }
            }
            id::AUDIO => {
                for (id, body) in children(body) {
                    match id {
                        id::SAMPLING_FREQUENCY => sample_rate = float(body),
                        id::CHANNELS => channels = u16::try_from(uint(body)).unwrap_or(0),
                        _ => {}
                    }
                }
            }
            id::CONTENT_ENCODINGS => {
                for (_, encoding) in children(body)
                    .into_iter()
                    .filter(|(id, _)| *id == id::CONTENT_ENCODING)
                {
                    for (id, body) in children(encoding) {
                        match id {
                            id::CONTENT_COMPRESSION => {
                                let mut algo = 0;
                                let mut settings: &[u8] = &[];
                                for (id, body) in children(body) {
                                    match id {
                                        id::CONTENT_COMP_ALGO => algo = uint(body),
                                        id::CONTENT_COMP_SETTINGS => settings = body,
                                        _ => {}
                                    }
                                }
                                if algo == 3 {
                                    prefix.extend_from_slice(settings);
                                } else {
                                    refused = Some(format!("{name} compressed (algorithm {algo})"));
                                }
                            }
                            id::CONTENT_ENCRYPTION => refused = Some(format!("{name} encrypted")),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let video = match kind {
        1 => true,
        2 => false,
        _ => return None,
    };
    if !enabled {
        return None;
    }
    let codec = match refused {
        Some(why) => Codec::Other(why),
        None => codec(&name, private),
    };
    Some(Entry {
        number: number?,
        video,
        codec,
        size,
        channels,
        // Rates are whole numbers of hertz in practice.
        sample_rate: format!("{sample_rate:.0}").parse().unwrap_or(0),
        default_duration,
        prefix,
    })
}

/// A track being filled from the clusters.
struct Building {
    entry: Entry,
    samples: Vec<Sample>,
    /// Laced runs whose later frames have no time of their own: (first index, count).
    laced: Vec<(usize, usize)>,
}

/// The time scale: ticks per second of block times, and a factor from block ticks.
#[derive(Clone, Copy)]
struct Scale {
    /// Nanoseconds per block tick (the segment's `TimestampScale`).
    nanos: u64,
    /// Ticks per second of the tracks' sample times.
    timescale: u32,
}

impl Scale {
    fn new(nanos: u64) -> Self {
        let nanos = nanos.max(1);
        match u32::try_from(1_000_000_000 / nanos) {
            Ok(timescale) if 1_000_000_000 % nanos == 0 && timescale > 0 => {
                Self { nanos, timescale }
            }
            // An odd scale: sample times in microseconds instead.
            _ => Self {
                nanos,
                timescale: 1_000_000,
            },
        }
    }

    /// Block ticks to sample ticks.
    fn ticks(&self, block: i64) -> i64 {
        if u64::from(self.timescale).checked_mul(self.nanos) == Some(1_000_000_000) {
            block
        } else {
            let nanos = i128::from(block) * i128::from(self.nanos);
            i64::try_from(nanos / 1000).unwrap_or(i64::MAX)
        }
    }

    /// Nanoseconds to sample ticks.
    fn nanos_to_ticks(&self, nanos: u64) -> i64 {
        let ticks = u128::from(nanos) * u128::from(self.timescale) / 1_000_000_000;
        i64::try_from(ticks).unwrap_or(i64::MAX)
    }
}

/// Opens `file` if it is a Matroska or WebM file. `Ok(None)` when it isn't.
///
/// # Errors
/// [`PlatformError::Os`] for one that doesn't read, or whose tracks come after its
/// first cluster.
pub(crate) fn open(mut file: File) -> Result<Option<Demuxed>, PlatformError> {
    let len = file.metadata().map_err(|e| io_error(&e))?.len();
    // A clone of a handle another reader has moved.
    file.rewind().map_err(|e| io_error(&e))?;
    let mut r = Reader {
        inner: BufReader::with_capacity(64 << 10, file),
        at: 0,
        len,
    };
    match r.header() {
        Ok(Some(h)) if h.id == id::EBML => {
            let body = r.body(&h)?;
            let doc = children(&body)
                .into_iter()
                .find(|(id, _)| *id == id::DOC_TYPE)
                .map(|(_, b)| {
                    String::from_utf8_lossy(b)
                        .trim_end_matches('\0')
                        .to_string()
                });
            if doc.is_some_and(|d| d != "matroska" && d != "webm") {
                return Ok(None);
            }
        }
        Ok(_) | Err(_) => return Ok(None),
    }
    let segment = loop {
        match r.header()? {
            Some(h) if h.id == id::SEGMENT => break h,
            Some(h) => {
                let end = h.end(len);
                r.seek(end)?;
            }
            None => return Err(invalid("no segment")),
        }
    };
    let segment_end = segment.end(len);
    let mut scale = Scale::new(1_000_000);
    let mut tracks: Option<(Option<Building>, Option<Building>)> = None;
    let mut pending = None;
    while r.at < segment_end {
        let h = match pending.take() {
            Some(h) => h,
            None => match r.header() {
                Ok(Some(h)) => h,
                // A file cut short ends here.
                Ok(None) | Err(_) => break,
            },
        };
        match h.id {
            id::INFO => {
                let body = r.body(&h)?;
                for (id, b) in children(&body) {
                    if id == id::TIMESTAMP_SCALE {
                        scale = Scale::new(uint(b));
                    }
                }
            }
            id::TRACKS => {
                let body = r.body(&h)?;
                let entries: Vec<Entry> = children(&body)
                    .into_iter()
                    .filter(|(id, _)| *id == id::TRACK_ENTRY)
                    .filter_map(|(_, b)| entry(b))
                    .collect();
                let pick = |video: bool| {
                    entries.iter().find(|e| e.video == video).map(|e| Building {
                        entry: e.clone(),
                        samples: Vec::new(),
                        laced: Vec::new(),
                    })
                };
                tracks = Some((pick(true), pick(false)));
            }
            id::CLUSTER => {
                let Some((video, audio)) = tracks.as_mut() else {
                    return Err(invalid("a cluster before the tracks"));
                };
                match cluster(&mut r, &h, segment_end, &scale, video, audio) {
                    Ok(next) => pending = next,
                    // A cluster cut short: keep what came before it.
                    Err(_) if r.at >= len.saturating_sub(1) || h.size.is_none() => break,
                    Err(err) => return Err(err),
                }
            }
            _ => match h.size {
                Some(_) => r.seek(h.end(segment_end))?,
                None => return Err(invalid("an unknown-size element that isn't a cluster")),
            },
        }
    }
    let Some((video, audio)) = tracks else {
        return Err(invalid("no tracks"));
    };
    let file = r.inner.into_inner();
    let finish = |b: Option<Building>| {
        b.map(|b| finish(b, &scale))
            .filter(|t| !t.samples.is_empty())
    };
    Ok(Some(Demuxed::new(file, finish(video), finish(audio))))
}

/// Reads one cluster's blocks into the tracks. An unknown-size cluster ends at the
/// next top-level element, whose header it returns.
fn cluster(
    r: &mut Reader,
    h: &Header,
    segment_end: u64,
    scale: &Scale,
    video: &mut Option<Building>,
    audio: &mut Option<Building>,
) -> Result<Option<Header>, PlatformError> {
    let end = h.end(segment_end);
    let mut base = 0_i64;
    while r.at < end {
        let Some(child) = r.header()? else {
            return Ok(None);
        };
        if h.size.is_none() && id::TOP_LEVEL.contains(&child.id) {
            return Ok(Some(child));
        }
        // A file cut short in the middle of an element: it ends before it.
        if child
            .size
            .is_some_and(|s| child.body.saturating_add(s) > r.len)
        {
            return Ok(None);
        }
        let child_end = child.end(end);
        match child.id {
            id::TIMESTAMP => base = i64::try_from(uint(&r.body(&child)?)).unwrap_or(0),
            id::SIMPLE_BLOCK => {
                block(r, child_end, base, None, scale, video, audio)?;
            }
            id::BLOCK_GROUP => {
                // The block, and whether anything says it refers to another frame.
                let mut found = None;
                let mut referenced = false;
                while r.at < child_end {
                    let Some(g) = r.header()? else {
                        break;
                    };
                    let g_end = g.end(child_end);
                    match g.id {
                        id::BLOCK => found = Some((r.at, g_end)),
                        id::REFERENCE_BLOCK => referenced = true,
                        _ => {}
                    }
                    r.seek(g_end)?;
                }
                if let Some((at, g_end)) = found {
                    let resume = r.at;
                    r.seek(at)?;
                    block(r, g_end, base, Some(!referenced), scale, video, audio)?;
                    r.seek(resume)?;
                }
            }
            _ => {}
        }
        r.seek(child_end)?;
    }
    Ok(None)
}

/// Reads one block's header and adds its frames to their track. `key` is `None` for a
/// `SimpleBlock`, whose flags say.
fn block(
    r: &mut Reader,
    end: u64,
    base: i64,
    key: Option<bool>,
    scale: &Scale,
    video: &mut Option<Building>,
    audio: &mut Option<Building>,
) -> Result<(), PlatformError> {
    let (number, _) = r.vint()?.ok_or_else(|| invalid("a block ends early"))?;
    let track = match (video.as_mut(), audio.as_mut()) {
        (Some(v), _) if v.entry.number == number => v,
        (_, Some(a)) if a.entry.number == number => a,
        _ => return Ok(()),
    };
    let rel = i16::from_be_bytes([r.need()?, r.need()?]);
    let flags = r.need()?;
    let key = key.unwrap_or(flags & 0x80 != 0) || !track.entry.video;
    let pts = scale.ticks(base.saturating_add(i64::from(rel)));
    let sizes = lace_sizes(r, flags, end)?;
    if track.samples.len() + sizes.len() > MAX_SAMPLES {
        return Err(invalid("more frames than a track may have"));
    }
    let first = track.samples.len();
    let step = track
        .entry
        .default_duration
        .map(|d| scale.nanos_to_ticks(d));
    let mut offset = r.at;
    for (i, size) in sizes.iter().enumerate() {
        let i_ticks = i64::try_from(i).unwrap_or(0);
        track.samples.push(Sample {
            offset,
            size: *size,
            dts: 0,
            pts: pts.saturating_add(step.unwrap_or(0).saturating_mul(i_ticks)),
            sync: key,
        });
        offset = offset.saturating_add(u64::from(*size));
    }
    if sizes.len() > 1 && step.is_none() {
        track.laced.push((first, sizes.len()));
    }
    Ok(())
}

/// The frame sizes of a block whose header has been read up to its lacing, from
/// `r.at` to `end`.
fn lace_sizes(r: &mut Reader, flags: u8, end: u64) -> Result<Vec<u32>, PlatformError> {
    let short = || invalid("a block's lacing doesn't fit it");
    let lacing = (flags >> 1) & 3;
    if lacing == 0 {
        let size = end.checked_sub(r.at).ok_or_else(short)?;
        return Ok(vec![u32::try_from(size).map_err(|_| short())?]);
    }
    let count = usize::from(r.need()?) + 1;
    let mut sizes: Vec<u64> = Vec::with_capacity(count);
    match lacing {
        // Xiph: each size as a run of 255s and a last byte.
        1 => {
            for _ in 1..count {
                let mut size = 0_u64;
                loop {
                    let b = r.need()?;
                    size += u64::from(b);
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        // EBML: the first size, then signed differences.
        3 => {
            let (first, _) = r.vint()?.ok_or_else(short)?;
            sizes.push(first);
            for _ in 2..count {
                let start = r.at;
                let (raw, _) = r.vint()?.ok_or_else(short)?;
                let len = u32::try_from(r.at - start).unwrap_or(1);
                let bias = (1_i64 << (7 * len - 1)) - 1;
                let delta = i64::try_from(raw).unwrap_or(0) - bias;
                let previous = i64::try_from(*sizes.last().unwrap_or(&0)).unwrap_or(0);
                let size = previous.checked_add(delta).ok_or_else(short)?;
                sizes.push(u64::try_from(size).map_err(|_| short())?);
            }
        }
        // Fixed: equal sizes.
        _ => {
            let total = end.checked_sub(r.at).ok_or_else(short)?;
            let each = total / u64::try_from(count).unwrap_or(1);
            sizes.resize(count - 1, each);
        }
    }
    let used = sizes
        .iter()
        .try_fold(0_u64, |sum, s| sum.checked_add(*s))
        .ok_or_else(short)?;
    let last = end
        .checked_sub(r.at)
        .and_then(|rest| rest.checked_sub(used))
        .ok_or_else(short)?;
    sizes.push(last);
    sizes
        .into_iter()
        .map(|s| u32::try_from(s).map_err(|_| short()))
        .collect()
}

/// A finished track: laced frames spread between their block and the next, and decode
/// times assigned.
fn finish(mut b: Building, scale: &Scale) -> Track {
    for &(first, count) in &b.laced {
        let start = b.samples[first].pts;
        let next = b
            .samples
            .get(first + count)
            .map_or(start, |s| s.pts)
            .max(start);
        let n = i64::try_from(count).unwrap_or(1);
        for i in 1..count {
            let i_ticks = i64::try_from(i).unwrap_or(0);
            let between =
                i128::from(next.saturating_sub(start)) * i128::from(i_ticks) / i128::from(n);
            b.samples[first + i].pts =
                start.saturating_add(i64::try_from(between).unwrap_or(i64::MAX));
        }
    }
    let mut times: Vec<i64> = b.samples.iter().map(|s| s.pts).collect();
    times.sort_unstable();
    for (s, dts) in b.samples.iter_mut().zip(times) {
        s.dts = dts;
    }
    Track {
        id: u32::try_from(b.entry.number).unwrap_or(0),
        video: b.entry.video,
        codec: b.entry.codec,
        timescale: scale.timescale,
        size: b.entry.size,
        channels: b.entry.channels,
        sample_rate: b.entry.sample_rate,
        samples: b.samples,
        prefix: b.entry.prefix,
    }
}

#[cfg(test)]
mod tests;
