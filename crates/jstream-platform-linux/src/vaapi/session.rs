//! One VA-API H.264 or HEVC encode session (ADR-0060 §6): a config and context on one
//! render node, input surfaces with a coded buffer each, and reconstructed surfaces for
//! the reference chain.
//!
//! The session takes CPU frames (NV12, or the compositor's RGBA and BGRA, converted by
//! `upload`). Each is written into a free input surface, through
//! `vaDeriveImage` where the driver maps its surfaces, otherwise through a staging image
//! and `vaPutImage`, and then encoded with the parameters `h264` or `hevc` builds.
//! Up to [`DEPTH`] frames are in flight; `poll` collects each finished one without
//! blocking (`vaQuerySurfaceStatus`), in submission order, which is also presentation
//! order because there are no B-frames. Packets are Annex B: the driver writes the coded
//! slice, preceded on IDR frames by the SPS and PPS ([`h264::sps`], [`h264::pps`]), or
//! the VPS, SPS and PPS ([`hevc::parameter_sets`]), when it takes packed headers, or by
//! its own otherwise. An HEVC session also sends each picture's slice header
//! ([`hevc::slice_header`]) when the driver takes packed slice headers, and codes with
//! the tools the driver's config attributes advertise ([`hevc::Tools`]).
//!
//! A session opened on the engine's GPU context (`open_on` with the context's state)
//! takes that context's encode targets instead (`VideoSurface::Gpu`, `gpu::export`): it
//! imports each target's DMA-BUF once as a VA surface (`vaCreateSurfaces` with a DRM
//! PRIME descriptor), waits until render's write of the frame is done, and encodes
//! from that surface with no copy. It holds the frame until its picture is collected,
//! because the driver reads the surface until then, so render doesn't write that
//! target again before. Its context has only the reconstructed surfaces as render
//! targets; the imported inputs are passed to `vaBeginPicture` alone.

use std::collections::VecDeque;
use std::ffi::{c_int, c_uint, c_void};
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::sync::{Arc, Weak};
use std::time::Duration;

use jstream_platform::media::{Codec, MediaTime, Packet, PixelFormat, VideoFrame};
use jstream_platform::{
    CodecInfo, FrameSync, GpuFrame, PlatformError, VideoEncoder, VideoEncoderConfig, VideoSurface,
};

use super::display::{Display, Va};
use super::ffi::{
    ATTRIB_NOT_SUPPORTED, CODED_BUF_STATUS_SLICE_OVERFLOW_MASK, CodedBufferSegment, ConfigAttrib,
    DrmPrimeSurfaceDescriptor, FOURCC_NV12, INVALID_ID, Id, Image, ImageFormat, Nv12Object,
    PACKED_HEADER_SEQUENCE, PACKED_HEADER_SLICE, PROGRESSIVE, PackedHeaderParam, RT_FORMAT_YUV420,
    SURFACE_RENDERING, SURFACE_SKIPPED, SurfaceAttrib, attrib, buffer, packed, surface_attrib,
};
use super::h264::{self, RcMode};
use super::hevc;
use super::upload::{Nv12Image, write_nv12};
use crate::gpu::Shared;
use crate::gpu::export::{Nv12DmaBuf, TargetSync};
use crate::{VaEncoder, VaProfile};

/// Frames in flight: input surfaces, each with its coded buffer. When all are busy a
/// frame is refused as busy, and the encode thread drops and counts it (R-RT-004).
pub(crate) const DEPTH: usize = 4;
/// Reconstructed surfaces. The driver encodes a context's pictures in order, so a
/// surface is free again once the picture after the one it holds has been submitted;
/// three leave one spare.
const RECON: usize = 3;
/// The most buffers one picture submits: sequence, three misc, picture, the packed
/// sequence header's two, the packed slice header's two (HEVC), slice.
const MAX_BUFFERS: usize = 10;
/// How long an encode waits for render's write of a GPU frame. Render submitted it
/// before queueing the frame, so this is reached only when the GPU hangs.
const WRITE_WAIT: Duration = Duration::from_secs(1);

/// What the session owns in libva, destroyed in reverse order on drop, before the
/// display is terminated.
struct Resources {
    display: Display,
    config: Option<Id>,
    context: Option<Id>,
    /// The [`DEPTH`] input surfaces on CPU frames (none on GPU frames), then the
    /// [`RECON`] reconstructed ones.
    surfaces: Vec<Id>,
    /// The encode targets imported as input surfaces on GPU frames, each with its
    /// target's hold. Weak, so the encode ring still sees a target it can reuse
    /// (`jstream-core`'s `EncodeRing`); the weak hold also keeps its address unique.
    imports: Vec<(Weak<dyn FrameSync>, Id)>,
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

