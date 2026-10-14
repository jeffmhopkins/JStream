//! How capture targets are named as [`DeviceId`]s.
//!
//! - A display is `monitor:` and its GDI device name, such as `monitor:\\.\DISPLAY1`.
//!   The name is stable while the display stays connected to the same output; Windows
//!   may renumber displays when the topology changes.
//! - A window is `hwnd:` and its handle in hexadecimal, such as `hwnd:0x1a2b`. A handle
//!   is valid until the window is destroyed, and Windows may reuse it afterwards.
//!
//! Platform-independent, so it is tested on every platform.

use jstream_platform::DeviceId;

const MONITOR: &str = "monitor:";
const WINDOW: &str = "hwnd:0x";

/// The ID of the display with GDI device name `device`.
pub(crate) fn monitor_id(device: &str) -> DeviceId {
    DeviceId::new(format!("{MONITOR}{device}"))
}

/// The GDI device name in a display's ID.
pub(crate) fn parse_monitor(id: &DeviceId) -> Option<&str> {
    id.as_str()
        .strip_prefix(MONITOR)
        .filter(|name| !name.is_empty())
}

/// The ID of the window with handle `hwnd`.
pub(crate) fn window_id(hwnd: usize) -> DeviceId {
    DeviceId::new(format!("{WINDOW}{hwnd:x}"))
}

/// The window handle in a window's ID. Never zero.
pub(crate) fn parse_window(id: &DeviceId) -> Option<usize> {
    let hex = id.as_str().strip_prefix(WINDOW)?;
    usize::from_str_radix(hex, 16).ok().filter(|&h| h != 0)
}

#[cfg(test)]
mod tests {
    use jstream_platform::DeviceId;

    use super::{monitor_id, parse_monitor, parse_window, window_id};

    #[test]
    fn ids_round_trip() {
        let m = monitor_id(r"\\.\DISPLAY2");
        assert_eq!(m.as_str(), r"monitor:\\.\DISPLAY2");
        assert_eq!(parse_monitor(&m), Some(r"\\.\DISPLAY2"));
        let w = window_id(0x1A2B);
        assert_eq!(w.as_str(), "hwnd:0x1a2b");
        assert_eq!(parse_window(&w), Some(0x1A2B));
    }

    #[test]
    fn other_ids_are_refused() {
        for bad in [
            "",
            "monitor:",
            "DISPLAY1",
            "hwnd:",
            "hwnd:0x",
            "hwnd:0x0",
            "hwnd:1a2b",
        ] {
            let id = DeviceId::new(bad);
            assert_eq!(parse_monitor(&id), None, "{bad:?}");
            assert_eq!(parse_window(&id), None, "{bad:?}");
        }
        assert_eq!(parse_window(&DeviceId::new("hwnd:0xzz")), None);
        assert_eq!(parse_monitor(&window_id(5)), None);
        assert_eq!(parse_window(&monitor_id("x")), None);
    }
}
