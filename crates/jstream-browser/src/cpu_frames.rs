//! Pages' frames in CPU memory, for the CEF backend on Linux (ADR-0095): the helper
//! paints in software, turns each paint into straight-alpha RGBA8 ([`straight_rgba`]),
//! and writes it into one of the page's slot files in a private folder the engine made
//! for it; the engine reads the file the helper names ([`slot_file`]) when the helper
//! announces the paint, and the slot is reused once the engine releases it.
//! Platform-neutral, so its arithmetic is tested everywhere.

use std::path::{Path, PathBuf};

/// The most slot files the helper paints a page into (its own ring), as on Windows.
pub(crate) const SLOTS: usize = 3;

/// The largest frame either side accepts each way, in pixels: Chromium's own limit on a
/// view.
pub(crate) const MAX_SIDE: u32 = 16_384;

/// The number the helper names slot `slot` of page `page` by, in its
/// [`Frame`](crate::wire::ToEngine::Frame) message's `handle`.
#[cfg(any(test, feature = "helper"))]
pub(crate) fn handle(page: u32, slot: u8) -> u64 {
    (u64::from(page) << 8) | u64::from(slot)
}

/// The slot file `handle` names, in the page folder `dir`.
pub(crate) fn slot_file(dir: &Path, handle: u64) -> PathBuf {
    dir.join(format!("slot-{handle:x}"))
}

/// The bytes of one `width` × `height` RGBA8 frame, or `None` over [`MAX_SIDE`] either
/// way or for an empty one.
pub(crate) fn frame_bytes(width: u32, height: u32) -> Option<usize> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    usize::try_from(u64::from(width) * u64::from(height) * 4).ok()
}

/// CEF's paint (`bgra`: BGRA8 with premultiplied alpha) as straight-alpha RGBA8 in
/// `rgba`, the CPU frame layout the compositor takes. Each takes as many whole pixels as
/// both hold.
#[cfg(any(test, feature = "helper"))]
pub(crate) fn straight_rgba(bgra: &[u8], rgba: &mut [u8]) {
    for (from, to) in bgra.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
        let [b, g, r, a] = [from[0], from[1], from[2], from[3]];
        let un = |c: u8| -> u8 {
            match a {
                0 => 0,
                255 => c,
                // Rounded, and capped: a premultiplied value is never above its alpha,
                // but a paint that breaks that must not wrap.
                _ => ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8,
            }
        };
        to.copy_from_slice(&[un(r), un(g), un(b), a]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplied_bgra_becomes_straight_rgba() {
        let bgra = [
            0, 0, 255, 255, // opaque red
            0, 0, 0, 0, // transparent
            64, 0, 128, 128, // half-transparent red with some blue, premultiplied
            0, 0, 255, 128, // out of range: more red than alpha
        ];
        let mut rgba = [9; 16];
        straight_rgba(&bgra, &mut rgba);
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[0, 0, 0, 0]);
        assert_eq!(&rgba[8..12], &[255, 0, 128, 128]);
        assert_eq!(&rgba[12..16], &[255, 0, 0, 128]);
    }

    #[test]
    fn frames_are_bounded_and_slots_are_named_apart() {
        assert_eq!(frame_bytes(1920, 1080), Some(1920 * 1080 * 4));
        assert_eq!(frame_bytes(0, 1080), None);
        assert_eq!(frame_bytes(MAX_SIDE + 1, 1), None);
        assert_ne!(handle(1, 2), handle(2, 1));
        let slots: std::collections::HashSet<u64> =
            (0..SLOTS as u8).map(|slot| handle(1, slot)).collect();
        assert_eq!(
            slots.len(),
            SLOTS,
            "each of a page's slots has its own file"
        );
        let dir = Path::new("/run/user/1000/jstream-browser-1");
        assert_eq!(
            slot_file(dir, handle(1, 2)),
            dir.join("slot-102"),
            "page 1, slot 2"
        );
    }
}
