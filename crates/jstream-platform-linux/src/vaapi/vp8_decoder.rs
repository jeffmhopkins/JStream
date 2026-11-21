//! [`VaapiVp8Decoder`]: VP8 decoding on the GPU through VA-API's VLD entry point, for
//! media files (ADR-0072 (b)). `media::vp8` reads each frame's header and keeps what
//! carries between frames; this module keeps which surface is the last, golden and
//! alternate frame, hands the GPU the parameters (`vp8_dec`) and the frame, and reads
//! the shown frames back through the shared [`Core`].
//!
//! VP8 shows frames in decoding order, so each shown frame is read back as soon as it
//! is decoded. A hidden frame (an alternate reference) is decoded and kept, not shown.
//!
//! The surfaces are made for the first key frame's size, so a stream that changes its
//! size partway is refused, as VP9's is.

use std::collections::VecDeque;

use jstream_platform::PlatformError;
use jstream_platform::media::{FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame};

use super::decoder::{Core, CoreConfig, check_format};
use super::ffi::{INVALID_ID, Id, dec_buffer};
use super::vp8_dec::{self, References};
use crate::media::h264::bits::ParseError;
use crate::media::vp8::{Header, Parser, Update, is_key_frame};
use crate::media::yuv::Colour;

/// What `MissingComponent` names.
const COMPONENT: &str = "a VA-API VP8 decoder";
/// What to install for it.
const REMEDY: &str = "install your GPU's VA-API driver with VP8 decoding: Mesa's \
    (mesa-va-drivers) for AMD, or intel-media-va-driver for Intel (Broadwell or newer). \
    NVIDIA's driver has no VA-API decoder of its own";

/// `VAProfileVP8Version0_3`.
const PROFILE: i32 = 14;

/// The surfaces: the last, golden and alternate frames, the frame being decoded, and
/// one more so a frame never has to decode into a surface a reference still holds.
const SURFACES: usize = 5;

/// Maps a parse error to the platform's.
fn parse_error(err: ParseError) -> PlatformError {
    match err {
        ParseError::Unsupported(what) => PlatformError::Unsupported {
            what: format!("{what} in a media file on Linux"),
        },
        ParseError::Truncated => PlatformError::Os {
            call: "VP8 parser",
            code: None,
            message: "a frame ends inside its header".into(),
        },
        ParseError::Invalid(what) => PlatformError::Os {
            call: "VP8 parser",
            code: None,
            message: format!("an invalid stream: {what}"),
        },
    }
}

/// The surfaces (by index) in the three reference slots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Slots {
    last: Option<usize>,
    golden: Option<usize>,
    alternate: Option<usize>,
}

impl Slots {
    fn holds(&self, surface: usize) -> bool {
        [self.last, self.golden, self.alternate].contains(&Some(surface))
    }

    /// The slots after a frame decoded into `target` (RFC 6386 §9.7, §9.8): every copy
    /// takes the slots as they were before the frame.
    fn after(self, h: &Header, target: usize) -> Self {
        let pick = |update: Update, own: Option<usize>, other: Option<usize>| match update {
            Update::Keep => own,
            Update::Current => Some(target),
            Update::Last => self.last,
            Update::Other => other,
        };
        Self {
            last: if h.refresh_last {
                Some(target)
            } else {
                self.last
            },
            golden: pick(h.golden, self.golden, self.alternate),
            alternate: pick(h.alternate, self.alternate, self.golden),
        }
    }
}

