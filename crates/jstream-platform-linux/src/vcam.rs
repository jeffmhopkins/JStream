//! The virtual camera on Linux, through v4l2loopback (ADR-0156): the parts that don't
//! call Linux, tested on every platform. The sink itself, [`LoopbackVcam`], is in
//! `v4l2/loopback.rs`.
//!
//! v4l2loopback is a kernel module the user installs and loads (it needs root, which
//! JStream never asks for). Each device it creates is a `/dev/videoN` node that one
//! writer feeds and any number of apps read as a webcam. The sink picks a node
//! ([`pick`]), sets its format to I420 at the canvas size, and writes each frame with
//! `write()`: v4l2loopback copies it into its next buffer and never waits for a reader.
//!
//! I420 (`V4L2_PIX_FMT_YUV420`) is the format every common reader takes: browsers
//! through WebRTC, video-call apps, OBS, GStreamer and FFmpeg. The frames are BT.709
//! limited range, converted by the converter every platform's encoders share
//! ([`jstream_platform::nv12`]), with the chroma planes split out of NV12's interleaved
//! one.
//!
//! [`LoopbackVcam`]: crate::LoopbackVcam

use jstream_platform::PlatformError;
use jstream_platform::nv12::{Nv12Image, RgbOrder, rgb_to_nv12_image};

/// The driver name v4l2loopback reports in `VIDIOC_QUERYCAP`.
pub const LOOPBACK_DRIVER: &str = "v4l2 loopback";

/// The card label the sink prefers: a device created with this label (any case) is
/// picked before other v4l2loopback devices.
pub const CARD_LABEL: &str = "JStream";

/// The command that creates JStream's camera, as the remedy and the docs give it. Run
/// once per boot as root; [`PERSIST_HINT`] makes it load at every boot.
pub const MODPROBE: &str =
    "sudo modprobe v4l2loopback devices=1 card_label=\"JStream Camera\" exclusive_caps=1";

/// How to keep the module loaded across reboots.
pub const PERSIST_HINT: &str = "To load it at every boot, put `v4l2loopback` in \
     /etc/modules-load.d/v4l2loopback.conf and `options v4l2loopback devices=1 \
     card_label=\"JStream Camera\" exclusive_caps=1` in /etc/modprobe.d/v4l2loopback.conf";

/// What [`PlatformError::MissingComponent`] names.
pub const COMPONENT: &str = "the v4l2loopback kernel module";

/// The remedy when no v4l2loopback device can be written: install the module and load
/// it. JStream never does either itself (both need root).
#[must_use]
pub fn missing_remedy() -> String {
    format!(
        "the virtual camera needs a v4l2loopback device. Install the module (Ubuntu and \
         Debian: `sudo apt install v4l2loopback-dkms`; Fedora: `sudo dnf install \
         v4l2loopback` from RPM Fusion; Arch: `sudo pacman -S v4l2loopback-dkms`), then \
         create the camera with `{MODPROBE}` and start the virtual camera again. \
         {PERSIST_HINT}"
    )
}

/// `V4L2_CAP_VIDEO_OUTPUT`: the node takes frames written to it.
pub(crate) const CAP_VIDEO_OUTPUT: u32 = 0x2;
/// `V4L2_CAP_DEVICE_CAPS`: `device_caps` is filled in and describes this node.
const CAP_DEVICE_CAPS: u32 = 0x8000_0000;

/// One `/dev/videoN` node as `VIDIOC_QUERYCAP` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Its path.
    pub path: String,
    /// Its number, `N`.
    pub number: u32,
    /// The driver's name.
    pub driver: String,
    /// The card's name: for v4l2loopback, the `card_label` it was created with.
    pub card: String,
    /// `capabilities`.
    pub capabilities: u32,
    /// `device_caps`.
    pub device_caps: u32,
}

impl Node {
    /// Whether it is a v4l2loopback device that takes frames now. With
    /// `exclusive_caps=1` a node another writer feeds offers only capture, so it is left
    /// out here; a node with `exclusive_caps=0` always offers output, and a busy one is
    /// refused when its format is set.
    #[must_use]
    pub fn is_writable_loopback(&self) -> bool {
        let caps = if self.capabilities & CAP_DEVICE_CAPS != 0 {
            self.device_caps
        } else {
            self.capabilities
        };
        self.driver == LOOPBACK_DRIVER && caps & CAP_VIDEO_OUTPUT != 0
    }

    /// Whether its label marks it as JStream's camera.
    #[must_use]
    pub fn is_labelled(&self) -> bool {
        self.card
            .get(..CARD_LABEL.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(CARD_LABEL))
    }
}

/// The nodes the sink tries, in order: v4l2loopback devices that take frames, those
/// labelled for JStream first, then by number.
#[must_use]
pub fn pick(nodes: &[Node]) -> Vec<&Node> {
    let mut out: Vec<&Node> = nodes.iter().filter(|n| n.is_writable_loopback()).collect();
    out.sort_by_key(|n| (!n.is_labelled(), n.number));
    out
}

/// The I420 frame size the camera offers for a canvas: the largest even size within it,
/// since I420's chroma is subsampled in both directions. `None` below 2x2.
#[must_use]
pub fn camera_size(width: u32, height: u32) -> Option<(u32, u32)> {
    jstream_platform::nv12::even_size(width, height)
}

/// The bytes of one I420 frame at `width` x `height` (both even): a full-size luma plane
/// and two quarter-size chroma planes, rows packed.
#[must_use]
pub fn i420_bytes(width: u32, height: u32) -> usize {
    let luma = width as usize * height as usize;
    luma + luma / 2
}

