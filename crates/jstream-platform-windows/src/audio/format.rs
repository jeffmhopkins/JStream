//! Platform-independent pieces of the WASAPI backend, tested on every platform: which
//! format a stream is opened in, sample copies, and packet metadata.

use jstream_platform::media::{AudioFormat, ChannelLayout, SampleFormat};
use jstream_platform::{AudioPacketFlags, PlatformError};

/// What a device's shared-mode mix format is, as far as opening a stream cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MixFormat {
    /// Frames per second.
    pub(crate) rate: u32,
    /// Channel count.
    pub(crate) channels: u16,
    /// Whether the samples are 32-bit IEEE float.
    pub(crate) float32: bool,
    /// The speaker positions (`dwChannelMask`), when the format names them.
    pub(crate) mask: Option<u32>,
}

/// How to open a stream on a device with a given mix format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    /// The stream's format: always interleaved `f32` at the mix rate.
    pub(crate) format: AudioFormat,
    /// `false`: open with the mix format itself. `true`: ask WASAPI to convert
    /// (`AUTOCONVERTPCM`) to `f32` with [`Plan::format`]'s layout, at the same rate, so
    /// WASAPI converts sample types or downmixes but never resamples (ADR-0014 leaves
    /// resampling to the engine).
    pub(crate) convert: bool,
}

/// The layout for a channel count, if `jstream-media` has one.
pub(crate) fn layout_for(channels: u16) -> Option<ChannelLayout> {
    match channels {
        1 => Some(ChannelLayout::Mono),
        2 => Some(ChannelLayout::Stereo),
        6 => Some(ChannelLayout::Surround51),
        _ => None,
    }
}

/// The `dwChannelMask` (`KSAUDIO_SPEAKER_*`) for a layout, in its storage order.
pub(crate) fn channel_mask(layout: ChannelLayout) -> u32 {
    match layout {
        // Front center.
        ChannelLayout::Mono => 0x4,
        // Front left, front right.
        ChannelLayout::Stereo => 0x3,
        // Front left, front right, center, LFE, back left, back right.
        ChannelLayout::Surround51 => 0x3F,
        // `ChannelLayout` is non-exhaustive; a layout added later gets its mask here.
        _ => 0,
    }
}

/// Chooses how to open a stream. A device whose mix format is already `f32` in a layout
/// `jstream-media` knows is opened as is. Any other sample type is converted to `f32`,
/// and any other channel count (a 4-channel mic array, 7.1 speakers) is downmixed to
/// stereo by WASAPI: the proposed default in the `jstream-platform` README.
///
/// A 6-channel mix is opened as is only when its speakers are `jstream-media`'s 5.1
/// (surround pair at the back). Any other 5.1 (surround pair at the side, `0x60F`) is
/// converted, so WASAPI maps its channels into that order.
///
/// # Errors
/// [`PlatformError::Unsupported`] for a mix format without a rate or channels.
pub(crate) fn plan(mix: MixFormat) -> Result<Plan, PlatformError> {
    if mix.rate == 0 || mix.channels == 0 {
        return Err(PlatformError::Unsupported {
            what: format!(
                "mix format with {} Hz and {} channels",
                mix.rate, mix.channels
            ),
        });
    }
    let native = layout_for(mix.channels);
    let layout = native.unwrap_or(ChannelLayout::Stereo);
    let format = AudioFormat::new(mix.rate, layout, SampleFormat::F32Interleaved)?;
    let speakers_match =
        layout != ChannelLayout::Surround51 || mix.mask.is_none_or(|m| m == channel_mask(layout));
    Ok(Plan {
        format,
        convert: !mix.float32 || native.is_none() || !speakers_match,
    })
}

/// Copies little-endian `f32` samples from a device buffer into `out`, as many as both
/// hold. Byte-wise, so the device buffer needs no particular alignment.
pub(crate) fn read_f32_le(bytes: &[u8], out: &mut [f32]) {
    for (o, c) in out.iter_mut().zip(bytes.chunks_exact(4)) {
        *o = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    }
}

