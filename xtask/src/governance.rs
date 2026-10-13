//! `cargo xtask governance --base <rev> [--event <path>]` (docs/DOCS.md#governance).
//!
//! Lists the governance changes between the merge base of `<rev>` and `HEAD` (that is,
//! `git diff <rev>...HEAD`). Given a `pull_request` event payload (`--event`, or
//! `GITHUB_EVENT_PATH` in Actions), it fails unless the PR carries the `governance-approved`
//! label and a non-empty "Governance change" section (R-GOV-001), or is a Dependabot PR that
//! only changes pinned `uses:` references (ADR-0018). Without a payload it only lists.
//!
//! Docs: docs/DOCS.md#governance, docs/adr/0018-ci-supply-chain-policy.md, docs/adr/0019-review-gate-v2.md

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::docs::is_tracked;
use crate::docs::parse::{Doc, parse};

/// The label that approves governance changes.
const LABEL: &str = "governance-approved";
/// The PR description section that justifies them.
const SECTION: &str = "Governance change";
/// The author of Dependabot PRs.
const DEPENDABOT: &str = "dependabot[bot]";

/// Files whose every edit is a governance change.
const PROTECTED_FILES: &[&str] = &[
    "deny.toml",
    "clippy.toml",
    "rustfmt.toml",
    "rust-toolchain.toml",
    "docs/DOCS.md",
    "REVIEW.md",
    "xtask/src/governance.rs",
    "xtask/src/lint_deps.rs",
];
/// Directories whose every edit is a governance change.
const PROTECTED_DIRS: &[&str] = &[".github/", "xtask/src/docs/"];

/// Metadata keys whose edits are governance changes, in `@doc`, `@section` and `@rule` blocks.
const KEYS: &[&str] = &["covers", "applies_to", "enforced_by", "status"];

/// One changed file: its contents at the merge base and at `HEAD` (`None`: absent).
#[derive(Debug)]
struct FileChange {
    path: String,
    base: Option<String>,
    head: Option<String>,
}

/// What a governance change requires beyond the label and justification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Nothing more.
    Plain,
    /// A change to an accepted ADR's metadata container, allowed only under a repo-wide
    /// format ADR (R-DOC-009): the justification must cite an accepted ADR.
    Container,
}

/// One governance change, as printed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    path: String,
    what: String,
    kind: Kind,
}

/// The pull request, from the event payload.
#[derive(Debug, Default)]
struct Pr {
    labels: Vec<String>,
    body: String,
    author: String,
}

/// The outcome for a pull request.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Pass(String),
    Fail(Vec<String>),
}

fn push(out: &mut Vec<Change>, path: &str, what: String) {
    out.push(Change {
        path: path.to_owned(),
        what,
        kind: Kind::Plain,
    });
}

/// Classifies every changed file. Pure: the git layer only gathers `files`.
fn classify(files: &[FileChange]) -> Vec<Change> {
    let mut out = Vec::new();
    for f in files {
        let p = f.path.as_str();
        if PROTECTED_FILES.contains(&p) || PROTECTED_DIRS.iter().any(|d| p.starts_with(d)) {
            let verb = match (&f.base, &f.head) {
                (None, _) => "adds",
                (_, None) => "deletes",
                _ => "edits",
            };
            push(&mut out, p, format!("{verb} a protected file"));
        }
        if p == "Cargo.toml" || p.ends_with("/Cargo.toml") {
            lints(f, &mut out);
        }
        if is_tracked(p) {
            doc_meta(f, &mut out);
        }
        if p.ends_with(".rs")
            && let Some(head) = &f.head
        {
            new_suppressions(p, f.base.as_deref(), head, &mut out);
        }
    }
    out
}

/// The value at `path` in a TOML table.
fn lookup<'t>(t: Option<&'t toml::Table>, path: &[&str]) -> Option<&'t toml::Value> {
    let (first, rest) = path.split_first()?;
    let mut v = t?.get(*first)?;
    for k in rest {
        v = v.get(k)?;
    }
    Some(v)
}

/// `[workspace.lints]` edits, and crate `[lints]` tables that stop being exactly
/// `workspace = true` (a crate's own table can loosen the workspace lints).
fn lints(f: &FileChange, out: &mut Vec<Change>) {
    let Some(head) = &f.head else {
        return; // a deleted crate takes its lints with it
    };
    let table = |s: &str| s.parse::<toml::Table>().ok();
    let (b, h) = (f.base.as_deref().and_then(table), table(head));
    if lookup(b.as_ref(), &["workspace", "lints"]) != lookup(h.as_ref(), &["workspace", "lints"]) {
        push(out, &f.path, "edits `[workspace.lints]`".into());
    }
    let new = lookup(h.as_ref(), &["lints"]);
    let inherits = new
        .and_then(toml::Value::as_table)
        .is_some_and(|t| t.len() == 1 && t.get("workspace") == Some(&toml::Value::Boolean(true)));
    if lookup(b.as_ref(), &["lints"]) != new && !inherits {
        push(
            out,
            &f.path,
            "edits the crate's `[lints]` table (not `workspace = true`)".into(),
        );
    }
}

