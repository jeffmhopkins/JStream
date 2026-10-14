//! Small RAII wrappers over Win32 and COM (R-UNSAFE-003), and the mapping of Windows
//! errors into [`PlatformError`].

use std::marker::PhantomData;

use jstream_platform::PlatformError;
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, RPC_E_CHANGED_MODE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Com::{
    CO_MTA_USAGE_COOKIE, COINIT_MULTITHREADED, CoDecrementMTAUsage, CoIncrementMTAUsage,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, SetEvent,
    WaitForMultipleObjects,
};
use windows::core::{PWSTR, w};

/// Maps a Windows error into [`PlatformError::Os`], naming the call.
pub(crate) fn os_error(call: &'static str, e: &windows::core::Error) -> PlatformError {
    PlatformError::Os {
        call,
        // The HRESULT's bits, as the error type documents (`0x8889_0004`, not negative).
        code: Some(e.code().0.cast_unsigned()),
        message: e.message(),
    }
}

/// `.os("Call")` on a Windows result: maps its error with [`os_error`].
pub(crate) trait OsResult<T> {
    fn os(self, call: &'static str) -> Result<T, PlatformError>;
}

impl<T> OsResult<T> for windows::core::Result<T> {
    fn os(self, call: &'static str) -> Result<T, PlatformError> {
        self.map_err(|e| os_error(call, &e))
    }
}

/// COM initialised (multithreaded) on this thread for as long as the guard lives.
pub(crate) struct Com {
    /// Whether this guard's `CoInitializeEx` succeeded and must be balanced.
    balance: bool,
    /// Not `Send`: the balancing call must run on the initialising thread.
    _thread: PhantomData<*const ()>,
}

impl Com {
    /// Joins the multithreaded apartment. A thread that is already single-threaded keeps
    /// its apartment: COM is usable there too, and the guard then balances nothing.
    pub(crate) fn init() -> Result<Self, PlatformError> {
        // SAFETY: plain call; a success (S_OK or S_FALSE) is balanced by CoUninitialize in
        // Drop, on this thread, because the guard is not Send.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == RPC_E_CHANGED_MODE {
            return Ok(Self {
                balance: false,
                _thread: PhantomData,
            });
        }
        hr.ok().os("CoInitializeEx")?;
        Ok(Self {
            balance: true,
            _thread: PhantomData,
        })
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.balance {
            // SAFETY: balances this guard's successful CoInitializeEx, on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// The process's multithreaded apartment kept alive for as long as the guard lives.
///
/// While the MTA exists, a thread that never initialised COM is an implicit MTA member,
/// so it may use free-threaded COM and WinRT objects. A WinRT capture is started, used
/// and dropped from such threads.
pub(crate) struct MtaUsage(CO_MTA_USAGE_COOKIE);

impl MtaUsage {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        // SAFETY: plain call; balanced by CoDecrementMTAUsage in Drop.
        let cookie = unsafe { CoIncrementMTAUsage() }.os("CoIncrementMTAUsage")?;
        Ok(Self(cookie))
    }
}

// SAFETY: the cookie is an opaque token, and CoDecrementMTAUsage may be called from any
// thread.
unsafe impl Send for MtaUsage {}
// SAFETY: `&MtaUsage` exposes nothing.
unsafe impl Sync for MtaUsage {}

impl Drop for MtaUsage {
    fn drop(&mut self) {
        // SAFETY: balances the successful CoIncrementMTAUsage in `new`, exactly once.
        let _ = unsafe { CoDecrementMTAUsage(self.0) };
    }
}

/// An unnamed auto-reset Win32 event, closed on drop.
pub(crate) struct Event(HANDLE);

impl Event {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        // SAFETY: plain call creating an unnamed, unsignalled auto-reset event.
        let h = unsafe { CreateEventW(None, false, false, None) }.os("CreateEventW")?;
        Ok(Self(h))
    }

    pub(crate) fn handle(&self) -> HANDLE {
        self.0
    }

    /// Signals the event. Failing to signal a live event can't happen, so the result is
    /// ignored.
    pub(crate) fn set(&self) {
        // SAFETY: the handle is a live event owned by `self`.
        let _ = unsafe { SetEvent(self.0) };
    }
}

// SAFETY: an event handle names a kernel object; signalling, waiting on and closing it
// are thread-safe, and `Event` owns the handle exclusively.
unsafe impl Send for Event {}
// SAFETY: as above; `&Event` only signals the event or exposes the handle for waiting.
unsafe impl Sync for Event {}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateEventW and is closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Which of the handles passed to [`wait_any`] was signalled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Woke {
    /// The handle at this index (the lowest one, if several were signalled).
    Index(usize),
    /// None within the timeout.
    Timeout,
}

/// Waits until one of `events` is signalled or `timeout_ms` passes.
pub(crate) fn wait_any(events: &[&Event], timeout_ms: u32) -> Result<Woke, PlatformError> {
    let mut handles = [HANDLE::default(); 4];
    let n = events.len().min(handles.len());
    for (slot, e) in handles.iter_mut().zip(events) {
        *slot = e.handle();
    }
    // SAFETY: the handles are live events borrowed for the duration of the call.
    let r = unsafe { WaitForMultipleObjects(&handles[..n], false, timeout_ms) };
    if r == WAIT_TIMEOUT {
        return Ok(Woke::Timeout);
    }
    let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
    if i < n {
        Ok(Woke::Index(i))
    } else {
        Err(PlatformError::Os {
            call: "WaitForMultipleObjects",
            code: Some(r.0),
            message: windows::core::Error::from_thread().message(),
        })
    }
}

/// This thread registered with MMCSS as "Pro Audio" (docs/ARCHITECTURE.md#threading)
/// until dropped. Registration is best effort: a thread that MMCSS refuses still runs.
pub(crate) struct Mmcss(Option<HANDLE>);

impl Mmcss {
    pub(crate) fn pro_audio() -> Self {
        let mut index = 0u32;
        // SAFETY: `index` is a valid out-pointer; the task name is a static string.
        let h = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut index) };
        Self(h.ok())
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            // SAFETY: `h` came from AvSetMmThreadCharacteristicsW on this thread, and the
            // guard is not Send.
            let _ = unsafe { AvRevertMmThreadCharacteristics(h) };
        }
    }
}

/// Takes ownership of a COM-allocated wide string: copies it out and frees it.
pub(crate) fn take_pwstr(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: `p` is a valid, null-terminated string allocated by COM.
    let s = unsafe { p.to_string() }.unwrap_or_default();
    // SAFETY: the string was allocated with CoTaskMemAlloc and is freed exactly once.
    unsafe { CoTaskMemFree(Some(p.0 as *const _)) };
    s
}
