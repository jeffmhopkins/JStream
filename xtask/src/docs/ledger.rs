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

    /// Gives each entry whose hash equals `theirs`'s entry for the same input that entry,
    /// `by` and `note` included; returns the sections changed, sorted. Both recorded the
    /// same content of that input, so no section's freshness changes, but the last note on
    /// the input becomes the one the merged side wrote for it, and the branch's ledger diff
    /// keeps only the inputs it verified at other content (#697). An entry whose note holds
    /// `theirs`'s note in full, and more, is kept instead (#874): it is the fuller record, as
    /// when a sync PR merges `main` into a wave whose note on the input carries main's.
    pub(crate) fn adopt(&mut self, theirs: &Self) -> Vec<String> {
        let mut changed = Vec::new();
        for (section, stamp) in &mut self.sections {
            let Some(other) = theirs.sections.get(section) else {
                continue;
            };
            let mut any = false;
            for (input, e) in stamp.iter_mut() {
                if let Some(t) = other
                    .get(input)
                    .filter(|t| t.hash == e.hash && *t != e && !holds_more(e, t))
                {
                    *e = t.clone();
                    any = true;
                }
            }
            if any {
                changed.push(section.clone());
            }
        }
        changed
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

/// Whether `ours`'s note holds `theirs`'s note in full and more, or `theirs` has none: on
/// the same hash, `ours` is then the fuller record of that input (#874).
pub(crate) fn holds_more(ours: &Entry, theirs: &Entry) -> bool {
    match (&ours.note, &theirs.note) {
        (Some(o), Some(t)) => o != t && o.contains(t.as_str()),
        (Some(_), None) => true,
        (None, _) => false,
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

/// Where a note written by `docs verify --merge` turns from the note before to what a merge
/// changed. Each later merge's `--merge` keeps every earlier segment and appends its own
/// (#874): on a sync PR into a wave, the segments after the first mark are other PRs'
/// records, not this branch's superseded text.
pub(crate) const MERGE_MARK: &str = " | merge: ";

/// The note `docs verify --merge` writes on an entry: the branch's `earlier` note, every
/// earlier merge's segment included but without the merged-side notes it kept, then
/// `merge`, what this merge changed in the input. A `merge` the earlier note already ends
/// with isn't added again.
pub(crate) fn merge_note(earlier: Option<&str>, merge: &str) -> String {
    let lead = MERGE_MARK.trim_start();
    let own = earlier
        .map(|n| split_kept(n).0.trim())
        .filter(|n| !n.is_empty());
    match own {
        Some(own)
            if own == format!("merge: {merge}")
                || own.ends_with(&format!("{MERGE_MARK}{merge}")) =>
        {
            own.to_owned()
        }
        // A note that starts at the mark had no text of the branch's before its merge.
        Some(own) => match own.strip_prefix(lead) {
            Some(rest) => format!("merge: {rest}{MERGE_MARK}{merge}"),
            None => format!("{own}{MERGE_MARK}{merge}"),
        },
        None => format!("merge: {merge}"),
    }
}

/// Where a note turns from this branch's text to the merged side's notes it keeps (#840):
/// each merged-side note follows this mark as that side wrote it, apart from pieces the
/// note already holds (#888), so a verify after a merge never drops the records a wave or
/// `main` wrote on the same input.
pub(crate) const KEPT_MARK: &str = " | kept from the merged side: ";

/// A note split into this branch's text and the merged side's notes it keeps: everything
/// from the first [`KEPT_MARK`] on, or `""`.
pub(crate) fn split_kept(note: &str) -> (&str, &str) {
    note.find(KEPT_MARK)
        .map_or((note, ""), |i| note.split_at(i))
}

/// The note a verify writes on an entry it rewrites after this branch's merge (#840):
/// `head`, its new text, then each kept segment once (#842). The segments are those the
/// entry's `earlier` note (this branch's own) kept, then `theirs`, the note the merged side
/// wrote on the input since the branch left it, split at [`KEPT_MARK`] into its own text
/// and the segments it kept in turn. Each segment is kept word for word, apart from the pieces
/// left out below, in the order it is first met. One that `head` or another segment already holds in full is left out, so a
/// record two sides both kept appears once and a newer segment that holds an older one in
/// full takes its place. Each piece of a segment (split at [`MERGE_MARK`]) that the note
/// already holds, in the new text or a segment before it, is left out too (#888).
pub(crate) fn keep_notes(head: &str, earlier: Option<&str>, theirs: Option<&str>) -> String {
    let earlier = earlier.map_or("", |n| split_kept(n).1);
    let theirs = theirs.map_or("", str::trim);
    let segments = earlier
        .split(KEPT_MARK)
        .chain(
            std::iter::once(theirs)
                .filter(|t| !t.is_empty())
                .flat_map(|t| t.split(KEPT_MARK)),
        )
        .map(str::trim)
        .filter(|s| !s.is_empty() && !head.contains(s));
    let mut kept: Vec<&str> = Vec::new();
    for s in segments {
        if kept.iter().any(|k| k.contains(s)) {
            continue;
        }
        // A newer segment that holds earlier ones in full takes the first one's place.
        match kept.iter().position(|k| s.contains(k)) {
            Some(i) => {
                kept[i] = s;
                let mut j = i + 1;
                while j < kept.len() {
                    if s.contains(kept[j]) {
                        kept.remove(j);
                    } else {
                        j += 1;
                    }
                }
            }
            None => kept.push(s),
        }
    }
    // #888: each kept segment is also checked piece by piece (split at [`MERGE_MARK`])
    // against everything already in the note, the new text and the segments before it: a
    // piece already there is left out, so a kept record that repeats this branch's own text,
    // or another kept segment's, adds only what is new. A segment with nothing new is left
    // out, and one with every piece new is kept word for word.
    let mut note = head.to_owned();
    for k in kept {
        let pieces: Vec<(usize, &str)> = k
            .split(MERGE_MARK)
            .map(str::trim)
            .enumerate()
            .filter(|(_, p)| !p.is_empty() && !note.contains(p))
            .collect();
        if pieces.is_empty() {
            continue;
        }
        note.push_str(KEPT_MARK);
        if pieces.len() == k.split(MERGE_MARK).count() {
            note.push_str(k);
            continue;
        }
        for (n, (i, p)) in pieces.into_iter().enumerate() {
            // A piece that followed a merge mark keeps that label.
            note.push_str(match (n, i) {
                (0, 0) => "",
                (0, _) => "merge: ",
                _ => MERGE_MARK,
            });
            note.push_str(p);
        }
    }
    note
}

/// The stamp `docs verify --merge` writes after `docs merge-main`. `branch` is the entries
/// this branch had written itself before the merge. Each entry that `restamp` would rewrite,
/// and each the branch had written whose input the merge has since changed (as when a
/// re-verify after the merge replaced the branch's note), gets [`merge_note`] of the branch's
/// note on it. `merged` is the section's stamp on the merged side: an input whose hash equals
/// that side's entry gets that entry as it is, since this branch changes nothing in it
/// (#596), unless the entry there is still the branch's own from before the merge and the
/// fuller record ([`holds_more`]), which `merge-main` kept (#874). The rest are kept, and
/// removed inputs dropped, as `restamp` does.
pub(crate) fn restamp_merge(
    old: Option<&Stamp>,
    inputs: &BTreeMap<String, String>,
    branch: Option<&Stamp>,
    merged: Option<&Stamp>,
    by: &str,
    merge: &str,
) -> Stamp {
    inputs
        .iter()
        .map(|(k, h)| {
            let own = branch.and_then(|b| b.get(k));
            let theirs = merged.and_then(|m| m.get(k)).filter(|e| &e.hash == h);
            let e = match (theirs, old.and_then(|o| o.get(k))) {
                // The branch's own entry from before the merge, which `merge-main` kept as the
                // fuller record, stays (#874).
                (Some(t), Some(e)) if e.hash == t.hash && own == Some(e) && holds_more(e, t) => {
                    e.clone()
                }
                (Some(t), _) => t.clone(),
                (None, Some(e)) if &e.hash == h && own.is_none_or(|o| &o.hash == h) => e.clone(),
                _ => Entry {
                    hash: h.clone(),
                    by: Some(by.to_owned()),
                    note: Some(merge_note(own.and_then(|o| o.note.as_deref()), merge)),
                },
            };
            (k.clone(), e)
        })
        .collect()
}

/// The stamp `docs verify --renote` writes on a fresh section: the entries this branch
/// wrote (those that differ from every one of `bases`, the section's stamps where the
/// branch left its upstreams) get this `by` and `note`, keeping their hashes; the rest are
/// kept as they were. `None` when the branch wrote none.
pub(crate) fn renote(old: &Stamp, bases: &[&Stamp], by: &str, note: &str) -> Option<Stamp> {
    let mut any = false;
    let stamp = old
        .iter()
        .map(|(k, e)| {
            if bases.iter().any(|b| b.get(k) == Some(e)) {
                return (k.clone(), e.clone());
            }
            any = true;
            let e = Entry {
                hash: e.hash.clone(),
                by: Some(by.to_owned()),
                note: Some(note.to_owned()),
            };
            (k.clone(), e)
        })
        .collect();
    any.then_some(stamp)
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

    /// #697: an entry recorded at the same hash as the merged side's takes that side's
    /// `by` and `note`; one at another hash, and sections or inputs the other side lacks,
    /// are kept.
    #[test]
    fn adopt_takes_the_other_sides_entry_at_the_same_hash() {
        let e = |hash: &str, who: Option<&str>| Entry {
            hash: hash.into(),
            by: who.map(|w| format!("agent:{w}")),
            note: who.map(|w| format!("{w}'s note")),
        };
        let ledger = |sections: Vec<(&str, Vec<(&str, Entry)>)>| Ledger {
            sections: sections
                .into_iter()
                .map(|(s, es)| {
                    let stamp = es.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
                    (s.to_owned(), stamp)
                })
                .collect(),
        };
        // #874: a note that holds the other side's in full, and more, is the fuller record.
        let full = Entry {
            hash: "b3:7".into(),
            by: Some("agent:wave".into()),
            note: Some("wave's own | kept from the merged side: main's note".into()),
        };
        let mut ours = ledger(vec![
            (
                "S#a",
                vec![
                    ("body", e("b3:1", Some("branch"))),
                    ("file:a.rs", e("b3:2", Some("branch"))),
                    ("file:b.rs", e("b3:3", Some("branch"))),
                    ("file:new.rs", e("b3:4", Some("branch"))),
                    ("file:hold.rs", full.clone()),
                    ("file:none.rs", e("b3:8", None)),
                ],
            ),
            ("T#a", vec![("body", e("b3:5", Some("branch")))]),
            ("U#a", vec![("body", e("b3:6", Some("branch")))]),
        ]);
        let theirs = ledger(vec![
            (
                "S#a",
                vec![
                    ("body", e("b3:1", Some("main"))),
                    ("file:a.rs", e("b3:2", None)),
                    ("file:b.rs", e("b3:9", Some("main"))),
                    ("file:hold.rs", e("b3:7", Some("main"))),
                    ("file:none.rs", e("b3:8", Some("main"))),
                ],
            ),
            ("T#a", vec![("body", e("b3:5", Some("branch")))]),
        ]);
        assert_eq!(ours.adopt(&theirs), vec!["S#a".to_owned()]);
        let s = &ours.sections["S#a"];
        assert_eq!(s["body"], e("b3:1", Some("main")));
        // A hash-only entry on the other side has no note to keep: ours holds more (#874).
        assert_eq!(s["file:a.rs"], e("b3:2", Some("branch")));
        assert_eq!(s["file:hold.rs"], full, "ours holds main's note in full");
        assert_eq!(
            s["file:none.rs"],
            e("b3:8", Some("main")),
            "ours has no note"
        );
        assert_eq!(s["file:b.rs"], e("b3:3", Some("branch")), "another hash");
        assert_eq!(
            s["file:new.rs"],
            e("b3:4", Some("branch")),
            "theirs lacks it"
        );
        assert_eq!(ours.sections["U#a"]["body"], e("b3:6", Some("branch")));
        // Nothing left to take: no section is reported again.
        assert!(ours.adopt(&theirs).is_empty());
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
        let root = std::env::temp_dir().join(format!("xtask-ledger-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // Runs git in `dir` and returns its output. Auto-maintenance is off: from git 2.47
        // a commit or merge starts it detached, so it could still be running in a
        // repository when the next step reuses that path (#779).
        let git = |dir: &std::path::Path, args: &[&str]| {
            std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
                .args(args)
                .current_dir(dir)
                .output()
                .expect("invariant: git runs")
        };
        // Like `git`, but a failure panics with git's own message.
        let ok = |dir: &std::path::Path, args: &[&str]| {
            let out = git(dir, args);
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let runs = std::cell::Cell::new(0);
        // Merges main into the branch and returns whether it was clean, and the ledger
        // after taking main's side of any conflict. Each merge gets a repository of its
        // own, so nothing a previous one left behind can reach it.
        let merge = |branch: &str, main: &str| -> (bool, Ledger) {
            runs.set(runs.get() + 1);
            let dir = root.join(runs.get().to_string());
            std::fs::create_dir_all(&dir).expect("invariant: temp dir is writable");
            let dir = dir.as_path();
            let commit = |text: &str, name: &str| {
                std::fs::write(dir.join("v.toml"), text).expect("invariant: writable");
                ok(dir, &["checkout", "-q", "-B", name]);
                ok(dir, &["add", "v.toml"]);
                ok(dir, &["commit", "-q", "-m", name]);
            };
            ok(dir, &["init", "-q"]);
            commit(&render(base_stamp.clone()), "base");
            commit(main, "main");
            ok(dir, &["checkout", "-q", "base"]);
            commit(branch, "branch");
            let out = git(dir, &["merge", "-q", "main"]);
            let clean = out.status.success();
            // A failed merge must be a conflict in the ledger, not some other refusal.
            let listed = git(dir, &["ls-files", "-u"]);
            assert!(
                listed.status.success(),
                "git ls-files -u failed: {}",
                String::from_utf8_lossy(&listed.stderr)
            );
            let unmerged = listed.stdout;
            assert!(
                clean || !unmerged.is_empty(),
                "git merge failed without a conflict: {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
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
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn merge_notes_keep_the_branch_part() {
        let m = MERGE_MARK;
        assert_eq!(merge_note(Some("pr part"), "now"), format!("pr part{m}now"));
        // #874: every earlier merge segment is kept, and the new one appended.
        assert_eq!(
            merge_note(Some(&format!("pr part{m}earlier")), "now"),
            format!("pr part{m}earlier{m}now")
        );
        assert_eq!(
            merge_note(Some(&format!("wave{m}one{m}two")), "now"),
            format!("wave{m}one{m}two{m}now")
        );
        assert_eq!(merge_note(None, "now"), "merge: now");
        assert_eq!(
            merge_note(Some("merge: earlier"), "now"),
            format!("merge: earlier{m}now")
        );
        assert_eq!(
            merge_note(Some(&format!("{m}earlier")), "now"),
            format!("merge: earlier{m}now")
        );
        // Running the same `--merge` again adds nothing.
        assert_eq!(
            merge_note(Some(&format!("pr part{m}now")), "now"),
            format!("pr part{m}now")
        );
        assert_eq!(merge_note(Some("merge: now"), "now"), "merge: now");
    }

    /// #874: on a sync PR, an input whose hash equals the merged side's keeps the branch's own
    /// entry from before the merge when its note holds the merged side's in full and more;
    /// an entry a verify after the merge wrote, or one that holds less, takes the merged side's.
    #[test]
    fn restamp_merge_keeps_the_fuller_own_entry_at_the_same_hash() {
        let e = |h: &str, n: &str| Entry {
            hash: h.to_owned(),
            by: Some("agent:x".to_owned()),
            note: Some(n.to_owned()),
        };
        let now = inputs(&[("body", "b3:1")]);
        let main = Stamp::from([("body".to_owned(), e("b3:1", "main's note"))]);
        let wave = Stamp::from([(
            "body".to_owned(),
            e(
                "b3:1",
                "wave's note | kept from the merged side: main's note",
            ),
        )]);
        let got = restamp_merge(Some(&wave), &now, Some(&wave), Some(&main), "agent:y", "m");
        assert_eq!(got, wave, "the wave's fuller record stays");
        let later = Stamp::from([(
            "body".to_owned(),
            e(
                "b3:1",
                "a re-verify | kept from the merged side: main's note",
            ),
        )]);
        let got = restamp_merge(Some(&later), &now, Some(&wave), Some(&main), "agent:y", "m");
        assert_eq!(got, main, "written after the merge (#596)");
        let thin = Stamp::from([("body".to_owned(), e("b3:1", "wave's note"))]);
        let got = restamp_merge(Some(&thin), &now, Some(&thin), Some(&main), "agent:y", "m");
        assert_eq!(got, main, "holds less than main's");
    }

    #[test]
    fn renote_rewrites_only_the_branch_entries() {
        let e = |h: &str, n: &str| Entry {
            hash: h.to_owned(),
            by: Some("agent:x".to_owned()),
            note: Some(n.to_owned()),
        };
        let base = Stamp::from([("body".to_owned(), e("b3:1", "main"))]);
        let old = Stamp::from([
            ("body".to_owned(), e("b3:1", "main")),
            ("file:a".to_owned(), e("b3:2", "thin")),
        ]);
        let new = renote(&old, &[&base], "agent:y", "better").expect("branch entry");
        assert_eq!(new["body"], old["body"]);
        let a = &new["file:a"];
        assert_eq!(
            (a.hash.as_str(), a.by.as_deref(), a.note.as_deref()),
            ("b3:2", Some("agent:y"), Some("better"))
        );
        assert!(renote(&base, &[&base], "agent:y", "better").is_none());
        // An entry equal to any base's isn't the branch's (#719 B1).
        let other = Stamp::from([("file:a".to_owned(), e("b3:2", "thin"))]);
        let new = renote(&old, &[&base, &other], "agent:y", "n");
        assert!(new.is_none(), "{new:?}");
        // With no stamp at the base, every entry is the branch's.
        assert_eq!(renote(&old, &[], "agent:y", "n").expect("all").len(), 2);
    }
}