/// A metadata value for messages.
fn show(v: Option<&toml::Value>) -> String {
    v.map_or_else(|| "(none)".to_owned(), ToString::to_string)
}

/// Edits of `KEYS` between two metadata blocks, as "`key`: old → new".
fn key_edits(base: Option<&toml::Table>, head: Option<&toml::Table>) -> Vec<String> {
    KEYS.iter()
        .filter_map(|k| {
            let (b, h) = (base.and_then(|t| t.get(*k)), head.and_then(|t| t.get(*k)));
            (b != h).then(|| format!("`{k}`: {} → {}", show(b), show(h)))
        })
        .collect()
}

/// Whether `doc` is an accepted ADR.
fn accepted_adr(doc: &Doc) -> bool {
    doc.is_adr() && doc.meta_str("status") == Some("accepted")
}

/// Governance-relevant metadata changes in a tracked doc: `KEYS` edits in any block,
/// deleted docs, sections and rules, and any edit to an accepted ADR.
fn doc_meta(f: &FileChange, out: &mut Vec<Change>) {
    let Some(base_text) = &f.base else {
        return; // a new doc edits no existing metadata
    };
    let p = f.path.as_str();
    let base = parse(p, base_text, &mut Vec::new());
    let Some(head_text) = &f.head else {
        push(out, p, "deletes a tracked doc (retire it instead)".into());
        if accepted_adr(&base) {
            push(out, p, "deletes an accepted ADR".into());
        }
        return;
    };
    let head = parse(p, head_text, &mut Vec::new());
    // A block that no longer parses (`docs check` fails on it) is left to `docs check`, and,
    // for accepted ADRs, to the container check below.
    if base.meta.is_some() && head.meta.is_some() {
        for e in key_edits(base.meta.as_ref(), head.meta.as_ref()) {
            push(out, p, format!("`@doc` {e}"));
        }
    }
    for s in &base.sections {
        match head.sections.iter().find(|h| h.anchor == s.anchor) {
            None => push(
                out,
                p,
                format!("section `#{}` deleted or retired", s.anchor),
            ),
            Some(h) => {
                for e in key_edits(s.meta.as_ref(), h.meta.as_ref()) {
                    push(out, p, format!("section `#{}` {e}", s.anchor));
                }
            }
        }
    }
    for r in &base.rules {
        match head.rules.iter().find(|h| h.id == r.id) {
            None => push(out, p, format!("rule `{}` deleted", r.id)),
            Some(h) => {
                for e in key_edits(r.meta.as_ref(), h.meta.as_ref()) {
                    push(out, p, format!("rule `{}` {e}", r.id));
                }
            }
        }
    }
    if accepted_adr(&base) && base_text != head_text {
        adr_edits(p, base_text, head_text, out);
    }
}

/// Splits a doc into its metadata container (an HTML comment, or `+++`/`---` front matter,
/// at the very start) and the content after it.
fn split_container(text: &str) -> (&str, &str) {
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return ("", text);
    };
    let close: Box<dyn Fn(&str) -> bool> = if first.starts_with("<!--") {
        if first.trim_end().ends_with("-->") && first.trim() != "<!--" {
            return text.split_at(first.len());
        }
        Box::new(|l: &str| l.trim_end().ends_with("-->"))
    } else if matches!(first.trim_end(), "+++" | "---") {
        let delim = first.trim_end().to_owned();
        Box::new(move |l: &str| l.trim_end() == delim)
    } else {
        return ("", text);
    };
    let mut end = first.len();
    for l in lines {
        end += l.len();
        if close(l) {
            return text.split_at(end);
        }
    }
    (text, "")
}

/// The TOML inside a container (between its first and last line), if it parses.
fn container_toml(container: &str) -> Option<toml::Table> {
    let lines: Vec<&str> = container.lines().collect();
    let inner = lines.get(1..lines.len().checked_sub(1)?)?;
    inner.join("\n").parse().ok()
}

/// The first and last line of a container: its delimiters.
fn delimiters(container: &str) -> (Option<&str>, Option<&str>) {
    (container.lines().next(), container.lines().last())
}

