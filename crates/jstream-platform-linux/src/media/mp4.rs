//! An MP4 and QuickTime (`.mov`) demuxer (ISO/IEC 14496-12, ADR-0072 (d)): the movie
//! box's sample tables, read once, then each track's samples by offset.
//!
//! It reads what playback needs of the first video track and the first audio track:
//! the codec configuration, and each sample's position, size, decode and presentation
//! times and whether it is a sync sample. The edit list's first entry shifts the
//! timeline so it starts at the first shown frame. Fragmented files (`moof`) are
//! refused: their samples aren't in the movie box.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use jstream_platform::PlatformError;

/// The largest movie box read: a few hours of 60 fps video stays far below it.
const MAX_MOOV: u64 = 256 << 20;
/// The most samples a track may list.
const MAX_SAMPLES: u64 = 20_000_000;

/// What `Unsupported` and `Os` errors name.
fn invalid(what: &str) -> PlatformError {
    PlatformError::Os {
        call: "MP4 demuxer",
        code: None,
        message: format!("a damaged MP4 file: {what}"),
    }
}

fn io_error(err: &std::io::Error) -> PlatformError {
    PlatformError::Os {
        call: "read",
        code: err.raw_os_error().and_then(|c| u32::try_from(c).ok()),
        message: err.to_string(),
    }
}

/// One sample: where it is and when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sample {
    pub(crate) offset: u64,
    pub(crate) size: u32,
    /// The decode time, in the track's time scale.
    pub(crate) dts: i64,
    /// The presentation time on the file's timeline (edit list applied), in the
    /// track's time scale.
    pub(crate) pts: i64,
    pub(crate) sync: bool,
}

/// A track's codec, as far as this crate decodes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
    /// H.264, with its `avcC` record.
    Avc(Vec<u8>),
    /// MPEG-1/2 Layer III audio.
    Mp3,
    /// FLAC, with its `STREAMINFO` block.
    Flac(Vec<u8>),
    /// Anything else, by its sample entry's four-character code or, for `mp4a`, its
    /// object type.
    Other(String),
}

/// One track.
#[derive(Clone, Debug)]
pub(crate) struct Track {
    pub(crate) id: u32,
    pub(crate) video: bool,
    pub(crate) codec: Codec,
    /// Ticks per second of the sample times.
    pub(crate) timescale: u32,
    /// Video: the sample entry's width and height.
    pub(crate) size: (u32, u32),
    /// Audio: channels and sample rate.
    pub(crate) channels: u16,
    pub(crate) sample_rate: u32,
    /// In decoding order.
    pub(crate) samples: Vec<Sample>,
}

impl Track {
    /// How long the track plays, in its time scale.
    pub(crate) fn duration(&self) -> i64 {
        self.samples
            .iter()
            .map(|s| s.pts)
            .max()
            .map_or(0, |last| last + self.frame_step())
    }

    /// A typical gap between samples, for the last one's duration.
    fn frame_step(&self) -> i64 {
        match self.samples.as_slice() {
            [.., a, b] => (b.dts - a.dts).max(0),
            _ => 0,
        }
    }

    /// The index to start decoding from to show `pts`: the last sync sample at or
    /// before it in decoding order, or the first sample. For a track without sync
    /// samples every sample is one.
    pub(crate) fn seek_index(&self, pts: i64) -> usize {
        let mut best = 0;
        for (i, s) in self.samples.iter().enumerate() {
            if s.sync && s.pts <= pts {
                best = i;
            }
            if s.dts > pts {
                break;
            }
        }
        best
    }
}

/// An opened MP4 file: its tracks, and the file to read samples from.
pub(crate) struct Mp4 {
    file: File,
    pub(crate) video: Option<Track>,
    pub(crate) audio: Option<Track>,
}

