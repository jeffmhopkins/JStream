//! Secrets in Windows Credential Manager (R-SEC-001): stream keys and OAuth refresh
//! tokens stay in the OS credential store, never in collection files, settings or logs.
//!
//! `WindowsCredentials` stores, reads and deletes generic credentials of the current
//! user. Each one is named by a [`CredentialTarget`], `JStream/stream-key/<output id>` or
//! `JStream/oauth-token/<account>`, and a value read back is a [`jstream_platform::Secret`], which zeroes its bytes when
//! dropped and never prints them. It implements [`jstream_platform::StreamKeyStore`], the
//! seam the engine reads stream keys through, and [`jstream_platform::OAuthTokenStore`],
//! the one the YouTube client keeps refresh tokens in. Nothing here logs, and no error carries a
//! secret.
//!
//! The target names and size checks are platform-independent and tested everywhere; the
//! store exists only on Windows.
//!
//! Docs: docs/ARCHITECTURE.md#persistence

use std::fmt;
#[cfg(any(windows, test))]
use std::sync::atomic::{Ordering, compiler_fence};

use jstream_platform::PlatformError;

/// The prefix of every credential JStream writes. Credential Manager's generic
/// credentials are one flat, per-user namespace shared by every application, so the
/// product name keeps ours apart from other apps' and groups them for a user browsing
/// Credential Manager.
const TARGET_PREFIX: &str = "JStream/";

/// The kind segment of a stream key's target. A kind per secret type (OAuth tokens get
/// their own) keeps two kinds of secret for one output from ever sharing a name.
const STREAM_KEY_KIND: &str = "stream-key/";

/// The kind segment of an OAuth refresh token's target, apart from stream keys.
const OAUTH_TOKEN_KIND: &str = "oauth-token/";

/// The longest generic target name, in UTF-16 code units, without its terminator:
/// `CRED_MAX_GENERIC_TARGET_NAME_LENGTH`. Copied so the checks build and are tested on
/// every platform; a Windows test checks it against the SDK value.
const MAX_TARGET_UNITS: usize = 32_767;

/// The largest credential blob, in bytes: `CRED_MAX_CREDENTIAL_BLOB_SIZE` (5 × 512).
/// Copied for the same reason as [`MAX_TARGET_UNITS`].
#[cfg(any(windows, test))]
const MAX_SECRET_BYTES: usize = 2_560;

/// The name of one credential in the store, validated: `JStream/<kind>/<id>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CredentialTarget(String);

impl CredentialTarget {
    /// The target that holds the stream key of the output with this ID:
    /// `JStream/stream-key/<output id>`.
    ///
    /// Output IDs are opaque strings, so any ID is taken except one that is empty, holds
    /// a control character (a NUL would cut the name short, so two IDs could name one
    /// credential; others can't be shown in Credential Manager), or makes the name longer
    /// than Windows allows. Those are `Unsupported`.
    pub fn stream_key(output_id: &str) -> Result<Self, PlatformError> {
        Self::new(STREAM_KEY_KIND, output_id)
    }

    /// The target that holds the OAuth refresh token of this account:
    /// `JStream/oauth-token/<account>`. Account IDs are checked as output IDs are.
    pub fn oauth_token(account: &str) -> Result<Self, PlatformError> {
        Self::new(OAUTH_TOKEN_KIND, account)
    }

    /// `JStream/<kind><id>`, after checking `id` and the length of the whole name.
    fn new(kind: &str, id: &str) -> Result<Self, PlatformError> {
        let refuse = |why: &str| PlatformError::Unsupported {
            what: format!("credential target for ID {id:?}: {why}"),
        };
        if id.is_empty() {
            return Err(refuse("the ID is empty"));
        }
        if id.chars().any(char::is_control) {
            return Err(refuse("the ID holds a control character"));
        }
        let name = format!("{TARGET_PREFIX}{kind}{id}");
        if name.encode_utf16().count() > MAX_TARGET_UNITS {
            return Err(refuse("the name is longer than Windows allows"));
        }
        Ok(Self(name))
    }

