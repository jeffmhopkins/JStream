//! One VA-API H.264 encode session (ADR-0060 §6): a config and context on one render
//! node, input surfaces with a coded buffer each, and reconstructed surfaces for the
//! reference chain.
//!
//! The session takes CPU frames (NV12, or the compositor's RGBA and BGRA, converted by
//! `upload`). Each is written into a free input surface, through
//! `vaDeriveImage` where the driver maps its surfaces, otherwise through a staging image
//! and `vaPutImage`, and then encoded with the parameters `h264` builds.
//! Up to [`DEPTH`] frames are in flight; `poll` collects each finished one without
//! blocking (`vaQuerySurfaceStatus`), in submission order, which is also presentation
//! order because there are no B-frames. Packets are Annex B: the driver writes the coded
//! slice, preceded on IDR frames by the SPS and PPS ([`h264::sps`], [`h264::pps`]) when
//! it takes packed headers, or by its own otherwise.
//!
//! Importing DMA-BUF frames (GPU encode targets) is a follow-up; a GPU frame is refused
//! as unsupported.

use std::collections::VecDeque;
use std::ffi::{c_int, c_uint, c_void};
use std::mem::size_of;

use jstream_platform::media::{Codec, MediaTime, Packet, VideoFrame};
use jstream_platform::{CodecInfo, PlatformError, VideoEncoder, VideoEncoderConfig, VideoSurface};

use super::display::{Display, Va};
use super::ffi::{
    ATTRIB_NOT_SUPPORTED, CODED_BUF_STATUS_SLICE_OVERFLOW_MASK, CodedBufferSegment, ConfigAttrib,
    FOURCC_NV12, INVALID_ID, Id, Image, ImageFormat, PACKED_HEADER_SEQUENCE, PROGRESSIVE,
    PackedHeaderParam, RT_FORMAT_YUV420, SURFACE_RENDERING, SURFACE_SKIPPED, attrib, buffer,
    packed,
};
use super::h264::{self, Gop, Picture, Settings};
use super::upload::{Nv12Image, write_nv12};
use crate::VaEncoder;

/// Frames in flight: input surfaces, each with its coded buffer. When all are busy a
/// frame is refused as busy, and the encode thread drops and counts it (R-RT-004).
pub(crate) const DEPTH: usize = 4;
/// Reconstructed surfaces. The driver encodes a context's pictures in order, so a
/// surface is free again once the picture after the one it holds has been submitted;
/// three leave one spare.
const RECON: usize = 3;
/// The most buffers one picture submits: sequence, three misc, picture, the packed
/// header's two, slice.
const MAX_BUFFERS: usize = 8;

/// What the session owns in libva, destroyed in reverse order on drop, before the
/// display is terminated.
struct Resources {
    display: Display,
    config: Option<Id>,
    context: Option<Id>,
    /// The [`DEPTH`] input surfaces, then the [`RECON`] reconstructed ones.
    surfaces: Vec<Id>,
    /// One coded buffer per input surface.
    coded: Vec<Id>,
    /// The staging image when the driver can't map its surfaces (`vaPutImage`).
    staging: Option<Image>,
}

impl Resources {
    fn va(&self) -> &Va {
        &self.display.va
    }

    fn context(&self) -> Id {
        self.context.unwrap_or(INVALID_ID)
    }

    /// A buffer of `kind` holding a copy of `value`.
    fn buffer<T>(&self, kind: c_int, value: &T) -> Result<Id, PlatformError> {
        // SAFETY: plain data; libva copies `size_of::<T>()` bytes from the pointer while
        // `value` is borrowed and doesn't write through it (`void *data` is `const` in
        // effect).
        unsafe { self.raw_buffer(kind, (value as *const T).cast(), size_of::<T>()) }
    }

    /// A buffer of `kind` holding a copy of `bytes`.
    fn data_buffer(&self, kind: c_int, bytes: &[u8]) -> Result<Id, PlatformError> {
        // SAFETY: libva copies `bytes.len()` bytes from the slice while it is borrowed.
        unsafe { self.raw_buffer(kind, bytes.as_ptr().cast(), bytes.len()) }
    }

