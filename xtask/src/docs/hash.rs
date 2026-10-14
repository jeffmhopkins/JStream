//! Hash-bound verification (docs/DOCS.md#verification). A section's hash covers:
//! 1. its own body text, normalized (metadata blocks removed, LF endings, trailing
//!    whitespace stripped, surrounding blank lines trimmed);
//! 2. every tracked file its `covers` globs match, in sorted order: path plus content hash;
//! 3. every `depends_on` target: the reference plus a hash of the target's normalized body
//!    (for a whole ADR, without its generated `adr-status` banner), or of one table row for
//!    a `path#anchor/<key>` reference.
//!
//! Wherever Markdown is hashed, well-formed `@doc`, `@section` and `@rule` blocks are
//! removed first, so recording a verification never stales another section (ADR-0039).
//! In covered workflow files, the commit SHA and version comment of `uses:` pins are
//! ignored, so Dependabot pin bumps don't stale the doc that specifies the workflow
//! (ADR-0018; review N5 on #7).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};

use super::generate::strip_status;
use super::parse::Doc;

/// Prefix of real hashes. Anything else (e.g. the old `"bootstrap"` placeholder) is stale.
pub(crate) const PREFIX: &str = "b3:";

/// How a hash is computed. Only `docs rehash` uses anything but [`Mode::CURRENT`], to tell
/// which stored hashes were fresh before ADR-0039.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mode {
    /// Remove well-formed metadata blocks from hashed Markdown (ADR-0039). `false` is the
    /// rule before ADR-0039.
    pub(crate) strip_metadata: bool,
    /// Hash each row reference `path#anchor/<key>` as its whole section `path#anchor`, as
    /// it was before being narrowed to the row.
    pub(crate) widen_rows: bool,
}

impl Mode {
    /// The rule `docs check` and `docs verify` apply.
    pub(crate) const CURRENT: Self = Self {
        strip_metadata: true,
        widen_rows: false,
    };
}

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

/// The normalized body of a section or rule (by anchor) or, with `anchor = None`, of the whole doc
/// after its `@doc` block, without metadata blocks. A whole ADR's body also leaves out its
/// generated `adr-status` banner, which is not ADR content (ADR-0038), so a status change
/// doesn't stale its dependents.
pub(crate) fn body(doc: &Doc, anchor: Option<&str>) -> Option<String> {
    body_by(doc, anchor, true)
}

/// [`body`]; with `strip = false`, nested metadata blocks are kept (the rule before
/// ADR-0039).
fn body_by(doc: &Doc, anchor: Option<&str>, strip: bool) -> Option<String> {
    let lines: Vec<&str> = doc.text.lines().collect();
    let (start, end) = match anchor {
        None => (doc.content_start, lines.len()),
        // A rule anchor (`R-…`) is not a section; its text is the rule's body (#100).
        Some(a) => doc
            .sections
            .iter()
            .find(|s| s.anchor == a)
            .map(|s| s.body)
            .or_else(|| doc.rules.iter().find(|r| r.id == a).map(|r| r.body))?,
    };
    let (start, end) = (start.min(lines.len()), end.min(lines.len()));
    let meta = doc.metadata_lines();
    let kept: Vec<&str> = (start..end)
        .filter(|&k| !strip || !meta[k])
        .map(|k| lines[k])
        .collect();
    if anchor.is_none() && doc.is_adr() {
        // The banner is never a metadata block, so stripping it after them is the same.
        let text = strip_status(&kept.join("\n"));
        return Some(normalize(&text.lines().collect::<Vec<_>>()));
    }
    Some(normalize(&kept))
}

/// Why a `path#anchor/<key>` row reference does not resolve.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RowError {
    NoSection,
    NoRow,
    Ambiguous,
}

/// The table row of section `anchor` that has a cell consisting exactly of `` `key` ``
/// (ADR-0039), with trailing whitespace stripped. Exactly one row must match.
pub(crate) fn row(doc: &Doc, anchor: &str, key: &str) -> Result<String, RowError> {
    row_by(doc, anchor, key, true)
}

