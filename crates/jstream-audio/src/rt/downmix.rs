//! Downmixing every device layout to the engine's stereo, before resampling (so 5.1
//! loopback costs no more to resample than stereo).
//!
//! Coefficients:
//!
//! | Layout | Left | Right |
//! |---|---|---|
//! | Mono `M` | `M` | `M` |
//! | Stereo `L R` | `L` | `R` |
//! | 5.1 `FL FR C LFE BL BR` | `FL + 0.7071·C + 0.7071·BL` | `FR + 0.7071·C + 0.7071·BR` |
//! | anything else | first channel | second channel (or the first, if there is one) |
//!
//! - **Mono** is copied to both sides at full level, so a mono microphone sounds as loud
//!   centred as it does on its own, as OBS does.
//! - **5.1** uses the ITU-R BS.775 downmix: centre and surrounds at −3 dB, LFE dropped.
//!   It is deliberately **not normalised**: on a 5.1 endpoint, ordinary stereo content
//!   arrives on FL/FR only and must come out unchanged, not 7.7 dB quieter (the cost of
//!   scaling by `1 / (1 + 0.7071 + 0.7071)`). Full-scale content on every channel can
//!   exceed 0 dBFS; the bus is `f32`, so nothing clips here.
//! - Non-finite samples (a misbehaving driver) become 0, and samples are limited to
//!   ±[`LIMIT`] (+36 dBFS), so NaN never enters the resampler's history and no later
//!   sum or gain can overflow to infinity.

use jstream_media::ChannelLayout;

/// The largest sample magnitude let through: +36 dBFS, far above any real signal.
pub(crate) const LIMIT: f32 = 64.0;

/// −3 dB: `1 / sqrt(2)`.
pub(crate) const MINUS_3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// How to fold one device frame into stereo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Downmix {
    Mono,
    Stereo,
    Surround51,
    /// An unknown layout: its first two channels (or the first, twice).
    FirstTwo,
}

impl Downmix {
    pub(crate) fn for_layout(layout: ChannelLayout) -> Self {
        match layout {
            ChannelLayout::Mono => Self::Mono,
            ChannelLayout::Stereo => Self::Stereo,
            ChannelLayout::Surround51 => Self::Surround51,
            _ => Self::FirstTwo,
        }
    }

    /// Folds one interleaved frame (`frame.len()` is the device's channel count, at
    /// least 1) into a stereo frame.
    pub(crate) fn apply(self, frame: &[f32]) -> [f32; 2] {
        let s = |i: usize| {
            frame
                .get(i)
                .copied()
                .filter(|v| v.is_finite())
                .map_or(0.0, |v| v.clamp(-LIMIT, LIMIT))
        };
        match self {
            Self::Mono => [s(0), s(0)],
            Self::Stereo => [s(0), s(1)],
            Self::Surround51 => {
                let c = MINUS_3DB * s(2);
                [s(0) + c + MINUS_3DB * s(4), s(1) + c + MINUS_3DB * s(5)]
            }
            Self::FirstTwo => {
                if frame.len() >= 2 {
                    [s(0), s(1)]
                } else {
                    [s(0), s(0)]
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_and_stereo() {
        assert_eq!(Downmix::Mono.apply(&[0.5]), [0.5, 0.5]);
        assert_eq!(Downmix::Stereo.apply(&[0.5, -0.25]), [0.5, -0.25]);
        assert_eq!(Downmix::FirstTwo.apply(&[0.5]), [0.5, 0.5]);
        assert_eq!(Downmix::FirstTwo.apply(&[0.5, 0.25, 1.0]), [0.5, 0.25]);
    }

    /// One impulse per 5.1 channel, in WAVEFORMATEXTENSIBLE order.
    #[test]
    fn surround_coefficients() {
        let d = Downmix::for_layout(ChannelLayout::Surround51);
        let unit = |ch: usize| {
            let mut f = [0.0f32; 6];
            f[ch] = 1.0;
            d.apply(&f)
        };
        let h = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(unit(0), [1.0, 0.0]); // FL
        assert_eq!(unit(1), [0.0, 1.0]); // FR
        assert_eq!(unit(2), [h, h]); // C
        assert_eq!(unit(3), [0.0, 0.0]); // LFE
        assert_eq!(unit(4), [h, 0.0]); // BL
        assert_eq!(unit(5), [0.0, h]); // BR
        // Stereo content on a 5.1 endpoint passes unchanged.
        assert_eq!(d.apply(&[0.3, -0.2, 0.0, 0.0, 0.0, 0.0]), [0.3, -0.2]);
    }

    #[test]
    fn non_finite_samples_become_silence() {
        assert_eq!(
            Downmix::Stereo.apply(&[f32::NAN, f32::INFINITY]),
            [0.0, 0.0]
        );
        let d = Downmix::Surround51;
        assert_eq!(
            d.apply(&[0.5, 0.5, f32::NAN, f32::NAN, 0.0, 0.0]),
            [0.5, 0.5]
        );
        assert_eq!(Downmix::Stereo.apply(&[1e30, -1e30]), [LIMIT, -LIMIT]);
        assert_eq!(Downmix::for_layout(ChannelLayout::Mono), Downmix::Mono);
        assert_eq!(Downmix::for_layout(ChannelLayout::Stereo), Downmix::Stereo);
    }
}
