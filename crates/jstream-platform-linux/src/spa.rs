//! SPA pods for the PipeWire video stream: the parameters JStream offers (formats, with
//! DMA-BUF modifiers on the GPU path, buffer types, metadata), the format PipeWire
//! settles on, and turning a mapped buffer into a CPU frame (ADR-0060 §3). The audio
//! streams' pods are in [`audio`].
//!
//! libspa builds and parses pods with inline header functions, which no library
//! exports, so these are written here against the pod layout in `<spa/pod/pod.h>`:
//! each pod is a `u32` body size and a `u32` type, then the body, padded to 8 bytes.
//! Platform-independent, so it is tested everywhere.

use jstream_platform::media::{
    ColorInfo, FrameSize, MediaError, MediaTime, PixelFormat, Plane, Timebase, VideoFrame,
    VideoFrameInfo,
};

pub(crate) mod audio;

// `enum spa_type` in <spa/utils/type.h>.
const TYPE_ID: u32 = 3;
const TYPE_INT: u32 = 4;
const TYPE_LONG: u32 = 5;
const TYPE_RECTANGLE: u32 = 10;
const TYPE_FRACTION: u32 = 11;
const TYPE_OBJECT: u32 = 15;
const TYPE_CHOICE: u32 = 19;
const TYPE_OBJECT_FORMAT: u32 = 0x40003;
const TYPE_OBJECT_PARAM_BUFFERS: u32 = 0x40004;
const TYPE_OBJECT_PARAM_META: u32 = 0x40005;

// `enum spa_choice_type` in <spa/pod/pod.h>.
const CHOICE_NONE: u32 = 0;
const CHOICE_RANGE: u32 = 1;
const CHOICE_ENUM: u32 = 3;

// Property flags in <spa/pod/pod.h>.
/// `SPA_POD_PROP_FLAG_MANDATORY`.
const PROP_MANDATORY: u32 = 1 << 3;
/// `SPA_POD_PROP_FLAG_DONT_FIXATE`.
const PROP_DONT_FIXATE: u32 = 1 << 4;

// `enum spa_param_type` in <spa/param/param.h>.
/// `SPA_PARAM_EnumFormat`.
pub(crate) const PARAM_ENUM_FORMAT: u32 = 3;
/// `SPA_PARAM_Format`: the negotiated format, in `param_changed`.
pub(crate) const PARAM_FORMAT: u32 = 4;
const PARAM_BUFFERS: u32 = 5;
const PARAM_META: u32 = 6;

// `enum spa_format` in <spa/param/format.h>.
const FORMAT_MEDIA_TYPE: u32 = 1;
const FORMAT_MEDIA_SUBTYPE: u32 = 2;
const FORMAT_VIDEO_FORMAT: u32 = 0x20001;
const FORMAT_VIDEO_MODIFIER: u32 = 0x20002;
const FORMAT_VIDEO_SIZE: u32 = 0x20003;
const FORMAT_VIDEO_FRAMERATE: u32 = 0x20004;
const MEDIA_TYPE_VIDEO: u32 = 2;
const MEDIA_SUBTYPE_RAW: u32 = 1;

// <spa/param/buffers.h> and <spa/buffer/buffer.h>, <spa/buffer/meta.h>.
const BUFFERS_DATA_TYPE: u32 = 6;
const META_TYPE: u32 = 1;
const META_SIZE: u32 = 2;
/// `SPA_DATA_MemPtr`.
pub(crate) const DATA_MEM_PTR: u32 = 1;
/// `SPA_DATA_MemFd`.
pub(crate) const DATA_MEM_FD: u32 = 2;
/// `SPA_DATA_DmaBuf`.
pub(crate) const DATA_DMA_BUF: u32 = 3;
/// `SPA_META_Header`.
pub(crate) const META_HEADER: u32 = 1;
/// `SPA_META_VideoCrop`.
pub(crate) const META_VIDEO_CROP: u32 = 2;
/// `sizeof(struct spa_meta_header)`.
pub(crate) const META_HEADER_SIZE: u32 = 32;
/// `sizeof(struct spa_meta_region)`.
pub(crate) const META_REGION_SIZE: u32 = 16;

