//! The verification ledger, `docs/verified.toml` (docs/DOCS.md#verification, ADR-0053).
//!
//! A section's stamp is one entry per input (its own text, each covered file, each
//! `depends_on` target), each with the hash that input had when someone last re-read the
//! section against a change to it. Every entry is a table header naming the section and
//! input, one `stamp` line, and a blank line. The header never changes while the input
//! exists, so it anchors git's diff: two PRs that change, add or remove different inputs of
//! one section touch different lines, and git (GitHub's merge included) merges them without
//! a conflict. Two PRs that change the same input still conflict on its `stamp` line, as
//! they should: neither re-read the combination. Only structural changes right next to each
//! other (one PR adding an input just before one the other removes, or both removing
//! neighbouring inputs) can also conflict.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};

use super::hash::toml_str;

/// Where the ledger lives, relative to the repository root.
pub(crate) const PATH: &str = "docs/verified.toml";

const HEADER: &str = "\
# Verification stamps: one entry per section input (docs/DOCS.md#verification, ADR-0053).
# Written by `cargo xtask docs verify`; never edit by hand. One header, one line and a
# blank line per entry let parallel PRs that re-verify different inputs merge cleanly.
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
            for (key, wrapper) in t {
                let w = wrapper
                    .as_table()
                    .ok_or_else(|| format!("`{section}` entry `{key}` must be a table"))?;
                if let Some(k) = w.keys().find(|k| k.as_str() != "stamp") {
                    return Err(format!("`{section}` entry `{key}`: unknown key `{k}`"));
                }
                let e = w
                    .get("stamp")
                    .ok_or_else(|| format!("`{section}` entry `{key}` has no `stamp`"))?;
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
                    .ok_or_else(|| format!("`{section}` entry `{key}`: `stamp` must be a table"))?;
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

    /// The canonical text: sections and entries sorted; each entry a `[section.input]`
    /// header, its `stamp` line and a blank line, the last one included, so an entry
    /// appended at the end doesn't touch the line before it.
    pub(crate) fn render(&self) -> String {
        let mut out = format!("{HEADER}\n");
        for (section, stamp) in &self.sections {
            for (key, e) in stamp {
                let _ = write!(
                    out,
                    "[{}.{}]\nstamp = {{ hash = {}",
                    toml_str(section),
                    toml_str(key),
                    toml_str(&e.hash)
                );
                if let (Some(by), Some(note)) = (&e.by, &e.note) {
                    let _ = write!(out, ", by = {}, note = {}", toml_str(by), toml_str(note));
                }
                out.push_str(" }\n\n");
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
    fn render_parses_back_with_a_header_line_and_blank_per_entry() {
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
        assert!(body[0].is_empty(), "{text}");
        for entry in body[1..].chunks(3) {
            assert!(
                entry[0].starts_with('[') && entry[0].ends_with(']'),
                "{text}"
            );
            assert!(entry[1].starts_with("stamp = { hash = "), "{text}");
            assert!(entry[2].is_empty(), "{text}");
        }
        assert_eq!(body.len(), 1 + 3 * 3, "{text}");
        assert!(
            text.find("[\"A#a\".\"body\"]") < text.find("[\"B#b\".\"body\"]"),
            "{text}"
        );
        assert_eq!(Ledger::parse(&l.render()).map(|p| p.render()), Ok(text));
    }

    #[test]
    fn parse_rejects_malformed_entries() {
        for (bad, why) in [
            ("x = 1", "table of entries"),
            ("[\"A#a\"]\nbody = 1", "must be a table"),
            ("[\"A#a\".body]\nother = 1", "unknown key"),
            ("[\"A#a\".body]\nstamp = 1", "`stamp` must be a table"),
            (
                "[\"A#a\".body]\nstamp = { by = \"a\", note = \"n\" }",
                "no `hash`",
            ),
            (
                "[\"A#a\".body]\nstamp = { hash = \"h\", by = \"a\" }",
                "go together",
            ),
            ("[\"A#a\".body]\nstamp = { hash = \"\" }", "non-empty"),
            (
                "[\"A#a\".body]\nstamp = { hash = \"h\", extra = \"x\" }",
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

    /// The layout's promise, checked with git itself: on one section, two branches that
    /// change, add or remove *different* inputs (including an input next to one the other
    /// branch changes) merge cleanly; two that change the same input conflict.
    #[test]
    fn parallel_changes_to_different_inputs_merge_cleanly_in_git() {
        type Entries = Vec<(&'static str, &'static str)>;
        let ledger = |entries: &[(&str, &str)]| {
            let mut l = Ledger::default();
            l.sections.insert(
                "S#s".into(),
                restamp(
                    None,
                    &inputs(entries),
                    "agent:x",
                    "same note on every entry",
                ),
            );
            l.render()
        };
        let base = [
            ("body", "b3:0"),
            ("file:a", "b3:1"),
            ("file:b", "b3:2"),
            ("file:c", "b3:3"),
        ];
        let with = |edit: &dyn Fn(&mut Entries)| {
            let mut e = base.to_vec();
            edit(&mut e);
            e.sort_unstable();
            ledger(&e)
        };
        let change = |k: &'static str| {
            with(&move |e: &mut Entries| {
                for x in e.iter_mut().filter(|x| x.0 == k) {
                    x.1 = "b3:9";
                }
            })
        };
        let add = |k: &'static str| with(&move |e: &mut Entries| e.push((k, "b3:8")));
        let remove = |k: &'static str| with(&move |e: &mut Entries| e.retain(|x| x.0 != k));
        let dir = std::env::temp_dir().join(format!("xtask-ledger-git-{}", std::process::id()));
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("invariant: git runs")
        };
        let merges = |a: &str, b: &str| -> bool {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("invariant: temp dir is writable");
            let commit = |text: &str, branch: &str| {
                std::fs::write(dir.join("v.toml"), text).expect("invariant: writable");
                git(&["checkout", "-q", "-B", branch]);
                git(&["add", "v.toml"]);
                git(&["commit", "-q", "-m", branch]);
            };
            git(&["init", "-q"]);
            commit(&ledger(&base), "base");
            commit(a, "one");
            git(&["checkout", "-q", "base"]);
            commit(b, "two");
            git(&["merge-tree", "--write-tree", "one", "two"])
                .status
                .success()
        };
        for (what, a, b) in [
            ("change a, change b", change("file:a"), change("file:b")),
            ("change body, change a", change("body"), change("file:a")),
            ("add aa, change b", add("file:aa"), change("file:b")),
            ("add aa, change a", add("file:aa"), change("file:a")),
            ("remove b, change c", remove("file:b"), change("file:c")),
            ("remove b, change a", remove("file:b"), change("file:a")),
            ("add aa, add bb", add("file:aa"), add("file:bb")),
            ("add aa, remove c", add("file:aa"), remove("file:c")),
            (
                "add zz after the last entry, change c",
                add("file:zz"),
                change("file:c"),
            ),
            ("remove c, change b", remove("file:c"), change("file:b")),
        ] {
            assert!(merges(&a, &b), "{what} must merge cleanly");
        }
        assert!(
            !merges(&change("file:a"), &with(&|e: &mut Entries| e[1].1 = "b3:7")),
            "changing the same input must conflict"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