/// Classifies an edit to an accepted ADR (R-DOC-009). `status` edits are reported by
/// `key_edits`; everything else is reported here.
fn adr_edits(p: &str, base: &str, head: &str, out: &mut Vec<Change>) {
    let (bc, bb) = split_container(base);
    let (hc, hb) = split_container(head);
    let before = out.len();
    if bb != hb {
        push(
            out,
            p,
            "edits an accepted ADR's content (R-DOC-009 allows only `status` and `superseded_by`)"
                .into(),
        );
    }
    let mut status_only = false;
    if bc != hc {
        let (bt, ht) = (container_toml(bc), container_toml(hc));
        if bt.is_none() || ht.is_none() || delimiters(bc) != delimiters(hc) {
            out.push(Change {
                path: p.to_owned(),
                what: "changes an accepted ADR's metadata container (R-DOC-009 exception: \
                       the justification must cite the accepted repo-wide format ADR)"
                    .into(),
                kind: Kind::Container,
            });
        }
        if let (Some(b), Some(h)) = (&bt, &ht) {
            let keys: BTreeSet<&String> = b.keys().chain(h.keys()).collect();
            for k in keys {
                if k != "status" && b.get(k) != h.get(k) {
                    push(out, p, format!("edits an accepted ADR's `{k}`"));
                }
            }
            status_only = b.get("status") != h.get("status");
        }
    }
    if out.len() == before && !status_only {
        push(
            out,
            p,
            "edits an accepted ADR (formatting inside its metadata block)".into(),
        );
    }
}

/// Length of the attribute at the start of `s` through its closing `]`, skipping string
/// literals.
fn attr_len(s: &str) -> Option<usize> {
    let (mut depth, mut in_str, mut escaped) = (0usize, false, false);
    for (k, c) in s.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' => depth += 1,
            ']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(k + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// `#[allow(…)]` / `#[expect(…)]` attributes (outer or inner) in Rust source, with
/// whitespace collapsed, and their 1-based line. `//` comment lines are skipped. This is a
/// text scan: attribute text inside string literals counts too.
fn suppressions(src: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut offset = 0;
    for (i, line) in src.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += line.len();
        if line.trim_start().starts_with("//") {
            continue;
        }
        for (j, _) in line.match_indices('#') {
            let at = start + j;
            let rest = &src[at + 1..];
            let rest = rest.strip_prefix('!').unwrap_or(rest);
            let Some(inner) = rest.strip_prefix('[') else {
                continue;
            };
            let name = inner.trim_start();
            let Some(args) = name
                .strip_prefix("allow")
                .or_else(|| name.strip_prefix("expect"))
            else {
                continue;
            };
            if !args.trim_start().starts_with('(') {
                continue;
            }
            if let Some(len) = attr_len(&src[at..]) {
                let attr = src[at..at + len].split_whitespace().collect::<Vec<_>>();
                out.push((attr.join(" "), i + 1));
            }
        }
    }
    out
}

/// Suppression attributes in `head` that `base` doesn't have (compared as a multiset).
fn new_suppressions(p: &str, base: Option<&str>, head: &str, out: &mut Vec<Change>) {
    let mut old: BTreeMap<String, usize> = BTreeMap::new();
    for (a, _) in suppressions(base.unwrap_or_default()) {
        *old.entry(a).or_default() += 1;
    }
    for (a, line) in suppressions(head) {
        if let Some(n) = old.get_mut(&a).filter(|n| **n > 0) {
            *n -= 1;
            continue;
        }
        let reason = if a.contains("reason =") || a.contains("reason=") {
            ""
        } else {
            " (missing `reason = \"…\"`)"
        };
        push(out, p, format!("line {line}: new `{a}`{reason}"));
    }
}

/// For a `uses: <action>@<ref>` line: the text before `@`, and whether `<ref>` is a full
/// commit SHA.
fn pin(line: &str) -> Option<(&str, bool)> {
    let u = line.find("uses:")?;
    let at = u + line[u..].find('@')?;
    let r = line[at + 1..].split_whitespace().next().unwrap_or_default();
    let sha = r.len() == 40 && r.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    Some((&line[..at], sha))
}

/// Whether every change is a modified workflow file whose only differing lines re-pin the
/// same action to a full commit SHA (ADR-0018's pre-approval).
fn pin_only(files: &[FileChange]) -> bool {
    !files.is_empty()
        && files.iter().all(|f| {
            let (Some(base), Some(head)) = (&f.base, &f.head) else {
                return false;
            };
            let (b, h): (Vec<&str>, Vec<&str>) = (base.lines().collect(), head.lines().collect());
            f.path.starts_with(".github/workflows/")
                && b.len() == h.len()
                && b.iter().zip(&h).all(|(x, y)| {
                    x == y
                        || matches!((pin(x), pin(y)), (Some((xa, _)), Some((ya, true))) if xa == ya)
                })
        })
}

