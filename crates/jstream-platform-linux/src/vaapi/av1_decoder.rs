//! [`VaapiAv1Decoder`]: AV1 decoding on the GPU through VA-API's VLD entry point, for
//! media files (ADR-0072 (b)). `media::av1` reads each sample's OBUs and frame headers
//! and keeps what carries between frames; this module keeps which surface is in each
//! of the eight reference slots, hands the GPU the parameters (`av1_dec`) and the
//! tiles, and reads the shown frames back through the shared [`Core`].
//!
//! AV1 shows frames in decoding order: a sample (a temporal unit) shows one frame,
//! which is either decoded in it or shown again from a slot (`show_existing_frame`),
//! and may first decode hidden frames that later samples show. Each shown frame takes
//! its sample's time.
//!
//! Film grain is added by the GPU as the frame is shown: a frame with grain decodes
//! into one surface, which later frames reference, and is shown from a second surface
//! the driver adds the grain to, as FFmpeg's hwaccel does. A slot keeps both, so a
//! frame shown again keeps its grain.
//!
//! Profile 0 at 8 bits only: 4:2:0, which is what WebM and MP4 files nearly always
//! hold. The surfaces are made for the first key frame's size, so a stream that
//! changes its size partway is refused, as VP9's is.

use std::collections::VecDeque;

use jstream_platform::PlatformError;
use jstream_platform::media::{FrameSize, MediaTime, PixelFormat, Timebase, VideoFrame};

use super::av1_dec::{self, Surfaces};
use super::decoder::{Core, CoreConfig, check_format};
use super::ffi::{INVALID_ID, dec_buffer};
use crate::media::av1::{Frame, FrameHeader, KEY_FRAME, NUM_REF_FRAMES, Parser};
use crate::media::h264::bits::ParseError;
use crate::media::yuv::Colour;

/// What `MissingComponent` names.
const COMPONENT: &str = "a VA-API AV1 decoder";
/// What to install for it.
const REMEDY: &str = "install your GPU's VA-API driver with AV1 decoding: Mesa's \
    (mesa-va-drivers) for AMD (RDNA 2 or newer), or intel-media-va-driver for Intel \
    (Tiger Lake or newer). NVIDIA's driver has no VA-API decoder of its own";

/// `VAProfileAV1Profile0`.
const PROFILE_0: i32 = 32;

/// The surfaces without film grain: one for each reference slot, the frame being
/// decoded, and one more so a frame never has to decode into a surface a slot holds.
const SURFACES: usize = NUM_REF_FRAMES + 2;
/// With film grain, each of those may need a second surface to show it from.
const SURFACES_WITH_GRAIN: usize = 2 * NUM_REF_FRAMES + 3;

/// The most tile columns or rows VA-API takes.
const VA_MAX_TILES: usize = 63;

/// Maps a parse error to the platform's.
fn parse_error(err: ParseError) -> PlatformError {
    match err {
        ParseError::Unsupported(what) => PlatformError::Unsupported {
            what: format!("{what} in a media file on Linux"),
        },
        ParseError::Truncated => PlatformError::Os {
            call: "AV1 parser",
            code: None,
            message: "a sample ends inside an OBU".into(),
        },
        ParseError::Invalid(what) => PlatformError::Os {
            call: "AV1 parser",
            code: None,
            message: format!("an invalid stream: {what}"),
        },
    }
}

/// The surfaces (by index) one frame in a slot holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    /// The frame as decoded, which later frames reference.
    decoded: usize,
    /// The frame as shown: with its film grain, or `decoded` itself.
    shown: usize,
}

/// A surface (of `count`) no slot holds and `other` isn't.
fn free_surface(slots: &[Option<Held>], count: usize, other: Option<usize>) -> usize {
    (0..count)
        .find(|s| {
            Some(*s) != other
                && !slots
                    .iter()
                    .flatten()
                    .any(|held| held.decoded == *s || held.shown == *s)
        })
        .unwrap_or(0)
}

