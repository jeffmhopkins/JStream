//! The stream-key and OAuth-token stores: the seams to the OS credential store
//! (R-SEC-001, docs/RULES.md#R-SEC-001).
//!
//! Stream keys and OAuth refresh tokens live only there, never in the collection, a log,
//! an event or an error. The host hands the engine a [`StreamKeyStore`] and the YouTube
//! client an [`OAuthTokenStore`]: a platform crate's (Windows Credential Manager first),
//! or [`fake::MemoryKeyStore`](crate::fake::MemoryKeyStore) and
//! [`fake::MemoryTokenStore`](crate::fake::MemoryTokenStore) in tests.

use std::fmt;
use std::sync::atomic::{Ordering, compiler_fence};

use crate::PlatformError;

/// Stores one stream key per output, keyed by the output's ID.
///
/// Implementations talk to the OS credential store, so every call may block briefly; the
/// engine calls them on its control thread, never on a real-time one and never while it
/// holds its collection lock. **No error may contain a key**: a [`PlatformError`] from a
/// store becomes an `action_failed` reason and a log line.
pub trait StreamKeyStore: Send + Sync {
    /// Stores `key` for `output_id`, replacing any key it had.
    ///
    /// # Errors
    /// The store refused or failed.
    fn set(&self, output_id: &str, key: &Secret) -> Result<(), PlatformError>;

    /// The key stored for `output_id`, or `None`.
    ///
    /// # Errors
    /// The store failed.
    fn get(&self, output_id: &str) -> Result<Option<Secret>, PlatformError>;

    /// Removes the key stored for `output_id`. Removing a key that isn't there succeeds.
    ///
    /// # Errors
    /// The store refused or failed.
    fn clear(&self, output_id: &str) -> Result<(), PlatformError>;

    /// Whether a key is stored for `output_id`. By default, [`get`](Self::get) and drop
    /// the key; a store that can check without reading the secret should do so.
    ///
    /// # Errors
    /// The store failed.
    fn is_set(&self, output_id: &str) -> Result<bool, PlatformError> {
        Ok(self.get(output_id)?.is_some())
    }
}

/// Stores one OAuth refresh token per account, keyed by an account ID the caller chooses
/// (for YouTube, the Google account the user signed in with).
///
/// A separate seam from [`StreamKeyStore`], so a token and a stream key never share a
/// name even when an account ID equals an output ID. Only the long-lived refresh token is
/// stored; access tokens are short-lived and stay in the caller's memory. The same rules
/// as [`StreamKeyStore`] hold: calls may block briefly, so they run off real-time
/// threads, and **no error may contain a token**.
pub trait OAuthTokenStore: Send + Sync {
    /// Stores `token` for `account`, replacing any token it had.
    ///
    /// # Errors
    /// The store refused or failed.
    fn set(&self, account: &str, token: &Secret) -> Result<(), PlatformError>;

    /// The token stored for `account`, or `None`.
    ///
    /// # Errors
    /// The store failed.
    fn get(&self, account: &str) -> Result<Option<Secret>, PlatformError>;

    /// Removes the token stored for `account`, as on sign-out or when the provider
    /// revoked it. Removing a token that isn't there succeeds.
    ///
    /// # Errors
    /// The store refused or failed.
    fn clear(&self, account: &str) -> Result<(), PlatformError>;

    /// Whether a token is stored for `account`. By default, [`get`](Self::get) and drop
    /// the token; a store that can check without reading the secret should do so.
    ///
    /// # Errors
    /// The store failed.
    fn is_set(&self, account: &str) -> Result<bool, PlatformError> {
        Ok(self.get(account)?.is_some())
    }
}

/// A secret, such as a stream key or an OAuth refresh token, on its way to or from the credential store.
///
/// It never prints: its `Debug` is `"***"` and it has no `Display`. The only way to read
/// it is [`expose`](Self::expose). It isn't `Clone`, so the only copies are the ones a
/// caller makes from `expose`, and its bytes are overwritten with zeros when it is
/// dropped. That is best effort: a copy a caller made, or one left behind when the
/// `String` given to [`new`](Self::new) grew, is not reached.
pub struct Secret(String);

impl Secret {
    /// Wraps `secret`.
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// Wraps a secret a store returned as bytes. Takes ownership so nothing is copied.
    ///
    /// # Errors
    /// `Unsupported` when the bytes aren't UTF-8; the bytes are zeroed first, and the
    /// error names neither them nor their length.
    pub fn from_utf8(bytes: Vec<u8>) -> Result<Self, PlatformError> {
        String::from_utf8(bytes).map(Self).map_err(|err| {
            wipe(err.into_bytes());
            PlatformError::Unsupported {
                what: "a stored secret that isn't UTF-8".into(),
            }
        })
    }

