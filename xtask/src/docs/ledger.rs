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
# blank line per entry let parallel PRs that re-verify different inputs usually merge.
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
/// Nothing else is rewritten: a verify that only drops removed inputs, or finds every
/// input unchanged, leaves the other entries' lines as they were, so it can't conflict
/// with a parallel change to them that `docs merge-main` would then resolve to fresh.
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
    inputs
        .iter()
        .map(|(k, h)| {
            let e = match old.and_then(|o| o.get(k)) {
                Some(e) if &e.hash == h => e.clone(),
                _ => fresh(h),
            };
            (k.clone(), e)
        })
        .collect()
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
    /// different inputs write different lines; a re-read with no change rewrites nothing.
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
        assert_eq!(
            again, new,
            "a re-read with nothing changed rewrites nothing"
        );
    }

    /// The layout's promise, checked with git itself on ledgers that `restamp` wrote from
    /// a shared base, as `docs verify` does on two branches. Two branches that change
    /// different inputs, or add or remove one next to an input the other changes, merge
    /// cleanly and the section is fresh. Two that change the same input conflict, and so
    /// can two that add or remove inputs at the same spot (ADR-0053 item 4); taking main's
    /// side of the conflict, as `docs merge-main` does, leaves the section stale unless
    /// both made the same change.
    #[test]
    fn parallel_verifies_merge_cleanly_or_resolve_to_stale_in_git() {
        #[derive(Clone, Copy)]
        enum Op {
            Change(&'static str),
            Add(&'static str),
            Remove(&'static str),
        }
        use Op::{Add, Change, Remove};
        let base = inputs(&[
            ("body", "b3:0"),
            ("file:a", "b3:1"),
            ("file:b", "b3:2"),
            ("file:c", "b3:3"),
            ("file:d", "b3:4"),
        ]);
        let apply = |to: &mut BTreeMap<String, String>, op: Op, hash: &str| match op {
            Change(k) | Add(k) => {
                to.insert(k.to_owned(), hash.to_owned());
            }
            Remove(k) => {
                to.remove(k);
            }
        };
        let base_stamp = restamp(None, &base, "agent:base", "base");
        let render = |stamp: Stamp| {
            let mut l = Ledger::default();
            l.sections.insert("S#s".into(), stamp);
            l.render()
        };
        let side = |op: Op, who: &str| {
            let mut now = base.clone();
            apply(&mut now, op, &format!("b3:{who}"));
            render(restamp(
                Some(&base_stamp),
                &now,
                &format!("agent:{who}"),
                who,
            ))
        };
        let dir = std::env::temp_dir().join(format!("xtask-ledger-git-{}", std::process::id()));
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&dir)
                .output()
                .expect("invariant: git runs")
        };
        // Merges main into the branch and returns whether it was clean, and the ledger
        // after taking main's side of any conflict.
        let merge = |branch: &str, main: &str| -> (bool, Ledger) {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("invariant: temp dir is writable");
            let commit = |text: &str, name: &str| {
                std::fs::write(dir.join("v.toml"), text).expect("invariant: writable");
                git(&["checkout", "-q", "-B", name]);
                git(&["add", "v.toml"]);
                git(&["commit", "-q", "-m", name]);
            };
            git(&["init", "-q"]);
            commit(&render(base_stamp.clone()), "base");
            commit(main, "main");
            git(&["checkout", "-q", "base"]);
            commit(branch, "branch");
            let clean = git(&["merge", "-q", "main"]).status.success();
            let text = std::fs::read_to_string(dir.join("v.toml")).expect("invariant: written");
            let text = if clean {
                text
            } else {
                super::super::merge::resolve_ledger(&text)
                    .expect("invariant: a ledger conflict resolves")
                    .0
            };
            (
                clean,
                Ledger::parse(&text).expect("invariant: the merge parses"),
            )
        };
        let pairs = [
            (Change("file:a"), Change("file:b"), true),
            (Change("body"), Change("file:a"), true),
            (Add("file:aa"), Change("file:b"), true),
            (Add("file:aa"), Change("file:a"), true),
            (Remove("file:b"), Change("file:c"), true),
            (Remove("file:b"), Change("file:a"), true),
            (Remove("file:b"), Change("body"), true),
            (Remove("file:b"), Remove("file:d"), true),
            (Add("file:aa"), Add("file:bb"), true),
            (Add("file:aa"), Remove("file:c"), true),
            (Add("file:zz"), Change("file:d"), true),
            (Remove("file:d"), Change("file:c"), true),
            (Change("file:a"), Change("file:a"), false),
            (Add("file:ba"), Add("file:bb"), false),
            (Remove("file:d"), Add("file:e"), false),
            (Remove("file:b"), Remove("file:c"), false),
        ];
        for (one, two, clean) in pairs {
            for (branch, main) in [(one, two), (two, one)] {
                let (merged_clean, merged) = merge(&side(branch, "one"), &side(main, "two"));
                let what = format!("{:?} into {:?}", ops(main), ops(branch));
                assert_eq!(merged_clean, clean, "{what}: clean merge expected: {clean}");
                // The tree after the merge: both edits, and a third version of an input
                // both changed.
                let mut tree = base.clone();
                apply(&mut tree, branch, "b3:one");
                apply(&mut tree, main, "b3:two");
                if let (Change(x), Change(y)) = (branch, main)
                    && x == y
                {
                    tree.insert(x.to_owned(), "b3:both".to_owned());
                }
                let stale = !diff(merged.sections.get("S#s"), &tree).is_empty();
                assert_eq!(stale, !clean, "{what}: stale after the merge: {stale}");
            }
        }
        // The one conflict that resolves fresh: both made the same change to an input, so
        // main's entry already verified exactly the merged content.
        let same = |who: &str| {
            let mut now = base.clone();
            now.insert("file:a".into(), "b3:same".into());
            render(restamp(
                Some(&base_stamp),
                &now,
                &format!("agent:{who}"),
                who,
            ))
        };
        let (clean, merged) = merge(&same("one"), &same("two"));
        let mut tree = base.clone();
        tree.insert("file:a".into(), "b3:same".into());
        assert!(!clean && diff(merged.sections.get("S#s"), &tree).is_empty());
        fn ops(op: Op) -> String {
            match op {
                Change(k) => format!("change {k}"),
                Add(k) => format!("add {k}"),
                Remove(k) => format!("remove {k}"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