fn row_by(doc: &Doc, anchor: &str, key: &str, strip: bool) -> Result<String, RowError> {
    let body = body_by(doc, Some(anchor), strip).ok_or(RowError::NoSection)?;
    let cell = format!("`{key}`");
    let mut rows = body.lines().filter(|l| {
        let t = l.trim();
        t.starts_with('|')
            && t.trim_matches('|')
                .split('|')
                .any(|c| c.trim() == cell.as_str())
    });
    match (rows.next(), rows.next()) {
        (Some(r), None) => Ok(r.trim_end().to_owned()),
        (None, _) => Err(RowError::NoRow),
        (Some(_), Some(_)) => Err(RowError::Ambiguous),
    }
}

/// A `depends_on` entry split into file, section anchor and row key. Anchors never contain
/// `/`, so everything after the first `/` in the fragment is the key (`apps/*` included).
pub(crate) fn split_dep(dep: &str) -> (&str, Option<&str>, Option<&str>) {
    match dep.split_once('#') {
        None => (dep, None, None),
        Some((file, frag)) => match frag.split_once('/') {
            Some((anchor, key)) => (file, Some(anchor), Some(key)),
            None => (file, Some(frag), None),
        },
    }
}

/// Markdown content without its well-formed metadata blocks (ADR-0039). The file is parsed
/// as a doc whatever its path, so a covered `.md` that is not a tracked doc gets the same
/// treatment. Kept lines are byte-identical, so a file without metadata hashes as before.
fn strip_metadata(path: &str, text: &str) -> String {
    let doc = super::parse::parse(path, text, &mut Vec::new());
    let meta = doc.metadata_lines();
    text.split_inclusive('\n')
        .enumerate()
        .filter(|(k, _)| !meta.get(*k).copied().unwrap_or(false))
        .map(|(_, l)| l)
        .collect()
}

/// File content as hashed: CRLF folded to LF; with `strip`, Markdown metadata blocks
/// removed; workflow `uses:` pins reduced to the action.
fn file_content(path: &str, bytes: &[u8], strip: bool) -> Vec<u8> {
    let mut text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    if strip && path.ends_with(".md") {
        text = strip_metadata(path, &text);
    }
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

/// Compiles `globs` into one matcher.
pub(crate) fn glob_set(globs: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        b.add(Glob::new(g).with_context(|| format!("invalid glob `{g}`"))?);
    }
    Ok(b.build()?)
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
        let set = glob_set(globs)?;
        Ok(self
            .paths
            .iter()
            .filter(|p| set.is_match(p.as_str()))
            .map(String::as_str)
            .collect())
    }

    fn content_hash(&self, path: &str, strip: bool) -> Result<String> {
        let bytes =
            std::fs::read(self.root.join(path)).with_context(|| format!("reading {path}"))?;
        Ok(short(&blake3::hash(&file_content(path, &bytes, strip))))
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
    section_hash_by(
        Mode::CURRENT,
        doc,
        anchor,
        covers,
        depends_on,
        files,
        by_path,
    )
}