/// Copies `f32` samples into a device buffer as little-endian bytes, as many as both
/// hold.
pub(crate) fn write_f32_le(samples: &[f32], bytes: &mut [u8]) {
    for (c, s) in bytes.chunks_exact_mut(4).zip(samples) {
        c.copy_from_slice(&s.to_le_bytes());
    }
}

/// `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`.
const DATA_DISCONTINUITY: u32 = 0x1;
/// `AUDCLNT_BUFFERFLAGS_SILENT`.
const SILENT: u32 = 0x2;
/// `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`.
const TIMESTAMP_ERROR: u32 = 0x4;

/// What one `IAudioCaptureClient::GetBuffer` packet reports, in the trait's terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PacketMeta {
    /// The flags to hand on.
    pub(crate) flags: AudioPacketFlags,
    /// The packet's QPC time in 100 ns ticks, unless the device flagged it.
    pub(crate) time_hns: Option<i64>,
}

/// Maps a packet's `AUDCLNT_BUFFERFLAGS_*` bits and QPC position. `forced_gap` marks the
/// first packet after the stream moved to a new device, which is never continuous with
/// the one before.
pub(crate) fn packet_meta(bits: u32, qpc_hns: u64, forced_gap: bool) -> PacketMeta {
    let time_hns = if bits & TIMESTAMP_ERROR != 0 {
        None
    } else {
        i64::try_from(qpc_hns).ok()
    };
    PacketMeta {
        flags: AudioPacketFlags {
            discontinuity: forced_gap || bits & DATA_DISCONTINUITY != 0,
            silent: bits & SILENT != 0,
        },
        time_hns,
    }
}

/// Device positions as the trait reports them: frames since the stream started, carried
/// across a move to a new device (whose own positions restart at zero).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Positions {
    /// Added to the current device's positions.
    base: u64,
    /// One past the last frame reported.
    end: u64,
}

impl Positions {
    /// The stream position of a packet the current device reports at `device_pos`, with
    /// `frames` frames; remembers where it ends.
    pub(crate) fn packet(&mut self, device_pos: u64, frames: u32) -> u64 {
        let pos = self.base.saturating_add(device_pos);
        self.end = self.end.max(pos.saturating_add(u64::from(frames)));
        pos
    }