/// Converts the top-left `width` x `height` (both even) of a BGRA image, rows `stride`
/// bytes apart, to packed I420 in `out` (BT.709 limited range), using `scratch` for the
/// NV12 step. Both buffers are reused across frames; `out` ends up
/// [`i420_bytes`] long.
///
/// # Errors
/// [`PlatformError::InvalidState`] when `src` is too small for the size.
pub fn bgra_to_i420(
    src: &[u8],
    stride: usize,
    width: u32,
    height: u32,
    scratch: &mut Vec<u8>,
    out: &mut Vec<u8>,
) -> Result<(), PlatformError> {
    let (w, h) = (width as usize, height as usize);
    let luma = w * h;
    scratch.resize(luma + luma / 2, 0);
    let mut nv12 = Nv12Image::new(scratch, [0, luma], [w, w], w, h)?;
    rgb_to_nv12_image(src, stride, RgbOrder::Bgra, &mut nv12)?;
    out.resize(luma + luma / 2, 0);
    let (y, chroma) = out.split_at_mut(luma);
    let (u, v) = chroma.split_at_mut(luma / 4);
    y.copy_from_slice(&scratch[..luma]);
    for (i, uv) in scratch[luma..].chunks_exact(2).enumerate() {
        u[i] = uv[0];
        v[i] = uv[1];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(number: u32, driver: &str, card: &str, caps: u32) -> Node {
        Node {
            path: format!("/dev/video{number}"),
            number,
            driver: driver.to_owned(),
            card: card.to_owned(),
            capabilities: caps | CAP_DEVICE_CAPS,
            device_caps: caps,
        }
    }

    #[test]
    fn picks_writable_loopback_nodes_labelled_ones_first() {
        let nodes = [
            node(0, "uvcvideo", "Integrated Camera", 0x1),
            node(
                2,
                LOOPBACK_DRIVER,
                "Dummy video device (0x0000)",
                CAP_VIDEO_OUTPUT,
            ),
            node(9, LOOPBACK_DRIVER, "JStream Camera", CAP_VIDEO_OUTPUT),
            // exclusive_caps=1 and another writer feeds it: capture only.
            node(4, LOOPBACK_DRIVER, "jstream", 0x1),
            node(7, LOOPBACK_DRIVER, "jstream two", CAP_VIDEO_OUTPUT | 0x1),
        ];
        let order: Vec<u32> = pick(&nodes).iter().map(|n| n.number).collect();
        assert_eq!(order, [7, 9, 2]);
    }

    #[test]
    fn without_device_caps_the_capabilities_count() {
        let mut n = node(1, LOOPBACK_DRIVER, "x", 0);
        n.capabilities = CAP_VIDEO_OUTPUT;
        assert!(n.is_writable_loopback());
        n.capabilities = CAP_VIDEO_OUTPUT | CAP_DEVICE_CAPS;
        assert!(
            !n.is_writable_loopback(),
            "device_caps (0) wins when filled in"
        );
    }

    #[test]
    fn the_camera_is_the_even_size_within_the_canvas() {
        assert_eq!(camera_size(1920, 1080), Some((1920, 1080)));
        assert_eq!(camera_size(1281, 721), Some((1280, 720)));
        assert_eq!(camera_size(1, 720), None);
        assert_eq!(i420_bytes(1920, 1080), 1920 * 1080 * 3 / 2);
    }

    #[test]
    fn bgra_becomes_planar_i420() {
        // 4x2: left half pure red, right half pure blue, with a padded stride.
        let (w, h, stride) = (4u32, 2u32, 20usize);
        let mut src = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let px = &mut src[y * stride + x * 4..][..4];
                px.copy_from_slice(if x < 2 {
                    &[0, 0, 255, 255]
                } else {
                    &[255, 0, 0, 255]
                });
            }
        }
        let (mut scratch, mut out) = (Vec::new(), Vec::new());
        bgra_to_i420(&src, stride, w, h, &mut scratch, &mut out).expect("converted");
        assert_eq!(out.len(), i420_bytes(w, h));
        let (y, chroma) = out.split_at(8);
        let (u, v) = chroma.split_at(2);
        // BT.709 limited: red has Y 63, blue Y 32; red's V is high, blue's U is high. The
        // right block's chroma also takes a quarter of the red column beside it (the
        // left-sited [1 2 1] filter).
        assert!(y[0].abs_diff(63) <= 1 && y[3].abs_diff(32) <= 1, "{y:?}");
        assert_eq!(y[..4], y[4..], "both rows match");
        assert!(v[0] > 200 && u[0] < 128, "red: u {u:?} v {v:?}");
        assert!(u[1] > 200 && v[1] < v[0], "blue: u {u:?} v {v:?}");
    }

    #[test]
    fn grey_is_neutral_chroma() {
        let src = vec![128u8; 2 * 2 * 4];
        let (mut scratch, mut out) = (Vec::new(), Vec::new());
        bgra_to_i420(&src, 8, 2, 2, &mut scratch, &mut out).expect("converted");
        assert_eq!(out[4..], [128, 128]);
    }

    #[test]
    fn a_short_source_is_refused() {
        let (mut scratch, mut out) = (Vec::new(), Vec::new());
        assert!(bgra_to_i420(&[0; 8], 8, 2, 2, &mut scratch, &mut out).is_err());
    }

    #[test]
    fn the_remedy_never_runs_anything_and_names_the_module() {
        let r = missing_remedy();
        assert!(r.contains("v4l2loopback-dkms") && r.contains(MODPROBE));
        assert!(MODPROBE.contains("exclusive_caps=1"));
    }
}
