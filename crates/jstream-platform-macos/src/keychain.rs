//! Secrets in the macOS Keychain (ADR-0063 §9, R-SEC-001): stream keys and OAuth refresh
//! tokens stay in the user's login keychain, never in collection files, settings or logs.
//!
//! `MacosKeychain` stores, reads and deletes generic-password items through the Security
//! framework's `SecItem` calls. Each item is named by a [`KeychainItem`]: the service
//! `JStream/stream-key` or `JStream/oauth-token`, and the output or account ID as its
//! account. A value read back is a [`jstream_platform::Secret`], which zeroes its bytes
//! when dropped and never prints them. It implements [`jstream_platform::StreamKeyStore`]
//! and [`jstream_platform::OAuthTokenStore`], as `WindowsCredentials` does on Windows.
//! Nothing here logs, and no error carries a secret.
//!
//! The item names, the secret checks and the mapping of `errSec*` codes to errors are
//! platform independent and tested everywhere; the store exists only on macOS.
//!
//! Docs: docs/ARCHITECTURE.md#persistence

use jstream_platform::PlatformError;

#[cfg(any(target_os = "macos", test))]
use crate::fourcc::os_status;

/// The service of every stream-key item. Generic passwords are found by service and
/// account, so a service per kind of secret keeps a stream key and a token for the same
/// ID apart, and the product name keeps JStream's items apart from other apps' in
/// Keychain Access.
const STREAM_KEY_SERVICE: &str = "JStream/stream-key";

/// The service of every OAuth refresh-token item, apart from stream keys.
const OAUTH_TOKEN_SERVICE: &str = "JStream/oauth-token";

/// `errSecSuccess`. The `errSec*` codes are copied from `SecBase.h` so the mapping builds
/// and is tested on every platform; a macOS test checks them against the framework's.
#[cfg(any(target_os = "macos", test))]
const ERR_SEC_SUCCESS: i32 = 0;
/// `errSecUserCanceled`: the user dismissed macOS's prompt to allow access.
#[cfg(any(target_os = "macos", test))]
const ERR_SEC_USER_CANCELED: i32 = -128;
/// `errSecAuthFailed`: the user denied access, or the password they gave was wrong.
#[cfg(any(target_os = "macos", test))]
const ERR_SEC_AUTH_FAILED: i32 = -25_293;
/// `errSecNoSuchKeychain`: there is no login keychain.
#[cfg(any(target_os = "macos", test))]
const ERR_SEC_NO_SUCH_KEYCHAIN: i32 = -25_294;
/// `errSecDuplicateItem`: an add found the item already there.
#[cfg(any(target_os = "macos", test))]
const ERR_SEC_DUPLICATE_ITEM: i32 = -25_299;
/// `errSecItemNotFound`.
#[cfg(any(target_os = "macos", test))]
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25_300;
/// `errSecInteractionNotAllowed`: the keychain is locked and macOS could not ask the user
/// to unlock it, as in a session without a login window.
#[cfg(any(target_os = "macos", test))]
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25_308;

/// One generic-password item in the keychain, validated: its service names the kind of
/// secret and its account the output or account ID.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeychainItem {
    service: &'static str,
    account: String,
}

impl KeychainItem {
    /// The item that holds the stream key of the output with this ID: service
    /// `JStream/stream-key`, account `<output id>`.
    ///
    /// Output IDs are opaque strings, so any ID is taken except an empty one or one that
    /// holds a control character (Keychain Access can't show it, and a NUL would end the
    /// name early for C callers). Those are `Unsupported`.
    pub fn stream_key(output_id: &str) -> Result<Self, PlatformError> {
        Self::new(STREAM_KEY_SERVICE, output_id)
    }

    /// The item that holds the OAuth refresh token of this account: service
    /// `JStream/oauth-token`, account `<account>`. Account IDs are checked as output IDs
    /// are.
    pub fn oauth_token(account: &str) -> Result<Self, PlatformError> {
        Self::new(OAUTH_TOKEN_SERVICE, account)
    }

    fn new(service: &'static str, id: &str) -> Result<Self, PlatformError> {
        let refuse = |why: &str| PlatformError::Unsupported {
            what: format!("keychain item for ID {id:?}: {why}"),
        };
        if id.is_empty() {
            return Err(refuse("the ID is empty"));
        }
        if id.chars().any(char::is_control) {
            return Err(refuse("the ID holds a control character"));
        }
        Ok(Self {
            service,
            account: id.to_owned(),
        })
    }

