//! Shared values (ADR-0102 decision 7, docs/adr/0102-page-api-for-browser-sources.md):
//! the named JSON values `values.set` sets and `values.list` reads. The engine holds them
//! in memory, beside the studio state: never saved, never `rev`, never an undo step.

use std::collections::BTreeMap;

use jstream_protocol::{
    MAX_VALUES, ProtocolError, SharedValue, check_value_name, check_value_size, prefix_covers,
};
use serde_json::Value;

/// Every shared value, by name.
#[derive(Debug, Default)]
pub(crate) struct SharedValues {
    values: BTreeMap<String, Value>,
}

impl SharedValues {
    /// Sets `name` to `value`, or removes it for `None`. Returns whether anything
    /// changed: setting the value a name already has, or removing a name that has none,
    /// changes nothing.
    ///
    /// # Errors
    /// `invalid_argument` at `name` for a name outside the rules or a new name when the
    /// store is full, and at `value` for a value over the size limit. Nothing changes.
    pub(crate) fn set(&mut self, name: &str, value: Option<Value>) -> Result<bool, ProtocolError> {
        check_value_name(name, "name")?;
        let Some(value) = value else {
            return Ok(self.values.remove(name).is_some());
        };
        check_value_size(&value, "value")?;
        if let Some(old) = self.values.get_mut(name) {
            if *old == value {
                return Ok(false);
            }
            *old = value;
            return Ok(true);
        }
        if self.values.len() >= MAX_VALUES {
            return Err(ProtocolError::invalid_argument(
                "name",
                "the engine holds at most 256 shared values; remove one first",
            ));
        }
        self.values.insert(name.to_owned(), value);
        Ok(true)
    }

    /// The values `prefix` covers ([`prefix_covers`]), ordered by name.
    pub(crate) fn list(&self, prefix: Option<&str>) -> Vec<SharedValue> {
        self.values
            .iter()
            .filter(|(name, _)| prefix_covers(prefix, name))
            .map(|(name, value)| SharedValue {
                name: name.clone(),
                value: value.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn set_reports_only_real_changes() {
        let mut values = SharedValues::default();
        assert_eq!(values.set("a.b", Some(json!(1))), Ok(true));
        assert_eq!(values.set("a.b", Some(json!(1))), Ok(false));
        assert_eq!(values.set("a.b", Some(json!(2))), Ok(true));
        assert_eq!(values.set("a.b", None), Ok(true));
        assert_eq!(values.set("a.b", None), Ok(false));
        assert!(values.list(None).is_empty());
    }

    #[test]
    fn a_refused_set_changes_nothing() {
        let mut values = SharedValues::default();
        values.set("a", Some(json!("x"))).unwrap();
        assert!(values.set("A", Some(json!(1))).is_err());
        let big = json!("x".repeat(jstream_protocol::MAX_VALUE_BYTES));
        let error = values.set("a", Some(big)).unwrap_err();
        assert!(matches!(&error, ProtocolError::InvalidArgument { field, .. } if field == "value"));
        assert_eq!(values.list(None)[0].value, json!("x"));
    }

    #[test]
    fn the_store_holds_at_most_256_names() {
        let mut values = SharedValues::default();
        for n in 0..MAX_VALUES {
            values.set(&format!("v{n}"), Some(json!(n))).unwrap();
        }
        let error = values.set("one_more", Some(json!(0))).unwrap_err();
        assert!(matches!(&error, ProtocolError::InvalidArgument { field, .. } if field == "name"));
        // A name it holds can still change, and a removal makes room.
        assert_eq!(values.set("v0", Some(json!("new"))), Ok(true));
        values.set("v0", None).unwrap();
        assert_eq!(values.set("one_more", Some(json!(0))), Ok(true));
    }

    #[test]
    fn list_is_by_name_and_follows_the_prefix() {
        let mut values = SharedValues::default();
        for name in ["status.text", "counter.a", "status", "statusbar"] {
            values.set(name, Some(json!(name))).unwrap();
        }
        let names =
            |prefix| -> Vec<String> { values.list(prefix).into_iter().map(|v| v.name).collect() };
        assert_eq!(
            names(None),
            ["counter.a", "status", "status.text", "statusbar"]
        );
        assert_eq!(names(Some("status")), ["status", "status.text"]);
    }
}