    /// # Safety
    /// `data` is null or points to `size` readable bytes.
    unsafe fn raw_buffer(
        &self,
        kind: c_int,
        data: *const c_void,
        size: usize,
    ) -> Result<Id, PlatformError> {
        let size = c_uint::try_from(size).map_err(|_| PlatformError::InvalidState {
            what: "vaCreateBuffer",
            state: "the buffer is larger than 4 GiB",
        })?;
        let mut id = INVALID_ID;
        // SAFETY: the context is live; `data` is null or holds `size` bytes (this
        // function's contract), which libva copies; `id` is a valid out-pointer.
        // C: `VAStatus vaCreateBuffer(VADisplay, VAContextID, VABufferType,
        // unsigned int size, unsigned int num_elements, void *data, VABufferID *buf_id)`.
        let status = unsafe {
            (self.va().create_buffer)(
                self.display.dpy,
                self.context(),
                kind,
                size,
                1,
                data.cast_mut(),
                &mut id,
            )
        };
        self.va().check("vaCreateBuffer", status)?;
        Ok(id)
    }

    fn destroy_buffer(&self, id: Id) {
        // SAFETY: `id` is a buffer this session created and destroys once.
        // C: `VAStatus vaDestroyBuffer(VADisplay, VABufferID)`.
        unsafe { (self.va().destroy_buffer)(self.display.dpy, id) };
    }