    /// The full target name, as Credential Manager shows it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name as a NUL-terminated UTF-16 string, for the `Cred*W` calls.
    #[cfg(any(windows, test))]
    fn to_wide(&self) -> Vec<u16> {
        self.0.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

impl fmt::Display for CredentialTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Overwrites `bytes` with zeros in a way the compiler can't remove as a dead store,
/// since the memory is about to be freed.
#[cfg(any(windows, test))]
fn wipe(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a live, aligned, exclusive reference to one `u8`, so writing
        // through it is sound; volatile only stops the write from being optimised away.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// Checks that a secret fits a credential blob: not empty (an output without a key has
/// no credential, see `WindowsCredentials::delete`) and at most
/// `CRED_MAX_CREDENTIAL_BLOB_SIZE` bytes. Returns its length for `CREDENTIALW`. The
/// error names the limit, never the secret or its length.
#[cfg(any(windows, test))]
fn blob_size(secret: &[u8]) -> Result<u32, PlatformError> {
    if secret.is_empty() {
        return Err(PlatformError::Unsupported {
            what: "an empty secret".into(),
        });
    }
    match u32::try_from(secret.len()) {
        Ok(n) if secret.len() <= MAX_SECRET_BYTES => Ok(n),
        _ => Err(PlatformError::Unsupported {
            what: format!("a secret longer than {MAX_SECRET_BYTES} bytes"),
        }),
    }
}

#[cfg(windows)]
pub use store::WindowsCredentials;

#[cfg(windows)]
mod store {
    use jstream_platform::{OAuthTokenStore, PlatformError, Secret, StreamKeyStore};
    use windows::Win32::Foundation::ERROR_NOT_FOUND;
    use windows::Win32::Security::Credentials::{
        CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW,
        CredFree, CredReadW, CredWriteW,
    };
    use windows::core::{PCWSTR, PWSTR};

    use super::{CredentialTarget, blob_size, wipe};
    use crate::com::os_error;

    /// The current user's generic credentials in Windows Credential Manager.
    ///
    /// Each is `CRED_TYPE_GENERIC` and persisted `CRED_PERSIST_LOCAL_MACHINE`: it
    /// survives logoff and reboot, for this user on this machine only. It doesn't roam
    /// with a roaming profile (`CRED_PERSIST_ENTERPRISE`), so a key typed into a portable
    /// install stays on the machine it was typed on. Windows encrypts it for the user;
    /// any process running as that user can read it, which is the store's own model.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct WindowsCredentials;

    impl WindowsCredentials {
        /// Creates or replaces the credential `target` with `secret`.
        ///
        /// `Unsupported` for an empty secret or one larger than
        /// `CRED_MAX_CREDENTIAL_BLOB_SIZE` (2560 bytes).
        pub fn set(&self, target: &CredentialTarget, secret: &[u8]) -> Result<(), PlatformError> {
            let size = blob_size(secret)?;
            let mut name = target.to_wide();
            let credential = CREDENTIALW {
                Flags: CRED_FLAGS(0),
                Type: CRED_TYPE_GENERIC,
                TargetName: PWSTR(name.as_mut_ptr()),
                CredentialBlobSize: size,
                // CredWriteW only reads the blob; the field is `*mut` in the C struct.
                CredentialBlob: secret.as_ptr().cast_mut(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                ..CREDENTIALW::default()
            };
            // SAFETY: `credential` points at `name`, a NUL-terminated UTF-16 string, and
            // at `secret`, `size` bytes long; both outlive the call, which reads them and
            // keeps no pointer. Every other pointer field is null, which CredWriteW
            // accepts for a generic credential.
            unsafe { CredWriteW(&raw const credential, 0) }.map_err(|e| os_error("CredWriteW", &e))
        }

        /// The secret stored under `target`, or `None` when there is none.
        ///
        /// `Unsupported` when the stored bytes aren't UTF-8 (not one JStream wrote).
        pub fn get(&self, target: &CredentialTarget) -> Result<Option<Secret>, PlatformError> {
            let Some(read) = Read::new(target)? else {
                return Ok(None);
            };
            let blob = read.blob();
            // Exactly `blob.len()` bytes, so the Vec never reallocates and leaves no
            // unzeroed copy behind.
            let mut bytes = Vec::with_capacity(blob.len());
            bytes.extend_from_slice(blob);
            Secret::from_utf8(bytes).map(Some)
        }

        /// Whether a credential is stored under `target`. Reads it to find out; the copy
        /// Windows returns is zeroed and freed before this returns.
        pub fn exists(&self, target: &CredentialTarget) -> Result<bool, PlatformError> {
            Ok(Read::new(target)?.is_some())
        }

        /// Deletes the credential `target`. Returns whether there was one; deleting a
        /// credential that isn't there is not an error.
        pub fn delete(&self, target: &CredentialTarget) -> Result<bool, PlatformError> {
            let name = target.to_wide();
            // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call.
            match unsafe { CredDeleteW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None) } {
                Ok(()) => Ok(true),
                Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(false),
                Err(e) => Err(os_error("CredDeleteW", &e)),
            }
        }
    }

    /// Stream keys, one credential per output: `JStream/stream-key/<output id>`. An
    /// output ID the target name can't hold, or an empty key, is `Unsupported`.
    impl StreamKeyStore for WindowsCredentials {
        fn set(&self, output_id: &str, key: &Secret) -> Result<(), PlatformError> {
            Self::set(
                self,
                &CredentialTarget::stream_key(output_id)?,
                key.expose().as_bytes(),
            )
        }

        fn get(&self, output_id: &str) -> Result<Option<Secret>, PlatformError> {
            Self::get(self, &CredentialTarget::stream_key(output_id)?)
        }

        fn clear(&self, output_id: &str) -> Result<(), PlatformError> {
            Self::delete(self, &CredentialTarget::stream_key(output_id)?).map(drop)
        }

        fn is_set(&self, output_id: &str) -> Result<bool, PlatformError> {
            self.exists(&CredentialTarget::stream_key(output_id)?)
        }
    }

    /// OAuth refresh tokens, one credential per account: `JStream/oauth-token/<account>`.
    /// An account ID the target name can't hold, or an empty token, is `Unsupported`.
    impl OAuthTokenStore for WindowsCredentials {
        fn set(&self, account: &str, token: &Secret) -> Result<(), PlatformError> {
            Self::set(
                self,
                &CredentialTarget::oauth_token(account)?,
                token.expose().as_bytes(),
            )
        }

        fn get(&self, account: &str) -> Result<Option<Secret>, PlatformError> {
            Self::get(self, &CredentialTarget::oauth_token(account)?)
        }

        fn clear(&self, account: &str) -> Result<(), PlatformError> {
            Self::delete(self, &CredentialTarget::oauth_token(account)?).map(drop)
        }

        fn is_set(&self, account: &str) -> Result<bool, PlatformError> {
            self.exists(&CredentialTarget::oauth_token(account)?)
        }
    }

    /// A credential CredReadW returned (R-UNSAFE-003): its blob is zeroed and the whole
    /// allocation is given back with CredFree on drop.
    struct Read(*mut CREDENTIALW);

    impl Read {
        /// Reads `target`; `None` when there is no such credential.
        fn new(target: &CredentialTarget) -> Result<Option<Self>, PlatformError> {
            let name = target.to_wide();
            let mut raw: *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call, and
            // `raw` is a valid out pointer. On success Windows stores one allocation in
            // it, which `Read` owns from here and frees exactly once.
            match unsafe { CredReadW(PCWSTR(name.as_ptr()), CRED_TYPE_GENERIC, None, &raw mut raw) }
            {
                Ok(()) if raw.is_null() => Err(PlatformError::InvalidState {
                    what: "CredReadW",
                    state: "succeeded without returning a credential",
                }),
                Ok(()) => Ok(Some(Self(raw))),
                Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(None),
                Err(e) => Err(os_error("CredReadW", &e)),
            }
        }

        /// The credential's blob: the secret, still in Windows' allocation.
        fn blob(&self) -> &[u8] {
            // SAFETY: `self.0` is the non-null credential CredReadW returned, alive until
            // drop. Its blob pointer is either null (an empty blob) or points at
            // `CredentialBlobSize` initialised bytes inside the same allocation, which
            // nothing else touches while `self` is borrowed.
            unsafe {
                let c = &*self.0;
                match c.CredentialBlobSize {
                    0 => &[],
                    _ if c.CredentialBlob.is_null() => &[],
                    n => std::slice::from_raw_parts(c.CredentialBlob, n as usize),
                }
            }
        }
    }

    impl Drop for Read {
        fn drop(&mut self) {
            // SAFETY: as in `blob`, and `&mut self` makes this the only access: the blob
            // is `CredentialBlobSize` writable bytes of the allocation CredReadW handed
            // over. It is zeroed and then freed once, with CredFree as CredReadW requires;
            // nothing reads `self.0` afterwards.
            unsafe {
                let c = &mut *self.0;
                if c.CredentialBlobSize != 0 && !c.CredentialBlob.is_null() {
                    wipe(std::slice::from_raw_parts_mut(
                        c.CredentialBlob,
                        c.CredentialBlobSize as usize,
                    ));
                }
                CredFree(self.0.cast_const().cast());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CredentialTarget, MAX_SECRET_BYTES, MAX_TARGET_UNITS, blob_size, wipe};
    use jstream_platform::PlatformError;

    #[test]
    fn stream_key_targets_are_prefixed() {
        let t = CredentialTarget::stream_key("out_yt").expect("a plain ID is valid");
        assert_eq!(t.as_str(), "JStream/stream-key/out_yt");
        assert_eq!(t.to_string(), "JStream/stream-key/out_yt");
        // Opaque IDs keep their characters, slashes and non-ASCII included.
        let t = CredentialTarget::stream_key("a/b é").expect("printable IDs are valid");
        assert_eq!(t.as_str(), "JStream/stream-key/a/b é");
    }

    #[test]
    fn oauth_token_targets_are_their_own_kind() {
        let t = CredentialTarget::oauth_token("user@example.com").expect("valid");
        assert_eq!(t.as_str(), "JStream/oauth-token/user@example.com");
        assert_ne!(
            CredentialTarget::oauth_token("same").ok(),
            CredentialTarget::stream_key("same").ok()
        );
        for id in ["", "a\0b", "tab\there"] {
            assert!(
                CredentialTarget::oauth_token(id).is_err(),
                "{id:?} must be refused"
            );
        }
    }

    #[test]
    fn wide_names_are_nul_terminated() {
        let t = CredentialTarget::stream_key("é").expect("valid");
        let wide = t.to_wide();
        assert_eq!(wide.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&wide[..wide.len() - 1]).ok().as_deref(),
            Some(t.as_str())
        );
    }

    #[test]
    fn rejects_bad_output_ids() {
        for id in ["", "a\0b", "line\nbreak", "\u{7f}"] {
            assert!(
                matches!(
                    CredentialTarget::stream_key(id),
                    Err(PlatformError::Unsupported { .. })
                ),
                "{id:?} must be refused"
            );
        }
    }

    #[test]
    fn target_length_is_capped_in_utf16_units() {
        let prefix = "JStream/stream-key/".len();
        let fits = "a".repeat(MAX_TARGET_UNITS - prefix);
        assert!(CredentialTarget::stream_key(&fits).is_ok());
        let over = "a".repeat(MAX_TARGET_UNITS - prefix + 1);
        assert!(CredentialTarget::stream_key(&over).is_err());
        // '𝄞' is one char but two UTF-16 units: half as many fit.
        let wide = "𝄞".repeat((MAX_TARGET_UNITS - prefix) / 2 + 1);
        assert!(CredentialTarget::stream_key(&wide).is_err());
    }

    #[test]
    fn secrets_must_fit_a_blob() {
        assert_eq!(blob_size(b"k").ok(), Some(1));
        assert_eq!(blob_size(&[7; MAX_SECRET_BYTES]).ok(), Some(2560));
        assert!(blob_size(&[]).is_err());
        let err = blob_size(&[7; MAX_SECRET_BYTES + 1]).expect_err("one byte over the limit");
        // The error names the limit, not the secret or its length.
        assert_eq!(
            err.to_string(),
            "not supported by this backend: a secret longer than 2560 bytes"
        );
    }

    #[test]
    fn wipe_zeroes_every_byte() {
        let mut bytes = *b"live_abc123";
        wipe(&mut bytes);
        assert_eq!(bytes, [0; 11]);
    }

    /// The limits copied for other platforms match the SDK's.
    #[cfg(windows)]
    #[test]
    fn limits_match_the_sdk() {
        use windows::Win32::Security::Credentials::{
            CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_MAX_GENERIC_TARGET_NAME_LENGTH,
        };
        assert_eq!(MAX_SECRET_BYTES, CRED_MAX_CREDENTIAL_BLOB_SIZE as usize);
        assert_eq!(
            MAX_TARGET_UNITS,
            CRED_MAX_GENERIC_TARGET_NAME_LENGTH as usize
        );
    }

    /// Writes, reads, replaces and deletes a uniquely named credential in the real store.
    /// The guard deletes it even when an assertion fails.
    #[cfg(windows)]
    #[test]
    fn round_trips_through_credential_manager() {
        use super::WindowsCredentials;

        struct Cleanup(CredentialTarget);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = WindowsCredentials.delete(&self.0);
            }
        }

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let id = format!("test-{}-{nanos}", std::process::id());
        let target = CredentialTarget::stream_key(&id).expect("valid");
        let guard = Cleanup(target.clone());
        let store = WindowsCredentials;

        assert!(!store.exists(&target).expect("exists"));
        assert!(store.get(&target).expect("get").is_none());
        assert!(!store.delete(&target).expect("delete of nothing"));

        store.set(&target, b"first").expect("set");
        assert!(store.exists(&target).expect("exists"));
        let s = store.get(&target).expect("get").expect("stored");
        assert_eq!(s.expose(), "first");

        let long = "a".repeat(MAX_SECRET_BYTES);
        store
            .set(&target, long.as_bytes())
            .expect("replace with the largest blob");
        let s = store.get(&target).expect("get").expect("stored");
        assert_eq!(s.expose(), long);
        assert!(store.set(&target, &[0; MAX_SECRET_BYTES + 1]).is_err());
        assert!(store.set(&target, &[]).is_err());

        assert!(store.delete(&target).expect("delete"));
        assert!(!store.exists(&target).expect("exists"));
        assert!(store.get(&target).expect("get").is_none());

        // Not UTF-8, so not a key JStream wrote: refused without naming it.
        store.set(&target, &[0xFF, 0xFE]).expect("set raw bytes");
        assert!(matches!(
            store.get(&target),
            Err(PlatformError::Unsupported { .. })
        ));
        drop(guard);
    }

    /// The same credential through the engine's `StreamKeyStore` seam.
    #[cfg(windows)]
    #[test]
    fn serves_as_the_stream_key_store() {
        use super::WindowsCredentials;
        use jstream_platform::{Secret, StreamKeyStore};

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let id = format!("test-store-{}-{nanos}", std::process::id());
        let store: &dyn StreamKeyStore = &WindowsCredentials;
        struct Cleanup<'a>(&'a dyn StreamKeyStore, &'a str);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = self.0.clear(self.1);
            }
        }
        let _guard = Cleanup(store, &id);

        assert!(!store.is_set(&id).expect("is_set"));
        store.set(&id, &Secret::new("live_abc")).expect("set");
        assert!(store.is_set(&id).expect("is_set"));
        let key = store.get(&id).expect("get").expect("stored");
        assert_eq!(key.expose(), "live_abc");
        store.clear(&id).expect("clear");
        store.clear(&id).expect("clearing nothing succeeds");
        assert!(!store.is_set(&id).expect("is_set"));
        assert!(store.set("", &Secret::new("k")).is_err());
    }

