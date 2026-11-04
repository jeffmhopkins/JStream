//! Where the pointer is over a capture's target, for
//! [`CaptureStream::pointer`](jstream_platform::CaptureStream::pointer): the cursor's
//! position and the display's or window's bounds, read in physical pixels and scaled to
//! the newest frame. Windows.Graphics.Capture doesn't send the pointer apart from the
//! frames, so it is read from the OS when asked. Each read is a few calls that return at
//! once, with no waits (R-RT-002).

use std::ffi::c_void;

use jstream_platform::PointerPosition;
use jstream_platform::media::FrameSize;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

/// What a capture shows, by its handle's value: handles are plain numbers, never
/// dereferenced, which keeps the stream `Send`.
#[derive(Clone, Copy, Debug)]
pub(super) enum Target {
    Monitor(isize),
    Window(isize),
}

impl Target {
    pub(super) fn monitor(handle: HMONITOR) -> Self {
        Self::Monitor(handle.0 as isize)
    }

    pub(super) fn window(handle: HWND) -> Self {
        Self::Window(handle.0 as isize)
    }

    /// Where the pointer is over a frame of `frame`, or `None` when the OS won't say
    /// (a secure desktop, a target that has gone).
    pub(super) fn pointer(self, frame: FrameSize) -> Option<PointerPosition> {
        // Physical pixels for the cursor and the bounds alike, whatever the process's
        // own awareness: the frames are in physical pixels.
        let _physical = PhysicalPixels::enter();
        let mut at = POINT::default();
        // SAFETY: `at` is a POINT the call fills.
        unsafe { GetCursorPos(&raw mut at) }.ok()?;
        let bounds = self.bounds()?;
        PointerPosition::in_frame(
            (f64::from(at.x), f64::from(at.y)),
            (f64::from(bounds.left), f64::from(bounds.top)),
            (
                f64::from(bounds.right) - f64::from(bounds.left),
                f64::from(bounds.bottom) - f64::from(bounds.top),
            ),
            frame,
        )
    }

    /// The target's bounds on the virtual screen: a display's whole area, or a window's
    /// extended frame bounds, the area its capture shows.
    fn bounds(self) -> Option<RECT> {
        match self {
            Self::Monitor(h) => {
                let mut info = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                let handle = HMONITOR(std::ptr::without_provenance_mut::<c_void>(h as usize));
                // SAFETY: `info` is a MONITORINFO whose cbSize says so; a handle that has
                // gone makes the call fail.
                unsafe { GetMonitorInfoW(handle, &raw mut info) }
                    .as_bool()
                    .then_some(info.rcMonitor)
            }
            Self::Window(h) => {
                let mut r = RECT::default();
                let handle = HWND(std::ptr::without_provenance_mut::<c_void>(h as usize));
                // SAFETY: `r` is a RECT of the size passed, which the call fills; a window
                // that has gone makes it fail.
                unsafe {
                    DwmGetWindowAttribute(
                        handle,
                        DWMWA_EXTENDED_FRAME_BOUNDS,
                        (&raw mut r).cast::<c_void>(),
                        size_of::<RECT>() as u32,
                    )
                }
                .ok()
                .map(|()| r)
            }
        }
    }
}

/// The calling thread made per-monitor DPI aware (v2) until this drops, when its earlier
/// awareness comes back: the render thread reads the pointer, and nothing else on it
/// changes.
struct PhysicalPixels(DPI_AWARENESS_CONTEXT);

impl PhysicalPixels {
    fn enter() -> Self {
        // SAFETY: plain call; it returns the thread's earlier context, or null for a
        // context it doesn't accept, which `drop` then leaves alone.
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for PhysicalPixels {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            // SAFETY: plain call, with the context the thread had.
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}
