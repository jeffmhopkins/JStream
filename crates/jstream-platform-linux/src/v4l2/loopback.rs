//! The virtual camera's sink on a v4l2loopback device (ADR-0156): [`LoopbackVcam`].
//!
//! A start lists the `/dev/videoN` nodes, tries the v4l2loopback ones in
//! [`pick`](crate::vcam::pick)'s order, and keeps the first whose output format it can
//! set to I420 at the canvas size; a node another writer holds refuses that and the next
//! is tried. Each frame is converted to I420 and written whole with `write()`, which
//! v4l2loopback copies into its next buffer without waiting for a reader. The node stays
//! open until the camera stops, and while it is open [`in_use`] names it, so the webcam
//! list leaves JStream's own camera out.

use std::sync::{Mutex, PoisonError};

use jstream_platform::media::PixelFormat;
use jstream_platform::{PlatformError, VcamFormat, VcamSink, VideoSurface};

use super::abi::{
    BUF_TYPE_VIDEO_OUTPUT, COLORSPACE_REC709, FIELD_NONE, PIX_FMT_YUV420, PixFormat,
    QUANTIZATION_LIM_RANGE, XFER_FUNC_709, YCBCR_ENC_709,
};
use super::device::Device;
use super::format::c_name;
use crate::vcam::{
    COMPONENT, Node, bgra_to_i420, camera_size, denied_message, i420_bytes, missing_remedy, pick,
};

/// The nodes this process's virtual cameras write to.
static IN_USE: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Whether a virtual camera in this process writes to `path` now: the webcam list leaves
/// it out, so the camera can't be added as its own source.
pub(crate) fn in_use(path: &str) -> bool {
    IN_USE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|p| p == path)
}

/// One started camera: the node it writes to and the buffers it converts through.
#[derive(Debug)]
struct Started {
    device: Device,
    path: String,
    format: VcamFormat,
    /// The I420 size, the canvas cropped to even dimensions.
    width: u32,
    height: u32,
    /// The NV12 step and the I420 frame, reused for every frame.
    scratch: Vec<u8>,
    i420: Vec<u8>,
}

impl Drop for Started {
    fn drop(&mut self) {
        IN_USE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|p| p != &self.path);
    }
}

/// The virtual camera on Linux: frames written to a v4l2loopback device, which apps
/// read as a webcam (ADR-0156).
///
/// The user installs and loads v4l2loopback; JStream never runs anything as root. A
/// start without a writable v4l2loopback device fails with
/// [`PlatformError::MissingComponent`], whose remedy gives the commands. A device
/// created with the label `JStream Camera` is preferred to other v4l2loopback devices.
#[derive(Debug, Default)]
pub struct LoopbackVcam {
    started: Option<Started>,
}

impl LoopbackVcam {
    /// A stopped camera. It opens nothing until [`start`](VcamSink::start).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The node the started camera writes to, such as `/dev/video10`, or `None` when
    /// stopped.
    #[must_use]
    pub fn device_path(&self) -> Option<&str> {
        self.started.as_ref().map(|s| s.path.as_str())
    }
}

/// Every `/dev/videoN` node this user can open, as `VIDIOC_QUERYCAP` describes it, with
/// the device opened, and the paths of those it isn't allowed to open.
fn loopback_nodes() -> (Vec<(Node, Device)>, Vec<String>) {
    let mut denied = Vec::new();
    let nodes = super::backend::nodes()
        .into_iter()
        .filter_map(|path| {
            let path = path.to_str()?.to_owned();
            let number = super::format::node_number(path.strip_prefix("/dev/")?)?;
            let mut device = match Device::open(&path) {
                Ok(device) => device,
                Err(PlatformError::Os { code, .. }) if code == u32::try_from(libc::EACCES).ok() => {
                    denied.push(path);
                    return None;
                }
                Err(_) => return None,
            };
            let cap = device.capability().ok()?;
            device.what = format!("virtual camera {path:?}");
            let node = Node {
                path,
                number,
                driver: c_name(&cap.driver),
                card: c_name(&cap.card),
                capabilities: cap.capabilities,
                device_caps: cap.device_caps,
            };
            Some((node, device))
        })
        .collect();
    (nodes, denied)
}

/// The I420 output format at `width` x `height`, BT.709 limited range.
fn i420_format(width: u32, height: u32) -> PixFormat {
    PixFormat {
        width,
        height,
        pixelformat: PIX_FMT_YUV420,
        field: FIELD_NONE,
        bytesperline: width,
        sizeimage: u32::try_from(i420_bytes(width, height)).unwrap_or(u32::MAX),
        colorspace: COLORSPACE_REC709,
        ycbcr_enc: YCBCR_ENC_709,
        quantization: QUANTIZATION_LIM_RANGE,
        xfer_func: XFER_FUNC_709,
        ..PixFormat::default()
    }
}

