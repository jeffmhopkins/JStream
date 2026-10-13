//! `cargo xtask docs for <path>` (docs/DOCS.md#back-references): what governs a file.
//!
//! - **Sections** whose `covers` match the path (retired docs excluded).
//! - **Rules** whose `applies_to` match it (retired rules excluded).
//! - **ADRs**, not superseded, cited by any of: a matched section's `depends_on`, a Markdown
//!   link inside a matched section or rule, or the `Docs:` line of the file or its crate root.
//!
//! The path need not be tracked yet, so it works for files about to be created.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use super::backrefs::{crates, docs_line, owner};
use super::check::normalize;
use super::hash::glob_set;
use super::parse::Doc;

/// What governs one path, as `path#anchor` (sections, rules) and `path` (ADRs).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Governing {
    pub(crate) sections: BTreeSet<String>,
    pub(crate) rules: BTreeSet<String>,
    pub(crate) adrs: BTreeSet<String>,
}

/// String values of `key` in a metadata table.
fn strings(meta: Option<&toml::Table>, key: &str) -> Vec<String> {
    meta.and_then(|m| m.get(key))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether any of `globs` matches `path`. Invalid globs never match; `docs check`
/// reports them.
fn matches(globs: &[String], path: &str) -> bool {
    !globs.is_empty() && glob_set(globs).is_ok_and(|s| s.is_match(path))
}

/// The repo path a Markdown link in `doc` points at, without its fragment.
fn link_target(doc: &Doc, dest: &str) -> Option<String> {
    let file = dest.split('#').next()?;
    if file.is_empty() || file.contains("://") {
        return None;
    }
    let dir = Path::new(&doc.path).parent()?;
    normalize(&dir.join(file))
}

/// Computes what governs `path`, given the parsed docs and the tracked files.
pub(crate) fn governing(root: &Path, docs: &[Doc], files: &[String], path: &str) -> Governing {
    let by_path: BTreeMap<&str, &Doc> = docs.iter().map(|d| (d.path.as_str(), d)).collect();
    let mut g = Governing::default();
    let mut cited: BTreeSet<String> = BTreeSet::new();

    for doc in docs
        .iter()
        .filter(|d| d.meta_str("status") != Some("retired"))
    {
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for s in &doc.sections {
            if matches(&strings(s.meta.as_ref(), "covers"), path) {
                g.sections.insert(format!("{}#{}", doc.path, s.anchor));
                cited.extend(
                    strings(s.meta.as_ref(), "depends_on")
                        .into_iter()
                        .map(|d| d.split('#').next().unwrap_or_default().to_owned()),
                );
                // `body` is 0-based; link lines are 1-based.
                ranges.push((s.body.0 + 1, s.body.1));
            }
        }
        for r in &doc.rules {
            let m = r.meta.as_ref();
            let retired =
                m.and_then(|m| m.get("status")).and_then(|v| v.as_str()) == Some("retired");
            if !retired && matches(&strings(m, "applies_to"), path) {
                g.rules.insert(format!("{}#{}", doc.path, r.id));
                // A rule's text runs to the next anchor.
                let end = doc
                    .anchors
                    .iter()
                    .map(|(_, l)| *l)
                    .find(|&l| l > r.line)
                    .unwrap_or(usize::MAX);
                ranges.push((r.line, end));
            }
        }
        for link in &doc.links {
            if ranges.iter().any(|&(a, b)| (a..=b).contains(&link.line))
                && let Some(t) = link_target(doc, &link.dest)
            {
                cited.insert(t);
            }
        }
    }

    // `Docs:` lines of the file itself and of its crate root.
    let crates = crates(files);
    let dirs: Vec<&str> = crates.iter().map(|(d, _)| *d).collect();
    let mut code = vec![path.to_owned()];
    if let Some(dir) = owner(path, &dirs)
        && let Some((_, r)) = crates.iter().find(|(d, _)| *d == dir)
    {
        code.push((*r).to_owned());
    }
    for f in code {
        if let Ok(text) = std::fs::read_to_string(root.join(&f))
            && let Some((_, refs)) = docs_line(&text)
        {
            cited.extend(
                refs.iter()
                    .map(|r| r.split('#').next().unwrap_or_default().to_owned()),
            );
        }
    }

    g.adrs = cited
        .into_iter()
        .filter(|p| {
            by_path
                .get(p.as_str())
                .is_some_and(|d| d.is_adr() && d.meta_str("status") != Some("superseded"))
        })
        .collect();
    g
}

/// A human label for a reference: the heading of a section or rule, or a doc's title.
fn label(r: &str, by_path: &BTreeMap<&str, &Doc>) -> Option<String> {
    let (file, anchor) = r.split_once('#').map_or((r, None), |(f, a)| (f, Some(a)));
    let doc = by_path.get(file)?;
    let Some(anchor) = anchor else {
        return doc.meta_str("title").map(str::to_owned);
    };
    let line = doc
        .sections
        .iter()
        .find(|s| s.anchor == anchor)
        .map(|s| s.line)
        .or_else(|| doc.rules.iter().find(|x| x.id == anchor).map(|x| x.line))?;
    let heading = doc.text.lines().nth(line.checked_sub(1)?)?;
    Some(heading.trim_start_matches('#').trim().to_owned())
}

/// Renders the report printed by `docs for`.
pub(crate) fn render(path: &str, g: &Governing, docs: &[Doc]) -> String {
    let by_path: BTreeMap<&str, &Doc> = docs.iter().map(|d| (d.path.as_str(), d)).collect();
    let mut out = format!("What governs {path}:\n");
    let lists: [(&str, &BTreeSet<String>); 3] = [
        ("Sections (covers)", &g.sections),
        ("Rules (applies_to)", &g.rules),
        ("ADRs", &g.adrs),
    ];
    for (title, items) in lists {
        let _ = writeln!(out, "\n{title}:");
        if items.is_empty() {
            out.push_str("  (none)\n");
        }
        for i in items {
            match label(i, &by_path) {
                Some(t) => {
                    let _ = writeln!(out, "  {i}  ({t})");
                }
                None => {
                    let _ = writeln!(out, "  {i}");
                }
            }
        }
    }
    out
}