    /// The item's service (`kSecAttrService`).
    pub fn service(&self) -> &str {
        self.service
    }

    /// The item's account (`kSecAttrAccount`): the output or account ID.
    pub fn account(&self) -> &str {
        &self.account
    }

    /// The name Keychain Access shows for the item (`kSecAttrLabel`):
    /// `JStream stream key (<id>)` or `JStream OAuth token (<id>)`.
    pub fn label(&self) -> String {
        let kind = if self.service == STREAM_KEY_SERVICE {
            "stream key"
        } else {
            "OAuth token"
        };
        format!("JStream {kind} ({})", self.account)
    }
}

/// Checks that a secret can be stored: not empty (an output without a key has no item,
/// see `MacosKeychain::delete`). The error never names the secret.
#[cfg(any(target_os = "macos", test))]
fn check_secret(secret: &[u8]) -> Result<(), PlatformError> {
    if secret.is_empty() {
        return Err(PlatformError::Unsupported {
            what: "an empty secret".into(),
        });
    }
    Ok(())
}

/// What a `SecItem` call's status means for the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
enum Outcome {
    /// `errSecSuccess`.
    Done,
    /// `errSecItemNotFound`.
    NotFound,
    /// `errSecDuplicateItem`.
    Duplicate,
    /// Any other status: an error.
    Failed,
}

#[cfg(any(target_os = "macos", test))]
fn outcome(status: i32) -> Outcome {
    match status {
        ERR_SEC_SUCCESS => Outcome::Done,
        ERR_SEC_ITEM_NOT_FOUND => Outcome::NotFound,
        ERR_SEC_DUPLICATE_ITEM => Outcome::Duplicate,
        _ => Outcome::Failed,
    }
}

/// The error for a failed `SecItem` call. The statuses a user can cause name what to do;
/// any other uses the framework's own message (`SecCopyErrorMessageString`) when there is
/// one. Neither the secret nor the item's ID is in it.
#[cfg(any(target_os = "macos", test))]
fn keychain_error(call: &'static str, status: i32, os_message: Option<&str>) -> PlatformError {
    let what = match status {
        ERR_SEC_USER_CANCELED | ERR_SEC_AUTH_FAILED => {
            "access to JStream's keychain item was not allowed; macOS asks again after each \
             rebuild of an unsigned build, so choose Allow or Always Allow"
                .to_owned()
        }
        ERR_SEC_INTERACTION_NOT_ALLOWED => {
            "the login keychain is locked and macOS could not ask to unlock it".to_owned()
        }
        ERR_SEC_NO_SUCH_KEYCHAIN => "this user has no login keychain".to_owned(),
        _ => os_message.map_or_else(|| "the keychain call failed".to_owned(), str::to_owned),
    };
    os_status(call, status, &what)
}

#[cfg(target_os = "macos")]
pub use store::MacosKeychain;

#[cfg(target_os = "macos")]
mod store {
    use std::ptr::{self, NonNull};