/// [`section_hash`] under `mode`.
pub(crate) fn section_hash_by(
    mode: Mode,
    doc: &Doc,
    anchor: &str,
    covers: &[String],
    depends_on: &[String],
    files: &Files<'_>,
    by_path: &BTreeMap<&str, &Doc>,
) -> Result<String> {
    let strip = mode.strip_metadata;
    let mut h = blake3::Hasher::new();
    h.update(b"jstream-docs-v1\0body\0");
    h.update(
        body_by(doc, Some(anchor), strip)
            .unwrap_or_default()
            .as_bytes(),
    );
    for path in files.matching(covers)? {
        h.update(b"\0file\0");
        h.update(path.as_bytes());
        h.update(b"\0");
        h.update(files.content_hash(path, strip)?.as_bytes());
    }
    for dep in depends_on {
        let (file, section, key) = split_dep(dep);
        let (reference, target) = match (section, key) {
            (Some(s), Some(_)) if mode.widen_rows => (
                format!("{file}#{s}"),
                by_path.get(file).and_then(|t| body_by(t, Some(s), strip)),
            ),
            (Some(s), Some(k)) => (
                dep.clone(),
                by_path.get(file).and_then(|t| row_by(t, s, k, strip).ok()),
            ),
            _ => (
                dep.clone(),
                by_path.get(file).and_then(|t| body_by(t, section, strip)),
            ),
        };
        h.update(b"\0dep\0");
        h.update(reference.as_bytes());
        h.update(b"\0");
        h.update(short(&blake3::hash(target.unwrap_or_default().as_bytes())).as_bytes());
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

    fn parse(text: &str) -> Doc {
        super::super::parse::parse("t.md", text, &mut Vec::new())
    }

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
            true,
        );
        let b = file_content(
            ".github/workflows/x.yml",
            b"- uses: actions/checkout@bbbb # v8\nrun: x\n",
            true,
        );
        assert_eq!(a, b);
        let c = file_content("src/x.rs", b"uses: a@1", true);
        let d = file_content("src/x.rs", b"uses: a@2", true);
        assert_ne!(c, d);
    }

    #[test]
    fn section_body_excludes_own_block_and_next_anchor() {
        let text = "<!-- @doc\nid = \"T\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\nverified = { hash = \"x\", by = \"y\", note = \"z\" }\n-->\n\nAlpha text.  \n\n<a id=\"b\"></a>\n## B\n<!-- @section\n-->\nBeta.\n";
        let doc = parse(text);
        assert_eq!(body(&doc, Some("a")).as_deref(), Some("Alpha text."));
        assert_eq!(body(&doc, Some("b")).as_deref(), Some("Beta."));
        assert!(body(&doc, Some("nope")).is_none());
    }

    /// #100: a rule anchor's body is its text, without its `@rule` block, up to the next
    /// rule's or section's anchor.
    #[test]
    fn rule_body_runs_to_the_next_anchor() {
        let text = "<!-- @doc\nid = \"T\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\n-->\nIntro.\n\n<a id=\"R-X-001\"></a>\n### R-X-001: One\n<!-- @rule\nstatus = \"active\"\n-->\nFirst.\n\n<a id=\"R-X-002\"></a>\n### R-X-002: Two\n<!-- @rule\nstatus = \"active\"\n-->\nSecond.\n\n<a id=\"b\"></a>\n## B\n<!-- @section\n-->\nBeta.\n";
        let doc = parse(text);
        assert_eq!(body(&doc, Some("R-X-001")).as_deref(), Some("First."));
        assert_eq!(body(&doc, Some("R-X-002")).as_deref(), Some("Second."));
        assert!(body(&doc, Some("R-X-003")).is_none());
    }

    const MD: &str = "<!-- @doc\nid = \"T\"\nreviewed = { by = \"human:x\", note = \"one\" }\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\nverified = { hash = \"b3:1\", by = \"agent:a\", note = \"first\" }\n-->\nAlpha.\n\n<a id=\"R-X-001\"></a>\n### R-X-001: Rule\n<!-- @rule\nstatus = \"active\"\n-->\nRule text.\n";

    /// ADR-0039: metadata blocks of a covered Markdown file are not hashed; prose is,
    /// byte for byte.
    #[test]
    fn markdown_metadata_is_not_hashed_but_content_is() {
        let hash = |t: &str| file_content("d/T.md", t.as_bytes(), true);
        let base = hash(MD);
        assert_eq!(
            String::from_utf8(base.clone()).unwrap(),
            "<a id=\"a\"></a>\n## A\nAlpha.\n\n<a id=\"R-X-001\"></a>\n### R-X-001: Rule\nRule text.\n"
        );
        for meta_edit in [
            MD.replace("b3:1", "b3:2").replace("first", "second"),
            MD.replace("note = \"one\"", "note = \"two\""),
            MD.replace("status = \"active\"", "status = \"retired\""),
        ] {
            assert_eq!(hash(&meta_edit), base, "{meta_edit}");
        }
        assert_ne!(hash(&MD.replace("Alpha.", "Alpha!")), base);
        assert_ne!(hash(&MD.replace("Rule text.", "Rule text!")), base);
        // The legacy rule hashes the file whole.
        assert_ne!(
            file_content("d/T.md", MD.replace("first", "second").as_bytes(), false),
            file_content("d/T.md", MD.as_bytes(), false)
        );
    }

    /// ADR-0039: text in anything that is not a well-formed, parser-recognised metadata
    /// block still counts: a look-alike in prose, one with text after its marker, one in a
    /// code fence, one whose TOML does not parse.
    #[test]
    fn fake_or_malformed_metadata_still_counts() {
        let hash = |t: &str| file_content("d/T.md", t.as_bytes(), true);
        let fakes = [
            // Not right after a heading.
            "\nProse.\n<!-- @section\nhidden = \"X\"\n-->\n",
            // Text after the opening marker.
            "\n<a id=\"b\"></a>\n## B\n<!-- @section hidden X\n-->\n",
            // Inside a code fence.
            "\n```\n<a id=\"c\"></a>\n## C\n<!-- @section\nhidden = \"X\"\n-->\n```\n",
            // Not TOML.
            "\n<a id=\"d\"></a>\n## D\n<!-- @section\nhidden X\n-->\n",
            // A rule block after a non-rule heading.
            "\n### Not a rule\n<!-- @rule\nhidden = \"X\"\n-->\n",
        ];
        for fake in fakes {
            let a = hash(&format!("{MD}{fake}"));
            let b = hash(&format!("{MD}{}", fake.replace('X', "Y")));
            assert_ne!(a, b, "hidden text escaped the hash in {fake:?}");
        }
        // A `@doc` look-alike that is not at the very start of the file is content too.
        let late = |x: &str| hash(&format!("\n<!-- @doc\nid = \"{x}\"\n-->\nText.\n"));
        assert_ne!(late("X"), late("Y"));
    }

    /// ADR-0039: `path#anchor/<key>` resolves to the one row with a cell `` `key` ``.
    #[test]
    fn rows_resolve_by_exact_code_cell() {
        let doc = parse(
            "<!-- @doc\nid = \"T\"\n-->\n<a id=\"map\"></a>\n## Map\n<!-- @section\n-->\n| L | Crate | What |\n|---|---|---|\n| 0 | `a` | uses `b` |\n| 1 | `b` | B  \n| 2 | `apps/*` | apps |\n| 3 | `dup` | x |\n| 4 | `dup` | y |\n",
        );
        assert_eq!(
            row(&doc, "map", "a").as_deref(),
            Ok("| 0 | `a` | uses `b` |")
        );
        assert_eq!(row(&doc, "map", "b").as_deref(), Ok("| 1 | `b` | B"));
        assert_eq!(
            row(&doc, "map", "apps/*").as_deref(),
            Ok("| 2 | `apps/*` | apps |")
        );
        assert_eq!(row(&doc, "map", "c"), Err(RowError::NoRow));
        assert_eq!(row(&doc, "map", "dup"), Err(RowError::Ambiguous));
        assert_eq!(row(&doc, "nope", "a"), Err(RowError::NoSection));
        assert_eq!(
            split_dep("docs/A.md#crate-map/apps/*"),
            ("docs/A.md", Some("crate-map"), Some("apps/*"))
        );
        assert_eq!(split_dep("docs/A.md#x"), ("docs/A.md", Some("x"), None));
        assert_eq!(split_dep("docs/A.md"), ("docs/A.md", None, None));
    }

    /// A whole ADR hashes the same with or without its generated banner (ADR-0038); a
    /// section body never drops text.
    #[test]
    fn whole_adr_body_ignores_the_status_banner() {
        let head = "<!-- @doc\nid = \"ADR-0001\"\nkind = \"adr\"\nstatus = \"accepted\"\n-->\n\n# ADR-0001: T\n\n";
        let banner = "<!-- @generated begin adr-status -->\n> **Status:** accepted\n<!-- @generated end adr-status -->\n\n";
        let rest = "<a id=\"context\"></a>\n## Context\nWhy.\n";
        let parse = |t: &str| super::super::parse::parse("a.md", t, &mut Vec::new());
        let (bare, with) = (
            parse(&format!("{head}{rest}")),
            parse(&format!("{head}{banner}{rest}")),
        );
        assert_eq!(body(&bare, None), body(&with, None));
        assert_eq!(body(&with, Some("context")).as_deref(), Some("Why."));
        let note = parse(&format!(
            "{}{banner}{rest}",
            head.replace("\"adr\"", "\"normative\"")
        ));
        assert!(body(&note, None).is_some_and(|b| b.contains("@generated")));
    }

    #[test]
    fn toml_strings_round_trip() {
        for v in [r#"say "hi""#, "back\\slash", "plain"] {
            let t: toml::Table = format!("k = {}", toml_str(v)).parse().unwrap();
            assert_eq!(t["k"].as_str(), Some(v));
        }
    }
}
