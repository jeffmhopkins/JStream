//! Listing what WGC can capture: the displays (`EnumDisplayMonitors`) and the windows a
//! user would pick (`EnumWindows`, filtered as below).

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};

use jstream_platform::PlatformError;
use jstream_platform::media::FrameSize;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW,
    HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetWindow, GetWindowLongW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    MONITORINFOF_PRIMARY, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PCWSTR};

use crate::com::OsResult;

/// A display.
pub(crate) struct Monitor {
    pub(crate) handle: HMONITOR,
    /// The GDI device name, such as `\\.\DISPLAY1`.
    pub(crate) device: String,
    /// The current mode's size in pixels.
    pub(crate) size: Option<FrameSize>,
    pub(crate) primary: bool,
}

/// A window that can be captured.
pub(crate) struct Window {
    pub(crate) hwnd: HWND,
    pub(crate) title: String,
    /// The window's bounds as DWM draws them, which is what WGC captures.
    pub(crate) size: Option<FrameSize>,
}

/// The displays, in the order Windows enumerates them.
pub(crate) fn monitors() -> Result<Vec<Monitor>, PlatformError> {
    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: `collect_monitor` reads `dwdata` as the `Vec<HMONITOR>` passed here, which
    // outlives the call; the enumeration is synchronous, on this thread.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&raw mut handles as isize),
        )
    };
    ok.ok().os("EnumDisplayMonitors")?;
    Ok(handles.into_iter().filter_map(monitor).collect())
}

/// The `EnumDisplayMonitors` callback: appends each monitor to the list.
unsafe extern "system" fn collect_monitor(
    handle: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    list: LPARAM,
) -> BOOL {
    // Called by user32 (R-UNSAFE-005): nothing may unwind out of it. A panic stops the
    // enumeration.
    let pushed = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `list` is the `&mut Vec<HMONITOR>` that `monitors` passed, borrowed by
        // nothing else for the duration of the enumeration.
        let list = unsafe { &mut *(list.0 as *mut Vec<HMONITOR>) };
        list.push(handle);
    }));
    pushed.is_ok().into()
}

/// The monitor's name, size and flags, or `None` if it has gone since it was listed.
fn monitor(handle: HMONITOR) -> Option<Monitor> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a MONITORINFOEXW whose cbSize says so, which GetMonitorInfoW fills
    // through its MONITORINFO pointer.
    let ok = unsafe { GetMonitorInfoW(handle, (&raw mut info).cast::<MONITORINFO>()) };
    if !ok.as_bool() {
        return None;
    }
    let device = wide(&info.szDevice);
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: `info.szDevice` is null-terminated (GetMonitorInfoW filled it) and lives for
    // the call; `mode` is a DEVMODEW whose dmSize says so.
    let size = unsafe {
        EnumDisplaySettingsW(
            PCWSTR(info.szDevice.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &raw mut mode,
        )
    }
    .as_bool()
    .then(|| FrameSize::new(mode.dmPelsWidth, mode.dmPelsHeight).ok())
    .flatten();
    Some(Monitor {
        handle,
        device,
        size,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

/// The top-level windows a user would pick, in Z order: visible, titled, not owned by
/// another window, not tool windows, not cloaked (on another virtual desktop, or a
/// suspended UWP app), and not this process's own.
pub(crate) fn top_level_windows() -> Result<Vec<Window>, PlatformError> {
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: `collect_window` reads `lparam` as the `Vec<HWND>` passed here, which
    // outlives the call; the enumeration is synchronous, on this thread.
    unsafe { EnumWindows(Some(collect_window), LPARAM(&raw mut handles as isize)) }
        .os("EnumWindows")?;
    // SAFETY: plain call.
    let own = unsafe { GetCurrentProcessId() };
    Ok(handles
        .into_iter()
        .filter_map(|h| capturable(h, own))
        .collect())
}

/// The `EnumWindows` callback: appends each window to the list.
unsafe extern "system" fn collect_window(hwnd: HWND, list: LPARAM) -> BOOL {
    // Called by user32 (R-UNSAFE-005): nothing may unwind out of it. A panic stops the
    // enumeration.
    let pushed = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `list` is the `&mut Vec<HWND>` that `top_level_windows` passed,
        // borrowed by nothing else for the duration of the enumeration.
        let list = unsafe { &mut *(list.0 as *mut Vec<HWND>) };
        list.push(hwnd);
    }));
    pushed.is_ok().into()
}

/// The window, if it passes the filter of [`top_level_windows`]. A window destroyed
/// meanwhile fails one of the checks.
fn capturable(hwnd: HWND, own_pid: u32) -> Option<Window> {
    // SAFETY: each call takes a window handle, which may be stale by now; user32 and DWM
    // fail such calls safely. The out-pointers and sizes describe valid locals.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || GetWindow(hwnd, GW_OWNER).is_ok() {
            return None;
        }
        if GetWindowLongW(hwnd, GWL_EXSTYLE).cast_unsigned() & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
        if pid == own_pid {
            return None;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast::<c_void>(),
            size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return None;
        }
        let len = usize::try_from(GetWindowTextLengthW(hwnd)).ok()?;
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len + 1];
        let n = usize::try_from(GetWindowTextW(hwnd, &mut buf)).ok()?;
        let title = String::from_utf16_lossy(&buf[..n.min(len)]);
        if title.trim().is_empty() {
            return None;
        }
        let mut r = RECT::default();
        let bounds = DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut r).cast::<c_void>(),
            size_of::<RECT>() as u32,
        )
        .or_else(|_| GetWindowRect(hwnd, &raw mut r));
        let size = bounds.ok().and_then(|()| rect_size(&r));
        Some(Window { hwnd, title, size })
    }
}

/// A rectangle's size, if it isn't empty.
fn rect_size(r: &RECT) -> Option<FrameSize> {
    let w = u32::try_from(r.right.checked_sub(r.left)?).ok()?;
    let h = u32::try_from(r.bottom.checked_sub(r.top)?).ok()?;
    FrameSize::new(w, h).ok()
}

/// A null-terminated UTF-16 buffer as a string.
fn wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}
