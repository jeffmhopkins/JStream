//! [`VaapiH264Decoder`]: H.264 decoding on the GPU through VA-API's VLD entry point, for
//! media files (ADR-0072 (b)). The planner (`media::h264::dpb`) says what each picture
//! references and when it is output; this module hands the GPU the parameters
//! (`h264_dec`) and the slices, and reads the decoded pictures back as NV12.
//!
//! Decoding is synchronous: each picture is read back when the planner outputs it, after
//! `vaSyncSurface`. Reading back maps the surface (`vaDeriveImage`) where the driver
//! allows it, and otherwise copies it into an NV12 image (`vaGetImage`).

use std::ffi::{c_int, c_uint, c_void};
use std::sync::Arc;

use jstream_platform::PlatformError;
use jstream_platform::media::{FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame};

use super::display::{Display, Va};
use super::ffi::{
    ConfigAttrib, ENTRYPOINT_VLD, FOURCC_NV12, INVALID_ID, Id, Image, ImageFormat, PROGRESSIVE,
    RT_FORMAT_YUV420, attrib, dec_buffer,
};
use super::h264_dec;
use crate::media::h264::bits::ParseError;
use crate::media::h264::dpb::{Output, Planner};
use crate::media::h264::params::Sps;
use crate::media::h264::{AvcConfig, split_length_prefixed};
use crate::media::yuv::{self, Colour, Nv12};

/// What `MissingComponent` names.
const COMPONENT: &str = "a VA-API H.264 decoder";
/// What to install for it.
const REMEDY: &str = "install your GPU's VA-API driver with H.264 decoding: Mesa's \
    (Debian and Ubuntu: mesa-va-drivers; Fedora: mesa-va-drivers-freeworld from RPM \
    Fusion, since Fedora's own build leaves H.264 out) for AMD, or intel-media-va-driver \
    for Intel. NVIDIA's driver has no VA-API decoder of its own";

/// libva's `VAProfile`s for H.264 decoding, best match first for a stream's
/// `profile_idc`: a High decoder decodes Main and Constrained Baseline streams too.
fn profiles(sps: &Sps) -> Vec<c_int> {
    const CONSTRAINED_BASELINE: c_int = 13;
    const MAIN: c_int = 6;
    const HIGH: c_int = 7;
    match sps.profile_idc {
        66 => vec![CONSTRAINED_BASELINE, MAIN, HIGH],
        77 => vec![MAIN, HIGH],
        _ => vec![HIGH],
    }
}

/// Maps a parse error to the platform's.
pub(crate) fn parse_error(err: ParseError) -> PlatformError {
    match err {
        ParseError::Unsupported(what) => PlatformError::Unsupported {
            what: format!("{what} in a media file on Linux"),
        },
        ParseError::Truncated => PlatformError::Os {
            call: "H.264 parser",
            code: None,
            message: "a NAL unit ends early".into(),
        },
        ParseError::Invalid(what) => PlatformError::Os {
            call: "H.264 parser",
            code: None,
            message: format!("an invalid stream: {what}"),
        },
    }
}

/// The libva objects of one decoder, destroyed on drop.
struct Resources {
    display: Display,
    config: Option<Id>,
    context: Option<Id>,
    surfaces: Vec<Id>,
    /// The image `vaGetImage` copies into, once `vaDeriveImage` has failed.
    image: Option<Image>,
}

impl Resources {
    fn va(&self) -> &Va {
        &self.display.va
    }

    /// A buffer of `kind` holding a copy of `value`.
    fn buffer<T>(&self, kind: c_int, value: &T) -> Result<Id, PlatformError> {
        // SAFETY: plain data; libva copies `size_of::<T>()` bytes from the pointer while
        // `value` is borrowed and doesn't write through it.
        unsafe { self.raw_buffer(kind, (value as *const T).cast(), size_of::<T>()) }
    }