/// The raw video formats JStream takes (`enum spa_video_format` in
/// <spa/param/video/raw.h>): 8-bit RGB, which every portal backend sends and the
/// compositor samples directly. The `x` formats' fourth byte is padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub(crate) enum SpaVideoFormat {
    /// `SPA_VIDEO_FORMAT_RGBx`
    Rgbx = 7,
    /// `SPA_VIDEO_FORMAT_BGRx`
    Bgrx = 8,
    /// `SPA_VIDEO_FORMAT_RGBA`
    Rgba = 11,
    /// `SPA_VIDEO_FORMAT_BGRA`
    Bgra = 12,
}

impl SpaVideoFormat {
    /// Offered in this order, the compositors' usual one first.
    pub(crate) const OFFERED: [Self; 4] = [Self::Bgrx, Self::Bgra, Self::Rgbx, Self::Rgba];

    fn from_id(id: u32) -> Option<Self> {
        Self::OFFERED.into_iter().find(|f| *f as u32 == id)
    }

    /// The engine's pixel format for frames in this one.
    pub(crate) fn pixel_format(self) -> PixelFormat {
        match self {
            Self::Bgrx | Self::Bgra => PixelFormat::Bgra8,
            Self::Rgbx | Self::Rgba => PixelFormat::Rgba8,
        }
    }

    /// Whether the fourth byte is padding, to be made opaque.
    pub(crate) fn padded(self) -> bool {
        matches!(self, Self::Bgrx | Self::Rgbx)
    }
}

/// The format PipeWire settled on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VideoFormat {
    pub(crate) format: SpaVideoFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// Frames per second as a fraction; `0/1` for a variable rate.
    pub(crate) framerate: (u32, u32),
    /// The DMA-BUF layout's DRM format modifier, when PipeWire settled on DMA-BUF
    /// buffers; `None` for shared memory.
    pub(crate) modifier: Option<u64>,
}

/// A pod under construction. Backed by `u64`s so it is 8-byte aligned, as libspa
/// expects of the pods it reads.
#[derive(Debug, Default)]
struct Builder {
    bytes: Vec<u8>,
}

impl Builder {
    fn u32(&mut self, v: u32) {
        self.bytes.extend_from_slice(&v.to_ne_bytes());
    }

    fn pad(&mut self) {
        let to = self.bytes.len().next_multiple_of(8);
        self.bytes.resize(to, 0);
    }

    /// A pod with `ty` and `body`, padded.
    fn pod(&mut self, ty: u32, body: &[u8]) {
        self.u32(u32::try_from(body.len()).unwrap_or(u32::MAX));
        self.u32(ty);
        self.bytes.extend_from_slice(body);
        self.pad();
    }

    /// An object of `object_type` for param `id`, with `props` (key, pod bytes).
    fn object(object_type: u32, id: u32, props: &[(u32, Vec<u8>)]) -> Pod {
        let flagged: Vec<(u32, u32, Vec<u8>)> = props
            .iter()
            .map(|(key, value)| (*key, 0, value.clone()))
            .collect();
        Self::object_flagged(object_type, id, &flagged)
    }

    /// As [`object`](Self::object), with each property's flags: (key, flags, pod bytes).
    fn object_flagged(object_type: u32, id: u32, props: &[(u32, u32, Vec<u8>)]) -> Pod {
        let mut body = Self::default();
        body.u32(object_type);
        body.u32(id);
        for (key, flags, value) in props {
            body.u32(*key);
            body.u32(*flags);
            body.bytes.extend_from_slice(value);
        }
        let mut b = Self::default();
        b.pod(TYPE_OBJECT, &body.bytes);
        Pod::from_bytes(&b.bytes)
    }
}

/// One value pod's bytes.
fn value(ty: u32, words: &[u32]) -> Vec<u8> {
    let mut b = Builder::default();
    let body: Vec<u8> = words.iter().flat_map(|w| w.to_ne_bytes()).collect();
    b.pod(ty, &body);
    b.bytes
}

/// A choice pod's bytes: `kind` over values of `child_type`, each `child_words` long.
fn choice(kind: u32, child_type: u32, child_words: usize, values: &[u32]) -> Vec<u8> {
    let mut body = Builder::default();
    body.u32(kind);
    body.u32(0); // flags
    body.u32(u32::try_from(child_words * 4).unwrap_or(u32::MAX));
    body.u32(child_type);
    for v in values {
        body.u32(*v);
    }
    let mut b = Builder::default();
    b.pod(TYPE_CHOICE, &body.bytes);
    b.bytes
}