impl Mp4 {
    /// Opens `file` if it is an MP4 or QuickTime file. `Ok(None)` when it isn't.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a fragmented file, and [`PlatformError::Os`]
    /// for one that doesn't read.
    pub(crate) fn open(mut file: File) -> Result<Option<Self>, PlatformError> {
        let len = file.metadata().map_err(|e| io_error(&e))?.len();
        let mut at = 0_u64;
        let mut moov = None;
        let mut saw_ftyp = false;
        let mut fragmented = false;
        while at + 8 <= len {
            file.seek(SeekFrom::Start(at)).map_err(|e| io_error(&e))?;
            let mut head = [0_u8; 16];
            file.read_exact(&mut head[..8]).map_err(|e| io_error(&e))?;
            let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
            let kind = [head[4], head[5], head[6], head[7]];
            let mut header = 8;
            if size == 1 {
                file.read_exact(&mut head[8..16])
                    .map_err(|e| io_error(&e))?;
                size = u64::from_be_bytes([
                    head[8], head[9], head[10], head[11], head[12], head[13], head[14], head[15],
                ]);
                header = 16;
            } else if size == 0 {
                size = len - at;
            }
            if at == 0
                && !matches!(
                    &kind,
                    b"ftyp" | b"moov" | b"wide" | b"free" | b"mdat" | b"skip"
                )
            {
                return Ok(None);
            }
            if size < header {
                return Err(invalid("a box shorter than its header"));
            }
            match &kind {
                b"ftyp" => saw_ftyp = true,
                b"moof" => fragmented = true,
                b"moov" => {
                    let body = size - header;
                    if body > MAX_MOOV {
                        return Err(invalid("a movie box too large to read"));
                    }
                    let mut data = vec![0; usize::try_from(body).unwrap_or(0)];
                    file.read_exact(&mut data).map_err(|e| io_error(&e))?;
                    moov = Some(data);
                }
                _ => {}
            }
            at = at.saturating_add(size);
        }
        let Some(moov) = moov else {
            if saw_ftyp {
                return Err(invalid("no movie box"));
            }
            return Ok(None);
        };
        let movie = parse_moov(&moov)?;
        if fragmented && movie.iter().all(|t| t.samples.is_empty()) {
            return Err(PlatformError::Unsupported {
                what: "fragmented MP4 files on Linux".into(),
            });
        }
        let video = movie
            .iter()
            .find(|t| t.video && !t.samples.is_empty())
            .cloned();
        let audio = movie
            .iter()
            .find(|t| !t.video && !t.samples.is_empty())
            .cloned();
        Ok(Some(Self { file, video, audio }))
    }

    /// Reads one sample's bytes.
    pub(crate) fn read(&mut self, sample: &Sample) -> Result<Vec<u8>, PlatformError> {
        self.file
            .seek(SeekFrom::Start(sample.offset))
            .map_err(|e| io_error(&e))?;
        let mut data = vec![0; usize::try_from(sample.size).unwrap_or(0)];
        self.file.read_exact(&mut data).map_err(|e| io_error(&e))?;
        Ok(data)
    }
}

/// A box's type and body.
type Child<'a> = ([u8; 4], &'a [u8]);

/// A reader over one box's body.
struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], PlatformError> {
        let out = self
            .data
            .get(self.at..self.at.saturating_add(n))
            .ok_or_else(|| invalid("a box ends early"))?;
        self.at += n;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, PlatformError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, PlatformError> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, PlatformError> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, PlatformError> {
        let b = self.bytes(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// The child boxes from here on: `(type, body)`.
    fn boxes(&mut self) -> Result<Vec<Child<'a>>, PlatformError> {
        let mut out = Vec::new();
        while self.data.len() - self.at.min(self.data.len()) >= 8 {
            let size = u64::from(self.u32()?);
            let kind: [u8; 4] = self.bytes(4)?.try_into().map_err(|_| invalid("box type"))?;
            let (size, header) = match size {
                1 => (self.u64()?, 16),
                0 => ((self.data.len() - self.at + 8) as u64, 8),
                n => (n, 8),
            };
            let body = usize::try_from(size.saturating_sub(header))
                .map_err(|_| invalid("a box too large"))?;
            if size < header {
                return Err(invalid("a box shorter than its header"));
            }
            out.push((kind, self.bytes(body)?));
        }
        Ok(out)
    }
}

fn child<'a>(boxes: &[Child<'a>], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes.iter().find(|(k, _)| k == kind).map(|(_, b)| *b)
}

/// The movie box's tracks.
fn parse_moov(moov: &[u8]) -> Result<Vec<Track>, PlatformError> {
    let boxes = Cursor::new(moov).boxes()?;
    let movie_timescale = match child(&boxes, b"mvhd") {
        Some(mvhd) => {
            let mut c = Cursor::new(mvhd);
            let version = c.u8()?;
            c.bytes(3)?;
            c.bytes(if version == 1 { 16 } else { 8 })?;
            c.u32()?
        }
        None => 1_000,
    };
    let mut tracks = Vec::new();
    for (kind, body) in &boxes {
        if kind != b"trak" {
            continue;
        }
        match parse_trak(body, movie_timescale) {
            Ok(Some(track)) => tracks.push(track),
            Ok(None) => {}
            Err(err) => tracing::debug!(%err, "skipping an MP4 track that doesn't parse"),
        }
    }
    Ok(tracks)
}