/// The text of the description's first `## <title>…` heading's section, with HTML comments
/// removed and trimmed; empty when absent. Mirrors `section()` in
/// .github/workflows/review-gate.yml: the title matches case-insensitively as a prefix of
/// the heading text, and the section ends at the next `## ` heading.
fn section(body: &str, title: &str) -> String {
    let lines: Vec<&str> = body.split('\n').collect();
    let title = title.to_lowercase();
    let is_heading = |l: &str| {
        l.strip_prefix("##").is_some_and(|r| {
            let t = r.trim_start();
            t.len() < r.len() && t.to_lowercase().starts_with(&title)
        })
    };
    // A heading must be followed by a newline, so it can't be the last segment.
    let Some(start) = (0..lines.len().saturating_sub(1)).find(|&i| is_heading(lines[i])) else {
        return String::new();
    };
    let ends = |k: usize| {
        lines[k].strip_prefix("##").is_some_and(|r| {
            r.starts_with(char::is_whitespace) || (r.is_empty() && k + 1 < lines.len())
        })
    };
    let end = (start + 1..lines.len())
        .find(|&k| ends(k))
        .unwrap_or(lines.len());
    let mut text = lines[start + 1..end].join("\n");
    while let Some(open) = text.find("<!--") {
        let Some(close) = text[open..].find("-->") else {
            break;
        };
        text.replace_range(open..open + close + 3, "");
    }
    text.trim().to_owned()
}

/// `ADR-NNNN` ids mentioned in `text`.
fn adr_ids(text: &str) -> BTreeSet<String> {
    text.match_indices("ADR-")
        .filter_map(|(i, _)| {
            let digits = text.get(i + 4..i + 8)?;
            let after = text[i + 8..].chars().next();
            (digits.bytes().all(|b| b.is_ascii_digit())
                && !after.is_some_and(|c| c.is_ascii_digit()))
            .then(|| format!("ADR-{digits}"))
        })
        .collect()
}

/// The verdict for a pull request. `accepted` holds the ids of the accepted ADRs at `HEAD`
/// (only consulted for metadata-container changes).
fn decide(changes: &[Change], pr: &Pr, pin_only: bool, accepted: &BTreeSet<String>) -> Verdict {
    if changes.is_empty() {
        return Verdict::Pass("no governance changes".into());
    }
    if pr.author == DEPENDABOT && pin_only {
        return Verdict::Pass(
            "pre-approved: Dependabot PR that only re-pins `uses:` references (ADR-0018)".into(),
        );
    }
    let mut problems = Vec::new();
    if !pr.labels.iter().any(|l| l == LABEL) {
        problems.push(format!("the PR lacks the `{LABEL}` label"));
    }
    let justification = section(&pr.body, SECTION);
    if justification.is_empty() {
        problems.push(format!(
            "the PR description has no non-empty \"## {SECTION}\" section (HTML comments don't count)"
        ));
    }
    if changes.iter().any(|c| c.kind == Kind::Container)
        && adr_ids(&justification).is_disjoint(accepted)
    {
        problems.push(format!(
            "an accepted ADR's metadata container changed: the \"## {SECTION}\" section must \
             cite the accepted repo-wide format ADR as `ADR-NNNN` (R-DOC-009)"
        ));
    }
    if problems.is_empty() {
        Verdict::Pass(format!("labeled `{LABEL}` and justified"))
    } else {
        Verdict::Fail(problems)
    }
}

/// The pull request in a GitHub event payload; `None` for other events.
fn pr_from_event(json: &str) -> Result<Option<Pr>> {
    let v: serde_json::Value = serde_json::from_str(json).context("parsing the event payload")?;
    let Some(pr) = v.get("pull_request") else {
        return Ok(None);
    };
    let text = |p: &str| {
        pr.pointer(p)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let labels = pr
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| l.get("name")?.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(Pr {
        labels,
        body: text("/body"),
        author: text("/user/login"),
    }))
}