/// An Enum choice pod's bytes over `Long` values (64-bit, such as DRM modifiers): the
/// default, then the alternatives.
fn choice_long(values: &[u64]) -> Vec<u8> {
    let mut body = Builder::default();
    body.u32(CHOICE_ENUM);
    body.u32(0); // flags
    body.u32(8);
    body.u32(TYPE_LONG);
    for v in values {
        body.bytes.extend_from_slice(&v.to_ne_bytes());
    }
    let mut b = Builder::default();
    b.pod(TYPE_CHOICE, &body.bytes);
    b.bytes
}

/// A finished pod, 8-byte aligned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pod {
    words: Vec<u64>,
    len: usize,
}

impl Pod {
    fn from_bytes(bytes: &[u8]) -> Self {
        let mut words = vec![0u64; bytes.len().div_ceil(8)];
        for (w, chunk) in words.iter_mut().zip(bytes.chunks(8)) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            *w = u64::from_ne_bytes(b);
        }
        Self {
            words,
            len: bytes.len(),
        }
    }

    /// The pod's bytes.
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.words
            .iter()
            .flat_map(|w| w.to_ne_bytes())
            .take(self.len)
            .collect()
    }

    /// A pointer to the pod, for `pw_stream_connect` and `pw_stream_update_params`;
    /// valid while `self` is.
    pub(crate) fn as_ptr(&self) -> *const std::ffi::c_void {
        self.words.as_ptr().cast()
    }
}

/// The `EnumFormat` param JStream offers for shared memory: raw video in one of
/// [`SpaVideoFormat::OFFERED`], any size up to 8192², any frame rate up to 360/1
/// (the source's own, 60/1 preferred).
pub(crate) fn enum_format() -> Pod {
    let mut formats = vec![SpaVideoFormat::OFFERED[0] as u32]; // the default, then the choices
    formats.extend(SpaVideoFormat::OFFERED.iter().map(|f| *f as u32));
    let [size, rate] = size_and_rate();
    Builder::object(
        TYPE_OBJECT_FORMAT,
        PARAM_ENUM_FORMAT,
        &[
            (FORMAT_MEDIA_TYPE, value(TYPE_ID, &[MEDIA_TYPE_VIDEO])),
            (FORMAT_MEDIA_SUBTYPE, value(TYPE_ID, &[MEDIA_SUBTYPE_RAW])),
            (
                FORMAT_VIDEO_FORMAT,
                choice(CHOICE_ENUM, TYPE_ID, 1, &formats),
            ),
            size,
            rate,
        ],
    )
}

/// The size and frame-rate properties every offer has.
fn size_and_rate() -> [(u32, Vec<u8>); 2] {
    [
        (
            FORMAT_VIDEO_SIZE,
            choice(
                CHOICE_RANGE,
                TYPE_RECTANGLE,
                2,
                &[1920, 1080, 1, 1, 8192, 8192],
            ),
        ),
        (
            FORMAT_VIDEO_FRAMERATE,
            choice(CHOICE_RANGE, TYPE_FRACTION, 2, &[60, 1, 0, 1, 360, 1]),
        ),
    ]
}

/// The `EnumFormat` params JStream offers, best first: on the GPU path one DMA-BUF
/// offer per format in `dmabuf` that has modifiers (the format fixed, its modifiers an
/// Enum choice flagged mandatory and don't-fixate, as PipeWire's DMA-BUF negotiation
/// asks of a consumer: the producer picks the modifier it allocates with), then the
/// shared-memory offer, which every producer takes and which is the CPU fallback.
pub(crate) fn enum_formats(dmabuf: &[(SpaVideoFormat, Vec<u64>)]) -> Vec<Pod> {
    let mut offers: Vec<Pod> = dmabuf
        .iter()
        .filter(|(_, modifiers)| !modifiers.is_empty())
        .map(|(format, modifiers)| {
            let mut choices = vec![modifiers[0]]; // the default, then the choices
            choices.extend_from_slice(modifiers);
            let [size, rate] = size_and_rate();
            Builder::object_flagged(
                TYPE_OBJECT_FORMAT,
                PARAM_ENUM_FORMAT,
                &[
                    (FORMAT_MEDIA_TYPE, 0, value(TYPE_ID, &[MEDIA_TYPE_VIDEO])),
                    (
                        FORMAT_MEDIA_SUBTYPE,
                        0,
                        value(TYPE_ID, &[MEDIA_SUBTYPE_RAW]),
                    ),
                    (FORMAT_VIDEO_FORMAT, 0, value(TYPE_ID, &[*format as u32])),
                    (
                        FORMAT_VIDEO_MODIFIER,
                        PROP_MANDATORY | PROP_DONT_FIXATE,
                        choice_long(&choices),
                    ),
                    (size.0, 0, size.1),
                    (rate.0, 0, rate.1),
                ],
            )
        })
        .collect();
    offers.push(enum_format());
    offers
}

