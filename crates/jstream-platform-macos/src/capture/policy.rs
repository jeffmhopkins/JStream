//! The parts of the ScreenCaptureKit backend that make no system call: target IDs and
//! names, frame sizes and layouts, frame statuses, the Screen Recording permission's
//! states and the errors ScreenCaptureKit reports. Built on macOS, and on every platform
//! for its tests.

use jstream_platform::media::FrameSize;
use jstream_platform::nv12::even_size;
use jstream_platform::{CaptureTarget, DeviceId, PlatformError};

use crate::fourcc::{CV_BGRA, fourcc_str};

const DISPLAY: &str = "display:";
const WINDOW: &str = "window:";

/// The largest capture dimension asked of ScreenCaptureKit, in pixels. Larger sizes are
/// treated as unknown rather than allocated.
const MAX_DIMENSION: f64 = 16_384.0;

/// The ID of the display with `CGDirectDisplayID` `id`, such as `display:1`. It is stable
/// while the display stays connected.
pub(crate) fn display_id(id: u32) -> DeviceId {
    DeviceId::new(format!("{DISPLAY}{id}"))
}

/// The ID of the window with `CGWindowID` `id`, such as `window:4242`. It is valid until
/// the window closes.
pub(crate) fn window_id(id: u32) -> DeviceId {
    DeviceId::new(format!("{WINDOW}{id}"))
}

/// The number after `prefix` in `id`, in decimal, never zero (`kCGNullDirectDisplay` and
/// `kCGNullWindowID`).
fn parse(id: &DeviceId, prefix: &str) -> Option<u32> {
    let digits = id.as_str().strip_prefix(prefix)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|&n| n != 0)
}

/// What [`resolve`] found a target to name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wanted {
    /// A display, by `CGDirectDisplayID`.
    Display(u32),
    /// A window, by `CGWindowID`.
    Window(u32),
}

/// The display or window `target` names.
///
/// # Errors
/// [`PlatformError::NotFound`] for an ID this backend never lists, and
/// [`PlatformError::Unsupported`] for a kind of target it can't capture.
pub(crate) fn resolve(target: &CaptureTarget) -> Result<Wanted, PlatformError> {
    let not_found = |kind, id: &DeviceId| PlatformError::NotFound {
        kind,
        id: id.as_str().to_owned(),
    };
    match target {
        CaptureTarget::Display(id) => parse(id, DISPLAY)
            .map(Wanted::Display)
            .ok_or_else(|| not_found("display", id)),
        CaptureTarget::Window(id) => parse(id, WINDOW)
            .map(Wanted::Window)
            .ok_or_else(|| not_found("window", id)),
        other => Err(PlatformError::Unsupported {
            what: format!("capturing {other:?} through ScreenCaptureKit"),
        }),
    }
}

/// A display's name in the target list, such as `Display 1`. macOS's own names for
/// displays come from AppKit, which this crate doesn't link.
pub(crate) fn display_name(id: u32) -> String {
    format!("Display {id}")
}

/// A window's name in the target list: its title, else its application's name. `None`
/// when it has neither, and such a window isn't listed.
pub(crate) fn window_name(title: Option<&str>, application: Option<&str>) -> Option<String> {
    usable(title).or(usable(application)).map(str::to_owned)
}

/// `s` trimmed, unless that leaves nothing.
fn usable(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// Whether a window is listed as a capture target: a normal window (layer 0, not the
/// menu bar, the Dock or a panel above them), not one of JStream's own, and at least
/// 2 x 2 points.
pub(crate) fn listable_window(layer: isize, own: bool, width: f64, height: f64) -> bool {
    layer == 0 && !own && width >= 2.0 && height >= 2.0
}

/// A content rectangle of `width` x `height` points, at `scale` pixels per point, as a
/// size in pixels, rounded to the nearest. `None` when it is empty, not finite, or larger
/// than 16384 pixels either way.
pub(crate) fn pixel_size(width: f64, height: f64, scale: f64) -> Option<FrameSize> {
    let px = |points: f64| {
        let v = (points * scale).round();
        (1.0..=MAX_DIMENSION).contains(&v).then_some(v as u32)
    };
    FrameSize::new(px(width)?, px(height)?).ok()
}

/// What to do with a frame, by its `SCStreamFrameInfoStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameUse {
    /// Convert and deliver it.
    Deliver,
    /// Nothing new to show (idle, blank, suspended or the start marker): skip it, and the
    /// render loop repeats the last frame.
    Skip,
    /// The stream stopped.
    Stopped,
}

