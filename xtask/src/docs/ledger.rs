//! The verification ledger, `docs/verified.toml` (docs/DOCS.md#verification, ADR-0050).
//!
//! A section's stamp is one entry per input (its own text, each covered file, each
//! `depends_on` target), each with the hash that input had when someone last re-read the
//! section against a change to it. Every entry is its own line with a blank line after it,
//! so two PRs that change different inputs of one section change different lines, and git
//! (GitHub's merge included) merges them without a conflict. Two PRs that change the same
//! input still conflict on its line, as they should: neither re-read the combination.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};

use super::hash::toml_str;

/// Where the ledger lives, relative to the repository root.
pub(crate) const PATH: &str = "docs/verified.toml";

const HEADER: &str = "\
# Verification stamps: one entry per section input (docs/DOCS.md#verification, ADR-0050).
# Written by `cargo xtask docs verify`; never edit by hand. The blank line after each entry
# lets parallel PRs that re-verify different inputs of a section merge without a conflict.
";

/// One input's stamp: its hash when last verified, and who verified it and why, when this
/// entry was written by `docs verify` (entries moved from inline stamps keep only the hash,
/// apart from the section's `body`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) hash: String,
    pub(crate) by: Option<String>,
    pub(crate) note: Option<String>,
}

/// The entries of one section, keyed as [`super::hash::inputs`] keys them.
pub(crate) type Stamp = BTreeMap<String, Entry>;

/// The whole ledger: each section's stamp, keyed `DOC-ID#anchor`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Ledger {
    pub(crate) sections: BTreeMap<String, Stamp>,
}

impl Ledger {
    /// Parses ledger text. The error says what is wrong.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let table: toml::Table = text.parse().map_err(|e| format!("{e}"))?;
        let mut sections = BTreeMap::new();
        for (section, v) in table {
            let t = v
                .as_table()
                .ok_or_else(|| format!("`{section}` must be a table of entries"))?;
            let mut stamp = Stamp::new();
            for (key, e) in t {
                let field = |k: &str| -> Result<Option<String>, String> {
                    match e.get(k) {
                        None => Ok(None),
                        Some(toml::Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
                        Some(_) => Err(format!(
                            "`{section}` entry `{key}`: `{k}` must be a non-empty string"
                        )),
                    }
                };
                let e_table = e
                    .as_table()
                    .ok_or_else(|| format!("`{section}` entry `{key}` must be a table"))?;
                if let Some(k) = e_table
                    .keys()
                    .find(|k| !["hash", "by", "note"].contains(&k.as_str()))
                {
                    return Err(format!("`{section}` entry `{key}`: unknown key `{k}`"));
                }
                let hash = field("hash")?
                    .ok_or_else(|| format!("`{section}` entry `{key}` has no `hash`"))?;
                let (by, note) = (field("by")?, field("note")?);
                if by.is_some() != note.is_some() {
                    return Err(format!(
                        "`{section}` entry `{key}`: `by` and `note` go together"
                    ));
                }
                stamp.insert(key.clone(), Entry { hash, by, note });
            }
            sections.insert(section, stamp);
        }
        Ok(Self { sections })
    }

    /// The ledger at `root`; empty when the file doesn't exist.
    pub(crate) fn read(root: &Path) -> Result<Self> {
        match std::fs::read_to_string(root.join(PATH)) {
            Ok(text) => Self::parse(&text).map_err(|e| anyhow::anyhow!("{PATH}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {PATH}")),
        }
    }

    /// The canonical text: sections and entries sorted, one entry per line, each followed
    /// by a blank line.
    pub(crate) fn render(&self) -> String {
        let mut out = HEADER.to_owned();
        for (section, stamp) in &self.sections {
            let _ = write!(out, "\n[{}]\n", toml_str(section));
            for (key, e) in stamp {
                let _ = write!(out, "\n{} = {{ hash = {}", toml_str(key), toml_str(&e.hash));
                if let (Some(by), Some(note)) = (&e.by, &e.note) {
                    let _ = write!(out, ", by = {}, note = {}", toml_str(by), toml_str(note));
                }
                out.push_str(" }\n");
            }
        }
        out
    }

    /// Writes the canonical text to `root`.
    pub(crate) fn write(&self, root: &Path) -> Result<()> {
        let path = root.join(PATH);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, self.render()).with_context(|| format!("writing {PATH}"))
    }

    /// Drops every section not in `keep`; returns the dropped keys.
    pub(crate) fn prune(&mut self, keep: &dyn Fn(&str) -> bool) -> Vec<String> {
        let gone: Vec<String> = self.sections.keys().filter(|k| !keep(k)).cloned().collect();
        for k in &gone {
            self.sections.remove(k);
        }
        gone
    }
}

