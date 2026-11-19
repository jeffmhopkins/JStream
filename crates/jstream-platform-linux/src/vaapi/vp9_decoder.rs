//! [`VaapiVp9Decoder`]: VP9 decoding on the GPU through VA-API's VLD entry point, for
//! media files (ADR-0072 (b)). `media::vp9` reads each frame's uncompressed header and
//! keeps what carries between frames; this module keeps which surface is in each of
//! the eight reference slots, hands the GPU the parameters (`vp9_dec`) and the frame,
//! and reads the shown frames back through the shared [`Core`].
//!
//! VP9 shows frames in decoding order, so each shown frame is read back as soon as it
//! is decoded. A sample may be a superframe, a hidden frame (an alternate reference)
//! and the frame shown after it; a frame may also show a reference slot again
//! (`show_existing_frame`). Each shown frame takes its sample's time.
//!
//! Profile 0 only: 8-bit 4:2:0, which is what WebM files nearly always hold. The
//! surfaces are made for the first key frame's size, so a stream that changes its size
//! partway is refused, as H.264's is.

use std::collections::VecDeque;

use jstream_platform::PlatformError;
use jstream_platform::media::{FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame};

use super::decoder::{Core, CoreConfig, check_format};
use super::ffi::{INVALID_ID, Id, dec_buffer};
use super::vp9_dec;
use crate::media::h264::bits::ParseError;
use crate::media::vp9::{Header, NUM_REF_FRAMES, Parser, frames, is_key_frame};
use crate::media::yuv::Colour;

/// What `MissingComponent` names.
const COMPONENT: &str = "a VA-API VP9 decoder";
/// What to install for it.
const REMEDY: &str = "install your GPU's VA-API driver with VP9 decoding: Mesa's \
    (mesa-va-drivers) for AMD, or intel-media-va-driver for Intel (Kaby Lake or newer). \
    NVIDIA's driver has no VA-API decoder of its own";

/// `VAProfileVP9Profile0`.
const PROFILE_0: i32 = 19;

/// The surfaces: one for each reference slot, the frame being decoded, and one more
/// so a frame never has to decode into a surface a slot still holds.
const SURFACES: usize = NUM_REF_FRAMES + 2;

/// Maps a parse error to the platform's.
fn parse_error(err: ParseError) -> PlatformError {
    match err {
        ParseError::Unsupported(what) => PlatformError::Unsupported {
            what: format!("{what} in a media file on Linux"),
        },
        ParseError::Truncated => PlatformError::Os {
            call: "VP9 parser",
            code: None,
            message: "a frame ends inside its header".into(),
        },
        ParseError::Invalid(what) => PlatformError::Os {
            call: "VP9 parser",
            code: None,
            message: format!("an invalid stream: {what}"),
        },
    }
}

/// The colour matrix a VP9 `color_space` names, as an H.264 VUI code for
/// [`Colour::from_vui`].
fn matrix(color_space: u8) -> Option<u8> {
    match color_space {
        1 | 3 | 4 => Some(6),
        2 => Some(1),
        _ => None,
    }
}

/// One VP9 stream decoded on the GPU. See the module docs.
pub(crate) struct VaapiVp9Decoder {
    core: Core,
    parser: Parser,
    /// The surface (by index) in each reference slot.
    slots: [Option<usize>; NUM_REF_FRAMES],
    /// The frame size the surfaces were made for.
    coded: (u16, u16),
    timebase: Timebase,
    /// Decoded frames, in presentation order, not yet taken.
    ready: VecDeque<VideoFrame>,
    /// After a reset: frames are skipped until a key frame.
    need_key: bool,
}

// SAFETY: as for `VaapiH264Decoder`: the display and its objects are used only through
// `&mut self`, from whichever one thread owns the decoder.
unsafe impl Send for VaapiVp9Decoder {}