/// The params to set once a format is settled: DMA-BUF buffers for a format with a
/// modifier (`dmabuf`), else buffers in shared or plain memory (mapped by PipeWire, so a
/// CPU copy can read them); and the header and crop metadata.
pub(crate) fn buffer_params(dmabuf: bool) -> [Pod; 3] {
    let data_types = if dmabuf {
        1 << DATA_DMA_BUF
    } else {
        (1 << DATA_MEM_PTR) | (1 << DATA_MEM_FD)
    };
    [
        Builder::object(
            TYPE_OBJECT_PARAM_BUFFERS,
            PARAM_BUFFERS,
            &[(BUFFERS_DATA_TYPE, value(TYPE_INT, &[data_types]))],
        ),
        meta(META_HEADER, META_HEADER_SIZE),
        meta(META_VIDEO_CROP, META_REGION_SIZE),
    ]
}

fn meta(ty: u32, size: u32) -> Pod {
    Builder::object(
        TYPE_OBJECT_PARAM_META,
        PARAM_META,
        &[
            (META_TYPE, value(TYPE_ID, &[ty])),
            (META_SIZE, value(TYPE_INT, &[size])),
        ],
    )
}

/// Reads pods out of a byte slice in native byte order, bounds-checked: PipeWire's
/// pods come from another process.
struct PodReader<'a> {
    data: &'a [u8],
}