    /// A packed header of `kind` holding `bytes` (whole NAL units, emulation prevention
    /// included): its parameter buffer and its data buffer, pushed onto `buffers`.
    fn packed_header(
        &self,
        kind: u32,
        bytes: &[u8],
        buffers: &mut Vec<Id>,
    ) -> Result<(), PlatformError> {
        let param = PackedHeaderParam {
            kind,
            bit_length: u32::try_from(bytes.len() * 8).unwrap_or(u32::MAX),
            has_emulation_bytes: 1,
            va_reserved: [0; 4],
        };
        buffers.push(self.buffer(buffer::PACKED_HEADER_PARAMETER, &param)?);
        buffers.push(self.data_buffer(buffer::PACKED_HEADER_DATA, bytes)?);
        Ok(())
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

    /// The input surface for `frame`, an encode target holding `dma_buf`: the one
    /// imported for it before, or a new import. Imports of targets that are gone are
    /// destroyed first.
    fn imported(&mut self, frame: &GpuFrame, dma_buf: &Nv12DmaBuf) -> Result<Id, PlatformError> {
        let (va, dpy) = (self.display.va.clone(), self.display.dpy);
        self.imports.retain(|(hold, id)| {
            let live = hold.strong_count() > 0;
            if !live {
                let mut id = *id;
                // SAFETY: an imported surface whose target is gone; no picture in flight
                // uses it, since an in-flight frame holds its target. Destroyed once.
                // C: `VAStatus vaDestroySurfaces(VADisplay, VASurfaceID *, int)`.
                unsafe { (va.destroy_surfaces)(dpy, &mut id, 1) };
            }
            live
        });
        let key = Arc::as_ptr(frame.sync());
        if let Some((_, id)) = self
            .imports
            .iter()
            .find(|(hold, _)| std::ptr::addr_eq(hold.as_ptr(), key))
        {
            return Ok(*id);
        }
        let id = self.import(dma_buf)?;
        self.imports.push((Arc::downgrade(frame.sync()), id));
        Ok(id)
    }

    /// Imports `dma_buf` as an NV12 surface.
    fn import(&self, dma_buf: &Nv12DmaBuf) -> Result<Id, PlatformError> {
        let size = u32::try_from(dma_buf.size).map_err(|_| PlatformError::Unsupported {
            what: "an encode target larger than 4 GiB".into(),
        })?;
        let mut desc = DrmPrimeSurfaceDescriptor::nv12(Nv12Object {
            fd: dma_buf.fd().as_raw_fd(),
            size,
            modifier: dma_buf.modifier,
            width: dma_buf.width,
            height: dma_buf.height,
            offsets: dma_buf.offsets,
            pitches: dma_buf.pitches,
        });
        let mut attribs = [
            SurfaceAttrib::integer(
                surface_attrib::MEMORY_TYPE,
                surface_attrib::MEM_TYPE_DRM_PRIME_2,
            ),
            SurfaceAttrib::pointer(
                surface_attrib::EXTERNAL_BUFFER_DESCRIPTOR,
                (&raw mut desc).cast(),
            ),
        ];
        let mut id = INVALID_ID;
        // SAFETY: the display is live; `attribs` holds two attributes, the second
        // pointing at `desc`, one descriptor for the one surface asked for, which libva
        // reads during the call; its fd is the target's, open while the target lives,
        // and libva doesn't close it; `id` has room for the one ID libva writes.
        // C: `VAStatus vaCreateSurfaces(VADisplay, unsigned int format, unsigned int
        // width, unsigned int height, VASurfaceID *surfaces, unsigned int
        // num_surfaces, VASurfaceAttrib *attrib_list, unsigned int num_attribs)`.
        let status = unsafe {
            (self.va().create_surfaces)(
                self.display.dpy,
                RT_FORMAT_YUV420,
                dma_buf.width,
                dma_buf.height,
                &mut id,
                1,
                attribs.as_mut_ptr().cast(),
                attribs.len() as c_uint,
            )
        };
        self.va()
            .check("vaCreateSurfaces (DRM PRIME import)", status)?;
        Ok(id)
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
                // Segments are whole NAL units: `bit_offset` is 0.
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
            for (_, id) in &mut self.imports {
                (va.destroy_surfaces)(dpy, id, 1);
            }
            if let Some(config) = self.config.take() {
                (va.destroy_config)(dpy, config);
            }
        }
    }
}