/// Whether `stamp` records exactly `inputs`; if not, the inputs that differ (changed,
/// added or removed), sorted.
pub(crate) fn diff(stamp: Option<&Stamp>, inputs: &BTreeMap<String, String>) -> Vec<String> {
    let empty = Stamp::new();
    let stamp = stamp.unwrap_or(&empty);
    let mut out: Vec<String> = inputs
        .iter()
        .filter(|(k, h)| stamp.get(*k).is_none_or(|e| &e.hash != *h))
        .map(|(k, _)| k.clone())
        .chain(stamp.keys().filter(|k| !inputs.contains_key(*k)).cloned())
        .collect();
    out.sort();
    out
}

/// The stamp `docs verify` writes: entries whose hash still matches keep their `by` and
/// `note`; changed and new inputs get this verification's; removed inputs are dropped.
/// When no entry changed or was added, the `body` entry takes this verification, so the
/// re-read is still recorded.
pub(crate) fn restamp(
    old: Option<&Stamp>,
    inputs: &BTreeMap<String, String>,
    by: &str,
    note: &str,
) -> Stamp {
    let fresh = |hash: &str| Entry {
        hash: hash.to_owned(),
        by: Some(by.to_owned()),
        note: Some(note.to_owned()),
    };
    let mut changed = false;
    let mut out: Stamp = inputs
        .iter()
        .map(|(k, h)| {
            let e = match old.and_then(|o| o.get(k)) {
                Some(e) if &e.hash == h => e.clone(),
                _ => {
                    changed = true;
                    fresh(h)
                }
            };
            (k.clone(), e)
        })
        .collect();
    if !changed && let Some(e) = out.get_mut(super::hash::BODY) {
        *e = fresh(&e.hash);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn render_parses_back_and_puts_a_blank_line_after_every_entry() {
        let mut l = Ledger::default();
        l.sections.insert(
            "B#b".into(),
            restamp(
                None,
                &inputs(&[("body", "b3:1"), ("file:a \"q\".rs", "b3:2")]),
                "agent:x",
                "said \"why\"",
            ),
        );
        l.sections.insert(
            "A#a".into(),
            [(
                "body".to_owned(),
                Entry {
                    hash: "b3:3".into(),
                    by: None,
                    note: None,
                },
            )]
            .into(),
        );
        let text = l.render();
        assert_eq!(Ledger::parse(&text), Ok(l.clone()));
        let body: Vec<&str> = text.lines().skip_while(|x| x.starts_with('#')).collect();
        for pair in body.windows(2) {
            assert!(pair[0].is_empty() || pair[1].is_empty(), "{text}");
        }
        assert!(text.find("[\"A#a\"]") < text.find("[\"B#b\"]"), "{text}");
        assert_eq!(Ledger::parse(&l.render()).map(|p| p.render()), Ok(text));
    }

    #[test]
    fn parse_rejects_malformed_entries() {
        for (bad, why) in [
            ("x = 1", "table of entries"),
            ("[\"A#a\"]\nbody = 1", "must be a table"),
            (
                "[\"A#a\"]\nbody = { by = \"a\", note = \"n\" }",
                "no `hash`",
            ),
            (
                "[\"A#a\"]\nbody = { hash = \"h\", by = \"a\" }",
                "go together",
            ),
            ("[\"A#a\"]\nbody = { hash = \"\" }", "non-empty"),
            (
                "[\"A#a\"]\nbody = { hash = \"h\", extra = \"x\" }",
                "unknown key",
            ),
        ] {
            let err = Ledger::parse(bad).expect_err(bad);
            assert!(err.contains(why), "{bad}: {err}");
        }
    }

    #[test]
    fn diff_names_changed_added_and_removed_inputs() {
        let stamp = restamp(
            None,
            &inputs(&[("body", "1"), ("file:a", "2"), ("file:b", "3")]),
            "agent:x",
            "n",
        );
        let now = inputs(&[("body", "1"), ("file:a", "9"), ("file:c", "4")]);
        assert_eq!(diff(Some(&stamp), &now), vec!["file:a", "file:b", "file:c"]);
        assert!(
            diff(
                Some(&stamp),
                &inputs(&[("body", "1"), ("file:a", "2"), ("file:b", "3")])
            )
            .is_empty()
        );
        assert_eq!(diff(None, &inputs(&[("body", "1")])), vec!["body"]);
    }

    /// Only changed inputs take the new verification, so parallel PRs that change
    /// different inputs write different lines; a re-read with no change restamps `body`.
    #[test]
    fn restamp_rewrites_only_changed_entries() {
        let old = restamp(
            None,
            &inputs(&[("body", "1"), ("file:a", "2"), ("file:b", "3")]),
            "agent:old",
            "old",
        );
        let new = restamp(
            Some(&old),
            &inputs(&[("body", "1"), ("file:a", "9")]),
            "agent:new",
            "new",
        );
        assert_eq!(new["body"], old["body"]);
        assert_eq!(new["file:a"].by.as_deref(), Some("agent:new"));
        assert!(!new.contains_key("file:b"));
        let again = restamp(
            Some(&new),
            &inputs(&[("body", "1"), ("file:a", "9")]),
            "agent:third",
            "third",
        );
        assert_eq!(again["body"].by.as_deref(), Some("agent:third"));
        assert_eq!(again["file:a"], new["file:a"]);
    }
}