/// Runs git in `root` and returns its stdout.
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("running `git {}`", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// The merge base of `base` and `HEAD`.
fn merge_base(root: &Path, base: &str) -> Result<String> {
    let out = git(root, &["merge-base", base, "HEAD"]).with_context(|| {
        format!("finding the merge base of `{base}` and HEAD (full history needed)")
    })?;
    Ok(String::from_utf8(out)?.trim().to_owned())
}

/// A file's contents at `rev`.
fn show_file(root: &Path, rev: &str, path: &str) -> Result<String> {
    let out = git(root, &["show", &format!("{rev}:{path}")])?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// The files changed between `mb` and `HEAD`, renames split into delete plus add.
fn changed_files(root: &Path, mb: &str) -> Result<Vec<FileChange>> {
    let out = git(
        root,
        &["diff", "--name-status", "-z", "--no-renames", mb, "HEAD"],
    )?;
    let out = String::from_utf8(out).context("git paths are not UTF-8")?;
    let mut fields = out.split('\0').filter(|s| !s.is_empty());
    let mut files = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        let base = if status == "A" {
            None
        } else {
            Some(show_file(root, mb, path)?)
        };
        let head = if status == "D" {
            None
        } else {
            Some(show_file(root, "HEAD", path)?)
        };
        files.push(FileChange {
            path: path.to_owned(),
            base,
            head,
        });
    }
    Ok(files)
}

/// Ids of the accepted ADRs at `HEAD`.
fn accepted_adrs(root: &Path) -> Result<BTreeSet<String>> {
    let out = git(
        root,
        &[
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            "HEAD",
            "--",
            "docs/adr",
        ],
    )?;
    let mut ids = BTreeSet::new();
    for p in String::from_utf8(out)?
        .split('\0')
        .filter(|p| is_tracked(p))
    {
        let doc = parse(p, &show_file(root, "HEAD", p)?, &mut Vec::new());
        if accepted_adr(&doc)
            && let Some(id) = doc.meta_str("id")
        {
            ids.insert(id.to_owned());
        }
    }
    Ok(ids)
}

/// Renders the list of governance changes.
fn render(mb: &str, changes: &[Change]) -> String {
    let short = mb.get(..12).unwrap_or(mb);
    if changes.is_empty() {
        return format!("governance: no governance changes since {short}\n");
    }
    let mut out = format!(
        "governance: {} governance change(s) since {short} (docs/DOCS.md#governance):\n",
        changes.len()
    );
    for c in changes {
        let _ = writeln!(out, "  {}: {}", c.path, c.what);
    }
    out
}

/// Entry point for `cargo xtask governance <args>`.
pub(crate) fn run(args: &[String], root: &Path) -> Result<bool> {
    let usage = "usage: cargo xtask governance --base <rev> [--event <event.json>]";
    let (mut base, mut event) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--base" => base = Some(it.next().context(usage)?),
            "--event" => event = Some(PathBuf::from(it.next().context(usage)?)),
            other => bail!("unexpected argument `{other}`\n{usage}"),
        }
    }
    let base = base.context(usage)?;
    let event = event.or_else(|| {
        std::env::var_os("GITHUB_EVENT_PATH")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    let pr = match &event {
        Some(p) => {
            let json =
                std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
            pr_from_event(&json)?
        }
        None => None,
    };

    let mb = merge_base(root, base)?;
    let files = changed_files(root, &mb)?;
    let changes = classify(&files);
    eprint!("{}", render(&mb, &changes));

    let Some(pr) = pr else {
        eprintln!("governance: no pull-request payload, so nothing is enforced (listing only)");
        return Ok(true);
    };
    let accepted = if changes.iter().any(|c| c.kind == Kind::Container) {
        accepted_adrs(root)?
    } else {
        BTreeSet::new()
    };
    match decide(&changes, &pr, pin_only(&files), &accepted) {
        Verdict::Pass(why) => {
            eprintln!("governance: ok ({why})");
            Ok(true)
        }
        Verdict::Fail(problems) => {
            for p in &problems {
                eprintln!("governance: {p}");
            }
            eprintln!(
                "governance: failed; see docs/DOCS.md#governance and docs/RULES.md#R-GOV-001"
            );
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway git repository with a `base` tag, removed on drop even when an assertion
    /// fails.
    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Fixture {
        /// Commits `files` into a fresh repo named after the test and tags it `base`.
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!("xtask-gov-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let f = Self(dir);
            for (path, text) in files {
                f.write(path, text);
            }
            f.git(&["init", "-q"]);
            f.commit();
            f.git(&["tag", "base"]);
            f
        }

        fn write(&self, path: &str, text: &str) {
            let p = self.0.join(path);
            std::fs::create_dir_all(p.parent().expect("invariant: files live in the repo"))
                .expect("invariant: temp dir is writable");
            std::fs::write(p, text).expect("invariant: temp dir is writable");
        }

        fn git(&self, args: &[&str]) {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&self.0)
                .output()
                .is_ok_and(|o| o.status.success());
            assert!(ok, "git {args:?} failed");
        }

        fn commit(&self) {
            self.git(&["add", "-A"]);
            self.git(&[
                "-c",
                "user.name=xtask",
                "-c",
                "user.email=xtask@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "change",
            ]);
        }

        /// Writes `files`, commits, and returns the changes and files since `base`.
        fn change(&self, files: &[(&str, &str)]) -> (Vec<Change>, Vec<FileChange>) {
            for (path, text) in files {
                self.write(path, text);
            }
            self.commit();
            let mb = merge_base(&self.0, "base").expect("invariant: base is an ancestor");
            let files = changed_files(&self.0, &mb).expect("invariant: the fixture is readable");
            (classify(&files), files)
        }
    }

    fn whats(changes: &[Change]) -> Vec<String> {
        changes
            .iter()
            .map(|c| format!("{}: {}", c.path, c.what))
            .collect()
    }

    fn pr(labels: &[&str], body: &str, author: &str) -> Pr {
        Pr {
            labels: labels.iter().map(|l| (*l).to_owned()).collect(),
            body: body.to_owned(),
            author: author.to_owned(),
        }
    }

    const JUSTIFIED: &str = "## What\nx\n\n## Governance change\nTightens the gate because…\n";

    const RULES: &str = "<!-- @doc\nid = \"RULES\"\nkind = \"normative\"\nstatus = \"active\"\n-->\n<a id=\"s\"></a>\n## S\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n\n<a id=\"R-X-001\"></a>\n### R-X-001: Thing\n<!-- @rule\nstatus = \"active\"\nenforced_by = [\"review\"]\napplies_to = [\"src/**\"]\n-->\nRule.\n";

    const ADR: &str = "<!-- @doc\nid = \"ADR-0001\"\ntitle = \"One\"\nkind = \"adr\"\nstatus = \"accepted\"\n-->\n\n# ADR-0001: One\n\n<a id=\"decision\"></a>\n## Decision\nWe do X.\n";

    #[test]
    fn lint_config_changes_are_flagged() {
        let cargo =
            "[workspace]\nmembers = [\"c\"]\n\n[workspace.lints.clippy]\nunwrap_used = \"deny\"\n";
        let f = Fixture::new(
            "lints",
            &[
                ("clippy.toml", "allow-unwrap-in-tests = true\n"),
                ("Cargo.toml", cargo),
                (
                    "c/Cargo.toml",
                    "[package]\nname = \"c\"\n\n[lints]\nworkspace = true\n",
                ),
            ],
        );
        let (changes, _) = f.change(&[
            ("clippy.toml", "allow-unwrap-in-tests = false\n"),
            ("Cargo.toml", &cargo.replace("\"deny\"", "\"warn\"")),
            (
                "c/Cargo.toml",
                "[package]\nname = \"c\"\n\n[lints.rust]\nunsafe_code = \"allow\"\n",
            ),
        ]);
        assert_eq!(
            whats(&changes),
            vec![
                "Cargo.toml: edits `[workspace.lints]`",
                "c/Cargo.toml: edits the crate's `[lints]` table (not `workspace = true`)",
                "clippy.toml: edits a protected file",
            ]
        );
    }

    #[test]
    fn lint_deps_allowlist_edits_are_flagged() {
        let src = "const UNSAFE_CRATES: &[&str] = &[\"jstream-platform-windows\"];\n";
        let f = Fixture::new("lint-deps", &[("xtask/src/lint_deps.rs", src)]);
        let (changes, _) = f.change(&[(
            "xtask/src/lint_deps.rs",
            &src.replace("]", ", \"jstream-render\"]"),
        )]);
        assert_eq!(
            whats(&changes),
            vec!["xtask/src/lint_deps.rs: edits a protected file"]
        );
    }

    #[test]
    fn metadata_key_edits_are_flagged() {
        let f = Fixture::new("covers", &[("docs/RULES.md", RULES)]);
        let edited = RULES
            .replace("covers = [\"src/**\"]", "covers = [\"src/a/**\"]")
            .replace(
                "status = \"active\"\nenforced_by",
                "status = \"retired\"\nenforced_by",
            )
            .replace("Text.", "Text, reworded.");
        let (changes, _) = f.change(&[("docs/RULES.md", &edited)]);
        assert_eq!(
            whats(&changes),
            vec![
                "docs/RULES.md: section `#s` `covers`: [\"src/**\"] → [\"src/a/**\"]",
                "docs/RULES.md: rule `R-X-001` `status`: \"active\" → \"retired\"",
            ]
        );
    }

    #[test]
    fn deleted_sections_rules_and_docs_are_flagged() {
        let f = Fixture::new("delete", &[("docs/RULES.md", RULES), ("docs/X.md", RULES)]);
        let cut = RULES
            .find("\n<a id=\"R-X-001\"")
            .expect("invariant: the rule is there");
        f.git(&["rm", "-q", "docs/X.md"]);
        let (changes, _) = f.change(&[("docs/RULES.md", &RULES[..cut])]);
        assert_eq!(
            whats(&changes),
            vec![
                "docs/RULES.md: rule `R-X-001` deleted",
                "docs/X.md: deletes a tracked doc (retire it instead)",
            ]
        );
    }

    #[test]
    fn accepted_adr_body_edit_is_flagged() {
        let f = Fixture::new("adr", &[("docs/adr/0001-one.md", ADR)]);
        let (changes, _) = f.change(&[("docs/adr/0001-one.md", &ADR.replace("X.", "Y."))]);
        assert_eq!(
            whats(&changes),
            vec![
                "docs/adr/0001-one.md: edits an accepted ADR's content (R-DOC-009 allows only `status` and `superseded_by`)"
            ]
        );
        assert!(changes.iter().all(|c| c.kind == Kind::Plain));
    }

    #[test]
    fn accepted_adr_status_and_metadata_edits() {
        let f = Fixture::new("adr-status", &[("docs/adr/0001-one.md", ADR)]);
        let edited = ADR
            .replace(
                "\"accepted\"",
                "\"superseded\"\nsuperseded_by = \"ADR-0002\"",
            )
            .replace("\"One\"", "\"Uno\"");
        let (changes, _) = f.change(&[("docs/adr/0001-one.md", &edited)]);
        assert_eq!(
            whats(&changes),
            vec![
                "docs/adr/0001-one.md: `@doc` `status`: \"accepted\" → \"superseded\"",
                "docs/adr/0001-one.md: edits an accepted ADR's `superseded_by`",
                "docs/adr/0001-one.md: edits an accepted ADR's `title`",
            ]
        );
    }

    /// R-DOC-009's container exception: a delimiter change with byte-identical content is
    /// a `Container` change, which passes only when the justification cites an accepted ADR.
    #[test]
    fn container_change_must_cite_an_accepted_format_adr() {
        let f = Fixture::new("container", &[("docs/adr/0001-one.md", ADR)]);
        let front = ADR
            .replacen("<!-- @doc", "+++", 1)
            .replacen("-->", "+++", 1);
        let (changes, _) = f.change(&[("docs/adr/0001-one.md", &front)]);
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].kind, Kind::Container);
        let accepted: BTreeSet<String> = ["ADR-0016".to_owned()].into();
        let uncited = pr(&[LABEL], JUSTIFIED, "someone");
        assert!(
            matches!(decide(&changes, &uncited, false, &accepted), Verdict::Fail(p) if p.len() == 1)
        );
        let wrong = pr(&[LABEL], &format!("{JUSTIFIED}Per ADR-0099.\n"), "someone");
        assert!(matches!(
            decide(&changes, &wrong, false, &accepted),
            Verdict::Fail(_)
        ));
        let cited = pr(&[LABEL], &format!("{JUSTIFIED}Per ADR-0016.\n"), "someone");
        assert!(matches!(
            decide(&changes, &cited, false, &accepted),
            Verdict::Pass(_)
        ));
    }

    /// Attribute text in these tests is written with `\x23` for `#`, so the governance
    /// check's text scan of this file doesn't report the test strings themselves.
    #[test]
    fn new_suppressions_are_flagged() {
        let src = "\x23![allow(clippy::x, reason = \"old\")]\nfn a() {}\n";
        let f = Fixture::new("allow", &[("src/lib.rs", src)]);
        let edited = format!(
            "{src}\x23[allow(dead_code)]\nfn b() {{}}\n\x23[expect(\n    clippy::y,\n    reason = \"r ]\"\n)]\nfn c() {{}}\n// \x23[allow(in_a_comment)]\n"
        );
        let (changes, _) = f.change(&[("src/lib.rs", &edited)]);
        assert_eq!(
            whats(&changes),
            vec![
                "src/lib.rs: line 3: new `\x23[allow(dead_code)]` (missing `reason = \"…\"`)",
                "src/lib.rs: line 5: new `\x23[expect( clippy::y, reason = \"r ]\" )]`",
            ]
        );
    }

    #[test]
    fn ordinary_code_change_is_not_flagged() {
        let f = Fixture::new(
            "plain",
            &[
                ("src/lib.rs", "\x23[allow(x, reason = \"y\")]\nfn a() {}\n"),
                ("docs/RULES.md", RULES),
                ("Cargo.toml", "[workspace]\nmembers = []\n"),
            ],
        );
        let (changes, _) = f.change(&[
            (
                "src/lib.rs",
                "\x23[allow(x, reason = \"y\")]\nfn a() { b() }\nfn b() {}\n",
            ),
            ("src/new.rs", "pub fn n() {}\n"),
            ("docs/RULES.md", &RULES.replace("Rule.", "Rule, clarified.")),
            ("Cargo.toml", "[workspace]\nmembers = [\"c\"]\n"),
            (
                "c/Cargo.toml",
                "[package]\nname = \"c\"\n\n[lints]\nworkspace = true\n",
            ),
        ]);
        assert_eq!(whats(&changes), Vec::<String>::new());
        assert_eq!(
            decide(&changes, &Pr::default(), false, &BTreeSet::new()),
            Verdict::Pass("no governance changes".into())
        );
    }

    #[test]
    fn missing_label_or_section_fails_and_both_pass() {
        let f = Fixture::new("label", &[("deny.toml", "a = 1\n")]);
        let (changes, _) = f.change(&[("deny.toml", "a = 2\n")]);
        let none = BTreeSet::new();
        let fail = |p: &Pr| match decide(&changes, p, false, &none) {
            Verdict::Fail(problems) => problems.len(),
            Verdict::Pass(_) => 0,
        };
        assert_eq!(fail(&pr(&[], "", "a")), 2);
        assert_eq!(fail(&pr(&[], JUSTIFIED, "a")), 1, "label missing");
        assert_eq!(
            fail(&pr(&[LABEL], "## What\nx\n", "a")),
            1,
            "section missing"
        );
        let empty =
            "## Governance change\n<!-- Required with the label -->\n\n## Review override\nx\n";
        assert_eq!(fail(&pr(&[LABEL], empty, "a")), 1, "comment-only section");
        assert_eq!(fail(&pr(&[LABEL], JUSTIFIED, "a")), 0);
    }

    const WORKFLOW: &str = "jobs:\n  a:\n    steps:\n      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n      - run: cargo test\n";

    #[test]
    fn dependabot_pin_only_change_is_pre_approved() {
        let f = Fixture::new("pin", &[(".github/workflows/ci.yml", WORKFLOW)]);
        let bumped = WORKFLOW.replace(
            "3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1",
            "0123456789abcdef0123456789abcdef01234567 # v7.1.0",
        );
        let (changes, files) = f.change(&[(".github/workflows/ci.yml", &bumped)]);
        assert!(pin_only(&files));
        let none = BTreeSet::new();
        assert!(matches!(
            decide(&changes, &pr(&[], "", DEPENDABOT), true, &none),
            Verdict::Pass(why) if why.contains("ADR-0018")
        ));
        // The same diff from anyone else needs the label.
        assert!(matches!(
            decide(&changes, &pr(&[], "", "someone"), true, &none),
            Verdict::Fail(_)
        ));
    }

    #[test]
    fn dependabot_change_beyond_pins_is_not_pre_approved() {
        let f = Fixture::new(
            "pin-plus",
            &[
                (".github/workflows/ci.yml", WORKFLOW),
                ("deny.toml", "a = 1\n"),
            ],
        );
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let bumped = WORKFLOW.replace("3d3c42e5aac5ba805825da76410c181273ba90b1", sha);
        let cases: [&[(&str, &str)]; 4] = [
            // A step edit alongside the pin bump.
            &[(
                ".github/workflows/ci.yml",
                &bumped.replace("cargo test", "true"),
            )],
            // A tag instead of a SHA.
            &[(
                ".github/workflows/ci.yml",
                &WORKFLOW.replace("3d3c42e5aac5ba805825da76410c181273ba90b1", "v8"),
            )],
            // A different action.
            &[(
                ".github/workflows/ci.yml",
                &bumped.replace("actions/checkout", "evil/checkout"),
            )],
            // Another governed file.
            &[
                (".github/workflows/ci.yml", &bumped),
                ("deny.toml", "a = 2\n"),
            ],
        ];
        for (k, case) in cases.into_iter().enumerate() {
            f.git(&["checkout", "-q", "-B", &format!("case{k}"), "base"]);
            let (changes, files) = f.change(case);
            assert!(!pin_only(&files), "case {k}");
            assert!(
                matches!(
                    decide(
                        &changes,
                        &pr(&[], "", DEPENDABOT),
                        pin_only(&files),
                        &BTreeSet::new()
                    ),
                    Verdict::Fail(_)
                ),
                "case {k}"
            );
        }
    }

    /// Same results as review-gate.yml's `section()` on the cases that matter.
    #[test]
    fn section_matches_review_gate() {
        let body = "## What\nx\n## governance change (why)\n<!-- hint -->\nReal text.\n### Sub\nmore\n## Review override\ny";
        assert_eq!(section(body, SECTION), "Real text.\n### Sub\nmore");
        assert_eq!(
            section("## Governance change\n<!-- only -->\n", SECTION),
            ""
        );
        assert_eq!(
            section("## Governance change", SECTION),
            "",
            "no newline after heading"
        );
        assert_eq!(
            section("##Governance change\nx\n", SECTION),
            "",
            "needs a space"
        );
        assert_eq!(section("## Governance change\r\nwhy\r\n", SECTION), "why");
        assert_eq!(
            section("## Governance change\na <!-- open\n", SECTION),
            "a <!-- open"
        );
    }

    #[test]
    fn event_payload_is_read() {
        let json = r#"{"action":"labeled","pull_request":{"body":null,"user":{"login":"dependabot[bot]"},"labels":[{"name":"governance-approved"},{"name":"x"}]}}"#;
        let p = pr_from_event(json)
            .expect("invariant: valid JSON")
            .expect("invariant: has a PR");
        assert_eq!(p.labels, vec![LABEL, "x"]);
        assert_eq!((p.body.as_str(), p.author.as_str()), ("", DEPENDABOT));
        assert!(
            pr_from_event(r#"{"ref":"refs/heads/main"}"#)
                .expect("invariant: valid JSON")
                .is_none()
        );
        assert!(pr_from_event("not json").is_err());
    }

    #[test]
    fn adr_ids_are_exact() {
        let ids = adr_ids("ADR-0016, ADR-12345, (ADR-0018) ADR-01");
        assert_eq!(ids, ["ADR-0016".to_owned(), "ADR-0018".to_owned()].into());
    }
}