    /// # Safety
    /// `data` points to `size` readable bytes.
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
        // SAFETY: the context is live; `data` holds `size` bytes (this function's
        // contract), which libva copies; `id` is a valid out-pointer.
        // C: `VAStatus vaCreateBuffer(VADisplay, VAContextID, VABufferType,
        // unsigned int size, unsigned int num_elements, void *data, VABufferID *buf_id)`.
        let status = unsafe {
            (self.va().create_buffer)(
                self.display.dpy,
                self.context.unwrap_or(INVALID_ID),
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

    fn destroy_buffers(&self, ids: &[Id]) {
        for &id in ids {
            // SAFETY: `id` is a buffer this decoder created and destroys once.
            // C: `VAStatus vaDestroyBuffer(VADisplay, VABufferID)`.
            unsafe { (self.va().destroy_buffer)(self.display.dpy, id) };
        }
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        let (va, dpy) = (&self.display.va, self.display.dpy);
        // SAFETY: each ID was created on this display, is destroyed once, here, in the
        // reverse order of creation; failures leave nothing to undo.
        // C: `VAStatus vaDestroyImage(VADisplay, VAImageID)`,
        // `VAStatus vaDestroyContext(VADisplay, VAContextID)`,
        // `VAStatus vaDestroySurfaces(VADisplay, VASurfaceID *surfaces, int num_surfaces)`,
        // `VAStatus vaDestroyConfig(VADisplay, VAConfigID)`.
        unsafe {
            if let Some(image) = self.image.take() {
                (va.destroy_image)(dpy, image.image_id);
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

/// The part of a VA-API decoder every codec shares: the display, config, context and
/// surfaces, and reading a decoded surface back as a frame.
pub(super) struct Core {
    res: Resources,
    /// The visible picture: its crop offset and size.
    crop: (usize, usize),
    size: FrameSize,
    coded: (u32, u32),
    colour: Colour,
    format: PixelFormat,
    /// Whether `vaDeriveImage` has failed once, so `vaGetImage` is used from then on.
    no_derive: bool,
    /// The driver's vendor string, for the logs.
    vendor: String,
}

/// What a codec's decoder asks [`Core::open`] for.
pub(super) struct CoreConfig<'a> {
    /// libva's `VAProfile`s that decode the stream, best first.
    pub(super) profiles: &'a [c_int],
    /// What `MissingComponent` names, and what to install for it.
    pub(super) component: &'static str,
    pub(super) remedy: &'static str,
    /// The surfaces' size, and how many.
    pub(super) coded: (u32, u32),
    pub(super) surfaces: usize,
    /// The visible picture: its crop offset and size.
    pub(super) crop: (usize, usize),
    pub(super) size: FrameSize,
    pub(super) colour: Colour,
    pub(super) format: PixelFormat,
}

/// One H.264 stream decoded on the GPU. See the module docs.
pub(crate) struct VaapiH264Decoder {
    core: Core,
    planner: Planner,
    length_size: usize,
    timebase: Timebase,
    /// Decoded frames, in presentation order, not yet taken.
    ready: std::collections::VecDeque<VideoFrame>,
}

// SAFETY: the VA display and every object on it are used only through `&mut self`, from
// whichever one thread owns the decoder; libva allows a display to move between threads
// as long as calls on it don't overlap.
unsafe impl Send for VaapiH264Decoder {}

/// The pixel formats a decoder puts out: `Unsupported` for any other.
pub(super) fn check_format(format: PixelFormat) -> Result<(), PlatformError> {
    if matches!(
        format,
        PixelFormat::Rgba8 | PixelFormat::Bgra8 | PixelFormat::Nv12
    ) {
        Ok(())
    } else {
        Err(PlatformError::Unsupported {
            what: format!("decoding media files to {format:?} on Linux"),
        })
    }
}

impl Core {
    /// Finds a render node whose driver decodes one of `config.profiles`, and opens a
    /// decoding context on it with its surfaces.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when libva or such a driver is missing, and
    /// [`PlatformError::Os`] when the driver refuses.
    pub(super) fn open(config: &CoreConfig<'_>) -> Result<Self, PlatformError> {
        let missing = || PlatformError::MissingComponent {
            component: config.component,
            remedy: config.remedy.into(),
        };
        let va = Va::load().map_err(|_| missing())?;
        let (display, profile) = find_decoder(&va, config.profiles).map_err(|_| missing())?;
        let vendor = display.vendor();
        let mut res = Resources {
            display,
            config: None,
            context: None,
            surfaces: Vec::new(),
            image: None,
        };
        let (va, dpy) = (res.display.va.clone(), res.display.dpy);
        let coded = config.coded;
        let slots = config.surfaces;
        let mut wanted = [ConfigAttrib {
            kind: attrib::RT_FORMAT,
            value: RT_FORMAT_YUV420,
        }];
        let mut id = INVALID_ID;
        // SAFETY: `wanted` holds one attribute, which libva reads; `id` is a valid
        // out-pointer.
        // C: `VAStatus vaCreateConfig(VADisplay, VAProfile, VAEntrypoint,
        // VAConfigAttrib *attrib_list, int num_attribs, VAConfigID *config_id)`.
        let status = unsafe {
            (va.create_config)(
                dpy,
                profile,
                ENTRYPOINT_VLD,
                wanted.as_mut_ptr(),
                1,
                &mut id,
            )
        };
        va.check("vaCreateConfig", status)?;
        res.config = Some(id);

        let mut surfaces = vec![INVALID_ID; slots];
        // SAFETY: `surfaces` has room for the `slots` IDs libva writes; no surface
        // attributes are passed.
        // C: `VAStatus vaCreateSurfaces(VADisplay, unsigned int format, unsigned int
        // width, unsigned int height, VASurfaceID *surfaces, unsigned int
        // num_surfaces, VASurfaceAttrib *attrib_list, unsigned int num_attribs)`.
        let status = unsafe {
            (va.create_surfaces)(
                dpy,
                RT_FORMAT_YUV420,
                coded.0,
                coded.1,
                surfaces.as_mut_ptr(),
                c_uint::try_from(slots).unwrap_or(0),
                std::ptr::null_mut(),
                0,
            )
        };
        va.check("vaCreateSurfaces", status)?;
        res.surfaces = surfaces.clone();
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
                c_int::try_from(coded.0).unwrap_or(0),
                c_int::try_from(coded.1).unwrap_or(0),
                PROGRESSIVE,
                surfaces.as_mut_ptr(),
                c_int::try_from(slots).unwrap_or(0),
                &mut context,
            )
        };
        va.check("vaCreateContext", status)?;
        res.context = Some(context);
        tracing::debug!(%vendor, profile, ?coded, "VA-API decoder");
        Ok(Self {
            res,
            crop: config.crop,
            size: config.size,
            coded,
            colour: config.colour,
            format: config.format,
            no_derive: false,
            vendor,
        })
    }

    /// The visible size of every frame.
    pub(super) fn size(&self) -> FrameSize {
        self.size
    }

    /// The surfaces, by slot.
    pub(super) fn surfaces(&self) -> &[Id] {
        &self.res.surfaces
    }

    /// The driver's vendor string.
    pub(super) fn vendor(&self) -> &str {
        &self.vendor
    }

    /// A parameter buffer of `kind` holding a copy of `value`.
    pub(super) fn buffer<T>(&self, kind: c_int, value: &T) -> Result<Id, PlatformError> {
        self.res.buffer(kind, value)
    }

    /// Destroys buffers that won't be rendered.
    pub(super) fn destroy_buffers(&self, buffers: &[Id]) {
        self.res.destroy_buffers(buffers);
    }

    /// A slice data buffer holding a copy of `data`.
    pub(super) fn data_buffer(&self, data: &[u8]) -> Result<Id, PlatformError> {
        // SAFETY: libva copies the bytes while `data` is borrowed.
        unsafe {
            self.res
                .raw_buffer(dec_buffer::SLICE_DATA, data.as_ptr().cast(), data.len())
        }
    }

    /// Decodes one picture into `target` from `buffers`, which this destroys either way.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the driver refuses the picture.
    pub(super) fn render(&self, target: Id, buffers: &[Id]) -> Result<(), PlatformError> {
        let result = (|| -> Result<(), PlatformError> {
            let (va, dpy) = (self.res.va(), self.res.display.dpy);
            let context = self.res.context.unwrap_or(INVALID_ID);
            // SAFETY: the context and the target surface are live.
            // C: `VAStatus vaBeginPicture(VADisplay, VAContextID, VASurfaceID)`.
            va.check("vaBeginPicture", unsafe {
                (va.begin_picture)(dpy, context, target)
            })?;
            let mut all = buffers.to_vec();
            // SAFETY: `all` holds `len` live buffers of this context, which libva reads.
            // C: `VAStatus vaRenderPicture(VADisplay, VAContextID, VABufferID *buffers,
            // int num_buffers)`.
            let rendered = va.check("vaRenderPicture", unsafe {
                (va.render_picture)(
                    dpy,
                    context,
                    all.as_mut_ptr(),
                    c_int::try_from(all.len()).unwrap_or(0),
                )
            });
            // SAFETY: a picture was begun on this context.
            // C: `VAStatus vaEndPicture(VADisplay, VAContextID)`.
            let ended = va.check("vaEndPicture", unsafe { (va.end_picture)(dpy, context) });
            rendered.and(ended)
        })();
        self.res.destroy_buffers(buffers);
        result
    }
}

impl VaapiH264Decoder {
    /// Opens a decoder for the stream `record` (an `avcC` record) describes, whose
    /// sample times are in `timebase`, putting out frames in `format`.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when libva or a driver with an H.264 decoder
    /// for the stream's profile is missing, [`PlatformError::Unsupported`] for a stream
    /// this doesn't decode (interlaced fields, 10-bit, 4:2:2, slice groups) or a format
    /// it can't put out, and [`PlatformError::Os`] when the driver refuses.
    pub(crate) fn open(
        record: &[u8],
        timebase: Timebase,
        format: PixelFormat,
    ) -> Result<Self, PlatformError> {
        check_format(format)?;
        let config = AvcConfig::parse(record).map_err(parse_error)?;
        let mut planner = Planner::new(0);
        for set in &config.parameter_sets {
            planner.parameter_set(set).map_err(parse_error)?;
        }
        let sps = planner
            .first_sps()
            .cloned()
            .ok_or_else(|| PlatformError::Os {
                call: "H.264 parser",
                code: None,
                message: "the avcC record has no SPS".into(),
            })?;
        if sps.chroma_format_idc != 1 || sps.bit_depth_luma_minus8 != 0 {
            return Err(PlatformError::Unsupported {
                what: "H.264 other than 8-bit 4:2:0 in a media file on Linux".into(),
            });
        }
        let slots = Planner::slots_for(&sps);
        planner = Planner::new(slots);
        for set in &config.parameter_sets {
            planner.parameter_set(set).map_err(parse_error)?;
        }
        let (visible_w, visible_h) = sps.visible_size();
        let size = FrameSize::new(visible_w & !1, visible_h & !1)?;
        let coded = sps.coded_size();
        let [left, _, top, _] = sps.crop;
        let core = Core::open(&CoreConfig {
            profiles: &profiles(&sps),
            component: COMPONENT,
            remedy: REMEDY,
            coded,
            surfaces: slots,
            crop: (left as usize, top as usize),
            size,
            colour: Colour::from_vui(sps.matrix_coefficients, sps.full_range, visible_h),
            format,
        })?;
        Ok(Self {
            core,
            planner,
            length_size: config.length_size,
            timebase,
            ready: std::collections::VecDeque::new(),
        })
    }

    /// The visible size of every frame.
    pub(crate) fn size(&self) -> FrameSize {
        self.core.size()
    }

    /// Decodes one sample (an access unit, its NAL units length-prefixed) shown at
    /// `pts` ticks. The frames it lets out are queued for [`pop`](Self::pop).
    ///
    /// # Errors
    /// As for [`open`](Self::open), for what the sample holds, and
    /// [`PlatformError::Os`] when the driver fails.
    pub(crate) fn decode(&mut self, sample: &[u8], pts: i64) -> Result<(), PlatformError> {
        let nals = split_length_prefixed(sample, self.length_size);
        let (before, plan) = self.planner.submit(&nals, pts).map_err(parse_error)?;
        self.output(&before)?;
        let Some(plan) = plan else {
            return Ok(());
        };
        // The surfaces were made for the SPS the decoder opened with: a stream whose
        // in-band SPS changes the picture size, or needs more buffers, is refused.
        if plan.sps.coded_size() != self.core.coded
            || Planner::slots_for(&plan.sps) > self.core.surfaces().len()
        {
            self.planner.reset();
            return Err(PlatformError::Unsupported {
                what: "an H.264 stream that changes its picture size partway, in a media \
                       file on Linux"
                    .into(),
            });
        }
        let slots = self.core.surfaces().to_vec();
        let target = slots[plan.slot];
        let mut buffers = Vec::with_capacity(2 + 2 * plan.slices.len());
        let made = (|| -> Result<(), PlatformError> {
            buffers.push(self.core.buffer(
                dec_buffer::PICTURE_PARAMETER,
                &h264_dec::picture_params(&plan, &slots),
            )?);
            buffers.push(
                self.core
                    .buffer(dec_buffer::IQ_MATRIX, &h264_dec::iq_matrix(&plan))?,
            );
            for slice in &plan.slices {
                buffers.push(self.core.buffer(
                    dec_buffer::SLICE_PARAMETER,
                    &h264_dec::slice_params(&plan, slice, &slots),
                )?);
                buffers.push(self.core.data_buffer(&slice.nal)?);
            }
            Ok(())
        })();
        let result = match made {
            Ok(()) => self.core.render(target, &buffers),
            Err(err) => {
                self.core.destroy_buffers(&buffers);
                Err(err)
            }
        };
        if let Err(err) = result {
            // The picture is lost; what references it decodes wrong, but the stream goes
            // on to its next IDR picture.
            tracing::debug!(%err, vendor = %self.core.vendor(), "an H.264 picture didn't decode");
        }
        let after = self.planner.finish();
        self.output(&after)
    }

    /// Outputs every frame still held: the end of the stream.
    pub(crate) fn flush(&mut self) -> Result<(), PlatformError> {
        let outputs = self.planner.drain();
        self.output(&outputs)
    }

    /// Forgets every frame held and queued, for a seek: decoding starts again at the
    /// next IDR picture.
    pub(crate) fn reset(&mut self) {
        self.planner.reset();
        self.ready.clear();
    }

    /// The next decoded frame, in presentation order.
    pub(crate) fn pop(&mut self) -> Option<VideoFrame> {
        self.ready.pop_front()
    }

    /// Reads back each output frame and queues it.
    fn output(&mut self, outputs: &[Output]) -> Result<(), PlatformError> {
        for out in outputs {
            let surface = self.core.surfaces()[out.slot];
            let frame = self
                .core
                .read_back(surface, MediaTime::new(out.pts, self.timebase))?;
            self.ready.push_back(frame);
        }
        Ok(())
    }
}

impl Core {
    /// Waits for `surface` and copies it into a frame.
    pub(super) fn read_back(
        &mut self,
        surface: Id,
        pts: MediaTime,
    ) -> Result<VideoFrame, PlatformError> {
        let va = self.res.display.va.clone();
        let dpy = self.res.display.dpy;
        // SAFETY: the surface is live.
        // C: `VAStatus vaSyncSurface(VADisplay, VASurfaceID)`.
        va.check("vaSyncSurface", unsafe { (va.sync_surface)(dpy, surface) })?;
        let mut derived = Image::EMPTY;
        let mut owned = false;
        if !self.no_derive {
            // SAFETY: the surface is live and idle; `derived` is a valid out-pointer.
            // C: `VAStatus vaDeriveImage(VADisplay, VASurfaceID, VAImage *image)`.
            let status = unsafe { (va.derive_image)(dpy, surface, &mut derived) };
            if va.check("vaDeriveImage", status).is_ok() && derived.format.fourcc == FOURCC_NV12 {
                owned = true;
            } else {
                if va.check("vaDeriveImage", status).is_ok() {
                    // SAFETY: derived above, destroyed once.
                    // C: `VAStatus vaDestroyImage(VADisplay, VAImageID)`.
                    unsafe { (va.destroy_image)(dpy, derived.image_id) };
                }
                self.no_derive = true;
            }
        }
        let image = if owned {
            derived
        } else {
            if self.res.image.is_none() {
                let mut format = ImageFormat::NV12;
                let mut image = Image::EMPTY;
                // SAFETY: `format` and `image` are valid pointers for the call.
                // C: `VAStatus vaCreateImage(VADisplay, VAImageFormat *format, int width,
                // int height, VAImage *image)`.
                let status = unsafe {
                    (va.create_image)(
                        dpy,
                        &mut format,
                        c_int::try_from(self.coded.0).unwrap_or(0),
                        c_int::try_from(self.coded.1).unwrap_or(0),
                        &mut image,
                    )
                };
                va.check("vaCreateImage", status)?;
                self.res.image = Some(image);
            }
            let image = self.res.image.unwrap_or(Image::EMPTY);
            // SAFETY: the surface and the image are live, and the image is the
            // surface's size.
            // C: `VAStatus vaGetImage(VADisplay, VASurfaceID, int x, int y,
            // unsigned int width, unsigned int height, VAImageID image)`.
            va.check("vaGetImage", unsafe {
                (va.get_image)(
                    dpy,
                    surface,
                    0,
                    0,
                    self.coded.0,
                    self.coded.1,
                    image.image_id,
                )
            })?;
            image
        };
        let frame = self.copy_image(&va, &image, pts);
        if owned {
            // SAFETY: derived above for this read, destroyed once.
            // C: `VAStatus vaDestroyImage(VADisplay, VAImageID)`.
            unsafe { (va.destroy_image)(dpy, image.image_id) };
        }
        frame
    }

    /// Maps `image` (NV12) and converts it into a frame.
    fn copy_image(
        &self,
        va: &Arc<Va>,
        image: &Image,
        pts: MediaTime,
    ) -> Result<VideoFrame, PlatformError> {
        let dpy = self.res.display.dpy;
        let mut ptr: *mut c_void = std::ptr::null_mut();
        // SAFETY: the image's buffer is live; `ptr` is a valid out-pointer.
        // C: `VAStatus vaMapBuffer(VADisplay, VABufferID, void **pbuf)`.
        va.check("vaMapBuffer", unsafe {
            (va.map_buffer)(dpy, image.buf, &mut ptr)
        })?;
        if ptr.is_null() {
            // SAFETY: mapped above.
            unsafe { (va.unmap_buffer)(dpy, image.buf) };
            return Err(PlatformError::Os {
                call: "vaMapBuffer",
                code: None,
                message: "the driver mapped the image at null".into(),
            });
        }
        // SAFETY: libva maps `data_size` bytes of the image at `ptr` until
        // `vaUnmapBuffer`, below, and nothing writes them meanwhile.
        let bytes = unsafe {
            std::slice::from_raw_parts(ptr.cast::<u8>().cast_const(), image.data_size as usize)
        };
        let y_at = image.offsets[0] as usize;
        let uv_at = image.offsets[1] as usize;
        let result = match (bytes.get(y_at..), bytes.get(uv_at..)) {
            (Some(y), Some(uv)) => yuv::frame(
                &Nv12 {
                    y,
                    y_stride: image.pitches[0] as usize,
                    uv,
                    uv_stride: image.pitches[1] as usize,
                },
                self.crop,
                self.size,
                self.format,
                self.colour,
                pts,
            ),
            _ => Err(PlatformError::Os {
                call: "vaDeriveImage",
                code: None,
                message: "the image's planes lie outside its buffer".into(),
            }),
        };
        // SAFETY: mapped above, unmapped once; `bytes` isn't used after this.
        // C: `VAStatus vaUnmapBuffer(VADisplay, VABufferID)`.
        unsafe { (va.unmap_buffer)(dpy, image.buf) };
        result
    }
}

/// The first render node whose driver decodes one of `profiles`, with the profile.
fn find_decoder(va: &Arc<Va>, profiles: &[c_int]) -> Result<(Display, c_int), PlatformError> {
    let mut nodes: Vec<String> = std::fs::read_dir("/dev/dri")
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter_map(|e| e.path().to_str().map(str::to_owned))
                .filter(|p| {
                    p.rsplit('/')
                        .next()
                        .is_some_and(|n| n.starts_with("renderD"))
                })
                .collect()
        })
        .unwrap_or_default();
    nodes.sort();
    for node in nodes {
        let Ok(display) = Display::open(va.clone(), &node) else {
            continue;
        };
        for &profile in profiles {
            if display.entrypoints(profile).contains(&ENTRYPOINT_VLD) {
                return Ok((display, profile));
            }
        }
    }
    Err(PlatformError::MissingComponent {
        component: COMPONENT,
        remedy: REMEDY.into(),
    })
}

#[cfg(all(test, feature = "hw-tests"))]
mod hw_tests;