    /// A refresh token through the `OAuthTokenStore` seam, in its own namespace: a stream
    /// key for the same ID is left alone.
    #[cfg(windows)]
    #[test]
    fn serves_as_the_oauth_token_store() {
        use super::WindowsCredentials;
        use jstream_platform::{OAuthTokenStore, Secret, StreamKeyStore};

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let id = format!("test-oauth-{}-{nanos}", std::process::id());
        let tokens: &dyn OAuthTokenStore = &WindowsCredentials;
        let keys: &dyn StreamKeyStore = &WindowsCredentials;
        struct Cleanup<'a>(&'a dyn OAuthTokenStore, &'a dyn StreamKeyStore, &'a str);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = self.0.clear(self.2);
                let _ = self.1.clear(self.2);
            }
        }
        let _guard = Cleanup(tokens, keys, &id);

        assert!(!tokens.is_set(&id).expect("is_set"));
        tokens.set(&id, &Secret::new("1//refresh")).expect("set");
        keys.set(&id, &Secret::new("live_key")).expect("set key");
        assert!(tokens.is_set(&id).expect("is_set"));
        let token = tokens.get(&id).expect("get").expect("stored");
        assert_eq!(token.expose(), "1//refresh");
        tokens.clear(&id).expect("clear");
        tokens.clear(&id).expect("clearing nothing succeeds");
        assert!(!tokens.is_set(&id).expect("is_set"));
        assert!(keys.is_set(&id).expect("the stream key stays"));
        assert!(tokens.set("", &Secret::new("t")).is_err());
    }
}