    /// Writes `frame` into `image`, an NV12 image of at least the frame's size.
    fn write_image(&self, image: &Image, frame: &VideoFrame) -> Result<(), PlatformError> {
        if image.format.fourcc != FOURCC_NV12 || image.num_planes < 2 {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a VA image in FOURCC 0x{:08X} with {} planes (NV12 expected)",
                    image.format.fourcc, image.num_planes
                ),
            });
        }
        let mut ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: `image.buf` is the image's live data buffer; `ptr` is a valid
        // out-pointer.
        // C: `VAStatus vaMapBuffer(VADisplay, VABufferID, void **pbuf)`.
        let status = unsafe { (self.va().map_buffer)(self.display.dpy, image.buf, &mut ptr) };
        self.va().check("vaMapBuffer (image)", status)?;
        let written = if ptr.is_null() {
            Err(PlatformError::Os {
                call: "vaMapBuffer (image)",
                code: None,
                message: "mapped no memory".into(),
            })
        } else {
            // SAFETY: libva mapped the image's `data_size` bytes at `ptr`, writable and
            // not otherwise referenced until the unmap below; the slice is dropped
            // before it.
            let data = unsafe {
                std::slice::from_raw_parts_mut(ptr.cast::<u8>(), image.data_size as usize)
            };
            let info = frame.info();
            Nv12Image::new(
                data,
                [image.offsets[0] as usize, image.offsets[1] as usize],
                [image.pitches[0] as usize, image.pitches[1] as usize],
                info.size.width() as usize,
                info.size.height() as usize,
            )
            .and_then(|mut dst| write_nv12(frame, &mut dst))
        };
        // SAFETY: the buffer is mapped, and nothing refers to its memory any more.
        // C: `VAStatus vaUnmapBuffer(VADisplay, VABufferID)`.
        let status = unsafe { (self.va().unmap_buffer)(self.display.dpy, image.buf) };
        written.and(self.va().check("vaUnmapBuffer (image)", status))
    }

    /// Writes `frame` into the input surface `surface`.
    fn upload(&self, surface: Id, frame: &VideoFrame) -> Result<(), PlatformError> {
        if let Some(staging) = &self.staging {
            self.write_image(staging, frame)?;
            let (w, h) = (frame.info().size.width(), frame.info().size.height());
            // SAFETY: the surface and image are live and at least `w` x `h` (both are
            // the coded size, which the frame's size rounds up to).
            // C: `VAStatus vaPutImage(VADisplay, VASurfaceID, VAImageID, int src_x,
            // int src_y, unsigned int src_width, unsigned int src_height, int dest_x,
            // int dest_y, unsigned int dest_width, unsigned int dest_height)`.
            let status = unsafe {
                (self.va().put_image)(
                    self.display.dpy,
                    surface,
                    staging.image_id,
                    0,
                    0,
                    w,
                    h,
                    0,
                    0,
                    w,
                    h,
                )
            };
            return self.va().check("vaPutImage", status);
        }
        let mut image = Image::EMPTY;
        // SAFETY: `surface` is live and idle (its last picture was collected); `image`
        // is a valid out-pointer.
        // C: `VAStatus vaDeriveImage(VADisplay, VASurfaceID, VAImage *image)`.
        let status = unsafe { (self.va().derive_image)(self.display.dpy, surface, &mut image) };
        self.va().check("vaDeriveImage", status)?;
        let written = self.write_image(&image, frame);
        // SAFETY: the derived image is destroyed once, after its buffer was unmapped.
        // C: `VAStatus vaDestroyImage(VADisplay, VAImageID)`.
        let status = unsafe { (self.va().destroy_image)(self.display.dpy, image.image_id) };
        written.and(self.va().check("vaDestroyImage", status))
    }

    /// Whether the picture on `surface` is done: `vaQuerySurfaceStatus` without
    /// blocking, or `vaSyncSurface` when `wait`. `Some(false)` means it was skipped.
    fn ready(&self, surface: Id, wait: bool) -> Result<Option<bool>, PlatformError> {
        if wait {
            // SAFETY: the surface is live.
            // C: `VAStatus vaSyncSurface(VADisplay, VASurfaceID)`.
            let status = unsafe { (self.va().sync_surface)(self.display.dpy, surface) };
            self.va().check("vaSyncSurface", status)?;
        }
        let mut state: c_int = 0;
        // SAFETY: the surface is live; `state` is a valid out-pointer.
        // C: `VAStatus vaQuerySurfaceStatus(VADisplay, VASurfaceID,
        // VASurfaceStatus *status)`.
        let status =
            unsafe { (self.va().query_surface_status)(self.display.dpy, surface, &mut state) };
        self.va().check("vaQuerySurfaceStatus", status)?;
        if state & SURFACE_SKIPPED != 0 {
            return Ok(Some(false));
        }
        Ok((wait || state & SURFACE_RENDERING == 0).then_some(true))
    }

    /// The coded picture in `coded`: every segment's bytes, in order.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when libva fails or the picture overflowed the buffer.
    fn read_coded(&self, coded: Id) -> Result<Vec<u8>, PlatformError> {
        let mut ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: `coded` is a live coded buffer whose picture is done; `ptr` is a valid
        // out-pointer.
        // C: `VAStatus vaMapBuffer(VADisplay, VABufferID, void **pbuf)`.
        let status = unsafe { (self.va().map_buffer)(self.display.dpy, coded, &mut ptr) };
        self.va().check("vaMapBuffer (coded buffer)", status)?;
        let mut out = Vec::new();
        let mut overflow = false;
        let mut segment = ptr.cast::<CodedBufferSegment>().cast_const();
        // A list longer than this is corrupt.
        for _ in 0..1024 {
            if segment.is_null() {
                break;
            }
            // SAFETY: a mapped coded buffer is a null-terminated list of
            // `VACodedBufferSegment`s, valid until it is unmapped below.
            let s = unsafe { &*segment };
            overflow |= s.status & CODED_BUF_STATUS_SLICE_OVERFLOW_MASK != 0;
            if !s.buf.is_null() && s.size > 0 {
                // SAFETY: the segment's `buf` holds `size` bytes, valid until the unmap.
                let bytes =
                    unsafe { std::slice::from_raw_parts(s.buf.cast::<u8>(), s.size as usize) };
                // H.264 segments are whole NAL units: `bit_offset` is 0.
                out.extend_from_slice(bytes);
            }
            segment = s.next.cast::<CodedBufferSegment>().cast_const();
        }
        // SAFETY: the buffer is mapped, and nothing refers to its memory any more.
        // C: `VAStatus vaUnmapBuffer(VADisplay, VABufferID)`.
        let status = unsafe { (self.va().unmap_buffer)(self.display.dpy, coded) };
        self.va().check("vaUnmapBuffer (coded buffer)", status)?;
        if overflow {
            return Err(PlatformError::Os {
                call: "vaMapBuffer (coded buffer)",
                code: None,
                message: "the coded picture didn't fit its buffer".into(),
            });
        }
        Ok(out)
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        let (va, dpy) = (&self.display.va, self.display.dpy);
        // SAFETY: each ID was created on this display, is destroyed once, here, and in
        // the reverse order of creation; failures leave nothing to undo.
        // C: `VAStatus vaDestroyImage(VADisplay, VAImageID)`,
        // `VAStatus vaDestroyBuffer(VADisplay, VABufferID)`,
        // `VAStatus vaDestroyContext(VADisplay, VAContextID)`,
        // `VAStatus vaDestroySurfaces(VADisplay, VASurfaceID *surfaces, int num_surfaces)`,
        // `VAStatus vaDestroyConfig(VADisplay, VAConfigID)`.
        unsafe {
            if let Some(image) = self.staging.take() {
                (va.destroy_image)(dpy, image.image_id);
            }
            for &id in &self.coded {
                (va.destroy_buffer)(dpy, id);
            }
            if let Some(context) = self.context.take() {
                (va.destroy_context)(dpy, context);
            }
            if !self.surfaces.is_empty() {
                let n = c_int::try_from(self.surfaces.len()).unwrap_or(0);
                (va.destroy_surfaces)(dpy, self.surfaces.as_mut_ptr(), n);
            }
            if let Some(config) = self.config.take() {
                (va.destroy_config)(dpy, config);
            }
        }
    }
}