/// The use of a frame whose status attachment is `status` (`SCFrameStatus`), or that has
/// none, which is treated as complete.
pub(crate) fn frame_use(status: Option<i64>) -> FrameUse {
    match status {
        None | Some(0) => FrameUse::Deliver,
        Some(5) => FrameUse::Stopped,
        // Idle 1, Blank 2, Suspended 3, Started 4, and any status added later.
        Some(_) => FrameUse::Skip,
    }
}

/// Checks a locked frame buffer: Core Video pixel format `format`, `width` x `height`
/// pixels, rows `stride` bytes apart, `len` bytes mapped. Returns the even size the
/// converter takes (an odd dimension loses its last column or row), or `None` when the
/// frame is smaller than 2 x 2.
///
/// # Errors
/// [`PlatformError::Unsupported`] for a format other than BGRA, and
/// [`PlatformError::InvalidState`] when the rows don't fit the buffer.
pub(crate) fn bgra_frame_size(
    format: u32,
    width: usize,
    height: usize,
    stride: usize,
    len: usize,
) -> Result<Option<(u32, u32)>, PlatformError> {
    if format != CV_BGRA {
        let shown = fourcc_str(format).unwrap_or_else(|| format!("0x{format:08X}"));
        return Err(PlatformError::Unsupported {
            what: format!(
                "a ScreenCaptureKit frame in pixel format '{shown}' (BGRA was asked for)"
            ),
        });
    }
    let (Ok(w), Ok(h)) = (u32::try_from(width), u32::try_from(height)) else {
        return Err(too_small());
    };
    let Some((w, h)) = even_size(w, h) else {
        return Ok(None);
    };
    let row = (w as usize).checked_mul(4).ok_or_else(too_small)?;
    let needed = (h as usize - 1)
        .checked_mul(stride)
        .and_then(|b| b.checked_add(row));
    if stride < row || needed.is_none_or(|n| n > len) {
        return Err(too_small());
    }
    Ok(Some((w, h)))
}

fn too_small() -> PlatformError {
    PlatformError::InvalidState {
        what: "ScreenCaptureKit frame",
        state: "its rows don't fit its buffer",
    }
}

/// What the backend does about the Screen Recording permission before it lists or
/// captures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccessStep {
    /// The permission is granted: go ahead.
    Proceed,
    /// Not granted, and this backend hasn't asked yet: ask once
    /// (`CGRequestScreenCaptureAccess`), then report the permission error.
    Ask,
    /// Not granted, and this backend asked already: report the permission error without
    /// asking again.
    Refuse,
}

/// The step for a permission that `CGPreflightScreenCaptureAccess` reports as `granted`,
/// when this backend has `asked` before.
pub(crate) fn access_step(granted: bool, asked: bool) -> AccessStep {
    match (granted, asked) {
        (true, _) => AccessStep::Proceed,
        (false, false) => AccessStep::Ask,
        (false, true) => AccessStep::Refuse,
    }
}

/// The error for a Screen Recording permission that isn't granted. `asked_now` says
/// whether macOS was just asked to show its prompt.
pub(crate) fn screen_recording_denied(asked_now: bool) -> PlatformError {
    let state = if asked_now {
        "the Screen Recording permission is not granted, and macOS was asked to prompt for it"
    } else {
        "the Screen Recording permission is not granted"
    };
    PlatformError::Os {
        call: "CGPreflightScreenCaptureAccess",
        code: None,
        message: format!(
            "{state}. Allow JStream, or the terminal app it runs from, in System Settings > \
             Privacy & Security > Screen & System Audio Recording (Screen Recording on \
             macOS 14), then restart JStream"
        ),
    }
}

/// `SCStreamErrorUserDeclined`: the user or the system refused the capture, which is
/// how a missing Screen Recording permission shows.
pub(crate) const SC_USER_DECLINED: isize = -3801;
/// `SCStreamErrorNoCaptureSource`: the display or window is gone.
pub(crate) const SC_NO_CAPTURE_SOURCE: isize = -3815;
/// `SCStreamErrorUserStopped`: the user stopped sharing from the menu bar.
pub(crate) const SC_USER_STOPPED: isize = -3817;
/// `SCStreamErrorSystemStoppedStream`: macOS stopped the stream.
pub(crate) const SC_SYSTEM_STOPPED: isize = -3821;