/// Sets `device`'s output format and checks that the driver kept it.
fn set_output(
    device: &mut Device,
    format: &VcamFormat,
    w: u32,
    h: u32,
) -> Result<(), PlatformError> {
    device.set_type(BUF_TYPE_VIDEO_OUTPUT);
    let settled = device.set_pix(i420_format(w, h))?;
    if settled.pixelformat != PIX_FMT_YUV420 || settled.width != w || settled.height != h {
        return Err(PlatformError::Unsupported {
            what: format!(
                "{} settled on a different format ({}x{}), not I420 at {w}x{h}",
                device.what, settled.width, settled.height
            ),
        });
    }
    // The frame rate readers see; v4l2loopback takes it, and a driver that has no say
    // over it is fine too.
    let _ = device.set_period(format.frame_period);
    Ok(())
}

impl VcamSink for LoopbackVcam {
    fn start(&mut self, format: VcamFormat) -> Result<(), PlatformError> {
        if self.started.is_some() {
            return Err(PlatformError::InvalidState {
                what: "virtual camera",
                state: "already started",
            });
        }
        if format.pixel_format != PixelFormat::Bgra8 {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "{:?} frames for the virtual camera (it takes BGRA8)",
                    format.pixel_format
                ),
            });
        }
        let (w, h) = camera_size(format.size.width(), format.size.height()).ok_or_else(|| {
            PlatformError::Unsupported {
                what: format!(
                    "a {}x{} virtual camera",
                    format.size.width(),
                    format.size.height()
                ),
            }
        })?;
        let (mut nodes, denied) = loopback_nodes();
        let order: Vec<String> = pick(&nodes.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>())
            .into_iter()
            .map(|n| n.path.clone())
            .collect();
        let mut last = None;
        for path in order {
            let Some(i) = nodes.iter().position(|(n, _)| n.path == path) else {
                continue;
            };
            let (_, mut device) = nodes.swap_remove(i);
            match set_output(&mut device, &format, w, h) {
                Ok(()) => {
                    IN_USE
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(path.clone());
                    self.started = Some(Started {
                        device,
                        path,
                        format,
                        width: w,
                        height: h,
                        scratch: Vec::new(),
                        i420: Vec::new(),
                    });
                    return Ok(());
                }
                Err(e) => last = Some(e),
            }
        }
        Err(match last {
            // Every device was there but none took the format: another writer has each.
            Some(PlatformError::Busy { .. }) => PlatformError::Busy {
                what: "every v4l2loopback device (another app is writing to each)",
            },
            Some(e) => e,
            // The module may be loaded, but its device isn't this user's to open.
            None if !denied.is_empty() => PlatformError::Os {
                call: "open",
                code: u32::try_from(libc::EACCES).ok(),
                message: denied_message(&denied),
            },
            None => PlatformError::MissingComponent {
                component: COMPONENT,
                remedy: missing_remedy(),
            },
        })
    }

    fn publish(&mut self, frame: VideoSurface) -> Result<(), PlatformError> {
        let Some(s) = self.started.as_mut() else {
            return Err(PlatformError::InvalidState {
                what: "virtual camera",
                state: "not started",
            });
        };
        let VideoSurface::Cpu(frame) = frame else {
            return Err(PlatformError::Unsupported {
                what: "a GPU frame for the virtual camera (it takes CPU frames)".into(),
            });
        };
        let info = frame.info();
        if info.format != PixelFormat::Bgra8 || info.size != s.format.size {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "a {:?} {}x{} frame for a {}x{} BGRA8 virtual camera",
                    info.format,
                    info.size.width(),
                    info.size.height(),
                    s.format.size.width(),
                    s.format.size.height()
                ),
            });
        }
        let Some(plane) = frame.plane(0) else {
            return Err(PlatformError::InvalidState {
                what: "virtual camera frame",
                state: "it has no plane",
            });
        };
        bgra_to_i420(
            plane.data(),
            plane.stride(),
            s.width,
            s.height,
            &mut s.scratch,
            &mut s.i420,
        )?;
        // A frame the driver can't take now is dropped: the newest frame wins.
        s.device.write_frame(&s.i420)?;
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlatformError> {
        // Closing the node is all: readers see no more frames, and with
        // `exclusive_caps=1` the device stops offering capture until a writer returns.
        self.started = None;
        Ok(())
    }

    fn format(&self) -> Option<VcamFormat> {
        self.started.as_ref().map(|s| s.format)
    }
}

#[cfg(all(test, feature = "hw-tests"))]
mod hw_tests {
    //! Run with v4l2loopback loaded and FFmpeg installed (CI's `vcam-linux` job does
    //! both): `cargo test -p jstream-platform-linux --features hw-tests -- --ignored
    //! v4l2::loopback::`.

    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use jstream_platform::media::{
        ColorInfo, ColorRange, ColorSpace, FrameSize, MediaTime, Plane, Timebase, VideoFrame,
        VideoFrameInfo,
    };