/// A frame submitted and not yet collected.
#[derive(Clone, Copy, Debug)]
struct InFlight {
    slot: usize,
    pts: MediaTime,
    keyframe: bool,
}

/// One H.264 encode session on a VA-API driver.
pub(crate) struct VaapiH264Encoder {
    res: Resources,
    info: CodecInfo,
    config: VideoEncoderConfig,
    settings: Settings,
    gop: Gop,
    /// The SPS and PPS, sent as one packed sequence header on IDR frames when the
    /// driver takes packed headers (`None`: it writes its own).
    headers: Option<Vec<u8>>,
    /// The last picture's reconstructed surface: the next P frame's reference.
    reference: Option<(Id, Picture)>,
    next_recon: usize,
    /// Input slots with no frame in flight.
    free: Vec<usize>,
    in_flight: VecDeque<InFlight>,
    /// One picture's parameter buffers, reused.
    buffers: Vec<Id>,
    pending: Vec<Packet>,
    finished: bool,
}

// SAFETY: the session is owned and driven by one thread at a time (`&mut self` for every
// call except `can_accept`, which reads plain fields). Its VA display is its own (its
// own render-node fd), and libva and its drivers have no thread affinity: a display may
// be used from any thread as long as calls on it don't race, which `&mut self` ensures.
unsafe impl Send for VaapiH264Encoder {}

