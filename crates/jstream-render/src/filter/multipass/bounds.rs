//! Room around a frame, in whole pixels: a drop shadow and a glow blur their input's
//! shape in a scratch frame grown by the blur radius on every side, so the blur fades
//! out before that frame's edge. Their output is the input's size (ADR-0065 §5).

/// Pixels added on each side of the frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Insets {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

/// `v` rounded up to whole pixels, 0 when negative or not finite.
fn px(v: f32) -> u32 {
    if v.is_finite() && v > 0.0 {
        v.ceil() as u32
    } else {
        0
    }
}

impl Insets {
    /// The same `pad` on every side.
    pub(super) fn even(pad: f32) -> Self {
        let pad = px(pad);
        Self {
            left: pad,
            top: pad,
            right: pad,
            bottom: pad,
        }
    }

    /// The grown size of a `width` x `height` frame.
    pub(super) fn grow(self, width: u32, height: u32) -> (u32, u32) {
        (
            width.saturating_add(self.left).saturating_add(self.right),
            height.saturating_add(self.top).saturating_add(self.bottom),
        )
    }

    /// Where the input's top-left sits in the grown frame.
    pub(super) fn origin(self) -> (i32, i32) {
        (self.left as i32, self.top as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::Insets;

    #[test]
    fn even_insets_pad_every_side() {
        assert_eq!(Insets::even(3.2).grow(10, 4), (18, 12));
        assert_eq!(Insets::even(-1.0), Insets::default());
        assert_eq!(Insets::even(5.0).grow(u32::MAX, 1), (u32::MAX, 11));
    }
}