    /// The secret itself. Never log it or put it in an error.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(\"***\")")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(std::mem::take(&mut self.0).into_bytes());
    }
}

/// Overwrites `bytes` with zeros before they are freed. `black_box` and the fence keep
/// the compiler from dropping the writes as dead stores.
fn wipe(mut bytes: Vec<u8>) {
    bytes.iter_mut().for_each(|b| *b = 0);
    std::hint::black_box(&bytes);
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::Secret;

    #[test]
    fn a_secret_never_prints() {
        let secret = Secret::new("abcd-1234");
        let debug = format!("{secret:?} {:?}", Some(&secret));
        assert!(!debug.contains("abcd"), "{debug}");
        assert_eq!(secret.expose(), "abcd-1234");
    }

    #[test]
    fn bytes_from_a_store_must_be_utf8_and_are_never_named() {
        let secret = Secret::from_utf8(b"live_abc".to_vec()).expect("UTF-8");
        assert_eq!(secret.expose(), "live_abc");
        let err = Secret::from_utf8(vec![b'k', 0xFF, b'y'])
            .expect_err("not UTF-8")
            .to_string();
        assert_eq!(
            err,
            "not supported by this backend: a stored secret that isn't UTF-8"
        );
    }

    #[test]
    fn the_memory_store_keeps_one_key_per_output_and_fails_without_naming_it() {
        use super::StreamKeyStore;
        use crate::fake::MemoryKeyStore;

        let store = MemoryKeyStore::new();
        let shared = store.clone();
        assert_eq!(store.is_set("out_1").ok(), Some(false));
        store.set("out_1", &Secret::new("first")).expect("stores");
        store
            .set("out_1", &Secret::new("second"))
            .expect("replaces");
        assert_eq!(
            shared
                .get("out_1")
                .ok()
                .flatten()
                .map(|k| k.expose().to_owned()),
            Some("second".to_owned())
        );
        assert_eq!(shared.is_set("out_2").ok(), Some(false));
        assert_eq!(shared.len(), 1);

        store.set_failing(true);
        let err = store
            .set("out_1", &Secret::new("third"))
            .expect_err("fails")
            .to_string();
        assert!(!err.contains("third") && !err.contains("second"), "{err}");
        store.set_failing(false);

        store.clear("out_1").expect("clears");
        store.clear("out_1").expect("clearing nothing succeeds");
        assert!(shared.is_empty());
    }

    #[test]
    fn the_memory_token_store_keeps_one_token_per_account_apart_from_stream_keys() {
        use super::{OAuthTokenStore, StreamKeyStore};
        use crate::fake::{MemoryKeyStore, MemoryTokenStore};

        let tokens = MemoryTokenStore::new();
        let keys = MemoryKeyStore::new();
        let shared = tokens.clone();
        assert_eq!(tokens.is_set("acct").ok(), Some(false));
        tokens
            .set("acct", &Secret::new("1//first"))
            .expect("stores");
        tokens
            .set("acct", &Secret::new("1//second"))
            .expect("replaces");
        keys.set("acct", &Secret::new("live_key")).expect("stores");
        assert_eq!(
            shared
                .get("acct")
                .ok()
                .flatten()
                .map(|t| t.expose().to_owned()),
            Some("1//second".to_owned())
        );
        assert_eq!(shared.is_set("other").ok(), Some(false));
        assert_eq!(shared.len(), 1);

        tokens.clear("acct").expect("clears");
        tokens.clear("acct").expect("clearing nothing succeeds");
        assert!(shared.is_empty());
        // Clearing the token left the stream key with the same name alone.
        assert_eq!(keys.is_set("acct").ok(), Some(true));
    }

    /// R-SEC-001: a refresh token never shows in the store's `Debug`, in what `get`
    /// returns, or in an error, whichever call fails.
    #[test]
    fn oauth_tokens_are_redacted() {
        use super::OAuthTokenStore;
        use crate::fake::MemoryTokenStore;

        let token = "1//0gRefreshTokenValue";
        let store = MemoryTokenStore::new();
        store.set("acct", &Secret::new(token)).expect("stores");
        let read = store.get("acct").expect("reads");
        let debug = format!("{store:?} {read:?}");
        assert!(!debug.contains("RefreshToken"), "{debug}");

        store.set_failing(true);
        let errors = [
            store.set("acct", &Secret::new(token)).err(),
            store.get("acct").err(),
            store.is_set("acct").err(),
            store.clear("acct").err(),
        ];
        for err in errors {
            let err = err.expect("every call fails");
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains("RefreshToken"), "{shown}");
        }
    }
}