/// One AV1 stream decoded on the GPU. See the module docs.
pub(crate) struct VaapiAv1Decoder {
    core: Core,
    parser: Parser,
    slots: [Option<Held>; NUM_REF_FRAMES],
    /// How many surfaces there are.
    surfaces: usize,
    /// The frame size (`UpscaledWidth` by `FrameHeight`) the surfaces were made for.
    coded: (u32, u32),
    timebase: Timebase,
    /// Decoded frames, in presentation order, not yet taken.
    ready: VecDeque<VideoFrame>,
    /// After a reset: samples are skipped until a shown key frame.
    need_key: bool,
}

// SAFETY: as for `VaapiH264Decoder`: the display and its objects are used only through
// `&mut self`, from whichever one thread owns the decoder.
unsafe impl Send for VaapiAv1Decoder {}

impl VaapiAv1Decoder {
    /// Opens a decoder for a stream whose configuration OBUs (from its `av1C` record,
    /// possibly none) are `config` and whose first sample is `first` (a key frame),
    /// whose sample times are in `timebase`, putting out frames in `format`.
    ///
    /// # Errors
    /// [`PlatformError::MissingComponent`] when libva or a driver with an AV1 decoder
    /// is missing, [`PlatformError::Unsupported`] for a stream other than 8-bit 4:2:0
    /// profile 0 or a format it can't put out, and [`PlatformError::Os`] when the first
    /// sample isn't a key frame that parses or the driver refuses.
    pub(crate) fn open(
        config: &[u8],
        first: &[u8],
        timebase: Timebase,
        format: PixelFormat,
    ) -> Result<Self, PlatformError> {
        check_format(format)?;
        let mut parser = Parser::new();
        parser.parse(config).map_err(parse_error)?;
        if !parser.is_key_frame(first) {
            return Err(PlatformError::Os {
                call: "AV1 parser",
                code: None,
                message: "the stream doesn't start with a key frame".into(),
            });
        }
        let frames = parser.parse(first).map_err(parse_error)?;
        let header = frames.first().map(|f| &f.header).ok_or(PlatformError::Os {
            call: "AV1 parser",
            code: None,
            message: "the first sample holds no frame".into(),
        })?;
        let seq = parser.sequence().cloned().ok_or(PlatformError::Os {
            call: "AV1 parser",
            code: None,
            message: "the stream has no sequence header".into(),
        })?;
        let c = &seq.color;
        if seq.profile != 0 || c.bit_depth != 8 || c.mono_chrome {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "AV1 profile {} ({}-bit{}) in a media file on Linux: 8-bit 4:2:0 \
                     profile 0 plays",
                    seq.profile,
                    c.bit_depth,
                    if c.mono_chrome { ", monochrome" } else { "" }
                ),
            });
        }
        let (w, h) = (header.upscaled_width, header.frame_height);
        let size = FrameSize::new(w & !1, h & !1)?;
        let surfaces = if seq.film_grain_params_present {
            SURFACES_WITH_GRAIN
        } else {
            SURFACES
        };
        // `matrix_coefficients` shares H.264's VUI codes; 2 is unspecified.
        let matrix = (c.matrix_coefficients != 2).then_some(c.matrix_coefficients);
        let core = Core::open(&CoreConfig {
            profiles: &[PROFILE_0],
            component: COMPONENT,
            remedy: REMEDY,
            coded: (w, h),
            surfaces,
            crop: (0, 0),
            size,
            colour: Colour::from_vui(matrix, c.color_range, h),
            format,
        })?;
        // A fresh parser, so the first sample decodes from the start.
        let mut parser = Parser::new();
        parser.parse(config).map_err(parse_error)?;
        Ok(Self {
            core,
            parser,
            slots: [None; NUM_REF_FRAMES],
            surfaces,
            coded: (w, h),
            timebase,
            ready: VecDeque::new(),
            need_key: true,
        })
    }

    /// The visible size of every frame.
    pub(crate) fn size(&self) -> FrameSize {
        self.core.size()
    }

    /// Decodes one sample (a temporal unit) shown at `pts` ticks. The frame it shows
    /// is queued for [`pop`](Self::pop).
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] for a frame of another size or with more tiles
    /// than VA-API takes, and [`PlatformError::Os`] for a sample that doesn't parse or
    /// when reading back fails.
    pub(crate) fn decode(&mut self, sample: &[u8], pts: i64) -> Result<(), PlatformError> {
        if self.need_key {
            if !self.parser.is_key_frame(sample) {
                return Ok(());
            }
            self.need_key = false;
        }
        let frames = match self.parser.parse(sample) {
            Ok(frames) => frames,
            Err(err) => {
                // A damaged sample: decoding starts again at the next key frame.
                self.reset();
                return Err(parse_error(err));
            }
        };
        let pts = MediaTime::new(pts, self.timebase);
        for frame in &frames {
            let h = &frame.header;
            if let Some(slot) = h.show_existing {
                let held = self.slots[slot];
                if h.frame_type == KEY_FRAME {
                    // A key frame shown again refreshes every slot.
                    self.slots = [held; NUM_REF_FRAMES];
                }
                if let Some(held) = held {
                    let surface = self.core.surfaces()[held.shown];
                    let frame = self.core.read_back(surface, pts)?;
                    self.ready.push_back(frame);
                }
                continue;
            }
            if (h.upscaled_width, h.frame_height) != self.coded {
                self.reset();
                return Err(PlatformError::Unsupported {
                    what: "an AV1 stream that changes its frame size partway, in a media file \
                           on Linux"
                        .into(),
                });
            }
            if h.tile_cols > VA_MAX_TILES || h.tile_rows > VA_MAX_TILES {
                self.reset();
                return Err(PlatformError::Unsupported {
                    what: "an AV1 frame of 64 tile columns or rows, in a media file on Linux"
                        .into(),
                });
            }
            let held = self.decode_frame(frame, sample);
            for (slot, out) in self.slots.iter_mut().enumerate() {
                if h.refresh_frame_flags >> slot & 1 == 1 {
                    *out = Some(held);
                }
            }
            if h.show_frame {
                let surface = self.core.surfaces()[held.shown];
                let frame = self.core.read_back(surface, pts)?;
                self.ready.push_back(frame);
            }
        }
        Ok(())
    }

    /// Decodes `frame` (from `sample`) into surfaces no slot holds, and returns them. A
    /// frame the driver refuses is logged and its surfaces used anyway: what references
    /// it decodes wrong, until the next key frame.
    fn decode_frame(&mut self, frame: &Frame, sample: &[u8]) -> Held {
        let h: &FrameHeader = &frame.header;
        let decoded = free_surface(&self.slots, self.surfaces, None);
        let shown = if h.film_grain.apply_grain {
            free_surface(&self.slots, self.surfaces, Some(decoded))
        } else {
            decoded
        };
        let ids = self.core.surfaces();
        let mut references = [INVALID_ID; NUM_REF_FRAMES];
        for (out, held) in references.iter_mut().zip(&self.slots) {
            if let Some(held) = held {
                *out = ids[held.decoded];
            }
        }
        let surfaces = Surfaces {
            current: ids[decoded],
            display: ids[shown],
            references,
        };
        let Some(seq) = self.parser.sequence() else {
            return Held { decoded, shown };
        };
        let mut buffers = Vec::with_capacity(1 + 2 * frame.tile_groups.len());
        let made = (|| -> Result<(), PlatformError> {
            buffers.push(self.core.buffer(
                dec_buffer::PICTURE_PARAMETER,
                &av1_dec::picture_params(seq, h, surfaces),
            )?);
            for group in &frame.tile_groups {
                buffers.push(
                    self.core
                        .buffer_array(dec_buffer::SLICE_PARAMETER, &av1_dec::slice_params(group))?,
                );
                let data = sample.get(group.start..group.end).unwrap_or_default();
                buffers.push(self.core.data_buffer(data)?);
            }
            Ok(())
        })();
        let result = match made {
            Ok(()) => self.core.render(surfaces.current, &buffers),
            Err(err) => {
                self.core.destroy_buffers(&buffers);
                Err(err)
            }
        };
        if let Err(err) = result {
            tracing::debug!(%err, vendor = %self.core.vendor(), "an AV1 frame didn't decode");
        }
        Held { decoded, shown }
    }

    /// Nothing is held back: AV1 shows frames in decoding order.
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

#[cfg(test)]
mod tests;