/// One track box, or `None` for a track that isn't video or audio.
fn parse_trak(trak: &[u8], movie_timescale: u32) -> Result<Option<Track>, PlatformError> {
    let boxes = Cursor::new(trak).boxes()?;
    let tkhd = child(&boxes, b"tkhd").ok_or_else(|| invalid("a track without tkhd"))?;
    let mut c = Cursor::new(tkhd);
    let version = c.u8()?;
    c.bytes(3)?;
    c.bytes(if version == 1 { 16 } else { 8 })?;
    let id = c.u32()?;

    let mdia = Cursor::new(child(&boxes, b"mdia").ok_or_else(|| invalid("no mdia"))?).boxes()?;
    let hdlr = child(&mdia, b"hdlr").ok_or_else(|| invalid("no hdlr"))?;
    let handler = hdlr.get(8..12).ok_or_else(|| invalid("hdlr"))?;
    let video = match handler {
        b"vide" => true,
        b"soun" => false,
        _ => return Ok(None),
    };
    let mdhd = child(&mdia, b"mdhd").ok_or_else(|| invalid("no mdhd"))?;
    let mut c = Cursor::new(mdhd);
    let version = c.u8()?;
    c.bytes(3)?;
    c.bytes(if version == 1 { 16 } else { 8 })?;
    let timescale = c.u32()?;
    if timescale == 0 {
        return Err(invalid("a zero time scale"));
    }
    let minf = Cursor::new(child(&mdia, b"minf").ok_or_else(|| invalid("no minf"))?).boxes()?;
    let stbl = Cursor::new(child(&minf, b"stbl").ok_or_else(|| invalid("no stbl"))?).boxes()?;

    let stsd = child(&stbl, b"stsd").ok_or_else(|| invalid("no stsd"))?;
    let entry = sample_entry(stsd, video)?;

    // The edit list's shift: an empty first edit delays the track, and the first
    // media edit says where in the media the timeline starts.
    let mut shift: i64 = 0;
    if let Some(edts) = child(&boxes, b"edts") {
        let edts = Cursor::new(edts).boxes()?;
        if let Some(elst) = child(&edts, b"elst") {
            shift = edit_shift(elst, movie_timescale, timescale)?;
        }
    }

    let samples = sample_table(&stbl, shift)?;
    Ok(Some(Track {
        id,
        video,
        codec: entry.codec,
        timescale,
        size: entry.size,
        channels: entry.channels,
        sample_rate: entry.sample_rate,
        samples,
    }))
}

/// What a sample description says.
struct Entry {
    codec: Codec,
    size: (u32, u32),
    channels: u16,
    sample_rate: u32,
}

/// The first entry of a sample description box.
fn sample_entry(stsd: &[u8], video: bool) -> Result<Entry, PlatformError> {
    let mut c = Cursor::new(stsd);
    c.bytes(8)?;
    let entries = c.boxes()?;
    let (kind, body) = entries.first().ok_or_else(|| invalid("an empty stsd"))?;
    let fourcc = String::from_utf8_lossy(kind).into_owned();
    let mut c = Cursor::new(body);
    c.bytes(8)?; // reserved, data_reference_index
    if video {
        c.bytes(16)?;
        let width = u32::from(c.u16()?);
        let height = u32::from(c.u16()?);
        c.bytes(50)?;
        let children = c.boxes().unwrap_or_default();
        let codec = match kind {
            b"avc1" | b"avc3" => match child(&children, b"avcC") {
                Some(avcc) => Codec::Avc(avcc.to_vec()),
                None => Codec::Other(format!("{fourcc} without avcC")),
            },
            _ => Codec::Other(fourcc),
        };
        return Ok(Entry {
            codec,
            size: (width, height),
            channels: 0,
            sample_rate: 0,
        });
    }
    let version = c.u16()?;
    c.bytes(6)?;
    let mut channels = c.u16()?;
    c.bytes(6)?;
    let mut sample_rate = c.u32()? >> 16;
    match version {
        1 => {
            c.bytes(16)?;
        }
        2 => {
            c.bytes(4)?;
            let rate = f64::from_bits(c.u64()?);
            channels = u16::try_from(c.u32()?).unwrap_or(channels);
            c.bytes(20)?;
            if rate.is_finite() && rate > 0.0 && rate < 1e6 {
                sample_rate = rate as u32;
            }
        }
        _ => {}
    }
    let children = c.boxes().unwrap_or_default();
    let codec = match kind {
        b".mp3" => Codec::Mp3,
        b"mp4a" => match child(&children, b"esds").map(esds_object_type) {
            Some(Ok(0x69 | 0x6B)) => Codec::Mp3,
            Some(Ok(0x40 | 0x66 | 0x67 | 0x68)) => Codec::Other("AAC".into()),
            Some(Ok(other)) => Codec::Other(format!("mp4a object type 0x{other:02X}")),
            _ => Codec::Other("mp4a".into()),
        },
        b"fLaC" => match child(&children, b"dfLa") {
            // Version and flags, then the first metadata block, STREAMINFO: a 4-byte
            // header and 34 bytes.
            Some(dfla) if dfla.len() >= 42 && dfla[4] & 0x7F == 0 => {
                Codec::Flac(dfla[8..42].to_vec())
            }
            _ => Codec::Other("fLaC without dfLa".into()),
        },
        _ => Codec::Other(fourcc),
    };
    Ok(Entry {
        codec,
        size: (0, 0),
        channels,
        sample_rate,
    })
}