impl VaapiH264Encoder {
    /// Opens a session on `encoder` (a probe result) for `config`, reported as `info`.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the configuration is outside what
    /// [`h264::check`] accepts or the driver lacks 4:2:0 surfaces or the rate-control
    /// mode, [`PlatformError::MissingComponent`] when libva is missing, and
    /// [`PlatformError::Os`] when the node or the driver fails.
    pub(crate) fn open(
        encoder: &VaEncoder,
        config: &VideoEncoderConfig,
        info: CodecInfo,
    ) -> Result<Self, PlatformError> {
        let settings = h264::check(config, encoder.profile)?;
        let display = Display::open(Va::load()?, &encoder.node)?;
        let mut res = Resources {
            display,
            config: None,
            context: None,
            surfaces: Vec::new(),
            coded: Vec::new(),
            staging: None,
        };
        let (va, dpy) = (res.display.va.clone(), res.display.dpy);
        let (profile, entrypoint) = (encoder.profile as c_int, encoder.entrypoint as c_int);
        let mut offered = [
            attrib::RT_FORMAT,
            attrib::RATE_CONTROL,
            attrib::ENC_PACKED_HEADERS,
        ]
        .map(|kind| ConfigAttrib { kind, value: 0 });
        // SAFETY: the display is initialised; libva fills the values of the three
        // attributes in place.
        // C: `VAStatus vaGetConfigAttributes(VADisplay, VAProfile, VAEntrypoint,
        // VAConfigAttrib *attrib_list, int num_attribs)`.
        let status = unsafe {
            (va.get_config_attributes)(dpy, profile, entrypoint, offered.as_mut_ptr(), 3)
        };
        va.check("vaGetConfigAttributes", status)?;
        let has =
            |a: ConfigAttrib, bits: u32| a.value != ATTRIB_NOT_SUPPORTED && a.value & bits == bits;
        if !has(offered[0], RT_FORMAT_YUV420) {
            return Err(PlatformError::Unsupported {
                what: format!("4:2:0 encode surfaces on {}", info.name),
            });
        }
        if !has(offered[1], settings.rc.va()) {
            return Err(PlatformError::Unsupported {
                what: format!("{:?} rate control on {}", settings.rc, info.name),
            });
        }
        let packed = has(offered[2], PACKED_HEADER_SEQUENCE);
        let mut wanted = vec![
            ConfigAttrib {
                kind: attrib::RT_FORMAT,
                value: RT_FORMAT_YUV420,
            },
            ConfigAttrib {
                kind: attrib::RATE_CONTROL,
                value: settings.rc.va(),
            },
        ];
        if packed {
            wanted.push(ConfigAttrib {
                kind: attrib::ENC_PACKED_HEADERS,
                value: PACKED_HEADER_SEQUENCE,
            });
        }
        let mut id = INVALID_ID;
        // SAFETY: `wanted` holds `len` attributes, which libva reads; `id` is a valid
        // out-pointer.
        // C: `VAStatus vaCreateConfig(VADisplay, VAProfile, VAEntrypoint,
        // VAConfigAttrib *attrib_list, int num_attribs, VAConfigID *config_id)`.
        let status = unsafe {
            (va.create_config)(
                dpy,
                profile,
                entrypoint,
                wanted.as_mut_ptr(),
                wanted.len() as c_int,
                &mut id,
            )
        };
        va.check("vaCreateConfig", status)?;
        res.config = Some(id);

        // Surfaces at the coded (macroblock-aligned) size; the SPS crops the excess.
        let (cw, ch) = (settings.width_mbs * 16, settings.height_mbs * 16);
        let mut surfaces = [INVALID_ID; DEPTH + RECON];
        // SAFETY: `surfaces` has room for the `DEPTH + RECON` IDs libva writes; no
        // surface attributes are passed.
        // C: `VAStatus vaCreateSurfaces(VADisplay, unsigned int format, unsigned int
        // width, unsigned int height, VASurfaceID *surfaces, unsigned int
        // num_surfaces, VASurfaceAttrib *attrib_list, unsigned int num_attribs)`.
        let status = unsafe {
            (va.create_surfaces)(
                dpy,
                RT_FORMAT_YUV420,
                cw,
                ch,
                surfaces.as_mut_ptr(),
                (DEPTH + RECON) as c_uint,
                std::ptr::null_mut(),
                0,
            )
        };
        va.check("vaCreateSurfaces", status)?;
        res.surfaces = surfaces.to_vec();
        let mut context = INVALID_ID;
        // SAFETY: the config is live; `surfaces` holds the context's render targets,
        // which libva reads; `context` is a valid out-pointer.
        // C: `VAStatus vaCreateContext(VADisplay, VAConfigID, int picture_width,
        // int picture_height, int flag, VASurfaceID *render_targets,
        // int num_render_targets, VAContextID *context)`.
        let status = unsafe {
            (va.create_context)(
                dpy,
                id,
                cw as c_int,
                ch as c_int,
                PROGRESSIVE,
                surfaces.as_mut_ptr(),
                (DEPTH + RECON) as c_int,
                &mut context,
            )
        };
        va.check("vaCreateContext", status)?;
        res.context = Some(context);
        for _ in 0..DEPTH {
            // SAFETY: a null `data` asks libva for an uninitialised coded buffer.
            let coded = unsafe {
                res.raw_buffer(
                    buffer::CODED,
                    std::ptr::null(),
                    settings.coded_buffer_size() as usize,
                )
            }?;
            res.coded.push(coded);
        }
        res.staging = Self::staging(&res, cw, ch)?;
        Ok(Self {
            res,
            info,
            config: *config,
            gop: Gop::new(&settings),
            headers: packed.then(|| [h264::sps(&settings), h264::pps(&settings)].concat()),
            settings,
            reference: None,
            next_recon: 0,
            free: (0..DEPTH).rev().collect(),
            in_flight: VecDeque::with_capacity(DEPTH),
            buffers: Vec::with_capacity(MAX_BUFFERS),
            pending: Vec::new(),
            finished: false,
        })
    }

