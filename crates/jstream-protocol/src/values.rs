//! Shared values (0.78,
//! [ADR-0102](../../../docs/adr/0102-page-api-for-browser-sources.md) decision 7): named
//! JSON values a client sets with `values.set` and reads with `values.list`, which
//! browser source pages granted them will read through `window.jstream` (ADR-0102 part
//! 2). They are held by the engine, never saved, never `rev` and never an undo step, so
//! they are not collection state. They are not ADR-0032's live values, which are a
//! gesture's uncommitted edits.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ProtocolError;

/// The longest name a shared value takes, in bytes.
pub const MAX_VALUE_NAME_BYTES: usize = 64;

/// The most bytes one shared value takes, serialized as compact JSON.
pub const MAX_VALUE_BYTES: usize = 16 * 1024;

/// The most names the store holds at once.
pub const MAX_VALUES: usize = 256;

/// One shared value, as `values.list` answers it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SharedValue {
    /// Its name: dotted segments of `a`–`z`, `0`–`9` and `_`, such as `status.text`.
    pub name: String,
    /// Its value: any JSON value but `null`, which `values.set` takes as a removal.
    pub value: serde_json::Value,
}

/// Checks a shared value's name: one or more segments of `a`–`z`, `0`–`9` and `_`,
/// joined by `.`, at most [`MAX_VALUE_NAME_BYTES`]. `field` names it in the error.
///
/// # Errors
/// [`ProtocolError::InvalidArgument`] at `field` when the name breaks a rule.
pub fn check_value_name(name: &str, field: &str) -> Result<(), ProtocolError> {
    let refuse = |reason: &str| Err(ProtocolError::invalid_argument(field, reason));
    if name.is_empty() {
        return refuse("a value's name can't be empty");
    }
    if name.len() > MAX_VALUE_NAME_BYTES {
        return refuse("a value's name is at most 64 bytes");
    }
    for segment in name.split('.') {
        if segment.is_empty() {
            return refuse("a value's name is segments joined by single dots, none empty");
        }
        if !segment
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return refuse("a value's name uses only a-z, 0-9, `_` and `.`");
        }
    }
    Ok(())
}

/// Checks a value a `values.set` carries: at most [`MAX_VALUE_BYTES`] as compact JSON.
/// `field` names it in the error.
///
/// # Errors
/// [`ProtocolError::InvalidArgument`] at `field` when it is larger.
pub fn check_value_size(value: &serde_json::Value, field: &str) -> Result<(), ProtocolError> {
    // Counted without building the text: serde_json writes into a sink that only counts.
    let mut counter = Counter(0);
    // Writing a `Value` can't fail but for the sink, which never does.
    let _ = serde_json::to_writer(&mut counter, value);
    if counter.0 > MAX_VALUE_BYTES {
        return Err(ProtocolError::invalid_argument(
            field,
            format!(
                "a value is at most 16 KiB as JSON; this one is {} bytes",
                counter.0
            ),
        ));
    }
    Ok(())
}

/// Whether a `values.list` `prefix` covers `name`: every name when there is none, and
/// otherwise the name itself and every name below it (`a` covers `a` and `a.b`, not
/// `ab`).
pub fn prefix_covers(prefix: Option<&str>, name: &str) -> bool {
    match prefix {
        None => true,
        Some(prefix) => {
            name == prefix
                || name
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
        }
    }
}

struct Counter(usize);

impl std::io::Write for Counter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_follow_the_rules() {
        for good in ["a", "status.text", "counter_1.a2", "x.y.z"] {
            assert!(check_value_name(good, "name").is_ok(), "{good}");
        }
        for bad in ["", "A", "a..b", ".a", "a.", "a-b", "a b", "é"] {
            assert!(check_value_name(bad, "name").is_err(), "{bad:?}");
        }
        assert!(check_value_name(&"a".repeat(64), "name").is_ok());
        assert!(check_value_name(&"a".repeat(65), "name").is_err());
    }

    #[test]
    fn values_are_at_most_16_kib() {
        // `"` + text + `"`.
        let fits = json!("x".repeat(MAX_VALUE_BYTES - 2));
        assert!(check_value_size(&fits, "value").is_ok());
        let over = json!("x".repeat(MAX_VALUE_BYTES - 1));
        let error = check_value_size(&over, "value").unwrap_err();
        assert_eq!(error.code(), "invalid_argument");
    }

    #[test]
    fn a_prefix_covers_its_own_name_and_below() {
        assert!(prefix_covers(None, "a.b"));
        assert!(prefix_covers(Some("a"), "a"));
        assert!(prefix_covers(Some("a"), "a.b"));
        assert!(!prefix_covers(Some("a"), "ab"));
        assert!(!prefix_covers(Some("a.b"), "a"));
    }
}