/// The object type of an `esds` box's decoder configuration.
fn esds_object_type(esds: &[u8]) -> Result<u8, PlatformError> {
    let mut c = Cursor::new(esds);
    c.bytes(4)?;
    // ES_Descriptor: tag 3, a length, ES_ID, flags.
    let read_len = |c: &mut Cursor<'_>| -> Result<(), PlatformError> {
        for _ in 0..4 {
            if c.u8()? & 0x80 == 0 {
                break;
            }
        }
        Ok(())
    };
    if c.u8()? != 3 {
        return Err(invalid("esds"));
    }
    read_len(&mut c)?;
    c.bytes(2)?;
    let flags = c.u8()?;
    if flags & 0x80 != 0 {
        c.bytes(2)?;
    }
    if flags & 0x40 != 0 {
        let n = usize::from(c.u8()?);
        c.bytes(n)?;
    }
    if flags & 0x20 != 0 {
        c.bytes(2)?;
    }
    if c.u8()? != 4 {
        return Err(invalid("esds"));
    }
    read_len(&mut c)?;
    c.u8()
}

/// How far the edit list moves the media's times: the presentation time of media time
/// `t` is `t + shift`.
fn edit_shift(elst: &[u8], movie_timescale: u32, timescale: u32) -> Result<i64, PlatformError> {
    let mut c = Cursor::new(elst);
    let version = c.u8()?;
    c.bytes(3)?;
    let count = c.u32()?;
    let mut delay: i64 = 0;
    for _ in 0..count.min(64) {
        let (duration, media_time) = if version == 1 {
            (
                c.u64()?,
                i64::from_be_bytes(c.bytes(8)?.try_into().map_err(|_| invalid("elst"))?),
            )
        } else {
            (u64::from(c.u32()?), i64::from(c.u32()? as i32))
        };
        c.bytes(4)?;
        if media_time == -1 {
            // An empty edit: the track starts later.
            let ticks =
                i128::from(duration) * i128::from(timescale) / i128::from(movie_timescale.max(1));
            delay += i64::try_from(ticks).unwrap_or(0);
            continue;
        }
        return Ok(delay - media_time);
    }
    Ok(delay)
}