impl PodReader<'_> {
    fn u32_at(&self, at: usize) -> Option<u32> {
        let b = self.data.get(at..at.checked_add(4)?)?;
        Some(u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// The first value of a property: the value itself, or a choice's default.
/// Returns the value type and its words' start.
fn first_value(r: &PodReader<'_>, pod_at: usize) -> Option<(u32, usize, usize)> {
    let size = usize::try_from(r.u32_at(pod_at)?).ok()?;
    let ty = r.u32_at(pod_at + 4)?;
    let body = pod_at + 8;
    if body.checked_add(size)? > r.data.len() {
        return None;
    }
    if ty == TYPE_CHOICE {
        // choice type, flags, child size, child type, values.
        let kind = r.u32_at(body)?;
        let child_size = usize::try_from(r.u32_at(body + 8)?).ok()?;
        let child_type = r.u32_at(body + 12)?;
        if !matches!(kind, CHOICE_NONE | CHOICE_RANGE | CHOICE_ENUM | 2 | 4)
            || size < 16 + child_size
        {
            return None;
        }
        return Some((child_type, body + 16, child_size));
    }
    Some((ty, body, size))
}

/// Parses the `Format` param PipeWire settled on. `None` when it is not raw video in a
/// format JStream offered. A DMA-BUF format's modifier is the `Long` value (or a
/// choice's default) of its modifier property.
pub(crate) fn parse_format(pod: &[u8]) -> Option<VideoFormat> {
    let r = PodReader { data: pod };
    let size = usize::try_from(r.u32_at(0)?).ok()?;
    if r.u32_at(4)? != TYPE_OBJECT || 8usize.checked_add(size)? > pod.len() || size < 8 {
        return None;
    }
    if r.u32_at(8)? != TYPE_OBJECT_FORMAT {
        return None;
    }
    let end = 8 + size;
    let mut at = 16; // past the object type and id
    let (mut media_type, mut subtype, mut format, mut dims, mut rate, mut modifier) =
        (None, None, None, None, (0, 1), None);
    while at + 16 <= end {
        let key = r.u32_at(at)?;
        let value_at = at + 8;
        let value_size = usize::try_from(r.u32_at(value_at)?).ok()?;
        let (ty, words, len) = first_value(&r, value_at)?;
        match (key, ty, len) {
            (FORMAT_MEDIA_TYPE, TYPE_ID, 4..) => media_type = r.u32_at(words),
            (FORMAT_MEDIA_SUBTYPE, TYPE_ID, 4..) => subtype = r.u32_at(words),
            (FORMAT_VIDEO_FORMAT, TYPE_ID, 4..) => format = r.u32_at(words),
            (FORMAT_VIDEO_MODIFIER, TYPE_LONG, 8..) => {
                let bytes = r.data.get(words..words + 8)?;
                let mut b = [0u8; 8];
                b.copy_from_slice(bytes);
                modifier = Some(u64::from_ne_bytes(b));
            }
            (FORMAT_VIDEO_MODIFIER, _, _) => return None,
            (FORMAT_VIDEO_SIZE, TYPE_RECTANGLE, 8..) => {
                dims = Some((r.u32_at(words)?, r.u32_at(words + 4)?));
            }
            (FORMAT_VIDEO_FRAMERATE, TYPE_FRACTION, 8..) => {
                rate = (r.u32_at(words)?, r.u32_at(words + 4)?);
            }
            _ => {}
        }
        at = value_at + (8 + value_size).next_multiple_of(8);
    }
    if media_type != Some(MEDIA_TYPE_VIDEO) || subtype != Some(MEDIA_SUBTYPE_RAW) {
        return None;
    }
    let (width, height) = dims.filter(|&(w, h)| w > 0 && h > 0)?;
    Some(VideoFormat {
        format: SpaVideoFormat::from_id(format?)?,
        width,
        height,
        framerate: rate,
        modifier,
    })
}

/// Where a frame's pixels are in a mapped buffer, from its `spa_chunk`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Chunk {
    /// Bytes from the mapping's start to the first row.
    pub(crate) offset: u32,
    /// Bytes of valid data from `offset`.
    pub(crate) size: u32,
    /// Bytes between rows; 0 or less means "tightly packed".
    pub(crate) stride: i32,
}

/// The part of the frame that holds the picture (`SPA_META_VideoCrop`). A window
/// capture is often a crop of a larger buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Crop {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// The part of a `format` frame to show: `crop` clamped to the frame (x, y, width,
/// height), or the whole frame when there is no crop or it is empty.
pub(crate) fn visible(format: VideoFormat, crop: Option<Crop>) -> (u32, u32, u32, u32) {
    match crop {
        Some(c) if c.width > 0 && c.height > 0 => {
            let x = u32::try_from(c.x.max(0))
                .unwrap_or(0)
                .min(format.width.saturating_sub(1));
            let y = u32::try_from(c.y.max(0))
                .unwrap_or(0)
                .min(format.height.saturating_sub(1));
            (
                x,
                y,
                c.width.min(format.width - x),
                c.height.min(format.height - y),
            )
        }
        _ => (0, 0, format.width, format.height),
    }
}

/// Copies a mapped buffer into a CPU frame, tightly packed, applying `crop` (clamped to
/// the frame; an empty one is ignored) and making padded formats opaque.
///
/// # Errors
/// [`MediaError`] when the chunk doesn't hold the rows the format needs.
pub(crate) fn copy_frame(
    data: &[u8],
    chunk: Chunk,
    format: VideoFormat,
    crop: Option<Crop>,
    pts: MediaTime,
) -> Result<VideoFrame, MediaError> {
    let overflow = |_| MediaError::Overflow {
        what: "a captured frame's layout",
    };
    let row = usize::try_from(format.width).map_err(overflow)? * 4;
    let stride = usize::try_from(chunk.stride)
        .ok()
        .filter(|&s| s > 0)
        .unwrap_or(row);
    let (x, y, w, h) = visible(format, crop);
    let size = FrameSize::new(w, h)?;
    let info = VideoFrameInfo {
        size,
        format: format.format.pixel_format(),
        color: ColorInfo::default(),
        pts,
    };
    let offset = usize::try_from(chunk.offset).map_err(overflow)?;
    let valid = usize::try_from(chunk.size).map_err(overflow)?;
    let src = data
        .get(offset..offset.saturating_add(valid).min(data.len()))
        .unwrap_or_default();
    let out_row = w as usize * 4;
    let mut out = vec![0u8; out_row * h as usize];
    for (r, dst) in out.chunks_exact_mut(out_row).enumerate() {
        let start = (y as usize + r) * stride + x as usize * 4;
        let line = src
            .get(start..start + out_row)
            .ok_or(MediaError::PlaneTooSmall {
                plane: 0,
                len: src.len(),
                required: start + out_row,
            })?;
        dst.copy_from_slice(line);
        if format.format.padded() {
            for px in dst.chunks_exact_mut(4) {
                px[3] = 0xff;
            }
        }
    }
    VideoFrame::new(info, vec![Plane::new(out, out_row)])
}

/// A frame's time on the media clock (100 ns ticks): the buffer header's PipeWire time
/// (`CLOCK_MONOTONIC` nanoseconds) when it is within a second before `now` and no
/// later than 100 ms after, else `now`. Some compositors stamp frames on another clock.
pub(crate) fn frame_pts(header_ns: Option<i64>, now: MediaTime, timebase: Timebase) -> MediaTime {
    let now_ticks = now.ticks();
    header_ns
        .map(|ns| ns / 100)
        .filter(|&t| t <= now_ticks + 1_000_000 && now_ticks - t <= 10_000_000)
        .map_or(now, |t| MediaTime::new(t, timebase))
}

#[cfg(test)]
mod tests {
    use jstream_platform::media::{MediaTime, PixelFormat};

    use super::{
        Chunk, Crop, PARAM_ENUM_FORMAT, PARAM_FORMAT, SpaVideoFormat, VideoFormat, buffer_params,
        copy_frame, enum_format, enum_formats, frame_pts, parse_format,
    };
    use crate::timebase;

    fn words(pod: &[u8]) -> Vec<u32> {
        pod.chunks(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    /// The EnumFormat pod, word by word, as `spa_pod_builder_add_object` lays it out.
    #[test]
    fn the_offered_format_has_the_spa_layout() {
        let pod = enum_format().bytes();
        assert_eq!(pod.len() % 8, 0);
        let w = words(&pod);
        assert_eq!(w[0] as usize, pod.len() - 8, "body size");
        assert_eq!(w[1], 15, "SPA_TYPE_Object");
        assert_eq!((w[2], w[3]), (0x40003, PARAM_ENUM_FORMAT));
        // mediaType: key, flags, Id pod (size 4, type 3), value, padding.
        assert_eq!(&w[4..10], &[1, 0, 4, 3, 2, 0]);
        // mediaSubtype raw.
        assert_eq!(&w[10..16], &[2, 0, 4, 3, 1, 0]);
        // format: an Enum choice of Ids, the default first.
        assert_eq!(&w[16..18], &[0x20001, 0]);
        assert_eq!(&w[18..24], &[16 + 5 * 4, 19, 3, 0, 4, 3]);
        assert_eq!(&w[24..29], &[8, 8, 12, 7, 11]);
        assert_eq!(w[29], 0, "padding");
        // size: a Range of rectangles.
        assert_eq!(&w[30..32], &[0x20003, 0]);
        assert_eq!(&w[32..38], &[16 + 3 * 8, 19, 1, 0, 8, 10]);
        assert_eq!(&w[38..44], &[1920, 1080, 1, 1, 8192, 8192]);
    }

    /// A DMA-BUF offer: the format fixed, then the modifiers as an Enum choice of
    /// Longs flagged mandatory and don't-fixate, as `spa_pod_builder_prop` with
    /// `SPA_POD_PROP_FLAG_MANDATORY | SPA_POD_PROP_FLAG_DONT_FIXATE` lays it out.
    #[test]
    fn dma_buf_offers_come_first_with_their_modifiers() {
        let modifiers = [0x0100_0000_0000_0001_u64, 0];
        let offers = enum_formats(&[
            (SpaVideoFormat::Bgrx, modifiers.to_vec()),
            (SpaVideoFormat::Rgba, Vec::new()),
        ]);
        assert_eq!(offers.len(), 2, "a format without modifiers isn't offered");
        assert_eq!(offers[1], enum_format(), "shared memory last");
        let pod = offers[0].bytes();
        assert_eq!(pod.len() % 8, 0);
        let w = words(&pod);
        assert_eq!(w[0] as usize, pod.len() - 8, "body size");
        assert_eq!((w[2], w[3]), (0x40003, PARAM_ENUM_FORMAT));
        assert_eq!(&w[4..16], &[1, 0, 4, 3, 2, 0, 2, 0, 4, 3, 1, 0]);
        // format: a plain Id, BGRx.
        assert_eq!(&w[16..22], &[0x20001, 0, 4, 3, 8, 0]);
        // modifier: flags 0b11000, an Enum choice of Longs: default, then each.
        assert_eq!(&w[22..24], &[0x20002, 0b1_1000]);
        assert_eq!(&w[24..30], &[16 + 3 * 8, 19, 3, 0, 8, 5]);
        let longs: Vec<u64> = pod[30 * 4..30 * 4 + 24]
            .chunks(8)
            .map(|c| u64::from_ne_bytes(c.try_into().expect("8 bytes")))
            .collect();
        assert_eq!(longs, [modifiers[0], modifiers[0], modifiers[1]]);
        // Then size and rate, as in the shared-memory offer.
        assert_eq!(&w[36..38], &[0x20003, 0]);
        // Without a GPU, only shared memory.
        assert_eq!(enum_formats(&[]), vec![enum_format()]);
    }

    #[test]
    fn buffer_params_ask_for_mappable_memory_and_metadata() {
        let [dma, _, _] = buffer_params(true);
        assert_eq!(
            &words(&dma.bytes())[4..9],
            &[6, 0, 4, 4, 0b1000],
            "DMA-BUF only"
        );
        let [buffers, header, crop] = buffer_params(false);
        let b = words(&buffers.bytes());
        assert_eq!((b[2], b[3]), (0x40004, 5));
        assert_eq!(&b[4..9], &[6, 0, 4, 4, 0b110]);
        let h = words(&header.bytes());
        assert_eq!((h[2], h[3]), (0x40005, 6));
        assert_eq!(&h[4..9], &[1, 0, 4, 3, 1]);
        assert_eq!(&h[10..15], &[2, 0, 4, 4, 32]);
        let c = words(&crop.bytes());
        assert_eq!(&c[4..9], &[1, 0, 4, 3, 2]);
        assert_eq!(c[14], 16);
        assert_eq!(header.as_ptr() as usize % 8, 0, "aligned for libspa");
    }

    /// A settled format, as PipeWire sends it: fixed values, some still wrapped in a
    /// `None` choice.
    fn settled(format: u32, modifier: bool) -> Vec<u8> {
        use super::{Builder, CHOICE_NONE, TYPE_FRACTION, TYPE_ID, TYPE_RECTANGLE, choice, value};
        let mut props = vec![
            (1, value(TYPE_ID, &[2])),
            (2, value(TYPE_ID, &[1])),
            (0x20001, choice(CHOICE_NONE, TYPE_ID, 1, &[format])),
            (0x20003, value(TYPE_RECTANGLE, &[1280, 720])),
            (0x20004, value(TYPE_FRACTION, &[0, 1])),
        ];
        if modifier {
            let m = 0x0100_0000_0000_0002_u64.to_ne_bytes();
            let m = [
                u32::from_ne_bytes([m[0], m[1], m[2], m[3]]),
                u32::from_ne_bytes([m[4], m[5], m[6], m[7]]),
            ];
            props.push((0x20002, value(5, &m)));
        }
        Builder::object(0x40003, PARAM_FORMAT, &props).bytes()
    }

    #[test]
    fn the_settled_format_parses() {
        assert_eq!(
            parse_format(&settled(8, false)),
            Some(VideoFormat {
                format: SpaVideoFormat::Bgrx,
                width: 1280,
                height: 720,
                framerate: (0, 1),
                modifier: None,
            })
        );
        assert_eq!(
            parse_format(&settled(23, false)),
            None,
            "NV12 was not offered"
        );
        let dma = parse_format(&settled(12, true)).expect("a DMA-BUF format parses");
        assert_eq!(
            (dma.format, dma.modifier),
            (SpaVideoFormat::Bgra, Some(0x0100_0000_0000_0002))
        );
        // The DMA-BUF offer parses to its default modifier.
        let offer = &enum_formats(&[(SpaVideoFormat::Rgbx, vec![7, 9])])[0];
        let offer = parse_format(&offer.bytes()).expect("parses");
        assert_eq!(
            (offer.format, offer.modifier),
            (SpaVideoFormat::Rgbx, Some(7))
        );
        // The offer itself parses to its defaults.
        let offer = parse_format(&enum_format().bytes()).expect("parses");
        assert_eq!(
            (offer.format, offer.width, offer.height),
            (SpaVideoFormat::Bgrx, 1920, 1080)
        );
        // Truncated or corrupt pods are refused without a panic.
        let good = settled(8, false);
        for n in 0..good.len() {
            let _ = parse_format(&good[..n]);
        }
        for i in 0..good.len() {
            let mut bad = good.clone();
            bad[i] ^= 0xff;
            let _ = parse_format(&bad);
        }
        assert_eq!(parse_format(&[]), None);
    }

    fn fmt(format: SpaVideoFormat, width: u32, height: u32) -> VideoFormat {
        VideoFormat {
            format,
            width,
            height,
            framerate: (60, 1),
            modifier: None,
        }
    }

    fn pts() -> MediaTime {
        MediaTime::new(0, timebase())
    }

    #[test]
    fn frames_copy_with_stride_offset_and_opaque_padding() {
        // 2x2 BGRx with a 12-byte stride, 4 bytes into the mapping.
        let mut data = vec![0xee; 4];
        for row in 0..2u8 {
            for px in 0..2u8 {
                data.extend_from_slice(&[row, px, 9, 0]);
            }
            data.extend_from_slice(&[0xdd; 4]);
        }
        let chunk = Chunk {
            offset: 4,
            size: 24,
            stride: 12,
        };
        let frame =
            copy_frame(&data, chunk, fmt(SpaVideoFormat::Bgrx, 2, 2), None, pts()).expect("copies");
        assert_eq!(frame.info().format, PixelFormat::Bgra8);
        assert_eq!(
            frame.planes()[0].data(),
            &[0, 0, 9, 255, 0, 1, 9, 255, 1, 0, 9, 255, 1, 1, 9, 255]
        );
        // RGBA keeps its alpha; a non-positive stride means packed.
        let rgba = [1, 2, 3, 4, 5, 6, 7, 8];
        let chunk = Chunk {
            offset: 0,
            size: 8,
            stride: 0,
        };
        let frame =
            copy_frame(&rgba, chunk, fmt(SpaVideoFormat::Rgba, 2, 1), None, pts()).expect("copies");
        assert_eq!(frame.info().format, PixelFormat::Rgba8);
        assert_eq!(frame.planes()[0].data(), &rgba);
    }

    #[test]
    fn frames_crop_and_clamp() {
        // 4x2 RGBA where each pixel's first byte is its index.
        let data: Vec<u8> = (0..8u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let chunk = Chunk {
            offset: 0,
            size: 32,
            stride: 16,
        };
        let crop = Crop {
            x: 1,
            y: 1,
            width: 2,
            height: 5,
        };
        let f = copy_frame(
            &data,
            chunk,
            fmt(SpaVideoFormat::Rgba, 4, 2),
            Some(crop),
            pts(),
        )
        .expect("copies");
        assert_eq!((f.info().size.width(), f.info().size.height()), (2, 1));
        assert_eq!(f.planes()[0].data(), &[5, 0, 0, 255, 6, 0, 0, 255]);
        let empty = Crop {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        let f = copy_frame(
            &data,
            chunk,
            fmt(SpaVideoFormat::Rgba, 4, 2),
            Some(empty),
            pts(),
        )
        .expect("copies");
        assert_eq!(f.info().size.width(), 4);
        let outside = Crop {
            x: 99,
            y: -3,
            width: 9,
            height: 9,
        };
        let f = copy_frame(
            &data,
            chunk,
            fmt(SpaVideoFormat::Rgba, 4, 2),
            Some(outside),
            pts(),
        )
        .expect("copies");
        assert_eq!((f.info().size.width(), f.info().size.height()), (1, 2));
    }

    #[test]
    fn a_short_buffer_is_an_error() {
        let data = [0u8; 12];
        let chunk = Chunk {
            offset: 0,
            size: 12,
            stride: 8,
        };
        assert!(copy_frame(&data, chunk, fmt(SpaVideoFormat::Bgra, 2, 2), None, pts()).is_err());
        let chunk = Chunk {
            offset: 40,
            size: 12,
            stride: 8,
        };
        assert!(copy_frame(&data, chunk, fmt(SpaVideoFormat::Bgra, 1, 1), None, pts()).is_err());
    }

    #[test]
    fn pts_uses_the_header_only_when_plausible() {
        let tb = timebase();
        let now = MediaTime::new(50_000_000, tb); // 5 s
        assert_eq!(frame_pts(Some(4_990_000_000), now, tb).ticks(), 49_900_000);
        assert_eq!(frame_pts(None, now, tb), now);
        assert_eq!(frame_pts(Some(1_000_000_000), now, tb), now, "4 s old");
        assert_eq!(frame_pts(Some(6_000_000_000), now, tb), now, "1 s ahead");
        assert_eq!(frame_pts(Some(i64::MIN), now, tb), now);
    }
}