/// A frame submitted and not yet collected.
#[derive(Debug)]
struct InFlight {
    slot: usize,
    /// The input surface the driver reads.
    surface: Id,
    pts: MediaTime,
    keyframe: bool,
    /// The encode target, held until the picture is done (GPU frames).
    _target: Option<GpuFrame>,
}

/// The codec's side of a session: its checked settings and its GOP.
#[derive(Debug)]
enum Stream {
    H264 {
        settings: h264::Settings,
        gop: h264::Gop,
    },
    Hevc {
        settings: hevc::Settings,
        gop: hevc::Gop,
    },
}

/// One picture of either codec's stream.
#[derive(Clone, Copy, Debug)]
enum Picture {
    H264(h264::Picture),
    Hevc(hevc::Picture),
}

impl Picture {
    fn idr(&self) -> bool {
        match self {
            Self::H264(p) => p.idr,
            Self::Hevc(p) => p.idr,
        }
    }
}

impl Stream {
    /// The stream for `config` on an encoder of `profile`. HEVC is checked with the
    /// default tools; [`Self::with_tools`] sets the driver's.
    fn new(config: &VideoEncoderConfig, profile: VaProfile) -> Result<Self, PlatformError> {
        Ok(match profile {
            VaProfile::H264(profile) => {
                let settings = h264::check(config, profile)?;
                Self::H264 {
                    gop: h264::Gop::new(&settings),
                    settings,
                }
            }
            VaProfile::HevcMain => {
                let settings = hevc::check(config, hevc::Tools::DEFAULT)?;
                Self::Hevc {
                    gop: hevc::Gop::new(&settings),
                    settings,
                }
            }
        })
    }

    /// An HEVC stream checked again for the driver's `tools`; an H.264 one as it is.
    fn with_tools(
        self,
        config: &VideoEncoderConfig,
        tools: hevc::Tools,
    ) -> Result<Self, PlatformError> {
        Ok(match self {
            Self::Hevc { .. } => {
                let settings = hevc::check(config, tools)?;
                Self::Hevc {
                    gop: hevc::Gop::new(&settings),
                    settings,
                }
            }
            h264 => h264,
        })
    }

    fn codec(&self) -> Codec {
        match self {
            Self::H264 { .. } => Codec::H264,
            Self::Hevc { .. } => Codec::Hevc,
        }
    }

    fn rc(&self) -> RcMode {
        match self {
            Self::H264 { settings, .. } => settings.rc,
            Self::Hevc { settings, .. } => settings.rate.rc,
        }
    }

    /// The size of the surfaces and the context: whole macroblocks for H.264, the
    /// aligned coded size for HEVC. The SPS crops the excess.
    fn coded_size(&self) -> (u32, u32) {
        match self {
            Self::H264 { settings, .. } => (settings.width_mbs * 16, settings.height_mbs * 16),
            Self::Hevc { settings, .. } => (settings.coded_width, settings.coded_height),
        }
    }

    fn coded_buffer_size(&self) -> u32 {
        match self {
            Self::H264 { settings, .. } => settings.coded_buffer_size(),
            Self::Hevc { settings, .. } => settings.coded_buffer_size(),
        }
    }

    /// The parameter sets, sent as one packed sequence header on IDR frames.
    fn parameter_sets(&self) -> Vec<u8> {
        match self {
            Self::H264 { settings, .. } => [h264::sps(settings), h264::pps(settings)].concat(),
            Self::Hevc { settings, .. } => hevc::parameter_sets(settings),
        }
    }

    fn next_picture(&mut self) -> Picture {
        match self {
            Self::H264 { gop, .. } => Picture::H264(gop.next_picture()),
            Self::Hevc { gop, .. } => Picture::Hevc(gop.next_picture()),
        }
    }

    fn force_idr(&mut self) {
        match self {
            Self::H264 { gop, .. } => gop.force_idr(),
            Self::Hevc { gop, .. } => gop.force_idr(),
        }
    }

