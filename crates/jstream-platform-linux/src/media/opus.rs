//! Opus audio through `rusty-opus`, a pure-Rust decoder (ADR-0159): the
//! identification header (`OpusHead`, RFC 7845 §5.1), and the packets of one stream or
//! several (channel mapping families 0, 1 and 255) decoded to interleaved `f32` at
//! 48 kHz.
//!
//! The container trims what the encoder put before and after the audio: Ogg through
//! symphonia's reader (each packet's `trim_start` and `trim_end`), Matroska through
//! its `CodecDelay` ([`super::mkv`]). This module applies the header's output gain.

use rusty_opus::Error as OpusError;
use rusty_opus::multistream::{ChannelLayout, OpusMSDecoder};
use symphonia::core::audio::Position;

/// Opus always decodes at this rate (RFC 7845 §5.1: the header's input rate is only a
/// hint for playback).
pub(crate) const RATE: u32 = 48_000;
/// [`RATE`] as the decoder takes it.
const RATE_I32: i32 = 48_000;

/// The longest Opus packet: 120 ms at 48 kHz.
const MAX_FRAMES: usize = 5_760;

/// An Opus identification header.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Head {
    pub(crate) channels: u8,
    /// Samples at 48 kHz to drop from the start (RFC 7845 §4.2).
    pub(crate) pre_skip: u16,
    /// The output gain, as a linear factor.
    gain: f32,
    family: u8,
    streams: u8,
    coupled: u8,
    /// For each output channel, the decoded channel it takes (255: silence).
    mapping: Vec<u8>,
}

impl Head {
    /// Parses `data`, or `None` when it isn't a header this decodes.
    pub(crate) fn parse(data: &[u8]) -> Option<Self> {
        let rest = data.strip_prefix(b"OpusHead")?;
        let (fixed, rest) = rest.split_at_checked(11)?;
        // Only the major version is checked: a later minor version stays compatible.
        if fixed[0] >> 4 != 0 {
            return None;
        }
        let channels = fixed[1];
        let pre_skip = u16::from_le_bytes([fixed[2], fixed[3]]);
        let gain_q8 = i16::from_le_bytes([fixed[8], fixed[9]]);
        let family = fixed[10];
        if channels == 0 {
            return None;
        }
        let (streams, coupled, mapping) = if family == 0 {
            if channels > 2 {
                return None;
            }
            (1, channels - 1, (0..channels).collect())
        } else {
            let (&streams, rest) = rest.split_first()?;
            let (&coupled, rest) = rest.split_first()?;
            let mapping = rest.get(..usize::from(channels))?.to_vec();
            let decoded = u16::from(streams) + u16::from(coupled);
            if streams == 0
                || coupled > streams
                || decoded > 255
                || mapping.iter().any(|&m| m != 255 && u16::from(m) >= decoded)
            {
                return None;
            }
            (streams, coupled, mapping)
        };
        Some(Self {
            channels,
            pre_skip,
            gain: 10_f32.powf(f32::from(gain_q8) / (20.0 * 256.0)),
            family,
            streams,
            coupled,
            mapping,
        })
    }

    /// The speaker of each output channel, in order, when the mapping family says
    /// (RFC 7845 §5.1.1.2, the Vorbis order); `None` for channels without positions.
    pub(crate) fn positions(&self) -> Option<Vec<Position>> {
        use Position as P;
        const FL: Position = Position::FRONT_LEFT;
        const FR: Position = Position::FRONT_RIGHT;
        const FC: Position = Position::FRONT_CENTER;
        let order: &[Position] = match (self.family, self.channels) {
            (0 | 1, 1) => &[FC],
            (0 | 1, 2) => &[FL, FR],
            (1, 3) => &[FL, FC, FR],
            (1, 4) => &[FL, FR, P::REAR_LEFT, P::REAR_RIGHT],
            (1, 5) => &[FL, FC, FR, P::REAR_LEFT, P::REAR_RIGHT],
            (1, 6) => &[FL, FC, FR, P::REAR_LEFT, P::REAR_RIGHT, P::LFE1],
            (1, 7) => &[
                FL,
                FC,
                FR,
                P::SIDE_LEFT,
                P::SIDE_RIGHT,
                P::REAR_CENTER,
                P::LFE1,
            ],
            (1, 8) => &[
                FL,
                FC,
                FR,
                P::SIDE_LEFT,
                P::SIDE_RIGHT,
                P::REAR_LEFT,
                P::REAR_RIGHT,
                P::LFE1,
            ],
            _ => return None,
        };
        Some(order.to_vec())
    }
}

/// One Opus stream's decoder.
pub(crate) struct OpusStream {
    decoder: OpusMSDecoder,
    /// The layout, to make the decoder again on a reset.
    layout: ChannelLayout,
    channels: usize,
    gain: f32,
    pcm: Vec<f32>,
}

impl OpusStream {
    /// A decoder for the stream `head` describes.
    ///
    /// # Errors
    /// The decoder's, for a layout it refuses.
    pub(crate) fn new(head: &Head) -> Result<Self, OpusError> {
        let channels = usize::from(head.channels);
        let layout = ChannelLayout::new(
            channels,
            usize::from(head.streams),
            usize::from(head.coupled),
            head.mapping.clone(),
        )?;
        let decoder = OpusMSDecoder::with_layout(RATE_I32, layout.clone())?;
        Ok(Self {
            decoder,
            layout,
            channels,
            gain: head.gain,
            pcm: vec![0.0; MAX_FRAMES * channels],
        })
    }

    /// Decodes one packet to interleaved samples, the header's gain applied.
    ///
    /// # Errors
    /// The decoder's, for a damaged packet: skip it and go on with the next.
    pub(crate) fn decode(&mut self, packet: &[u8]) -> Result<Vec<f32>, OpusError> {
        let frames = self.decoder.decode(packet, MAX_FRAMES, &mut self.pcm)?;
        let mut out = self
            .pcm
            .get(..frames * self.channels)
            .ok_or(OpusError::BufferTooSmall("output"))?
            .to_vec();
        if (self.gain - 1.0).abs() > f32::EPSILON {
            for s in &mut out {
                *s *= self.gain;
            }
        }
        Ok(out)
    }

    /// The output channel count.
    pub(crate) fn channels(&self) -> usize {
        self.channels
    }

    /// Forgets the decoder's state, after a seek: a new decoder for the same layout.
    pub(crate) fn reset(&mut self) {
        match OpusMSDecoder::with_layout(RATE_I32, self.layout.clone()) {
            Ok(decoder) => self.decoder = decoder,
            // It was made from this layout once, so this doesn't happen; the old
            // decoder carries on, its run-in settling it as after any packet loss.
            Err(err) => tracing::debug!(%err, "an Opus decoder didn't reset"),
        }
    }
}

#[cfg(test)]
mod tests;