/// One VP8 stream decoded on the GPU. See the module docs.
pub(crate) struct VaapiVp8Decoder {
    core: Core,
    parser: Parser,
    slots: Slots,
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
unsafe impl Send for VaapiVp8Decoder {}

impl VaapiVp8Decoder {
    /// Opens a decoder for a stream whose first sample is `first` (a key frame), whose
    /// sample times are in `timebase`, putting out frames in `format`.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when libva or a driver with a VP8 decoder is
    /// missing, [`PlatformError::Unsupported`] for a format it can't put out, and
    /// [`PlatformError::Os`] when the first frame isn't a key frame that parses or the
    /// driver refuses.
    pub(crate) fn open(
        first: &[u8],
        timebase: Timebase,
        format: PixelFormat,
    ) -> Result<Self, PlatformError> {
        check_format(format)?;
        if !is_key_frame(first) {
            return Err(PlatformError::Os {
                call: "VP8 parser",
                code: None,
                message: "the stream doesn't start with a key frame".into(),
            });
        }
        let header = Parser::new().parse(first).map_err(parse_error)?;
        let (w, h) = (u32::from(header.width), u32::from(header.height));
        let size = FrameSize::new(w & !1, h & !1)?;
        // VP8 is BT.601, studio range.
        let core = Core::open(&CoreConfig {
            profiles: &[PROFILE],
            component: COMPONENT,
            remedy: REMEDY,
            coded: (w, h),
            surfaces: SURFACES,
            crop: (0, 0),
            size,
            colour: Colour::from_vui(Some(6), false, h),
            format,
        })?;
        Ok(Self {
            core,
            parser: Parser::new(),
            slots: Slots::default(),
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

    /// Decodes one sample (one frame) shown at `pts` ticks. A shown frame is queued for
    /// [`pop`](Self::pop).
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a frame of another size, and
    /// [`PlatformError::Os`] for one that doesn't parse or when reading back fails.
    pub(crate) fn decode(&mut self, sample: &[u8], pts: i64) -> Result<(), PlatformError> {
        if self.need_key {
            if !is_key_frame(sample) {
                return Ok(());
            }
            self.need_key = false;
        }
        let header = match self.parser.parse(sample) {
            Ok(header) => header,
            Err(err) => {
                // A damaged frame: decoding starts again at the next key frame.
                self.reset();
                return Err(parse_error(err));
            }
        };
        if (header.width, header.height) != self.coded {
            self.reset();
            return Err(PlatformError::Unsupported {
                what: "a VP8 stream that changes its frame size partway, in a media file on \
                       Linux"
                    .into(),
            });
        }
        let target = self.decode_frame(&header, sample);
        self.slots = self.slots.after(&header, target);
        if header.show_frame {
            let surface = self.core.surfaces()[target];
            let frame = self
                .core
                .read_back(surface, MediaTime::new(pts, self.timebase))?;
            self.ready.push_back(frame);
        }
        Ok(())
    }

    /// Decodes `frame` into a surface no slot holds, and returns its index. A frame the
    /// driver refuses is logged and its surface used anyway: what references it decodes
    /// wrong, until the next key frame.
    fn decode_frame(&mut self, header: &Header, frame: &[u8]) -> usize {
        let target = (0..SURFACES).find(|s| !self.slots.holds(*s)).unwrap_or(0);
        let surfaces = self.core.surfaces();
        let id = |slot: Option<usize>| slot.map_or(INVALID_ID, |s| surfaces[s]);
        let refs = if header.key_frame {
            References::NONE
        } else {
            References {
                last: id(self.slots.last),
                golden: id(self.slots.golden),
                alternate: id(self.slots.alternate),
            }
        };
        let target_id: Id = surfaces[target];
        let data = frame.get(header.header_size..).unwrap_or_default();
        let mut buffers = Vec::with_capacity(5);
        let made = (|| -> Result<(), PlatformError> {
            buffers.push(self.core.buffer(
                dec_buffer::PICTURE_PARAMETER,
                &vp8_dec::picture_params(header, refs),
            )?);
            buffers.push(
                self.core
                    .buffer(dec_buffer::PROBABILITY, &vp8_dec::probabilities(header))?,
            );
            buffers.push(
                self.core
                    .buffer(dec_buffer::IQ_MATRIX, &vp8_dec::iq_matrix(header))?,
            );
            buffers.push(self.core.buffer(
                dec_buffer::SLICE_PARAMETER,
                &vp8_dec::slice_params(header, data.len()),
            )?);
            buffers.push(self.core.data_buffer(data)?);
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
            tracing::debug!(%err, vendor = %self.core.vendor(), "a VP8 frame didn't decode");
        }
        target
    }

    /// Nothing is held back: VP8 shows frames in decoding order.
    pub(crate) fn flush(&mut self) {}

    /// Forgets every reference and queued frame, for a seek: decoding starts again at
    /// the next key frame.
    pub(crate) fn reset(&mut self) {
        self.parser.reset();
        self.slots = Slots::default();
        self.ready.clear();
        self.need_key = true;
    }

    /// The next decoded frame, in presentation order.
    pub(crate) fn pop(&mut self) -> Option<VideoFrame> {
        self.ready.pop_front()
    }
}

#[cfg(test)]
mod tests;