    /// What failing encode calls name.
    fn encode_what(&self) -> &'static str {
        match self {
            Self::H264 { .. } => "H.264 encode",
            Self::Hevc { .. } => "HEVC encode",
        }
    }

    fn keyframe_what(&self) -> &'static str {
        match self {
            Self::H264 { .. } => "H.264 keyframe request",
            Self::Hevc { .. } => "HEVC keyframe request",
        }
    }

    fn encoder_what(&self) -> &'static str {
        match self {
            Self::H264 { .. } => "VA-API H.264 encoder",
            Self::Hevc { .. } => "VA-API HEVC encoder",
        }
    }
}

/// One H.264 or HEVC encode session on a VA-API driver.
pub(crate) struct VaapiEncoder {
    res: Resources,
    info: CodecInfo,
    config: VideoEncoderConfig,
    stream: Stream,
    /// The parameter sets, sent as one packed sequence header on IDR frames when the
    /// driver takes packed headers (`None`: it writes its own).
    headers: Option<Vec<u8>>,
    /// Whether the driver takes packed slice headers (HEVC only).
    packed_slice: bool,
    /// The last picture's reconstructed surface: the next P frame's reference.
    reference: Option<(Id, Picture)>,
    /// How many of `res.surfaces` are input surfaces: [`DEPTH`] on CPU frames, none on
    /// GPU frames. The reconstructed ones follow.
    inputs: usize,
    next_recon: usize,
    /// The GPU context's state, on GPU frames.
    gpu: Option<Arc<Shared>>,
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
unsafe impl Send for VaapiEncoder {}

impl VaapiEncoder {
    /// Opens a session on `encoder` (a probe result) for `config`, reported as `info`,
    /// that takes CPU frames, or, with `gpu`, that context's NV12 encode targets. The
    /// encoder's profile says which codec it encodes.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the configuration is outside what
    /// [`h264::check`] or [`hevc::check`] accepts, a GPU session isn't configured for
    /// NV12, the driver lacks 4:2:0 surfaces or the rate-control mode, or, for HEVC, the
    /// driver requires a tool JStream's parameter sets turn off
    /// ([`hevc::Tools::required_but_off`]), [`PlatformError::MissingComponent`] when libva is missing, and
    /// [`PlatformError::Os`] when the node or the driver fails.
    pub(crate) fn open_on(
        encoder: &VaEncoder,
        config: &VideoEncoderConfig,
        info: CodecInfo,
        gpu: Option<Arc<Shared>>,
    ) -> Result<Self, PlatformError> {
        // Checked before the driver is opened, so a refusal costs nothing.
        let stream = Stream::new(config, encoder.profile)?;
        if gpu.is_some() && config.input_format != PixelFormat::Nv12 {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "{:?} GPU frames on {} (encode targets are NV12)",
                    config.input_format, info.name
                ),
            });
        }
        let inputs = if gpu.is_some() { 0 } else { DEPTH };
        let display = Display::open(Va::load()?, &encoder.node)?;
        let mut res = Resources {
            display,
            config: None,
            context: None,
            surfaces: Vec::new(),
            imports: Vec::new(),
            coded: Vec::new(),
            staging: None,
        };
        let (va, dpy) = (res.display.va.clone(), res.display.dpy);
        let (profile, entrypoint) = (encoder.profile.va(), encoder.entrypoint as c_int);
        let hevc = stream.codec() == Codec::Hevc;
        let mut offered = [
            attrib::RT_FORMAT,
            attrib::RATE_CONTROL,
            attrib::ENC_PACKED_HEADERS,
            // HEVC only: what the driver codes.
            attrib::ENC_HEVC_FEATURES,
            attrib::ENC_HEVC_BLOCK_SIZES,
            attrib::PREDICTION_DIRECTION,
        ]
        .map(|kind| ConfigAttrib { kind, value: 0 });
        let asked: c_int = if hevc { 6 } else { 3 };
        // SAFETY: the display is initialised; `offered` holds at least `asked`
        // attributes, whose values libva fills in place.
        // C: `VAStatus vaGetConfigAttributes(VADisplay, VAProfile, VAEntrypoint,
        // VAConfigAttrib *attrib_list, int num_attribs)`.
        let status = unsafe {
            (va.get_config_attributes)(dpy, profile, entrypoint, offered.as_mut_ptr(), asked)
        };
        va.check("vaGetConfigAttributes", status)?;
        let stream = if hevc {
            if let Some(tool) = hevc::Tools::required_but_off(offered[3].value) {
                return Err(PlatformError::Unsupported {
                    what: format!("HEVC encoding that requires {tool} on {}", info.name),
                });
            }
            let tools =
                hevc::Tools::from_attributes(offered[3].value, offered[4].value, offered[5].value);
            stream.with_tools(config, tools)?
        } else {
            stream
        };
        let has =
            |a: ConfigAttrib, bits: u32| a.value != ATTRIB_NOT_SUPPORTED && a.value & bits == bits;
        if !has(offered[0], RT_FORMAT_YUV420) {
            return Err(PlatformError::Unsupported {
                what: format!("4:2:0 encode surfaces on {}", info.name),
            });
        }
        let rc = stream.rc();
        if !has(offered[1], rc.va()) {
            return Err(PlatformError::Unsupported {
                what: format!("{rc:?} rate control on {}", info.name),
            });
        }
        let packed = has(offered[2], PACKED_HEADER_SEQUENCE);
        // JStream's slice headers match its own parameter sets, not the driver's.
        let packed_slice = packed && hevc && has(offered[2], PACKED_HEADER_SLICE);
        let mut wanted = vec![
            ConfigAttrib {
                kind: attrib::RT_FORMAT,
                value: RT_FORMAT_YUV420,
            },
            ConfigAttrib {
                kind: attrib::RATE_CONTROL,
                value: rc.va(),
            },
        ];
        let packed_bits = if packed { PACKED_HEADER_SEQUENCE } else { 0 }
            | if packed_slice { PACKED_HEADER_SLICE } else { 0 };
        if packed_bits != 0 {
            wanted.push(ConfigAttrib {
                kind: attrib::ENC_PACKED_HEADERS,
                value: packed_bits,
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

        // Surfaces at the coded (macroblock- or block-aligned) size; the SPS crops the
        // excess.
        let (cw, ch) = stream.coded_size();
        let mut surfaces = vec![INVALID_ID; inputs + RECON];
        // SAFETY: `surfaces` has room for the `inputs + RECON` IDs libva writes; no
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
                surfaces.len() as c_uint,
                std::ptr::null_mut(),
                0,
            )
        };
        va.check("vaCreateSurfaces", status)?;
        res.surfaces = surfaces.clone();
        let mut context = INVALID_ID;
        // SAFETY: the config is live; `surfaces` holds the context's render targets
        // (the input and reconstructed surfaces it owns; imported inputs are named only
        // to `vaBeginPicture`), which libva reads; `context` is a valid out-pointer.
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
                surfaces.len() as c_int,
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
                    stream.coded_buffer_size() as usize,
                )
            }?;
            res.coded.push(coded);
        }
        if gpu.is_none() {
            res.staging = Self::staging(&res, cw, ch)?;
        }
        Ok(Self {
            res,
            info,
            config: *config,
            headers: packed.then(|| stream.parameter_sets()),
            packed_slice,
            stream,
            reference: None,
            inputs,
            next_recon: 0,
            gpu,
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

    /// Submits `picture` from `input`, with `slot`'s coded buffer.
    fn submit(&mut self, slot: usize, input: Id, picture: &Picture) -> Result<(), PlatformError> {
        let recon = self.res.surfaces[self.inputs + self.next_recon];
        let reference = self.reference;
        let res = &self.res;
        let stream = &self.stream;
        let headers = self.headers.as_deref().filter(|_| picture.idr());
        let packed_slice = self.packed_slice;
        let buffers = &mut self.buffers;
        buffers.clear();
        let mut built = || -> Result<(), PlatformError> {
            let coded = res.coded[slot];
            match (stream, picture) {
                (Stream::H264 { settings: s, .. }, Picture::H264(p)) => {
                    let reference = reference.as_ref().and_then(|(id, r)| match r {
                        Picture::H264(r) => Some((*id, r)),
                        Picture::Hevc(_) => None,
                    });
                    if p.idr {
                        buffers.push(res.buffer(buffer::SEQUENCE_PARAMETER, &h264::sequence(s))?);
                        buffers.push(res.buffer(buffer::MISC_PARAMETER, &h264::rate_control(s))?);
                        buffers.push(res.buffer(buffer::MISC_PARAMETER, &h264::hrd(s))?);
                        buffers.push(res.buffer(buffer::MISC_PARAMETER, &h264::frame_rate(s))?);
                    }
                    buffers.push(res.buffer(
                        buffer::PICTURE_PARAMETER,
                        &h264::picture(s, p, recon, reference, coded),
                    )?);
                    if let Some(headers) = headers {
                        res.packed_header(packed::SEQUENCE, headers, buffers)?;
                    }
                    buffers
                        .push(res.buffer(buffer::SLICE_PARAMETER, &h264::slice(s, p, reference))?);
                }
                (Stream::Hevc { settings: s, .. }, Picture::Hevc(p)) => {
                    let reference = reference.as_ref().and_then(|(id, r)| match r {
                        Picture::Hevc(r) => Some((*id, r)),
                        Picture::H264(_) => None,
                    });
                    if p.idr {
                        buffers.push(res.buffer(buffer::SEQUENCE_PARAMETER, &hevc::sequence(s))?);
                        buffers.push(res.buffer(buffer::MISC_PARAMETER, &hevc::rate_control(s))?);
                        buffers.push(res.buffer(buffer::MISC_PARAMETER, &hevc::hrd(s))?);
                        buffers.push(res.buffer(buffer::MISC_PARAMETER, &hevc::frame_rate(s))?);
                    }
                    buffers.push(res.buffer(
                        buffer::PICTURE_PARAMETER,
                        &hevc::picture(s, p, recon, reference, coded),
                    )?);
                    if let Some(headers) = headers {
                        res.packed_header(packed::SEQUENCE, headers, buffers)?;
                    }
                    if packed_slice {
                        let header = hevc::slice_header(s, p, reference);
                        res.packed_header(packed::SLICE, &header, buffers)?;
                    }
                    buffers
                        .push(res.buffer(buffer::SLICE_PARAMETER, &hevc::slice(s, p, reference))?);
                }
                _ => {
                    return Err(PlatformError::InvalidState {
                        what: stream.encode_what(),
                        state: "the picture is of the other codec",
                    });
                }
            }
            let (va, dpy, ctx) = (res.va(), res.display.dpy, res.context());
            // SAFETY: the context and input surface are live, and the surface is idle.
            // C: `VAStatus vaBeginPicture(VADisplay, VAContextID, VASurfaceID)`.
            let status = unsafe { (va.begin_picture)(dpy, ctx, input) };
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
        while let Some(front) = self.in_flight.front() {
            let Some(coded) = self.res.ready(front.surface, wait)? else {
                return Ok(());
            };
            let Some(front) = self.in_flight.pop_front() else {
                return Ok(());
            };
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
                    self.stream.force_idr();
                    self.reference = None;
                    other?;
                    continue;
                }
            };
            self.pending.push(Packet::new(
                self.stream.codec(),
                front.keyframe,
                front.pts,
                front.pts,
                data,
            )?);
        }
        Ok(())
    }
}

