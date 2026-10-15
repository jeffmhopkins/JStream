//! Reading strings the plugin owns, which may be null, unterminated within reason, or
//! not UTF-8.

use std::ffi::{CStr, c_char};

/// The longest string read from a plugin's pointer, bytes. Names and ids are far
/// shorter; this only bounds a broken plugin.
const MAX_LEN: usize = 4096;

/// The string at `ptr`, lossily decoded, or `None` for a null pointer.
///
/// # Safety
/// `ptr` is null or points to a NUL-terminated string that stays valid for this call.
pub(crate) unsafe fn from_ptr(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: non-null, and the caller guarantees a NUL-terminated string.
    let s = unsafe { CStr::from_ptr(ptr) };
    let bytes = s.to_bytes();
    Some(String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_LEN)]).into_owned())
}

/// A fixed-size `char` array the plugin filled, up to its first NUL (or all of it).
pub(crate) fn from_array(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_null_and_strings() {
        // SAFETY: null is allowed.
        assert_eq!(unsafe { from_ptr(std::ptr::null()) }, None);
        // SAFETY: a literal is NUL-terminated and static.
        assert_eq!(unsafe { from_ptr(c"gain".as_ptr()) }, Some("gain".into()));
    }

    #[test]
    fn arrays_stop_at_nul_or_their_end() {
        let a = [b'a' as c_char, b'b' as c_char, 0, b'c' as c_char];
        assert_eq!(from_array(&a), "ab");
        let full = [b'x' as c_char; 3];
        assert_eq!(from_array(&full), "xxx");
    }
}