    use super::*;

    const W: u32 = 320;
    const H: u32 = 240;

    /// A BGRA frame: the left half `left`, the right half `right` (B, G, R).
    fn frame(left: [u8; 3], right: [u8; 3], n: i64) -> VideoFrame {
        let mut data = Vec::with_capacity((W * H * 4) as usize);
        for _ in 0..H {
            for x in 0..W {
                let [b, g, r] = if x < W / 2 { left } else { right };
                data.extend_from_slice(&[b, g, r, 255]);
            }
        }
        let info = VideoFrameInfo {
            size: FrameSize::new(W, H).expect("size"),
            format: PixelFormat::Bgra8,
            color: ColorInfo {
                space: ColorSpace::Bt709,
                range: ColorRange::Full,
            },
            pts: MediaTime::new(n, Timebase::new(1, 30).expect("timebase")),
        };
        VideoFrame::new(info, vec![Plane::new(data, (W * 4) as usize)]).expect("frame")
    }

    fn format() -> VcamFormat {
        VcamFormat {
            size: FrameSize::new(W, H).expect("size"),
            pixel_format: PixelFormat::Bgra8,
            frame_period: Timebase::new(1, 30).expect("period"),
        }
    }

    /// The camera publishes, and FFmpeg reading the device as a webcam gets its frames
    /// back: red on the left and blue on the right, within the round trip's rounding.
    /// Then it stops and starts again, as a user restarting the output does.
    #[test]
    #[ignore = "needs v4l2loopback loaded and FFmpeg"]
    fn loopback_frames_read_back_through_ffmpeg() {
        let mut cam = LoopbackVcam::new();
        cam.start(format()).expect("start: is v4l2loopback loaded?");
        // A frame before FFmpeg opens the node: with `exclusive_caps=1` it offers
        // capture only once a writer has written.
        cam.publish(VideoSurface::Cpu(frame([0, 0, 255], [255, 0, 0], 0)))
            .expect("first publish");
        let path = cam.device_path().expect("a device").to_owned();
        assert!(in_use(&path), "the webcam list leaves our camera out");

        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut n = 1;
                while !stop.load(Ordering::Acquire) {
                    cam.publish(VideoSurface::Cpu(frame([0, 0, 255], [255, 0, 0], n)))
                        .expect("publish");
                    n += 1;
                    std::thread::sleep(Duration::from_millis(33));
                }
                cam
            })
        };

        let mut ffmpeg = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "v4l2",
                "-i",
                &path,
            ])
            // FFmpeg's V4L2 input doesn't read the format's colour fields and would
            // convert as BT.601, so the BT.709 limited range the camera writes (and
            // signals) is named here; this checks the pixels, not FFmpeg's default.
            .args(["-vf", "scale=in_color_matrix=bt709:in_range=tv"])
            .args(["-frames:v", "5", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("ffmpeg");
        // Read on a thread of its own, so a hung FFmpeg fails the test at the deadline
        // instead of blocking the read.
        let mut stdout = ffmpeg.stdout.take().expect("stdout");
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            stdout.read_to_end(&mut out).map(|_| out)
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(s) = ffmpeg.try_wait().expect("wait") {
                break Some(s);
            }
            if Instant::now() >= deadline {
                let _ = ffmpeg.kill();
                let _ = ffmpeg.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        stop.store(true, Ordering::Release);
        let mut cam = writer.join().expect("writer");
        let status = status.expect("ffmpeg didn't finish in 20 s");
        assert!(status.success(), "ffmpeg failed: {status}");
        let out = reader.join().expect("reader").expect("read");

        let frame_bytes = (W * H * 3) as usize;
        assert_eq!(out.len(), frame_bytes * 5, "five {W}x{H} frames");
        let last = &out[frame_bytes * 4..];
        let px = |x: u32, y: u32| {
            let i = ((y * W + x) * 3) as usize;
            [last[i], last[i + 1], last[i + 2]]
        };
        let near =
            |got: [u8; 3], want: [u8; 3]| got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 12);
        assert!(
            near(px(W / 4, H / 2), [255, 0, 0]),
            "left is red: {:?}",
            px(W / 4, H / 2)
        );
        assert!(
            near(px(3 * W / 4, H / 2), [0, 0, 255]),
            "right is blue: {:?}",
            px(3 * W / 4, H / 2)
        );

        cam.stop().expect("stop");
        assert!(!in_use(&path));
        assert_eq!(cam.format(), None);

        cam.start(format()).expect("start again");
        cam.publish(VideoSurface::Cpu(frame([0, 0, 255], [255, 0, 0], 0)))
            .expect("publish after a restart");
        cam.stop().expect("stop again");
    }
}