    /// The stream moved to a new device: its positions continue after the last frame.
    pub(crate) fn new_device(&mut self) {
        self.base = self.end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mix(rate: u32, channels: u16, float32: bool) -> MixFormat {
        MixFormat {
            rate,
            channels,
            float32,
            mask: None,
        }
    }

    #[test]
    fn float_mix_formats_open_as_is() {
        for (ch, layout) in [
            (1, ChannelLayout::Mono),
            (2, ChannelLayout::Stereo),
            (6, ChannelLayout::Surround51),
        ] {
            let p = plan(mix(48_000, ch, true)).expect("plannable");
            assert!(!p.convert);
            assert_eq!(p.format.layout(), layout);
            assert_eq!(p.format.sample_rate(), 48_000);
            assert_eq!(p.format.sample_format(), SampleFormat::F32Interleaved);
        }
    }

    #[test]
    fn side_surround_51_is_remapped_to_back() {
        let with = |mask| MixFormat {
            mask: Some(mask),
            ..mix(48_000, 6, true)
        };
        // Back surround (KSAUDIO_SPEAKER_5POINT1): already jstream-media's order.
        let back = plan(with(0x3F)).expect("plannable");
        assert!(!back.convert);
        // Side surround (KSAUDIO_SPEAKER_5POINT1_SURROUND): WASAPI remaps it.
        let side = plan(with(0x60F)).expect("plannable");
        assert!(side.convert);
        assert_eq!(side.format.layout(), ChannelLayout::Surround51);
        // Stereo and mono masks don't affect the plan.
        let stereo = MixFormat {
            mask: Some(0x3),
            ..mix(48_000, 2, true)
        };
        assert!(!plan(stereo).expect("plannable").convert);
    }

    #[test]
    fn integer_mix_formats_convert_at_the_same_rate() {
        let p = plan(mix(44_100, 2, false)).expect("plannable");
        assert!(p.convert);
        assert_eq!(p.format.sample_rate(), 44_100);
        assert_eq!(p.format.layout(), ChannelLayout::Stereo);
    }

    #[test]
    fn unknown_channel_counts_downmix_to_stereo() {
        for ch in [3, 4, 8] {
            let p = plan(mix(48_000, ch, true)).expect("plannable");
            assert!(p.convert);
            assert_eq!(p.format.layout(), ChannelLayout::Stereo);
        }
    }

    #[test]
    fn empty_mix_formats_are_unsupported() {
        assert!(matches!(
            plan(mix(0, 2, true)),
            Err(PlatformError::Unsupported { .. })
        ));
        assert!(matches!(
            plan(mix(48_000, 0, true)),
            Err(PlatformError::Unsupported { .. })
        ));
    }

    #[test]
    fn masks_match_the_layouts() {
        assert_eq!(channel_mask(ChannelLayout::Mono).count_ones(), 1);
        assert_eq!(channel_mask(ChannelLayout::Stereo).count_ones(), 2);
        assert_eq!(channel_mask(ChannelLayout::Surround51).count_ones(), 6);
    }

    #[test]
    fn samples_round_trip_through_bytes() {
        let samples = [0.0f32, -1.0, 0.5, 1.0e-7];
        let mut bytes = [0u8; 16];
        write_f32_le(&samples, &mut bytes);
        assert_eq!(&bytes[4..8], &(-1.0f32).to_le_bytes());
        let mut back = [9.0f32; 4];
        read_f32_le(&bytes, &mut back);
        assert_eq!(back, samples);
        // Unaligned input works too.
        let mut shifted = [0u8; 17];
        shifted[1..].copy_from_slice(&bytes);
        let mut again = [9.0f32; 4];
        read_f32_le(&shifted[1..], &mut again);
        assert_eq!(again, samples);
    }

    #[test]
    fn copies_stop_at_the_shorter_side() {
        let mut out = [7.0f32; 3];
        read_f32_le(&1.0f32.to_le_bytes(), &mut out);
        assert_eq!(out, [1.0, 7.0, 7.0]);
        let mut bytes = [0u8; 4];
        write_f32_le(&[2.0, 3.0], &mut bytes);
        assert_eq!(bytes, 2.0f32.to_le_bytes());
    }

    #[test]
    fn packet_flags_map() {
        let m = packet_meta(0, 1_234, false);
        assert_eq!(m.flags, AudioPacketFlags::default());
        assert_eq!(m.time_hns, Some(1_234));
        let m = packet_meta(DATA_DISCONTINUITY | SILENT, 5, false);
        assert!(m.flags.discontinuity && m.flags.silent);
        assert_eq!(packet_meta(TIMESTAMP_ERROR, 5, false).time_hns, None);
        assert_eq!(packet_meta(0, u64::MAX, false).time_hns, None);
        assert!(packet_meta(0, 5, true).flags.discontinuity);
    }

    #[test]
    fn positions_continue_across_devices() {
        let mut p = Positions::default();
        assert_eq!(p.packet(0, 480), 0);
        assert_eq!(p.packet(480, 480), 480);
        // A glitch on the device: the jump shows in the stream position too.
        assert_eq!(p.packet(1_440, 480), 1_440);
        p.new_device();
        // The new device restarts at zero; the stream continues after frame 1_920.
        assert_eq!(p.packet(0, 480), 1_920);
        assert_eq!(p.packet(480, 480), 2_400);
    }
}