impl VaapiEncoder {
    /// The input surface for `frame`, written or imported, and the encode target to
    /// hold while the driver reads it.
    fn input(
        &mut self,
        slot: usize,
        frame: VideoSurface,
    ) -> Result<(Id, Option<GpuFrame>), PlatformError> {
        match (frame, &self.gpu) {
            (VideoSurface::Cpu(frame), None) => {
                let surface = self.res.surfaces[slot];
                self.res.upload(surface, &frame)?;
                Ok((surface, None))
            }
            (VideoSurface::Gpu(frame), Some(shared)) => {
                let sync = frame
                    .sync()
                    .platform()
                    .and_then(|p| p.downcast_ref::<TargetSync>())
                    .filter(|s| s.is_on(shared))
                    .ok_or_else(|| PlatformError::Unsupported {
                        what: "a GPU frame that is not one of this context's encode targets".into(),
                    })?;
                sync.wait_written(WRITE_WAIT)?;
                let surface = self.res.imported(&frame, sync.dma_buf())?;
                Ok((surface, Some(frame)))
            }
            (VideoSurface::Cpu(_), Some(_)) => Err(PlatformError::Unsupported {
                what: "a CPU frame on a VA-API session opened for GPU frames".into(),
            }),
            (VideoSurface::Gpu(_), None) => Err(PlatformError::Unsupported {
                what: "a GPU frame on a VA-API session opened for CPU frames".into(),
            }),
            _ => Err(PlatformError::Unsupported {
                what: "a kind of frame the VA-API session doesn't know".into(),
            }),
        }
    }
}