    /// `None` when the driver maps its surfaces as NV12 (`vaDeriveImage`), which most
    /// do; otherwise a `cw` x `ch` NV12 image to stage frames in for `vaPutImage`.
    fn staging(res: &Resources, cw: u32, ch: u32) -> Result<Option<Image>, PlatformError> {
        let (va, dpy) = (res.va(), res.display.dpy);
        let mut derived = Image::EMPTY;
        // SAFETY: the surface is live and idle; `derived` is a valid out-pointer.
        // C: `VAStatus vaDeriveImage(VADisplay, VASurfaceID, VAImage *image)`.
        let status = unsafe { (va.derive_image)(dpy, res.surfaces[0], &mut derived) };
        if va.check("vaDeriveImage", status).is_ok() {
            let nv12 = derived.format.fourcc == FOURCC_NV12;
            // SAFETY: the derived image is live and destroyed once.
            // C: `VAStatus vaDestroyImage(VADisplay, VAImageID)`.
            unsafe { (va.destroy_image)(dpy, derived.image_id) };
            if nv12 {
                return Ok(None);
            }
        }
        let mut format = ImageFormat::NV12;
        let mut image = Image::EMPTY;
        // SAFETY: `format` is a valid NV12 description, which libva reads; `image` is a
        // valid out-pointer.
        // C: `VAStatus vaCreateImage(VADisplay, VAImageFormat *format, int width,
        // int height, VAImage *image)`.
        let status =
            unsafe { (va.create_image)(dpy, &mut format, cw as c_int, ch as c_int, &mut image) };
        va.check("vaCreateImage", status)?;
        Ok(Some(image))
    }

    /// Submits `picture` from the input surface in `slot`.
    fn submit(&mut self, slot: usize, picture: &Picture) -> Result<(), PlatformError> {
        let recon = self.res.surfaces[DEPTH + self.next_recon];
        let reference = self.reference.as_ref().map(|(s, p)| (*s, p));
        let res = &self.res;
        let s = &self.settings;
        let buffers = &mut self.buffers;
        buffers.clear();
        let mut built = || -> Result<(), PlatformError> {
            if picture.idr {
                buffers.push(res.buffer(buffer::SEQUENCE_PARAMETER, &h264::sequence(s))?);
                buffers.push(res.buffer(buffer::MISC_PARAMETER, &h264::rate_control(s))?);
                buffers.push(res.buffer(buffer::MISC_PARAMETER, &h264::hrd(s))?);
                buffers.push(res.buffer(buffer::MISC_PARAMETER, &h264::frame_rate(s))?);
            }
            let coded = res.coded[slot];
            buffers.push(res.buffer(
                buffer::PICTURE_PARAMETER,
                &h264::picture(s, picture, recon, reference, coded),
            )?);
            if let (true, Some(headers)) = (picture.idr, &self.headers) {
                let param = PackedHeaderParam {
                    kind: packed::SEQUENCE,
                    bit_length: u32::try_from(headers.len() * 8).unwrap_or(u32::MAX),
                    has_emulation_bytes: 1,
                    va_reserved: [0; 4],
                };
                buffers.push(res.buffer(buffer::PACKED_HEADER_PARAMETER, &param)?);
                buffers.push(res.data_buffer(buffer::PACKED_HEADER_DATA, headers)?);
            }
            buffers.push(res.buffer(buffer::SLICE_PARAMETER, &h264::slice(s, picture, reference))?);
            let (va, dpy, ctx) = (res.va(), res.display.dpy, res.context());
            // SAFETY: the context and input surface are live, and the surface is idle.
            // C: `VAStatus vaBeginPicture(VADisplay, VAContextID, VASurfaceID)`.
            let status = unsafe { (va.begin_picture)(dpy, ctx, res.surfaces[slot]) };
            va.check("vaBeginPicture", status)?;
            // SAFETY: `buffers` holds `len` live buffer IDs, which libva reads.
            // C: `VAStatus vaRenderPicture(VADisplay, VAContextID, VABufferID *buffers,
            // int num_buffers)`.
            let rendered = unsafe {
                (va.render_picture)(dpy, ctx, buffers.as_mut_ptr(), buffers.len() as c_int)
            };
            // SAFETY: a picture was begun on the context; it is ended even when
            // rendering failed, as libva expects.
            // C: `VAStatus vaEndPicture(VADisplay, VAContextID)`.
            let ended = unsafe { (va.end_picture)(dpy, ctx) };
            va.check("vaRenderPicture", rendered)?;
            va.check("vaEndPicture", ended)
        };
        let result = built();
        // libva copied what it needs at vaRenderPicture; the buffers go now.
        for &id in buffers.iter() {
            res.destroy_buffer(id);
        }
        buffers.clear();
        if result.is_ok() {
            self.reference = Some((recon, *picture));
            self.next_recon = (self.next_recon + 1) % RECON;
        }
        result
    }

