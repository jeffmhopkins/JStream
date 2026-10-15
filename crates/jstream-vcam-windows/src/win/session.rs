//! What the trust rule ([`crate::trust`]) needs to know about a session: whether it is
//! active, and its logged-on user's SID.
//!
//! Frame Server hosts the DLL as LOCAL SERVICE, which lacks `SeTcbPrivilege`, so
//! `WTSQueryUserToken` is out of reach. Instead the session's user and domain names come
//! from `WTSQuerySessionInformationW`, which LOCAL SERVICE may call for local sessions,
//! and `LookupAccountNameW` turns them into a SID; the logged-on user's account is known
//! to the local LSA, so this needs no network access of the caller's own. Anything that
//! fails reads as "inactive" or "no user", so the block is refused.

use windows::Win32::Security::{LookupAccountNameW, PSID, SID_NAME_USE, SidTypeUser};
use windows::Win32::System::RemoteDesktop::{
    WTS_CONNECTSTATE_CLASS, WTS_CURRENT_SERVER_HANDLE, WTS_INFO_CLASS, WTSActive, WTSConnectState,
    WTSDomainName, WTSFreeMemory, WTSQuerySessionInformationW, WTSUserName,
};
use windows_core::{HSTRING, PWSTR};

use super::shared::sid_string;

/// A buffer `WTSQuerySessionInformationW` allocated, freed on drop.
struct WtsBuffer {
    ptr: PWSTR,
    len: u32,
}

impl WtsBuffer {
    fn query(session: u32, class: WTS_INFO_CLASS) -> Option<Self> {
        let mut ptr = PWSTR::null();
        let mut len = 0u32;
        // SAFETY: valid out-pointers; on success the buffer is ours to free.
        unsafe {
            WTSQuerySessionInformationW(
                Some(WTS_CURRENT_SERVER_HANDLE),
                session,
                class,
                &mut ptr,
                &mut len,
            )
        }
        .ok()?;
        let buf = Self { ptr, len };
        (!buf.ptr.is_null()).then_some(buf)
    }

    /// The buffer as a string (empty if it isn't one).
    fn text(&self) -> String {
        // SAFETY: string classes return a NUL-terminated wide string, valid until drop.
        unsafe { self.ptr.to_string() }.unwrap_or_default()
    }
}

impl Drop for WtsBuffer {
    fn drop(&mut self) {
        // SAFETY: the buffer came from WTSQuerySessionInformationW and is freed once.
        unsafe { WTSFreeMemory(self.ptr.0.cast()) };
    }
}

/// Whether `session` is active: connected to the console, or an active remote session.
pub(crate) fn is_active(session: u32) -> bool {
    let Some(buf) = WtsBuffer::query(session, WTSConnectState) else {
        return false;
    };
    if (buf.len as usize) < std::mem::size_of::<WTS_CONNECTSTATE_CLASS>() {
        return false;
    }
    // SAFETY: WTSConnectState returns a WTS_CONNECTSTATE_CLASS (`len` checked above) in a
    // buffer the system allocated suitably aligned; read once before drop.
    let state = unsafe { buf.ptr.0.cast::<WTS_CONNECTSTATE_CLASS>().read() };
    state == WTSActive
}

/// The SID of the user logged on to `session`, as a string, if there is one.
pub(crate) fn user_sid(session: u32) -> Option<String> {
    let user = WtsBuffer::query(session, WTSUserName)?.text();
    if user.is_empty() {
        return None;
    }
    let domain = WtsBuffer::query(session, WTSDomainName)
        .map(|b| b.text())
        .unwrap_or_default();
    let account = if domain.is_empty() {
        user
    } else {
        format!("{domain}\\{user}")
    };

    // SECURITY_MAX_SID_SIZE is 68 bytes; u64 elements keep the SID buffer aligned.
    let mut sid = [0u64; 9];
    let mut sid_len = std::mem::size_of_val(&sid) as u32;
    let mut domain_buf = [0u16; 256];
    let mut domain_len = domain_buf.len() as u32;
    let mut kind = SID_NAME_USE::default();
    // SAFETY: the buffers are writable for the sizes passed, and the lengths and `kind`
    // are valid out-pointers.
    unsafe {
        LookupAccountNameW(
            None,
            &HSTRING::from(account),
            Some(PSID(sid.as_mut_ptr().cast())),
            &mut sid_len,
            Some(PWSTR(domain_buf.as_mut_ptr())),
            &mut domain_len,
            &mut kind,
        )
    }
    .ok()?;
    // A name that resolves to a group or alias is not a logged-on user.
    if kind != SidTypeUser {
        return None;
    }
    // SAFETY: on success `sid` holds a valid SID, alive for this call.
    unsafe { sid_string(PSID(sid.as_mut_ptr().cast())) }.ok()
}