/// An error ScreenCaptureKit returned from `call`: `code` in its own error domain when
/// `screencapturekit` is true, and `description` as it reports it.
pub(crate) fn sck_error(
    call: &'static str,
    screencapturekit: bool,
    code: isize,
    description: &str,
) -> PlatformError {
    if screencapturekit && code == SC_USER_DECLINED {
        return screen_recording_denied(false);
    }
    PlatformError::Os {
        call,
        code: i32::try_from(code).ok().map(i32::cast_unsigned),
        message: description.to_owned(),
    }
}

/// Why a running stream stopped, from the error its delegate got.
pub(crate) fn stop_reason(screencapturekit: bool, code: isize, description: &str) -> String {
    match (screencapturekit, code) {
        (true, SC_USER_STOPPED) => "screen sharing was stopped from the menu bar".into(),
        (true, SC_SYSTEM_STOPPED) => format!("macOS stopped the capture: {description}"),
        (true, SC_USER_DECLINED) => {
            "the Screen Recording permission was withdrawn or not confirmed".into()
        }
        (true, SC_NO_CAPTURE_SOURCE) => "the display or window is gone".into(),
        _ => description.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use jstream_platform::{CaptureTarget, DeviceId, PlatformError};

    use super::{
        AccessStep, FrameUse, SC_NO_CAPTURE_SOURCE, SC_SYSTEM_STOPPED, SC_USER_DECLINED,
        SC_USER_STOPPED, Wanted, access_step, bgra_frame_size, display_id, display_name, frame_use,
        listable_window, pixel_size, resolve, sck_error, screen_recording_denied, stop_reason,
        window_id, window_name,
    };
    use crate::fourcc::{CV_420V, CV_BGRA};

    #[test]
    fn ids_round_trip() {
        let d = display_id(69_733_382);
        assert_eq!(d.as_str(), "display:69733382");
        assert_eq!(
            resolve(&CaptureTarget::Display(d)),
            Ok(Wanted::Display(69_733_382))
        );
        let w = window_id(4242);
        assert_eq!(w.as_str(), "window:4242");
        assert_eq!(resolve(&CaptureTarget::Window(w)), Ok(Wanted::Window(4242)));
    }

    #[test]
    fn other_ids_are_not_found() {
        for bad in [
            "",
            "display:",
            "display:0",
            "display:-1",
            "display:+1",
            "display:1a",
            "display:99999999999",
            "window:1",
            "monitor:1",
        ] {
            assert!(
                matches!(
                    resolve(&CaptureTarget::Display(DeviceId::new(bad))),
                    Err(PlatformError::NotFound {
                        kind: "display",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
        assert!(matches!(
            resolve(&CaptureTarget::Window(display_id(1))),
            Err(PlatformError::NotFound { kind: "window", .. })
        ));
    }

    #[test]
    fn names() {
        assert_eq!(display_name(2), "Display 2");
        assert_eq!(
            window_name(Some(" Notes "), Some("Notes app")).as_deref(),
            Some("Notes")
        );
        assert_eq!(
            window_name(Some(""), Some("Finder")).as_deref(),
            Some("Finder")
        );
        assert_eq!(window_name(None, Some("  ")), None);
        assert_eq!(window_name(None, None), None);
    }

    #[test]
    fn only_normal_windows_of_other_apps_are_listed() {
        assert!(listable_window(0, false, 800.0, 600.0));
        assert!(!listable_window(25, false, 800.0, 600.0), "the menu bar");
        assert!(!listable_window(0, true, 800.0, 600.0), "JStream's own");
        assert!(!listable_window(0, false, 1.0, 600.0), "a sliver");
    }

    #[test]
    fn sizes_are_points_times_scale() {
        let s = pixel_size(1512.0, 982.0, 2.0).expect("a size");
        assert_eq!((s.width(), s.height()), (3024, 1964));
        let s = pixel_size(100.4, 50.6, 1.0).expect("a size");
        assert_eq!((s.width(), s.height()), (100, 51));
        assert_eq!(pixel_size(0.0, 100.0, 2.0), None);
        assert_eq!(pixel_size(100.0, 100.0, 0.0), None);
        assert_eq!(pixel_size(f64::NAN, 100.0, 1.0), None);
        assert_eq!(pixel_size(10_000.0, 100.0, 2.0), None, "over 16384 pixels");
    }

    #[test]
    fn frame_statuses() {
        assert_eq!(frame_use(None), FrameUse::Deliver);
        assert_eq!(frame_use(Some(0)), FrameUse::Deliver);
        for skipped in [1, 2, 3, 4, 99] {
            assert_eq!(frame_use(Some(skipped)), FrameUse::Skip, "{skipped}");
        }
        assert_eq!(frame_use(Some(5)), FrameUse::Stopped);
    }

    #[test]
    fn frame_buffers_are_checked() {
        // 1920 x 1080 BGRA with padded rows.
        assert_eq!(
            bgra_frame_size(CV_BGRA, 1920, 1080, 7680 + 64, (7680 + 64) * 1080),
            Ok(Some((1920, 1080)))
        );
        // Odd sizes lose their last column and row; the last row needs no padding.
        assert_eq!(
            bgra_frame_size(CV_BGRA, 101, 51, 404, 404 * 51),
            Ok(Some((100, 50)))
        );
        assert_eq!(bgra_frame_size(CV_BGRA, 1, 600, 4, 2400), Ok(None));
        assert!(matches!(
            bgra_frame_size(CV_420V, 1920, 1080, 1920, 1920 * 1620),
            Err(PlatformError::Unsupported { what }) if what.contains("'420v'")
        ));
        assert!(matches!(
            bgra_frame_size(CV_BGRA, 100, 100, 399, 1 << 20),
            Err(PlatformError::InvalidState { .. })
        ));
        assert!(matches!(
            bgra_frame_size(CV_BGRA, 100, 100, 400, 400 * 99),
            Err(PlatformError::InvalidState { .. })
        ));
        assert!(matches!(
            bgra_frame_size(CV_BGRA, 100, 100, usize::MAX, usize::MAX),
            Err(PlatformError::InvalidState { .. })
        ));
    }

    #[test]
    fn the_permission_is_asked_for_once() {
        assert_eq!(access_step(true, false), AccessStep::Proceed);
        assert_eq!(access_step(true, true), AccessStep::Proceed);
        assert_eq!(access_step(false, false), AccessStep::Ask);
        assert_eq!(access_step(false, true), AccessStep::Refuse);
    }

    #[test]
    fn a_missing_permission_names_the_settings_pane() {
        for asked in [false, true] {
            let PlatformError::Os { message, .. } = screen_recording_denied(asked) else {
                panic!("an OS error");
            };
            assert!(message.contains("Screen Recording permission is not granted"));
            assert!(message.contains("Privacy & Security > Screen & System Audio Recording"));
            assert!(message.contains("restart JStream"));
            assert_eq!(message.contains("asked to prompt"), asked);
        }
    }

    #[test]
    fn screencapturekit_errors_map() {
        assert_eq!(
            sck_error("SCStream startCapture", true, SC_USER_DECLINED, "declined"),
            screen_recording_denied(false)
        );
        assert_eq!(
            sck_error("SCStream startCapture", true, -3802, "failed to start"),
            PlatformError::Os {
                call: "SCStream startCapture",
                code: Some(0xFFFF_F126),
                message: "failed to start".into(),
            }
        );
        // The same code in another domain is not a permission error.
        assert!(matches!(
            sck_error("x", false, SC_USER_DECLINED, "other"),
            PlatformError::Os { code: Some(_), .. }
        ));
        assert!(matches!(
            sck_error("x", false, isize::MAX, "huge"),
            PlatformError::Os { code: None, .. }
        ));
        assert!(stop_reason(true, SC_USER_STOPPED, "x").contains("menu bar"));
        assert!(stop_reason(true, SC_SYSTEM_STOPPED, "asleep").ends_with("asleep"));
        assert!(stop_reason(true, SC_USER_DECLINED, "x").contains("Screen Recording"));
        assert!(stop_reason(true, SC_NO_CAPTURE_SOURCE, "x").contains("gone"));
        assert_eq!(stop_reason(false, SC_USER_STOPPED, "why"), "why");
    }
}