    /// Collects every finished picture, in order, into `pending`; with `wait`, every
    /// picture in flight.
    fn collect(&mut self, wait: bool) -> Result<(), PlatformError> {
        while let Some(&front) = self.in_flight.front() {
            let Some(coded) = self.res.ready(self.res.surfaces[front.slot], wait)? else {
                return Ok(());
            };
            self.in_flight.pop_front();
            self.free.push(front.slot);
            let data = if coded {
                self.res.read_coded(self.res.coded[front.slot])
            } else {
                Ok(Vec::new())
            };
            let data = match data {
                Ok(data) if !data.is_empty() => data,
                other => {
                    // A lost or skipped picture breaks the reference chain: start again
                    // from an IDR frame.
                    self.gop.force_idr();
                    self.reference = None;
                    other?;
                    continue;
                }
            };
            self.pending.push(Packet::new(
                Codec::H264,
                front.keyframe,
                front.pts,
                front.pts,
                data,
            )?);
        }
        Ok(())
    }
}

impl VideoEncoder for VaapiH264Encoder {
    fn info(&self) -> &CodecInfo {
        &self.info
    }

    /// Reflects the pictures collected by the last `encode` or `poll`, so the encode
    /// thread calls `poll` every tick.
    fn can_accept(&self) -> bool {
        !self.finished && !self.free.is_empty()
    }

    fn encode(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        if self.finished {
            return Err(PlatformError::InvalidState {
                what: "H.264 encode",
                state: "the encoder is finished",
            });
        }
        self.collect(false)?;
        let info = *frame.info();
        if info.size != self.config.size || info.format != self.config.input_format {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a {:?} {}x{} frame (configured for {:?} {}x{})",
                    info.format,
                    info.size.width(),
                    info.size.height(),
                    self.config.input_format,
                    self.config.size.width(),
                    self.config.size.height()
                ),
            });
        }
        let VideoSurface::Cpu(frame) = frame else {
            return Err(PlatformError::Unsupported {
                what: "a GPU frame on the VA-API H.264 encoder (it takes CPU frames; \
                       DMA-BUF import is a follow-up)"
                    .into(),
            });
        };
        let Some(slot) = self.free.pop() else {
            return Err(PlatformError::Busy {
                what: "VA-API H.264 encoder",
            });
        };
        let submitted = self
            .res
            .upload(self.res.surfaces[slot], &frame)
            .and_then(|()| {
                let picture = self.gop.next_picture();
                self.submit(slot, &picture).map(|()| picture)
            });
        match submitted {
            Ok(picture) => {
                self.in_flight.push_back(InFlight {
                    slot,
                    pts: info.pts,
                    keyframe: picture.idr,
                });
                Ok(())
            }
            Err(e) => {
                self.free.push(slot);
                self.gop.force_idr();
                self.reference = None;
                Err(e)
            }
        }
    }

    fn poll(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.collect(false)?;
        out.append(&mut self.pending);
        Ok(())
    }

    /// The next frame is encoded as an IDR frame, with the SPS and PPS, and the
    /// keyframe cadence restarts from it.
    fn request_keyframe(&mut self) -> Result<(), PlatformError> {
        if self.finished {
            return Err(PlatformError::InvalidState {
                what: "H.264 keyframe request",
                state: "the encoder is finished",
            });
        }
        self.gop.force_idr();
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.finished = true;
        let collected = self.collect(true);
        out.append(&mut self.pending);
        collected
    }
}

impl Drop for VaapiH264Encoder {
    fn drop(&mut self) {
        // Let the driver finish what it is encoding before the context goes.
        for f in &self.in_flight {
            // SAFETY: the surface is live; blocking here is fine at teardown.
            // C: `VAStatus vaSyncSurface(VADisplay, VASurfaceID)`.
            unsafe {
                (self.res.va().sync_surface)(self.res.display.dpy, self.res.surfaces[f.slot])
            };
        }
    }
}

