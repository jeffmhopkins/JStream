//! The stream-key store: the seam to the OS credential store (R-SEC-001,
//! docs/RULES.md#R-SEC-001).
//!
//! Stream keys live only there, never in the collection, a log, an event or an error. The
//! engine is handed a [`StreamKeyStore`] by the host: a platform crate's (Windows
//! Credential Manager first), or [`fake::MemoryKeyStore`](crate::fake::MemoryKeyStore)
//! in tests.

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

/// A secret, such as a stream key, on its way to or from the credential store.
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
}
