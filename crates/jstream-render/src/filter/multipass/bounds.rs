//! How drop shadow, outline and glow fit an effect that reaches past the source's
//! frame: the output grows by the effect's extent on each side, rounded up to whole
//! pixels, and the input is drawn 1:1 at the matching inset. This changes the source's
//! reported size; the inset is where the original frame sits in the grown one, which
//! each of them reports as its [`ContentBox`].

use crate::filter::ContentBox;

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

    /// Room for a copy of the shape blurred by `radius` and moved by `(dx, dy)`: it
    /// spans `radius` past the moved shape on each side.
    pub(super) fn shadow(radius: f32, dx: f32, dy: f32) -> Self {
        let r = px(radius) as f32;
        Self {
            left: px(r - dx),
            top: px(r - dy),
            right: px(r + dx),
            bottom: px(r + dy),
        }
    }

    /// The grown size of a `width` x `height` frame.
    pub(super) fn grow(self, width: u32, height: u32) -> (u32, u32) {
        (
            width.saturating_add(self.left).saturating_add(self.right),
            height.saturating_add(self.top).saturating_add(self.bottom),
        )
    }

    /// Where a `width` x `height` input sits in the grown frame.
    pub(super) fn content(self, width: u32, height: u32) -> ContentBox {
        ContentBox {
            x: self.left,
            y: self.top,
            width,
            height,
        }
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
    fn a_shadow_grows_toward_its_offset_and_by_its_blur() {
        let i = Insets::shadow(4.0, 6.0, -2.0);
        assert_eq!((i.left, i.top, i.right, i.bottom), (0, 6, 10, 2));
        assert_eq!(i.grow(20, 10), (30, 18));
        assert_eq!(i.origin(), (0, 6));
        let content = i.content(20, 10);
        assert_eq!(
            (content.x, content.y, content.width, content.height),
            (0, 6, 20, 10)
        );
        // Fractions round up: the blur first, then the offset.
        let i = Insets::shadow(2.5, 0.5, 0.0);
        assert_eq!((i.left, i.top, i.right, i.bottom), (3, 3, 4, 3));
        assert_eq!(Insets::shadow(0.0, 0.0, 0.0), Insets::default());
        assert_eq!(Insets::shadow(f32::NAN, 1.0, 0.0).right, 1);
    }

    #[test]
    fn even_insets_pad_every_side() {
        assert_eq!(Insets::even(3.2).grow(10, 4), (18, 12));
        assert_eq!(Insets::even(-1.0), Insets::default());
        assert_eq!(Insets::even(5.0).grow(u32::MAX, 1), (u32::MAX, 11));
    }
}