/// Platform tests against a VA-API driver (R-TEST-004): behind `hw-tests` and
/// `#[ignore]`d, because CI has no GPU. Run them on a Linux machine with an Intel or AMD
/// GPU and its VA-API driver:
/// `cargo test -p jstream-platform-linux --features hw-tests -- --ignored`.
#[cfg(all(test, feature = "hw-tests"))]
mod device_tests {
    use jstream_platform::media::{
        Codec, ColorInfo, FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame, VideoFrameInfo,
    };
    use jstream_platform::{
        Codecs, HardwarePreference, RateControl, VideoEncoderConfig, VideoSurface,
    };

    use crate::VaapiCodecs;

    /// Encodes 90 frames of a moving gradient from `format` frames of `size`, with a
    /// keyframe every 30 and one requested at frame 45. 1280x720 is whole macroblocks;
    /// 1920x1080 is coded at 1088 rows and cropped.
    fn encode_three_seconds(format: PixelFormat, size: FrameSize, rate_control: RateControl) {
        let codecs = VaapiCodecs::new();
        let mut enc = codecs
            .video_encoder(&VideoEncoderConfig {
                codec: Codec::H264,
                size,
                input_format: format,
                frame_period: Timebase::new(1, 30).expect("30 fps"),
                rate_control,
                keyframe_interval: 30,
                hardware: HardwarePreference::Require,
            })
            .expect("open the VA-API H.264 encoder");
        assert!(enc.info().hardware);
        let tb = Timebase::MPEG_90K;
        let h = size.height();
        let mut out = Vec::new();
        for i in 0..90i64 {
            let info = VideoFrameInfo {
                size,
                format,
                color: ColorInfo::default(),
                pts: MediaTime::new(i * 3000, tb),
            };
            let mut frame = VideoFrame::zeroed(info).expect("frame");
            let bytes_per_px = if format == PixelFormat::Nv12 { 1 } else { 4 };
            for y in 0..h {
                if let Some(row) = frame.row_mut(0, y) {
                    for (x, p) in row.iter_mut().enumerate() {
                        *p = ((x / bytes_per_px) as i64 + i64::from(y) + i * 4) as u8;
                    }
                }
            }
            for y in 0..h / 2 {
                if let Some(row) = frame.row_mut(1, y) {
                    row.fill(128);
                }
            }
            for _ in 0..1000 {
                enc.poll(&mut out).expect("poll");
                if enc.can_accept() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            if i == 45 {
                enc.request_keyframe().expect("keyframe request");
            }
            enc.encode(VideoSurface::Cpu(frame)).expect("encode");
        }
        enc.finish(&mut out).expect("finish");
        assert_eq!(out.len(), 90, "one packet per frame");
        let keyframes: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_keyframe())
            .map(|(i, _)| i)
            .collect();
        // The cadence restarts at the requested keyframe.
        assert_eq!(keyframes, [0, 30, 45, 75]);
        for (i, p) in out.iter().enumerate() {
            assert_eq!(p.codec(), Codec::H264);
            assert_eq!(p.pts(), MediaTime::new(i as i64 * 3000, tb));
            assert_eq!(p.pts(), p.dts());
            assert!(p.data().starts_with(&[0, 0, 1]) || p.data().starts_with(&[0, 0, 0, 1]));
        }
        // Each keyframe carries an SPS (NAL type 7), ours or the driver's.
        for &k in &keyframes {
            let sps = out[k]
                .data()
                .windows(4)
                .any(|w| w[..3] == [0, 0, 1] && w[3] & 0x1F == 7);
            assert!(sps, "packet {k} carries an SPS");
        }
    }

    #[test]
    #[ignore = "needs a GPU with a VA-API H.264 encode driver"]
    fn encodes_nv12_at_720p_cbr() {
        encode_three_seconds(
            PixelFormat::Nv12,
            FrameSize::new(1280, 720).expect("size"),
            RateControl::Cbr { kbps: 3000 },
        );
    }

    /// #424: the engine's pipeline hands the encoder the compositor's RGBA.
    #[test]
    #[ignore = "needs a GPU with a VA-API H.264 encode driver"]
    fn encodes_rgba_at_1080p_vbr() {
        encode_three_seconds(
            PixelFormat::Rgba8,
            FrameSize::new(1920, 1080).expect("size"),
            RateControl::Vbr {
                kbps: 4000,
                max_kbps: 6000,
            },
        );
    }
}