impl VaapiVp9Decoder {
    /// Opens a decoder for a stream whose first sample is `first` (a key frame), whose
    /// sample times are in `timebase`, putting out frames in `format`.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when libva or a driver with a VP9 decoder is
    /// missing, [`PlatformError::Unsupported`] for a stream other than profile 0 or a
    /// format it can't put out, and [`PlatformError::Os`] when the first frame doesn't
    /// parse or the driver refuses.
    pub(crate) fn open(
        first: &[u8],
        timebase: Timebase,
        format: PixelFormat,
    ) -> Result<Self, PlatformError> {
        check_format(format)?;
        let key = frames(first)
            .into_iter()
            .find(|f| is_key_frame(f))
            .ok_or_else(|| PlatformError::Os {
                call: "VP9 parser",
                code: None,
                message: "the stream doesn't start with a key frame".into(),
            })?;
        let header = Parser::new().parse(key).map_err(parse_error)?;
        if header.profile != 0 || header.bit_depth != 8 || header.subsampling != (true, true) {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "VP9 profile {} ({}-bit) in a media file on Linux: profile 0 plays",
                    header.profile, header.bit_depth
                ),
            });
        }
        let (w, h) = (u32::from(header.width), u32::from(header.height));
        let size = FrameSize::new(w & !1, h & !1)?;
        let core = Core::open(&CoreConfig {
            profiles: &[PROFILE_0],
            component: COMPONENT,
            remedy: REMEDY,
            coded: (w, h),
            surfaces: SURFACES,
            crop: (0, 0),
            size,
            colour: Colour::from_vui(matrix(header.color_space), header.full_range, h),
            format,
        })?;
        Ok(Self {
            core,
            parser: Parser::new(),
            slots: [None; NUM_REF_FRAMES],
            coded: (header.width, header.height),
            timebase,
            ready: VecDeque::new(),
            need_key: true,
        })
    }

    /// The visible size of every frame.
    pub(crate) fn size(&self) -> FrameSize {
        self.core.size()
    }

    /// Decodes one sample shown at `pts` ticks. The frames it shows are queued for
    /// [`pop`](Self::pop).
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a frame of another size, and
    /// [`PlatformError::Os`] for one that doesn't parse or when reading back fails.
    pub(crate) fn decode(&mut self, sample: &[u8], pts: i64) -> Result<(), PlatformError> {
        for frame in frames(sample) {
            if self.need_key {
                if !is_key_frame(frame) {
                    continue;
                }
                self.need_key = false;
            }
            let header = match self.parser.parse(frame) {
                Ok(header) => header,
                Err(err) => {
                    // A damaged frame: decoding starts again at the next key frame.
                    self.reset();
                    return Err(parse_error(err));
                }
            };
            let pts = MediaTime::new(pts, self.timebase);
            if let Some(slot) = header.show_existing {
                if let Some(surface) = self.slots[slot] {
                    let surface = self.core.surfaces()[surface];
                    let frame = self.core.read_back(surface, pts)?;
                    self.ready.push_back(frame);
                }
                continue;
            }
            if (header.width, header.height) != self.coded {
                self.reset();
                return Err(PlatformError::Unsupported {
                    what: "a VP9 stream that changes its frame size partway, in a media file \
                           on Linux"
                        .into(),
                });
            }
            let target = self.decode_frame(&header, frame);
            for (slot, held) in self.slots.iter_mut().enumerate() {
                if header.refresh_frame_flags >> slot & 1 == 1 {
                    *held = Some(target);
                }
            }
            if header.show_frame {
                let surface = self.core.surfaces()[target];
                let frame = self.core.read_back(surface, pts)?;
                self.ready.push_back(frame);
            }
        }
        Ok(())
    }

    /// Decodes `frame` into a surface no slot holds, and returns its index. A frame the
    /// driver refuses is logged and its surface used anyway: what references it decodes
    /// wrong, until the next key frame.
    fn decode_frame(&mut self, header: &Header, frame: &[u8]) -> usize {
        let target = (0..SURFACES)
            .find(|s| !self.slots.contains(&Some(*s)))
            .unwrap_or(0);
        let surfaces = self.core.surfaces();
        let mut references = [INVALID_ID; NUM_REF_FRAMES];
        for (out, held) in references.iter_mut().zip(&self.slots) {
            if let Some(s) = held {
                *out = surfaces[*s];
            }
        }
        let target_id: Id = surfaces[target];
        let mut buffers = Vec::with_capacity(3);
        let made = (|| -> Result<(), PlatformError> {
            buffers.push(self.core.buffer(
                dec_buffer::PICTURE_PARAMETER,
                &vp9_dec::picture_params(header, &references),
            )?);
            buffers.push(self.core.buffer(
                dec_buffer::SLICE_PARAMETER,
                &vp9_dec::slice_params(header, frame.len()),
            )?);
            buffers.push(self.core.data_buffer(frame)?);
            Ok(())
        })();
        let result = match made {
            Ok(()) => self.core.render(target_id, &buffers),
            Err(err) => {
                self.core.destroy_buffers(&buffers);
                Err(err)
            }
        };
        if let Err(err) = result {
            tracing::debug!(%err, vendor = %self.core.vendor(), "a VP9 frame didn't decode");
        }
        target
    }

    /// Nothing is held back: VP9 shows frames in decoding order.
    pub(crate) fn flush(&mut self) {}

    /// Forgets every slot and queued frame, for a seek: decoding starts again at the
    /// next key frame.
    pub(crate) fn reset(&mut self) {
        self.parser.reset();
        self.slots = [None; NUM_REF_FRAMES];
        self.ready.clear();
        self.need_key = true;
    }

    /// The next decoded frame, in presentation order.
    pub(crate) fn pop(&mut self) -> Option<VideoFrame> {
        self.ready.pop_front()
    }
}