/// Every sample of a sample table box, in decoding order.
fn sample_table(stbl: &[Child<'_>], shift: i64) -> Result<Vec<Sample>, PlatformError> {
    // Sizes.
    let mut sizes = Vec::new();
    if let Some(stsz) = child(stbl, b"stsz") {
        let mut c = Cursor::new(stsz);
        c.bytes(4)?;
        let fixed = c.u32()?;
        let count = c.u32()?;
        if u64::from(count) > MAX_SAMPLES {
            return Err(invalid("too many samples"));
        }
        if fixed != 0 {
            sizes = vec![fixed; count as usize];
        } else {
            sizes.reserve(count as usize);
            for _ in 0..count {
                sizes.push(c.u32()?);
            }
        }
    } else if let Some(stz2) = child(stbl, b"stz2") {
        let mut c = Cursor::new(stz2);
        c.bytes(7)?;
        let field = c.u8()?;
        let count = c.u32()?;
        if u64::from(count) > MAX_SAMPLES {
            return Err(invalid("too many samples"));
        }
        match field {
            4 => {
                let packed = c.bytes(usize::try_from(count.div_ceil(2)).unwrap_or(0))?;
                for i in 0..count as usize {
                    let b = packed[i / 2];
                    sizes.push(u32::from(if i % 2 == 0 { b >> 4 } else { b & 0xF }));
                }
            }
            8 => {
                for _ in 0..count {
                    sizes.push(u32::from(c.u8()?));
                }
            }
            16 => {
                for _ in 0..count {
                    sizes.push(u32::from(c.u16()?));
                }
            }
            _ => return Err(invalid("stz2 field size")),
        }
    }
    let count = sizes.len();
    if count == 0 {
        return Ok(Vec::new());
    }

    // Chunk offsets.
    let mut chunks = Vec::new();
    if let Some(stco) = child(stbl, b"stco") {
        let mut c = Cursor::new(stco);
        c.bytes(4)?;
        let n = c.u32()?;
        for _ in 0..n.min(MAX_SAMPLES as u32) {
            chunks.push(u64::from(c.u32()?));
        }
    } else if let Some(co64) = child(stbl, b"co64") {
        let mut c = Cursor::new(co64);
        c.bytes(4)?;
        let n = c.u32()?;
        for _ in 0..n.min(MAX_SAMPLES as u32) {
            chunks.push(c.u64()?);
        }
    }
    // Samples per chunk.
    let stsc = child(stbl, b"stsc").ok_or_else(|| invalid("no stsc"))?;
    let mut c = Cursor::new(stsc);
    c.bytes(4)?;
    let n = c.u32()?;
    let mut runs = Vec::new();
    for _ in 0..n.min(MAX_SAMPLES as u32) {
        let first = c.u32()?;
        let per = c.u32()?;
        c.u32()?;
        runs.push((first.max(1), per));
    }
    let mut offsets = Vec::with_capacity(count);
    'chunks: for (index, &start) in chunks.iter().enumerate() {
        let chunk = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let per = runs
            .iter()
            .rev()
            .find(|(first, _)| *first <= chunk)
            .map_or(0, |(_, per)| *per);
        let mut at = start;
        for _ in 0..per {
            let Some(&size) = sizes.get(offsets.len()) else {
                break 'chunks;
            };
            offsets.push(at);
            at += u64::from(size);
        }
    }
    if offsets.len() < count {
        return Err(invalid("the chunk table is shorter than the sample table"));
    }

    // Decode times.
    let stts = child(stbl, b"stts").ok_or_else(|| invalid("no stts"))?;
    let mut c = Cursor::new(stts);
    c.bytes(4)?;
    let n = c.u32()?;
    let mut dts = Vec::with_capacity(count);
    let mut t: i64 = 0;
    for _ in 0..n {
        let run = c.u32()?;
        let delta = i64::from(c.u32()?);
        for _ in 0..run {
            if dts.len() == count {
                break;
            }
            dts.push(t);
            t += delta;
        }
    }
    while dts.len() < count {
        dts.push(t);
    }

    // Composition offsets.
    let mut cts = vec![0_i64; count];
    if let Some(ctts) = child(stbl, b"ctts") {
        let mut c = Cursor::new(ctts);
        let version = c.u8()?;
        c.bytes(3)?;
        let n = c.u32()?;
        let mut i = 0;
        for _ in 0..n {
            let run = c.u32()?;
            let raw = c.u32()?;
            let offset = if version == 1 {
                i64::from(raw as i32)
            } else {
                i64::from(raw)
            };
            for _ in 0..run {
                if i == count {
                    break;
                }
                cts[i] = offset;
                i += 1;
            }
        }
    }

    // Sync samples: every one when the table is absent.
    let mut sync = vec![true; count];
    if let Some(stss) = child(stbl, b"stss") {
        sync = vec![false; count];
        let mut c = Cursor::new(stss);
        c.bytes(4)?;
        let n = c.u32()?;
        for _ in 0..n {
            let index = c.u32()? as usize;
            if (1..=count).contains(&index) {
                sync[index - 1] = true;
            }
        }
    }

    Ok((0..count)
        .map(|i| Sample {
            offset: offsets[i],
            size: sizes[i],
            dts: dts[i],
            pts: dts[i] + cts[i] + shift,
            sync: sync[i],
        })
        .collect())
}

#[cfg(test)]
pub(crate) mod tests;
