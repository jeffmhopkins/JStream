//! Color metadata carried with video frames.

/// The YUV matrix and primaries a frame was encoded with.
///
/// For RGB pixel formats only the primaries apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColorSpace {
    /// ITU-R BT.601 (standard definition).
    Bt601,
    /// ITU-R BT.709 (HD), the engine's default output.
    #[default]
    Bt709,
    /// ITU-R BT.2020 (UHD and HDR).
    Bt2020,
}

/// The numeric range of the stored samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColorRange {
    /// "TV" range: 8-bit luma 16..=235 and chroma 16..=240 (scaled for higher depths).
    #[default]
    Limited,
    /// "PC" range: every code value is used.
    Full,
}

/// Color space and range of a frame.
///
/// The default is BT.709 limited range, the engine's default output
/// (docs/ARCHITECTURE.md#data-model).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ColorInfo {
    /// The matrix and primaries.
    pub space: ColorSpace,
    /// The sample range.
    pub range: ColorRange,
}

#[cfg(test)]
mod tests {
    use super::{ColorInfo, ColorRange, ColorSpace};

    #[test]
    fn default_is_bt709_limited() {
        assert_eq!(
            ColorInfo::default(),
            ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Limited
            }
        );
    }
}
