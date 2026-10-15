//! A tiny JSON writer, so the spike needs no `serde` (as in spikes S1–S3).

/// Builds one JSON object, field by field, in insertion order.
#[derive(Default)]
pub struct Obj(Vec<String>);

impl Obj {
    pub fn new() -> Self {
        Self::default()
    }

    /// A field whose value is already JSON.
    pub fn raw(mut self, key: &str, json: impl Into<String>) -> Self {
        self.0.push(format!("{}:{}", string(key), json.into()));
        self
    }

    pub fn str(self, key: &str, v: &str) -> Self {
        self.raw(key, string(v))
    }

    pub fn opt_str(self, key: &str, v: Option<&str>) -> Self {
        self.raw(key, v.map_or_else(|| "null".into(), string))
    }

    pub fn num(self, key: &str, v: f64) -> Self {
        self.raw(key, num(v))
    }

    pub fn int(self, key: &str, v: impl Into<i128>) -> Self {
        self.raw(key, v.into().to_string())
    }

    pub fn build(self) -> String {
        format!("{{{}}}", self.0.join(","))
    }
}

/// A JSON array of already-JSON values.
pub fn array(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(","))
}

/// A JSON number; non-finite values become `null`.
pub fn num(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.4}")
    } else {
        "null".into()
    }
}

/// A JSON string literal.
pub fn string(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_objects() {
        let o = Obj::new()
            .str("a", "x\"y")
            .num("b", 1.5)
            .num("c", f64::NAN)
            .int("d", 3u32)
            .opt_str("e", None)
            .raw("f", array(["1".into(), "2".into()]))
            .build();
        assert_eq!(
            o,
            r#"{"a":"x\"y","b":1.5000,"c":null,"d":3,"e":null,"f":[1,2]}"#
        );
    }
}