impl VideoEncoder for VaapiEncoder {
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
                what: self.stream.encode_what(),
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
        let Some(slot) = self.free.pop() else {
            return Err(PlatformError::Busy {
                what: self.stream.encoder_what(),
            });
        };
        let submitted = self.input(slot, frame).and_then(|(surface, target)| {
            let picture = self.stream.next_picture();
            self.submit(slot, surface, &picture)
                .map(|()| (surface, target, picture))
        });
        match submitted {
            Ok((surface, target, picture)) => {
                self.in_flight.push_back(InFlight {
                    slot,
                    surface,
                    pts: info.pts,
                    keyframe: picture.idr(),
                    _target: target,
                });
                Ok(())
            }
            Err(e) => {
                self.free.push(slot);
                self.stream.force_idr();
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

    /// The next frame is encoded as an IDR frame, with the parameter sets, and the
    /// keyframe cadence restarts from it.
    fn request_keyframe(&mut self) -> Result<(), PlatformError> {
        if self.finished {
            return Err(PlatformError::InvalidState {
                what: self.stream.keyframe_what(),
                state: "the encoder is finished",
            });
        }
        self.stream.force_idr();
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), PlatformError> {
        self.finished = true;
        let collected = self.collect(true);
        out.append(&mut self.pending);
        collected
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        // Let the driver finish what it is encoding before the context goes.
        for f in &self.in_flight {
            // SAFETY: the surface is live; blocking here is fine at teardown.
            // C: `VAStatus vaSyncSurface(VADisplay, VASurfaceID)`.
            unsafe { (self.res.va().sync_surface)(self.res.display.dpy, f.surface) };
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
        Codecs, GpuContext, GpuFrame, GpuInterop, HardwarePreference, RateControl,
        VideoEncoderConfig, VideoSurface,
    };

    use crate::{LinuxGpu, VaapiCodecs};

    /// Encodes 90 frames of a moving gradient as `codec` from `format` frames of `size`,
    /// with a keyframe every 30 and one requested at frame 45. 1280x720 is whole
    /// macroblocks; 1920x1080 is coded at 1088 rows and cropped (in H.264, and in HEVC
    /// on a driver whose coding blocks are 16x16).
    fn encode_three_seconds(
        codec: Codec,
        format: PixelFormat,
        size: FrameSize,
        rate_control: RateControl,
    ) {
        let codecs = VaapiCodecs::new();
        let mut enc = codecs
            .video_encoder(&VideoEncoderConfig {
                codec,
                size,
                input_format: format,
                frame_period: Timebase::new(1, 30).expect("30 fps"),
                rate_control,
                keyframe_interval: 30,
                hardware: HardwarePreference::Require,
            })
            .expect("open the VA-API encoder");
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
            assert_eq!(p.codec(), codec);
            assert_eq!(p.pts(), MediaTime::new(i as i64 * 3000, tb));
            assert_eq!(p.pts(), p.dts());
            assert!(p.data().starts_with(&[0, 0, 1]) || p.data().starts_with(&[0, 0, 0, 1]));
        }
        // Each keyframe carries an SPS (H.264 NAL type 7, HEVC 33), ours or the
        // driver's.
        let is_sps = |header: u8| match codec {
            Codec::Hevc => (header >> 1) & 0x3F == 33,
            _ => header & 0x1F == 7,
        };
        for &k in &keyframes {
            let sps = out[k]
                .data()
                .windows(4)
                .any(|w| w[..3] == [0, 0, 1] && is_sps(w[3]));
            assert!(sps, "packet {k} carries an SPS");
        }
    }

    #[test]
    #[ignore = "needs a GPU with a VA-API H.264 encode driver"]
    fn encodes_nv12_at_720p_cbr() {
        encode_three_seconds(
            Codec::H264,
            PixelFormat::Nv12,
            FrameSize::new(1280, 720).expect("size"),
            RateControl::Cbr { kbps: 3000 },
        );
    }

    /// Draws a flat NV12 picture into `target`, as render's NV12 pass does: both plane
    /// views as render targets, the hold staged, the texture handed back in `PRESENT`.
    fn draw(context: &GpuContext, target: &GpuFrame, luma: f64) {
        let device = context.device();
        let view = |aspect, format| {
            target.texture().create_view(&wgpu::TextureViewDescriptor {
                format: Some(format),
                aspect,
                ..Default::default()
            })
        };
        let y = view(wgpu::TextureAspect::Plane0, wgpu::TextureFormat::R8Unorm);
        let uv = view(wgpu::TextureAspect::Plane1, wgpu::TextureFormat::Rg8Unorm);
        let mut encoder = device.create_command_encoder(&Default::default());
        for (view, value) in [(&y, (luma, 0.0)), (&uv, (0.5, 0.5))] {
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: value.0,
                            g: value.1,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: target.texture(),
                selector: None,
                state: wgpu::TextureUses::PRESENT,
            }),
        );
        target.stage(context.queue()).expect("stage");
        context.queue().submit([encoder.finish()]);
    }

