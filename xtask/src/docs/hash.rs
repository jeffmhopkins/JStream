//! Hash-bound verification (docs/DOCS.md#verification). A section's hash covers:
//! 1. its own body text, normalized (metadata block removed, LF endings, trailing
//!    whitespace stripped, surrounding blank lines trimmed);
//! 2. every tracked file its `covers` globs match, in sorted order: path plus content hash;
//! 3. every `depends_on` target: the reference plus a hash of the target's normalized body.
//!
//! In covered workflow files, the commit SHA and version comment of `uses:` pins are
//! ignored, so Dependabot pin bumps don't stale the doc that specifies the workflow
//! (ADR-0018; review N5 on #7).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};

use super::parse::Doc;

/// Prefix of real hashes. Anything else (e.g. the old `"bootstrap"` placeholder) is stale.
pub(crate) const PREFIX: &str = "b3:";

/// Normalizes a block of lines: trailing whitespace stripped, outer blank lines dropped.
fn normalize(lines: &[&str]) -> String {
    let trimmed: Vec<&str> = lines.iter().map(|l| l.trim_end()).collect();
    let start = trimmed
        .iter()
        .position(|l| !l.is_empty())
        .unwrap_or(trimmed.len());
    let end = trimmed
        .iter()
        .rposition(|l| !l.is_empty())
        .map_or(start, |e| e + 1);
    trimmed[start..end].join("\n")
}

/// The normalized body of a section (by anchor) or, with `anchor = None`, of the whole doc
/// after its `@doc` block.
pub(crate) fn body(doc: &Doc, anchor: Option<&str>) -> Option<String> {
    let lines: Vec<&str> = doc.text.lines().collect();
    let (start, end) = match anchor {
        None => (doc.content_start, lines.len()),
        Some(a) => doc.sections.iter().find(|s| s.anchor == a)?.body,
    };
    Some(normalize(
        &lines[start.min(lines.len())..end.min(lines.len())],
    ))
}

/// File content as hashed: CRLF folded to LF; workflow `uses:` pins reduced to the action.
fn file_content(path: &str, bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    if !path.starts_with(".github/workflows/") {
        return text.into_bytes();
    }
    text.lines()
        .map(|l| {
            match l
                .find("uses:")
                .and_then(|u| l[u..].find('@').map(|at| u + at))
            {
                Some(at) => &l[..at],
                None => l,
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

fn short(h: &blake3::Hash) -> String {
    h.to_hex().as_str()[..16].to_owned()
}

/// Repository files, indexed for glob matching and hashing.
pub(crate) struct Files<'a> {
    root: &'a Path,
    paths: &'a [String],
}

impl<'a> Files<'a> {
    pub(crate) fn new(root: &'a Path, paths: &'a [String]) -> Self {
        Self { root, paths }
    }

    /// Every tracked file, sorted.
    pub(crate) fn paths(&self) -> &'a [String] {
        self.paths
    }

    /// Tracked files matched by any of `globs`, sorted.
    pub(crate) fn matching(&self, globs: &[String]) -> Result<Vec<&'a str>> {
        let mut b = GlobSetBuilder::new();
        for g in globs {
            b.add(Glob::new(g).with_context(|| format!("invalid glob `{g}`"))?);
        }
        let set = b.build()?;
        Ok(self
            .paths
            .iter()
            .filter(|p| set.is_match(p.as_str()))
            .map(String::as_str)
            .collect())
    }

    fn content_hash(&self, path: &str) -> Result<String> {
        let bytes =
            std::fs::read(self.root.join(path)).with_context(|| format!("reading {path}"))?;
        Ok(short(&blake3::hash(&file_content(path, &bytes))))
    }
}

/// Computes the hash for a section from its body, covers and depends_on.
pub(crate) fn section_hash(
    doc: &Doc,
    anchor: &str,
    covers: &[String],
    depends_on: &[String],
    files: &Files<'_>,
    by_path: &BTreeMap<&str, &Doc>,
) -> Result<String> {
    let mut h = blake3::Hasher::new();
    h.update(b"jstream-docs-v1\0body\0");
    h.update(body(doc, Some(anchor)).unwrap_or_default().as_bytes());
    for path in files.matching(covers)? {
        h.update(b"\0file\0");
        h.update(path.as_bytes());
        h.update(b"\0");
        h.update(files.content_hash(path)?.as_bytes());
    }
    for dep in depends_on {
        let (file, frag) = dep
            .split_once('#')
            .map_or((dep.as_str(), None), |(f, a)| (f, Some(a)));
        let target = by_path
            .get(file)
            .and_then(|t| body(t, frag))
            .unwrap_or_default();
        h.update(b"\0dep\0");
        h.update(dep.as_bytes());
        h.update(b"\0");
        h.update(short(&blake3::hash(target.as_bytes())).as_bytes());
    }
    Ok(format!("{PREFIX}{}", short(&h.finalize())))
}

/// Escapes a string as a TOML basic string (with quotes).
pub(crate) fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims_and_strips() {
        assert_eq!(normalize(&["", "  a  ", "b\t", "", ""]), "  a\nb");
        assert_eq!(normalize(&["", ""]), "");
    }

    #[test]
    fn workflow_pins_are_ignored_other_files_are_not() {
        let a = file_content(
            ".github/workflows/x.yml",
            b"- uses: actions/checkout@aaaa # v7\nrun: x\r\n",
        );
        let b = file_content(
            ".github/workflows/x.yml",
            b"- uses: actions/checkout@bbbb # v8\nrun: x\n",
        );
        assert_eq!(a, b);
        let c = file_content("src/x.rs", b"uses: a@1");
        let d = file_content("src/x.rs", b"uses: a@2");
        assert_ne!(c, d);
    }

    #[test]
    fn section_body_excludes_own_block_and_next_anchor() {
        let text = "<!-- @doc\nid = \"T\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\nverified = { hash = \"x\", by = \"y\", note = \"z\" }\n-->\n\nAlpha text.  \n\n<a id=\"b\"></a>\n## B\n<!-- @section\n-->\nBeta.\n";
        let mut diags = Vec::new();
        let doc = super::super::parse::parse("t.md", text, &mut diags);
        assert_eq!(body(&doc, Some("a")).as_deref(), Some("Alpha text."));
        assert_eq!(body(&doc, Some("b")).as_deref(), Some("Beta."));
        assert!(body(&doc, Some("nope")).is_none());
    }

    #[test]
    fn toml_strings_round_trip() {
        for v in [r#"say "hi""#, "back\\slash", "plain"] {
            let t: toml::Table = format!("k = {}", toml_str(v)).parse().unwrap();
            assert_eq!(t["k"].as_str(), Some(v));
        }
    }
}