    use jstream_platform::{OAuthTokenStore, PlatformError, Secret, StreamKeyStore};
    use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFRetained, CFString, CFType};
    use objc2_security::{
        SecCopyErrorMessageString, SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate,
        kSecAttrAccount, kSecAttrLabel, kSecAttrService, kSecClass, kSecClassGenericPassword,
        kSecMatchLimit, kSecMatchLimitOne, kSecReturnData, kSecUseDataProtectionKeychain,
        kSecValueData,
    };

    use super::{KeychainItem, Outcome, check_secret, keychain_error, outcome};

    /// The current user's generic-password items in their login keychain (ADR-0063 §9).
    ///
    /// The login keychain, not the data-protection keychain, which needs a signed app with
    /// a keychain access-group entitlement. macOS encrypts the items with the user's
    /// login and lets the app that wrote an item read it; after a rebuild changes an
    /// unsigned app, macOS asks the user to allow access again. Items don't sync to
    /// iCloud.
    #[derive(Clone, Copy, Debug, Default)]
    pub struct MacosKeychain;

    impl MacosKeychain {
        /// Creates or replaces the item with `secret`. `Unsupported` for an empty secret.
        ///
        /// The `CFData` the secret is copied into for the call is released, not zeroed:
        /// Core Foundation owns that buffer.
        pub fn set(&self, item: &KeychainItem, secret: &[u8]) -> Result<(), PlatformError> {
            check_secret(secret)?;
            let data = CFData::from_bytes(secret);
            let update = dictionary(&[(value_data(), &data)]);
            // SAFETY: both dictionaries are valid CFDictionaries of CFString keys and
            // CF values, alive for the call, which only reads them.
            let status = unsafe { SecItemUpdate(opaque(&query(item, &[])), opaque(&update)) };
            match outcome(status) {
                Outcome::Done => return Ok(()),
                Outcome::NotFound => {}
                Outcome::Duplicate | Outcome::Failed => return Err(error("SecItemUpdate", status)),
            }
            let label = CFString::from_str(&item.label());
            // SAFETY: Security framework constant, set before any code runs, never written.
            let label_key = unsafe { kSecAttrLabel };
            let attributes = query(item, &[(label_key, &label), (value_data(), &data)]);
            // SAFETY: `attributes` is a valid dictionary alive for the call; a null
            // result pointer asks for nothing back.
            let status = unsafe { SecItemAdd(opaque(&attributes), ptr::null_mut()) };
            match outcome(status) {
                Outcome::Done => Ok(()),
                _ => Err(error("SecItemAdd", status)),
            }
        }

        /// The secret stored in the item, or `None` when there is none.
        ///
        /// `Unsupported` when the stored bytes aren't UTF-8 (not one JStream wrote). The
        /// framework's own copy is released, not zeroed.
        pub fn get(&self, item: &KeychainItem) -> Result<Option<Secret>, PlatformError> {
            // SAFETY: Security framework and Core Foundation constants, set before any
            // code runs, never written.
            let (return_data, limit, one) =
                unsafe { (kSecReturnData, kSecMatchLimit, kSecMatchLimitOne) };
            let yes = CFBoolean::new(true);
            let search = query(item, &[(return_data, yes), (limit, one)]);
            let mut raw: *const CFType = ptr::null();
            // SAFETY: `search` is a valid dictionary alive for the call and `raw` a valid
            // out pointer. On success with one match and `kSecReturnData`, the framework
            // stores one retained CFData in it, which is owned from here.
            let status = unsafe { SecItemCopyMatching(opaque(&search), &raw mut raw) };
            match outcome(status) {
                Outcome::Done => {}
                Outcome::NotFound => return Ok(None),
                Outcome::Duplicate | Outcome::Failed => {
                    return Err(error("SecItemCopyMatching", status));
                }
            }
            let Some(raw) = NonNull::new(raw.cast_mut()) else {
                return Err(PlatformError::InvalidState {
                    what: "SecItemCopyMatching",
                    state: "succeeded without returning the item's data",
                });
            };
            // SAFETY: `raw` is the +1 reference SecItemCopyMatching handed over (the
            // "Copy" rule), released exactly once when the CFRetained drops.
            let value = unsafe { CFRetained::from_raw(raw) };
            let data = value
                .downcast::<CFData>()
                .map_err(|_| PlatformError::InvalidState {
                    what: "SecItemCopyMatching",
                    state: "returned something other than data",
                })?;
            // `to_vec` allocates exactly the data's length, so the Vec never reallocates
            // and leaves no unzeroed copy of its own behind.
            Secret::from_utf8(data.to_vec()).map(Some)
        }

        /// Whether the item exists. Asks for no data back, so the secret is not read.
        pub fn exists(&self, item: &KeychainItem) -> Result<bool, PlatformError> {
            // SAFETY: as in `get`, and a null result pointer asks for nothing back.
            let status = unsafe { SecItemCopyMatching(opaque(&query(item, &[])), ptr::null_mut()) };
            match outcome(status) {
                Outcome::Done => Ok(true),
                Outcome::NotFound => Ok(false),
                Outcome::Duplicate | Outcome::Failed => Err(error("SecItemCopyMatching", status)),
            }
        }

        /// Deletes the item. Returns whether there was one; deleting an item that isn't
        /// there is not an error.
        pub fn delete(&self, item: &KeychainItem) -> Result<bool, PlatformError> {
            // SAFETY: `query` is a valid dictionary alive for the call.
            let status = unsafe { SecItemDelete(opaque(&query(item, &[]))) };
            match outcome(status) {
                Outcome::Done => Ok(true),
                Outcome::NotFound => Ok(false),
                Outcome::Duplicate | Outcome::Failed => Err(error("SecItemDelete", status)),
            }
        }
    }

    /// Stream keys, one item per output in the service `JStream/stream-key`. An empty or
    /// control-character output ID, or an empty key, is `Unsupported`.
    impl StreamKeyStore for MacosKeychain {
        fn set(&self, output_id: &str, key: &Secret) -> Result<(), PlatformError> {
            Self::set(
                self,
                &KeychainItem::stream_key(output_id)?,
                key.expose().as_bytes(),
            )
        }

        fn get(&self, output_id: &str) -> Result<Option<Secret>, PlatformError> {
            Self::get(self, &KeychainItem::stream_key(output_id)?)
        }

        fn clear(&self, output_id: &str) -> Result<(), PlatformError> {
            Self::delete(self, &KeychainItem::stream_key(output_id)?).map(drop)
        }

        fn is_set(&self, output_id: &str) -> Result<bool, PlatformError> {
            self.exists(&KeychainItem::stream_key(output_id)?)
        }
    }

    /// OAuth refresh tokens, one item per account in the service `JStream/oauth-token`.
    /// Account IDs and tokens are checked as for stream keys.
    impl OAuthTokenStore for MacosKeychain {
        fn set(&self, account: &str, token: &Secret) -> Result<(), PlatformError> {
            Self::set(
                self,
                &KeychainItem::oauth_token(account)?,
                token.expose().as_bytes(),
            )
        }

        fn get(&self, account: &str) -> Result<Option<Secret>, PlatformError> {
            Self::get(self, &KeychainItem::oauth_token(account)?)
        }

        fn clear(&self, account: &str) -> Result<(), PlatformError> {
            Self::delete(self, &KeychainItem::oauth_token(account)?).map(drop)
        }

        fn is_set(&self, account: &str) -> Result<bool, PlatformError> {
            self.exists(&KeychainItem::oauth_token(account)?)
        }
    }

    /// `kSecValueData`.
    fn value_data() -> &'static CFString {
        // SAFETY: Security framework constant, set before any code runs, never written.
        unsafe { kSecValueData }
    }

    fn dictionary(pairs: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
        let keys: Vec<&CFString> = pairs.iter().map(|(k, _)| *k).collect();
        let values: Vec<&CFType> = pairs.iter().map(|(_, v)| *v).collect();
        CFDictionary::from_slices(&keys, &values)
    }

    /// `dictionary` as the untyped `CFDictionary` the `SecItem` calls take.
    fn opaque(dictionary: &CFDictionary<CFString, CFType>) -> &CFDictionary {
        dictionary.as_ref()
    }

    /// The query that finds `item` in the login keychain, with `extra` pairs added: class
    /// generic password, its service and account, and not the data-protection keychain.
    fn query(
        item: &KeychainItem,
        extra: &[(&CFString, &CFType)],
    ) -> CFRetained<CFDictionary<CFString, CFType>> {
        let service = CFString::from_str(item.service());
        let account = CFString::from_str(item.account());
        let no = CFBoolean::new(false);
        // SAFETY: Security framework constants, set before any code runs, never written.
        let (class, generic, service_key, account_key, data_protection) = unsafe {
            (
                kSecClass,
                kSecClassGenericPassword,
                kSecAttrService,
                kSecAttrAccount,
                kSecUseDataProtectionKeychain,
            )
        };
        let mut pairs: Vec<(&CFString, &CFType)> = vec![
            (class, generic),
            (service_key, &service),
            (account_key, &account),
            (data_protection, no),
        ];
        pairs.extend_from_slice(extra);
        dictionary(&pairs)
    }

    /// The error for `status`, with the framework's message for it.
    fn error(call: &'static str, status: i32) -> PlatformError {
        // SAFETY: any status is accepted, and the reserved argument must be null.
        let message = unsafe { SecCopyErrorMessageString(status, ptr::null_mut()) };
        keychain_error(call, status, message.map(|m| m.to_string()).as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ERR_SEC_AUTH_FAILED, ERR_SEC_DUPLICATE_ITEM, ERR_SEC_INTERACTION_NOT_ALLOWED,
        ERR_SEC_ITEM_NOT_FOUND, ERR_SEC_NO_SUCH_KEYCHAIN, ERR_SEC_SUCCESS, ERR_SEC_USER_CANCELED,
        KeychainItem, Outcome, check_secret, keychain_error, outcome,
    };
    use jstream_platform::PlatformError;

    #[test]
    fn stream_key_items_are_in_their_own_service() {
        let item = KeychainItem::stream_key("out_yt").expect("a plain ID is valid");
        assert_eq!(item.service(), "JStream/stream-key");
        assert_eq!(item.account(), "out_yt");
        assert_eq!(item.label(), "JStream stream key (out_yt)");
        // Opaque IDs keep their characters, slashes and non-ASCII included.
        let item = KeychainItem::stream_key("a/b é").expect("printable IDs are valid");
        assert_eq!(item.account(), "a/b é");
    }

    #[test]
    fn oauth_token_items_are_apart_from_stream_keys() {
        let item = KeychainItem::oauth_token("user@example.com").expect("valid");
        assert_eq!(item.service(), "JStream/oauth-token");
        assert_eq!(item.account(), "user@example.com");
        assert_eq!(item.label(), "JStream OAuth token (user@example.com)");
        assert_ne!(
            KeychainItem::oauth_token("same").ok(),
            KeychainItem::stream_key("same").ok()
        );
    }

    #[test]
    fn rejects_bad_ids() {
        for id in ["", "a\0b", "line\nbreak", "tab\there", "\u{7f}"] {
            for item in [KeychainItem::stream_key(id), KeychainItem::oauth_token(id)] {
                assert!(
                    matches!(item, Err(PlatformError::Unsupported { .. })),
                    "{id:?} must be refused"
                );
            }
        }
    }

    #[test]
    fn secrets_must_not_be_empty() {
        assert!(check_secret(b"k").is_ok());
        let err = check_secret(&[]).expect_err("empty");
        assert_eq!(
            err.to_string(),
            "not supported by this backend: an empty secret"
        );
    }

    #[test]
    fn statuses_map_to_outcomes() {
        assert_eq!(outcome(ERR_SEC_SUCCESS), Outcome::Done);
        assert_eq!(outcome(ERR_SEC_ITEM_NOT_FOUND), Outcome::NotFound);
        assert_eq!(outcome(ERR_SEC_DUPLICATE_ITEM), Outcome::Duplicate);
        for status in [ERR_SEC_AUTH_FAILED, ERR_SEC_USER_CANCELED, -1, 1] {
            assert_eq!(outcome(status), Outcome::Failed, "{status}");
        }
    }

    #[test]
    fn errors_say_what_to_do_and_never_name_the_secret() {
        let denied = keychain_error("SecItemAdd", ERR_SEC_AUTH_FAILED, Some("ignored"));
        let shown = denied.to_string();
        assert!(shown.starts_with("SecItemAdd failed"), "{shown}");
        assert!(
            shown.contains("Always Allow") && shown.contains("-25293"),
            "{shown}"
        );
        assert!(!shown.contains("ignored"), "{shown}");
        assert!(matches!(
            denied,
            PlatformError::Os { code: Some(c), .. } if c == u32::from_ne_bytes((-25_293i32).to_ne_bytes())
        ));
        let cancelled = keychain_error("SecItemCopyMatching", ERR_SEC_USER_CANCELED, None);
        assert!(cancelled.to_string().contains("Always Allow"));
        let locked = keychain_error("SecItemAdd", ERR_SEC_INTERACTION_NOT_ALLOWED, None);
        assert!(locked.to_string().contains("locked"), "{locked}");
        let none = keychain_error("SecItemAdd", ERR_SEC_NO_SUCH_KEYCHAIN, None);
        assert!(none.to_string().contains("no login keychain"), "{none}");
        // Anything else carries the framework's message, or a plain one without it.
        let other = keychain_error(
            "SecItemDelete",
            -50,
            Some("One or more parameters passed to a function were not valid."),
        );
        assert_eq!(
            other.to_string(),
            "SecItemDelete failed (0xFFFFFFCE): One or more parameters passed to a function \
             were not valid. (OSStatus -50)"
        );
        let plain = keychain_error("SecItemDelete", -50, None);
        assert!(
            plain.to_string().contains("the keychain call failed"),
            "{plain}"
        );
    }

    /// The codes copied for other platforms match the framework's.
    #[cfg(target_os = "macos")]
    #[test]
    fn codes_match_the_framework() {
        use objc2_security::{
            errSecAuthFailed, errSecDuplicateItem, errSecInteractionNotAllowed, errSecItemNotFound,
            errSecNoSuchKeychain, errSecSuccess, errSecUserCanceled,
        };
        assert_eq!(ERR_SEC_SUCCESS, errSecSuccess);
        assert_eq!(ERR_SEC_USER_CANCELED, errSecUserCanceled);
        assert_eq!(ERR_SEC_AUTH_FAILED, errSecAuthFailed);
        assert_eq!(ERR_SEC_NO_SUCH_KEYCHAIN, errSecNoSuchKeychain);
        assert_eq!(ERR_SEC_DUPLICATE_ITEM, errSecDuplicateItem);
        assert_eq!(ERR_SEC_ITEM_NOT_FOUND, errSecItemNotFound);
        assert_eq!(ERR_SEC_INTERACTION_NOT_ALLOWED, errSecInteractionNotAllowed);
    }
    /// A unique ID, so runs and parallel tests never share an item.
    #[cfg(target_os = "macos")]
    fn unique(tag: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("test-{tag}-{}-{nanos}", std::process::id())
    }

    /// Writes, reads, replaces and deletes a uniquely named item in the real login
    /// keychain. The guard deletes it even when an assertion fails.
    #[cfg(target_os = "macos")]
    #[test]
    fn round_trips_through_the_keychain() {
        use super::MacosKeychain;

        struct Cleanup(KeychainItem);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = MacosKeychain.delete(&self.0);
            }
        }

        let item = KeychainItem::stream_key(&unique("item")).expect("valid");
        let guard = Cleanup(item.clone());
        let store = MacosKeychain;

        assert!(!store.exists(&item).expect("exists"));
        assert!(store.get(&item).expect("get").is_none());
        assert!(!store.delete(&item).expect("delete of nothing"));

        store.set(&item, b"first").expect("set");
        assert!(store.exists(&item).expect("exists"));
        let s = store.get(&item).expect("get").expect("stored");
        assert_eq!(s.expose(), "first");

        store.set(&item, "second é".as_bytes()).expect("replace");
        let s = store.get(&item).expect("get").expect("stored");
        assert_eq!(s.expose(), "second é");
        assert!(store.set(&item, &[]).is_err());

        assert!(store.delete(&item).expect("delete"));
        assert!(!store.exists(&item).expect("exists"));
        assert!(store.get(&item).expect("get").is_none());

        // Not UTF-8, so not a secret JStream wrote: refused without naming it.
        store.set(&item, &[0xFF, 0xFE]).expect("set raw bytes");
        assert!(matches!(
            store.get(&item),
            Err(PlatformError::Unsupported { .. })
        ));
        drop(guard);
    }

    /// Stream keys and refresh tokens through the engine's seams, each in its own
    /// service: clearing a token leaves a stream key with the same ID alone.
    #[cfg(target_os = "macos")]
    #[test]
    fn serves_as_the_stream_key_and_oauth_token_stores() {
        use super::MacosKeychain;
        use jstream_platform::{OAuthTokenStore, Secret, StreamKeyStore};

        let id = unique("seams");
        let keys: &dyn StreamKeyStore = &MacosKeychain;
        let tokens: &dyn OAuthTokenStore = &MacosKeychain;
        struct Cleanup<'a>(&'a dyn StreamKeyStore, &'a dyn OAuthTokenStore, &'a str);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = self.0.clear(self.2);
                let _ = self.1.clear(self.2);
            }
        }
        let _guard = Cleanup(keys, tokens, &id);

        assert!(!keys.is_set(&id).expect("is_set"));
        keys.set(&id, &Secret::new("live_abc")).expect("set key");
        tokens
            .set(&id, &Secret::new("1//refresh"))
            .expect("set token");
        assert!(keys.is_set(&id).expect("is_set"));
        assert_eq!(
            keys.get(&id).expect("get").expect("stored").expose(),
            "live_abc"
        );
        assert_eq!(
            tokens.get(&id).expect("get").expect("stored").expose(),
            "1//refresh"
        );
        tokens.clear(&id).expect("clear");
        tokens.clear(&id).expect("clearing nothing succeeds");
        assert!(!tokens.is_set(&id).expect("is_set"));
        assert!(keys.is_set(&id).expect("the stream key stays"));
        keys.clear(&id).expect("clear");
        assert!(!keys.is_set(&id).expect("is_set"));
        assert!(keys.set("", &Secret::new("k")).is_err());
        assert!(tokens.set("", &Secret::new("t")).is_err());
    }
}