    /// The GPU path end to end: the context's encode targets, exported by Vulkan as
    /// DMA-BUFs, drawn in turn and encoded by VA-API from the imported surfaces, each
    /// reused once the encoder has let it go.
    #[test]
    #[ignore = "needs a GPU whose Vulkan and VA-API H.264 drivers share a render node"]
    fn encodes_the_contexts_encode_targets() {
        let gpu = LinuxGpu::new();
        let context = gpu.open_device().expect("a Vulkan device");
        let codecs = VaapiCodecs::new()
            .on_context(&context)
            .expect("our context")
            .expect("VA-API H.264 on the context's render node");
        let size = FrameSize::new(1280, 720).expect("size");
        let mut enc = codecs
            .video_encoder(&VideoEncoderConfig {
                codec: Codec::H264,
                size,
                input_format: PixelFormat::Nv12,
                frame_period: Timebase::new(1, 30).expect("30 fps"),
                rate_control: RateControl::Cbr { kbps: 3000 },
                keyframe_interval: 30,
                hardware: HardwarePreference::Require,
            })
            .expect("open the VA-API H.264 encoder on GPU frames");
        let targets: Vec<GpuFrame> = (0..6)
            .map(|_| gpu.encode_target(&context, size).expect("an encode target"))
            .collect();
        let tb = Timebase::MPEG_90K;
        let mut out = Vec::new();
        for i in 0..60usize {
            let mut target = None;
            for _ in 0..1000 {
                enc.poll(&mut out).expect("poll");
                target = targets
                    .iter()
                    .cycle()
                    .skip(i)
                    .take(targets.len())
                    .find(|t| std::sync::Arc::strong_count(t.sync()) == 1);
                if target.is_some() && enc.can_accept() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let target = target.expect("a target the encoder let go");
            draw(&context, target, (i % 30) as f64 / 30.0);
            let frame = target.clone().with_pts(MediaTime::new(i as i64 * 3000, tb));
            enc.encode(VideoSurface::Gpu(frame)).expect("encode");
        }
        enc.finish(&mut out).expect("finish");
        assert_eq!(out.len(), 60, "one packet per frame");
        assert!(out[0].is_keyframe() && out[30].is_keyframe());
        for (i, p) in out.iter().enumerate() {
            assert_eq!(p.pts(), MediaTime::new(i as i64 * 3000, tb));
        }
        // Every target is free again once the encoder is finished.
        drop(enc);
        assert!(
            targets
                .iter()
                .all(|t| std::sync::Arc::strong_count(t.sync()) == 1)
        );
    }

    /// #424: the engine's pipeline hands the encoder the compositor's RGBA.
    #[test]
    #[ignore = "needs a GPU with a VA-API H.264 encode driver"]
    fn encodes_rgba_at_1080p_vbr() {
        encode_three_seconds(
            Codec::H264,
            PixelFormat::Rgba8,
            FrameSize::new(1920, 1080).expect("size"),
            RateControl::Vbr {
                kbps: 4000,
                max_kbps: 6000,
            },
        );
    }

    /// #736: HEVC through the same session.
    #[test]
    #[ignore = "needs a GPU with a VA-API HEVC encode driver"]
    fn encodes_hevc_nv12_at_720p_cbr() {
        encode_three_seconds(
            Codec::Hevc,
            PixelFormat::Nv12,
            FrameSize::new(1280, 720).expect("size"),
            RateControl::Cbr { kbps: 3000 },
        );
    }

    #[test]
    #[ignore = "needs a GPU with a VA-API HEVC encode driver"]
    fn encodes_hevc_rgba_at_1080p_vbr() {
        encode_three_seconds(
            Codec::Hevc,
            PixelFormat::Rgba8,
            FrameSize::new(1920, 1080).expect("size"),
            RateControl::Vbr {
                kbps: 4000,
                max_kbps: 6000,
            },
        );
    }
}
