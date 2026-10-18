//! `cargo xtask docs`: checks and maintains the documentation graph (docs/DOCS.md).
//!
//! Docs: docs/DOCS.md#bootstrap, docs/DOCS.md#gate, docs/DOCS.md#verification, docs/DOCS.md#back-references, docs/DOCS.md#generated

mod backrefs;
mod check;
pub(crate) mod generate;
mod govern;
mod hash;
mod history;
pub(crate) mod ledger;
mod merge;
pub(crate) mod parse;
mod progress;
mod summary;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

pub(crate) use parse::Diag;

/// Files that look like docs but are not tracked (docs/DOCS.md#identity).
pub(crate) fn is_tracked(path: &str) -> bool {
    path.ends_with(".md")
        && !path.starts_with(".github/")
        && !path.starts_with("LICENSE")
        && path != "docs/adr/TEMPLATE.md"
}

/// Stock phrases that say nothing about what was checked (docs/DOCS.md#notes).
const STOCK_NOTES: &[&str] = &[
    "ok",
    "lgtm",
    "no change",
    "still accurate",
    "n/a",
    "verified",
    "looks good",
];

/// All files tracked by git under `root`, sorted.
fn git_files(root: &Path) -> Result<Vec<String>> {
    git_ls(root, &[])
}

/// `git ls-files -z <extra>` under `root`, sorted.
fn git_ls(root: &Path, extra: &[&str]) -> Result<Vec<String>> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .args(extra)
        .current_dir(root)
        .output()
        .context("running `git ls-files`")?;
    if !out.status.success() {
        bail!(
            "`git ls-files` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut paths: Vec<String> = String::from_utf8(out.stdout)?
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    paths.sort();
    Ok(paths)
}

/// Parsed docs plus everything the checks need.
struct Loaded {
    files: Vec<String>,
    docs: Vec<parse::Doc>,
    parse_diags: Vec<Diag>,
}

fn load(root: &Path) -> Result<Loaded> {
    let files = git_files(root)?;
    let mut parse_diags = Vec::new();
    let mut docs = Vec::new();
    for p in files.iter().filter(|p| is_tracked(p)) {
        let text = std::fs::read_to_string(root.join(p)).with_context(|| format!("reading {p}"))?;
        docs.push(parse::parse(p, &text, &mut parse_diags));
    }
    Ok(Loaded {
        files,
        docs,
        parse_diags,
    })
}

/// Rewrites every `@generated` block from its source, as `docs generate` does: the ADR
/// status banners and index, the README's progress table and the UI features summary. Returns what was written
/// and the problems that stopped a block.
pub(crate) fn write_generated(root: &Path) -> Result<generate::Generated> {
    let l = load(root)?;
    let mut g = generate::generate(&l.docs);
    let (readme, problems) = progress::generate(root, &l.docs);
    g.files
        .extend(readme.map(|(path, line, text)| (path, Some(line), text)));
    g.problems.extend(problems);
    let (ui, problems) = summary::generate(&l.docs);
    g.files
        .extend(ui.map(|(path, line, text)| (path, Some(line), text)));
    g.problems.extend(problems);
    for (path, _, text) in &g.files {
        std::fs::write(root.join(path), text).with_context(|| format!("writing {path}"))?;
    }
    Ok(g)
}

fn read_toml(root: &Path, p: &str) -> Result<toml::Table> {
    let text = std::fs::read_to_string(root.join(p)).with_context(|| format!("reading {p}"))?;
    text.parse().with_context(|| format!("parsing {p}"))
}

/// Runs every check over the repository at `root` and returns the diagnostics.
pub(crate) fn check_repo(root: &Path) -> Result<Vec<Diag>> {
    let l = load(root)?;
    let mut ledger_diags = Vec::new();
    let repo = check::Repo {
        root,
        docs: &l.docs,
        external: read_toml(root, "docs/external.toml")?,
        facts: read_toml(root, "docs/facts.toml")?,
        rules_md: std::fs::read_to_string(root.join("docs/RULES.md"))
            .context("reading docs/RULES.md")?,
        files: hash::Files::new(root, &l.files),
        ledger: check::check_ledger(root, &l.docs, &mut ledger_diags),
    };
    let mut diags = l.parse_diags;
    diags.extend(ledger_diags);
    diags.extend(check::check(&repo));
    diags.extend(backrefs::check(&repo));
    diags.extend(generate::check(&l.docs));
    diags.extend(progress::check(root, &l.docs));
    diags.extend(summary::check(&l.docs));
    diags.sort();
    diags.dedup();
    Ok(diags)
}

/// Checks the `--by` and `--note` of a `verify` or `review` stamp; returns the trimmed note.
fn check_stamp<'a>(by: &str, note: &'a str) -> Result<&'a str> {
    let note = note.trim();
    if note.len() < 20 || STOCK_NOTES.contains(&note.to_lowercase().trim_end_matches('.')) {
        bail!("the note must say what you checked and why it still holds (docs/DOCS.md#notes)");
    }
    if note.contains("-->") || by.contains("-->") {
        bail!("`-->` is not allowed in metadata values");
    }
    if !(by.starts_with("human:") || by.starts_with("agent:")) {
        bail!("--by must be `human:<github-user>` or `agent:<name>`");
    }
    Ok(note)
}

/// `docs review <DOC-ID | path> --by <who> --note <text>`: records a doc-level TTL review
/// by writing `reviewed` into the `@doc` block (docs/DOCS.md#ttl). The commit that changes
/// that line restarts the doc's TTL clock, so the new line must differ from the old one.
fn review(root: &Path, target: &str, by: &str, note: &str) -> Result<()> {
    let note = check_stamp(by, note)?;
    if target.contains('#') {
        bail!("`docs review` stamps a whole doc; a section is re-read with `docs verify`");
    }
    let l = load(root)?;
    let doc = l
        .docs
        .iter()
        .find(|d| d.path == target || d.meta_str("id") == Some(target))
        .with_context(|| format!("no tracked doc `{target}`"))?;
    if doc.is_adr() {
        bail!(
            "ADRs have no TTL; a decision that no longer holds gets a superseding ADR (docs/DOCS.md#ttl)"
        );
    }
    if doc.meta_str("review_every").is_none() {
        bail!("{} has no `review_every`, so it has no TTL", doc.path);
    }
    let Some((start, end)) = doc.doc_block.filter(|_| doc.meta.is_some()) else {
        bail!("{} has no valid `@doc` block", doc.path);
    };
    let line = format!(
        "reviewed = {{ by = {}, note = {} }}",
        hash::toml_str(by),
        hash::toml_str(note)
    );
    let mut lines: Vec<String> = doc.text.lines().map(str::to_owned).collect();
    let key = |l: &str| l.split('=').next().unwrap_or_default().trim() == "reviewed";
    match (start + 1..end).find(|&k| key(&lines[k])) {
        Some(k) if lines[k] == line => bail!(
            "{} already records this review; only a changed `reviewed` line restarts the TTL \
             clock, so say what you re-read this time",
            doc.path
        ),
        Some(k) => lines[k] = line,
        None => lines.insert(end, line),
    }
    let mut out = lines.join("\n");
    if doc.text.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(root.join(&doc.path), out).with_context(|| format!("writing {}", doc.path))?;
    eprintln!("reviewed {}", doc.meta_str("id").unwrap_or(&doc.path));
    Ok(())
}

/// String values of `key` in a section's metadata.
pub(crate) fn strings(meta: &toml::Table, key: &str) -> Vec<String> {
    meta.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A section's key in the ledger and in messages: `DOC-ID#anchor`.
pub(crate) fn section_key(doc: &parse::Doc, anchor: &str) -> String {
    format!("{}#{anchor}", doc.meta_str("id").unwrap_or(&doc.path))
}

/// The ledger keys of every section that needs a stamp: one with `covers` or `depends_on`.
pub(crate) fn stamped_sections(docs: &[parse::Doc]) -> std::collections::BTreeSet<String> {
    docs.iter()
        .flat_map(|d| {
            d.sections.iter().filter_map(move |s| {
                let meta = s.meta.as_ref()?;
                (!strings(meta, "covers").is_empty() || !strings(meta, "depends_on").is_empty())
                    .then(|| section_key(d, &s.anchor))
            })
        })
        .collect()
}

/// `text` without the lines at `drop` (0-based), keeping its final newline.
fn without_lines(text: &str, drop: &[usize]) -> String {
    let mut out: String = text
        .lines()
        .enumerate()
        .filter(|(k, _)| !drop.contains(k))
        .map(|(_, l)| format!("{l}\n"))
        .collect();
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}

/// The line of an inline `verified` stamp (from before ADR-0053) in a section's
/// `@section` block, if it has one.
fn inline_stamp(doc: &parse::Doc, section: &parse::Section) -> Option<usize> {
    let (start, end) = section.block?;
    let lines: Vec<&str> = doc.text.lines().collect();
    (start + 1..end).find(|&k| {
        lines[k]
            .trim_start()
            .strip_prefix("verified")
            .is_some_and(|r| r.trim_start().starts_with('='))
    })
}

/// `docs verify <DOC-ID#anchor | path#anchor> --by <who> --note <text>`: records that a
/// section was re-read and still holds, by writing its inputs' current hashes into the
/// ledger (ADR-0053). It also drops ledger stamps of sections that no longer need one.
fn verify(root: &Path, target: &str, by: &str, note: &str) -> Result<()> {
    let note = check_stamp(by, note)?;
    stamp_section(root, target, &|_, _, old, inputs| {
        Ok(ledger::restamp(old, inputs, by, note))
    })
}

/// `docs verify <DOC#anchor> --by <who> --merge <text>`: the same after `docs merge-main`,
/// for a section the merge left stale. An input whose hash equals the merged side's entry
/// gets that entry unchanged (#596). Each other rewritten entry keeps the note this branch
/// had written on it before the merge and adds `text`, what the merge changed in the
/// section's inputs (docs/DOCS.md#notes). It refuses when the file of a rewritten input changed after
/// the merge: the branch's earlier note doesn't cover that edit, so it needs `--note`.
fn verify_merge(root: &Path, target: &str, by: &str, text: &str) -> Result<()> {
    let text = check_stamp(by, text)?;
    let point = merge::last_merge(root)?.context(
        "no merge on this branch to carry notes across (run `cargo xtask docs merge-main` \
         first, or verify with --note)",
    )?;
    let branch = merge::branch_entries(root, &point)?;
    let merged = merge::merged_ledger(root, &point)?;
    stamp_section(root, target, &|doc, key, old, inputs| {
        let theirs = merged.sections.get(key);
        let stamp = ledger::restamp_merge(old, inputs, branch.get(key), theirs, by, text);
        // An entry taken as it is from the merged side carries no note of this branch's, so
        // a later edit to its file doesn't refuse it.
        let rewritten: Vec<(&String, String)> = stamp
            .iter()
            .filter(|(k, e)| old.and_then(|o| o.get(*k)) != Some(*e))
            .filter(|(k, e)| theirs.and_then(|t| t.get(*k)) != Some(*e))
            .map(|(k, _)| (k, merge::input_path(doc, k)))
            .collect();
        let mut paths: Vec<String> = rewritten.iter().map(|(_, p)| p.clone()).collect();
        paths.sort();
        paths.dedup();
        let later = merge::changed_after(root, &point, &paths)?;
        let refused: Vec<String> = rewritten
            .iter()
            .filter(|(_, p)| later.contains(p))
            .map(|(k, p)| format!("{k} ({p})"))
            .collect();
        if !refused.is_empty() {
            bail!(
                "{key}: changed after the merge, so this branch's earlier note doesn't cover \
                 it: {}; verify this section with --note, saying what this branch changed \
                 and what the merge changed (docs/DOCS.md#notes)",
                refused.join(", ")
            );
        }
        Ok(stamp)
    })
}

/// `docs verify <DOC#anchor> --by <who> --renote <text> [--from <branch>]`: replaces the
/// `by` and `note` of the entries this branch wrote on a section that is already fresh,
/// keeping their hashes, as when a reviewer asks for a better note. The branch's entries are
/// those that differ from the ledger where it left `origin/main`, the wave it targets, or
/// the PR branch it is stacked on (#715).
/// A stale section is refused: re-read it and verify with `--note` or `--merge`.
fn verify_renote(root: &Path, target: &str, by: &str, note: &str, branch: &str) -> Result<()> {
    let note = check_stamp(by, note)?;
    let base = merge::base_ledger(root, branch)?;
    stamp_section(root, target, &|_, key, old, inputs| {
        if !ledger::diff(old, inputs).is_empty() {
            bail!(
                "{key} is stale: re-read it and verify with --note (or --merge after \
                 `docs merge-main`); --renote only rewrites the note of a fresh section"
            );
        }
        let old = old.context("invariant: a fresh section has a stamp")?;
        ledger::renote(old, base.sections.get(key), by, note).with_context(|| {
            format!("this branch wrote no entry on {key} since it left origin/{branch}; nothing to renote")
        })
    })
}

/// The section's new stamp from its doc's path, its key, its current stamp and its current
/// inputs.
type Restamp<'a> = dyn Fn(&str, &str, Option<&ledger::Stamp>, &BTreeMap<String, String>) -> Result<ledger::Stamp>
    + 'a;

/// Writes the stamp `restamp` makes for `target` into the ledger, after the checks every
/// verify makes.
fn stamp_section(root: &Path, target: &str, restamp: &Restamp<'_>) -> Result<()> {
    let (doc_ref, anchor) = target
        .split_once('#')
        .context("target must be `DOC#anchor`")?;
    let l = load(root)?;
    let by_path: BTreeMap<&str, &parse::Doc> =
        l.docs.iter().map(|d| (d.path.as_str(), d)).collect();
    let doc = l
        .docs
        .iter()
        .find(|d| d.path == doc_ref || d.meta_str("id") == Some(doc_ref))
        .with_context(|| format!("no tracked doc `{doc_ref}`"))?;
    let section = doc
        .sections
        .iter()
        .find(|s| s.anchor == anchor)
        .with_context(|| format!("no section `{anchor}` in {}", doc.path))?;
    let Some(meta) = &section.meta else {
        bail!("section `{anchor}` has no `@section` block");
    };
    let (covers, deps) = (strings(meta, "covers"), strings(meta, "depends_on"));
    if covers.is_empty() && deps.is_empty() {
        bail!("section `{anchor}` has no `covers` or `depends_on`, so it has nothing to verify");
    }
    // Hashes only cover tracked files, so an untracked covered file would make this hash
    // differ from the one CI computes after the commit (review B1 on #10).
    let untracked = git_ls(root, &["--others", "--exclude-standard"])?;
    let pending = hash::Files::new(root, &untracked).matching(&covers)?;
    if !pending.is_empty() {
        bail!(
            "untracked files match this section's `covers`; `git add` them first: {}",
            pending.join(", ")
        );
    }
    let files = hash::Files::new(root, &l.files);
    let inputs = hash::inputs(doc, anchor, &covers, &deps, &files, &by_path)?;
    let key = section_key(doc, anchor);
    let mut ledger = ledger::Ledger::read(root)?;
    let old = ledger.sections.get(&key);
    let stamp = restamp(&doc.path, &key, old, &inputs)?;
    let changed = changed_entries(old, &stamp);
    ledger.sections.insert(key.clone(), stamp);
    let keep = stamped_sections(&l.docs);
    ledger.prune(&|k| keep.contains(k));
    ledger.write(root)?;
    // A branch from before ADR-0053 may still carry the section's inline stamp.
    if let Some(k) = inline_stamp(doc, section) {
        std::fs::write(root.join(&doc.path), without_lines(&doc.text, &[k]))
            .with_context(|| format!("writing {}", doc.path))?;
    }
    if changed.is_empty() {
        eprintln!("verified {key}: already fresh, nothing to record");
    } else {
        eprintln!("verified {key}: {}", changed.join(", "));
    }
    Ok(())
}

/// The inputs whose entry `new` changes, adds or removes relative to `old`, sorted.
fn changed_entries(old: Option<&ledger::Stamp>, new: &ledger::Stamp) -> Vec<String> {
    let empty = ledger::Stamp::new();
    let old = old.unwrap_or(&empty);
    let mut out: Vec<String> = new
        .iter()
        .filter(|(k, e)| old.get(*k) != Some(*e))
        .map(|(k, _)| k.clone())
        .chain(old.keys().filter(|k| !new.contains_key(*k)).cloned())
        .collect();
    out.sort();
    out
}

/// What `docs migrate` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Migrate {
    /// Sections whose inline stamp still held and moved to the ledger, as `DOC#anchor`.
    pub(crate) moved: Vec<String>,
    /// Sections whose inline stamp no longer held, so it was dropped, as `DOC#anchor`.
    pub(crate) stale: Vec<String>,
    /// Ledger stamps dropped because their section is gone or needs no stamp.
    pub(crate) pruned: Vec<String>,
}

/// `docs migrate`: moves inline `verified` stamps (from before ADR-0053) into the ledger.
/// A stamp whose hash still matches the section becomes a ledger stamp of its current
/// inputs, with its `by` and `note` on the `body` entry. A stamp that no longer matches is
/// dropped, never moved, so the section is reported stale until someone re-reads it. Every
/// decision is made against the unmodified tree before anything is written. It also drops
/// ledger stamps of sections that no longer need one, and rewrites the ledger canonically.
pub(crate) fn migrate(root: &Path) -> Result<Migrate> {
    let l = load(root)?;
    let by_path: BTreeMap<&str, &parse::Doc> =
        l.docs.iter().map(|d| (d.path.as_str(), d)).collect();
    let files = hash::Files::new(root, &l.files);
    let untracked = git_ls(root, &["--others", "--exclude-standard"])?;
    let untracked = hash::Files::new(root, &untracked);
    let mut ledger = ledger::Ledger::read(root)?;
    let before = std::fs::read_to_string(root.join(ledger::PATH)).ok();
    let mut report = Migrate::default();
    let mut edits: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for doc in &l.docs {
        for s in &doc.sections {
            let (Some(meta), Some(line)) = (&s.meta, inline_stamp(doc, s)) else {
                continue;
            };
            edits.entry(doc.path.as_str()).or_default().push(line);
            let key = section_key(doc, &s.anchor);
            let (covers, deps) = (strings(meta, "covers"), strings(meta, "depends_on"));
            let stored = meta.get("verified").and_then(|v| v.as_table());
            let field = |k: &str| {
                stored
                    .and_then(|t| t.get(k))
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty())
            };
            let holds = !(covers.is_empty() && deps.is_empty())
                && untracked.matching(&covers)?.is_empty()
                && field("hash")
                    == Some(
                        hash::legacy_section_hash(
                            doc, &s.anchor, &covers, &deps, &files, &by_path,
                        )?
                        .as_str(),
                    );
            match (holds, field("by"), field("note")) {
                (true, Some(by), Some(note)) => {
                    let inputs = hash::inputs(doc, &s.anchor, &covers, &deps, &files, &by_path)?;
                    let stamp = inputs
                        .into_iter()
                        .map(|(k, hash)| {
                            let body = k == hash::BODY;
                            let e = ledger::Entry {
                                hash,
                                by: body.then(|| by.to_owned()),
                                note: body.then(|| note.to_owned()),
                            };
                            (k, e)
                        })
                        .collect();
                    ledger.sections.insert(key.clone(), stamp);
                    report.moved.push(key);
                }
                _ => report.stale.push(key),
            }
        }
    }
    let keep = stamped_sections(&l.docs);
    report.pruned = ledger.prune(&|k| keep.contains(k));
    if !edits.is_empty() || before.is_some_and(|b| b != ledger.render()) {
        ledger.write(root)?;
    }
    for (path, lines) in edits {
        let doc = by_path[path];
        std::fs::write(root.join(path), without_lines(&doc.text, &lines))
            .with_context(|| format!("writing {path}"))?;
    }
    Ok(report)
}

/// `arg` as a repo-relative path with `/` separators. Absolute paths must be inside `root`.
/// They are compared after canonicalizing both sides, so Windows verbatim paths and
/// symlinks work; the argument may not exist yet, so its deepest existing ancestor is
/// canonicalized and the rest re-appended (review N1 on #14). `..` components are resolved
/// lexically first, so `<repo>/nope/../../x` is reported as outside the repository rather
/// than as having no existing ancestor (review N1 on #15).
fn repo_path(root: &Path, arg: &str) -> Result<String> {
    let p = Path::new(arg);
    let rel = if p.is_absolute() {
        let root = root.canonicalize().context("resolving the repo root")?;
        let mut lexical = std::path::PathBuf::new();
        for c in p.components() {
            match c {
                std::path::Component::CurDir => {}
                // `..` at the root stays at the root, as the OS resolves it.
                std::path::Component::ParentDir => {
                    lexical.pop();
                }
                other => lexical.push(other),
            }
        }
        let mut existing = lexical.as_path();
        let mut rest = Vec::new();
        let base = loop {
            if let Ok(c) = existing.canonicalize() {
                break c;
            }
            rest.push(existing.file_name().context("no existing ancestor")?);
            existing = existing.parent().context("no existing ancestor")?;
        };
        let full = rest.iter().rev().fold(base, |acc, part| acc.join(part));
        full.strip_prefix(&root)
            .with_context(|| format!("`{arg}` is outside the repository"))?
            .to_path_buf()
    } else {
        p.to_path_buf()
    };
    check::normalize(&rel).with_context(|| format!("`{arg}` escapes the repository"))
}

/// Entry point for `cargo xtask docs <args>`.
pub(crate) fn run(args: &[String], root: &Path) -> Result<bool> {
    match args.first().map(String::as_str) {
        Some("check") => {
            let diags = check_repo(root)?;
            for d in &diags {
                let kind = if d.warning { "warning: " } else { "" };
                eprintln!("{}:{}: {kind}{}", d.path, d.line, d.msg);
            }
            let errors = diags.iter().filter(|d| !d.warning).count();
            let warnings = diags.len() - errors;
            if errors == 0 {
                eprintln!("docs check: ok ({warnings} warning(s))");
            } else {
                eprintln!("docs check: {errors} problem(s), {warnings} warning(s)");
            }
            Ok(errors == 0)
        }
        Some("generate") => {
            let [_] = args else {
                bail!("usage: cargo xtask docs generate");
            };
            let g = write_generated(root)?;
            for d in &g.problems {
                eprintln!("{}:{}: {}", d.path, d.line, d.msg);
            }
            for (path, _, _) in &g.files {
                eprintln!("generated {path}");
            }
            if g.files.iter().any(|(_, line, _)| line.is_none()) {
                eprintln!(
                    "note: new files must be added with `git add` before `docs check` sees them"
                );
            }
            eprintln!(
                "docs generate: {} file(s) written, {} problem(s)",
                g.files.len(),
                g.problems.len()
            );
            Ok(g.problems.is_empty())
        }
        Some("verify") => {
            let mut target = None;
            let mut by = None;
            let mut note = None;
            let mut merge = None;
            let mut renote = None;
            let mut from = None;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--by" => by = it.next(),
                    "--note" => note = it.next(),
                    "--merge" => merge = it.next(),
                    "--renote" => renote = it.next(),
                    "--from" => from = it.next(),
                    t if target.is_none() => target = Some(t),
                    other => bail!("unexpected argument `{other}`"),
                }
            }
            match (target, by, note, merge, renote, from) {
                (Some(target), Some(by), Some(note), None, None, None) => {
                    verify(root, target, by, note)?;
                }
                (Some(target), Some(by), None, Some(merge), None, None) => {
                    verify_merge(root, target, by, merge)?;
                }
                (Some(target), Some(by), None, None, Some(renote), from) => {
                    let branch = from.map_or(merge::BRANCH, String::as_str);
                    verify_renote(root, target, by, renote, branch)?;
                }
                _ => bail!(
                    "usage: cargo xtask docs verify <DOC#anchor> --by <human:..|agent:..> \
                     (--note \"<what you checked>\" | --merge \"<what the merge changed>\" \
                     | --renote \"<the better note>\" [--from <base branch>])"
                ),
            }
            Ok(true)
        }
        Some("review") => {
            let mut target = None;
            let mut by = None;
            let mut note = None;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--by" => by = it.next(),
                    "--note" => note = it.next(),
                    t if target.is_none() => target = Some(t),
                    other => bail!("unexpected argument `{other}`"),
                }
            }
            let (Some(target), Some(by), Some(note)) = (target, by, note) else {
                bail!(
                    "usage: cargo xtask docs review <DOC> --by <human:..|agent:..> --note \"<what you re-read>\""
                );
            };
            review(root, target, by, note)?;
            Ok(true)
        }
        Some("log") => {
            let mut target = None;
            let mut rev = None;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--rev" => rev = it.next(),
                    t if target.is_none() => target = Some(t),
                    other => bail!("unexpected argument `{other}`"),
                }
            }
            let Some(target) = target else {
                bail!("usage: cargo xtask docs log <DOC[#anchor]> [--rev <rev>]");
            };
            let l = load(root)?;
            let log = history::log(root, &l.docs, target, rev.map_or("HEAD", String::as_str))?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            eprint!("{}", history::render(&log, now));
            Ok(true)
        }
        Some("migrate") => {
            let [_] = args else {
                bail!("usage: cargo xtask docs migrate");
            };
            let m = migrate(root)?;
            for s in &m.moved {
                eprintln!("moved to {}: {s}", ledger::PATH);
            }
            for s in &m.stale {
                eprintln!("stale, dropped for a re-read: {s}");
            }
            for s in &m.pruned {
                eprintln!("pruned: {s}");
            }
            eprintln!(
                "docs migrate: {} moved, {} stale, {} pruned",
                m.moved.len(),
                m.stale.len(),
                m.pruned.len()
            );
            Ok(true)
        }
        Some("merge-main") => {
            let branch = match args {
                [_] => merge::BRANCH,
                [_, flag, b] if flag == "--from" => b.as_str(),
                _ => bail!("usage: cargo xtask docs merge-main [--from wave/<name>]"),
            };
            let m = merge::merge_main(root, branch)?;
            for s in &m.stamps {
                eprintln!("took {branch}'s stamp: {s}");
            }
            for s in &m.adopted {
                eprintln!("took {branch}'s note where it verified the same content: {s}");
            }
            for path in &m.generated {
                if m.committed {
                    eprintln!("regenerated after a conflict in its @generated blocks: {path}");
                } else {
                    eprintln!(
                        "took {branch}'s side of the @generated blocks in {path}; run \
                         `cargo xtask docs generate` once the files below are resolved"
                    );
                }
            }
            for (path, why) in &m.refused {
                eprintln!("left for you: {path}: {why}");
            }
            if !m.committed {
                eprintln!(
                    "docs merge-main: stamp and @generated-block conflicts are resolved and \
                     staged; resolve the files above by hand, run `cargo xtask docs generate` if \
                     it named any @generated blocks, `git commit`, then run `cargo xtask docs check`"
                );
                return Ok(false);
            }
            if let Some(mig) = &m.migrated {
                eprintln!(
                    "moved this branch's inline stamps to {} and committed: {} moved, {} dropped as stale",
                    ledger::PATH,
                    mig.moved.len(),
                    mig.stale.len()
                );
            }
            // Merged commits listed per input; past this many, a count.
            const SHOWN: usize = 6;
            for s in &m.stale {
                eprintln!("stale: {s}");
                for i in m.inputs.get(s).into_iter().flatten() {
                    eprintln!("  {}", i.input);
                    match &i.note {
                        Some(n) => eprintln!("    this branch's note: {n}"),
                        None => {
                            eprintln!("    this branch's note: none (it didn't change this input)")
                        }
                    }
                    for c in i.commits.iter().take(SHOWN) {
                        eprintln!("    merged: {c}");
                    }
                    if i.commits.len() > SHOWN {
                        eprintln!("    merged: … and {} more", i.commits.len() - SHOWN);
                    }
                }
            }
            eprintln!(
                "docs merge-main: merged {} stamp conflict(s) and resolved {} @generated conflict(s); {} stale \
                 section(s) to re-read. Re-read each against what the merge brought in, then \
                 `cargo xtask docs verify <DOC#anchor> --by <who> --merge \"<what the merge \
                 changed in this section's inputs>\"`: it keeps this branch's note on each entry \
                 and adds yours (docs/DOCS.md#notes)",
                m.stamps.len(),
                m.generated.len(),
                m.stale.len()
            );
            Ok(true)
        }
        Some("for") => {
            let [_, arg] = args else {
                bail!("usage: cargo xtask docs for <path>");
            };
            let path = repo_path(root, arg)?;
            let l = load(root)?;
            let g = govern::governing(root, &l.docs, &l.files, &path);
            eprint!("{}", govern::render(&path, &g, &l.docs));
            // Paths are repo-relative wherever you run from; flag the likely mistake of
            // passing a path relative to a subdirectory (review N2 on #14).
            let parent = Path::new(&path).parent().unwrap_or(Path::new(""));
            if !l.files.contains(&path) && !root.join(parent).is_dir() {
                eprintln!(
                    "\nnote: `{path}` is not tracked and its directory does not exist; paths are \
                     relative to the repository root"
                );
            }
            Ok(true)
        }
        Some("progress") => {
            let [_] = args else {
                bail!("usage: cargo xtask docs progress");
            };
            let report = progress::report(root)?;
            std::io::stdout()
                .lock()
                .write_all(report.as_bytes())
                .context("writing the report")?;
            Ok(true)
        }
        _ => bail!(
            "usage: cargo xtask docs check | generate | progress | verify <DOC#anchor> --by <who> (--note | --merge | --renote) <text> | review <DOC> --by <who> --note <text> | log <DOC[#anchor]> [--rev <rev>] | for <path> | merge-main [--from wave/<name>] | migrate"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracked_file_filter() {
        assert!(is_tracked("README.md"));
        assert!(is_tracked("docs/adr/0001-x.md"));
        assert!(is_tracked("crates/jstream-types/README.md"));
        assert!(!is_tracked("docs/adr/TEMPLATE.md"));
        assert!(!is_tracked(".github/pull_request_template.md"));
        assert!(!is_tracked("Cargo.toml"));
    }

    /// A throwaway git repository, removed on drop even when an assertion fails.
    struct Fixture(std::path::PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Fixture {
        /// Writes `files` into a fresh repo named after the test, then `git add`s them.
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir = std::env::temp_dir().join(format!("xtask-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let f = Self(dir);
            for (path, text) in files {
                f.write(path, text);
            }
            f.git(&["init", "-q"]);
            f.git(&["add", "-A"]);
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

        /// The commit `rev` names.
        fn rev(&self, rev: &str) -> String {
            let out = Command::new("git")
                .args(["rev-parse", rev])
                .current_dir(&self.0)
                .output()
                .expect("invariant: git runs");
            assert!(out.status.success(), "git rev-parse {rev} failed");
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        }

        /// `docs check` messages containing `needle`.
        fn diags(&self, needle: &str) -> Vec<String> {
            check_repo(&self.0)
                .expect("invariant: the fixture is readable")
                .into_iter()
                .filter(|d| d.msg.contains(needle))
                .map(|d| format!("{}:{}: {}", d.path, d.line, d.msg))
                .collect()
        }
    }

    /// Registries `check_repo` reads; empty is fine for these tests.
    const REGISTRIES: [(&str, &str); 3] = [
        ("docs/external.toml", ""),
        ("docs/facts.toml", ""),
        ("docs/RULES.md", ""),
    ];

    fn doc(id: &str, status: &str, anchor: &str, covers: &str) -> String {
        format!(
            "<!-- @doc\nid = \"{id}\"\nstatus = \"{status}\"\n-->\n<a id=\"{anchor}\"></a>\n## A\n<!-- @section\ncovers = [\"{covers}\"]\n-->\nText.\n"
        )
    }

    /// `verify` refuses while an untracked file matches `covers`: CI hashes only the
    /// committed tree, so the recorded hash would be wrong (review N4 on #10).
    #[test]
    fn verify_refuses_untracked_covered_files() {
        let f = Fixture::new(
            "verify",
            &[
                ("T.md", &doc("T", "active", "a", "src/**")),
                ("src/a.rs", "a"),
            ],
        );
        f.write("src/b.rs", "b");
        let note = "checked the section text against src in this test";
        let err = verify(&f.0, "T#a", "agent:test", note).expect_err("untracked src/b.rs");
        assert!(format!("{err:#}").contains("src/b.rs"), "{err:#}");
        f.git(&["add", "src/b.rs"]);
        verify(&f.0, "T#a", "agent:test", note).expect("all covered files tracked");
        let written =
            std::fs::read_to_string(f.0.join(ledger::PATH)).expect("invariant: just written");
        assert!(
            written.contains("[\"T#a\".\"file:src/b.rs\"]\nstamp = { hash = \"b3:"),
            "{written}"
        );
    }

    const NOTE: &str = "checked the section text against its inputs in this test";

    /// #100: a `depends_on` naming a rule anchor hashes that rule's text, so editing the
    /// rule stales its dependents; editing another rule or the rule's metadata does not.
    #[test]
    fn rule_text_edits_stale_dependents() {
        let rules = "<!-- @doc\nid = \"RULES\"\n-->\n<a id=\"code\"></a>\n## Code\n<!-- @section\n-->\n<a id=\"R-X-001\"></a>\n### R-X-001: One\n<!-- @rule\nstatus = \"active\"\n-->\nFirst rule.\n\n<a id=\"R-X-002\"></a>\n### R-X-002: Two\n<!-- @rule\nstatus = \"active\"\n-->\nSecond rule.\n";
        let dep = "<!-- @doc\nid = \"D\"\n-->\n<a id=\"d\"></a>\n## D\n<!-- @section\ndepends_on = [\"docs/RULES.md#R-X-001\"]\n-->\nFollows R-X-001.\n";
        let mut files = REGISTRIES.to_vec();
        files.retain(|(p, _)| *p != "docs/RULES.md");
        files.extend([("docs/RULES.md", rules), ("D.md", dep)]);
        let f = Fixture::new("rule-deps", &files);
        verify(&f.0, "D#d", "agent:test", NOTE).expect("D#d verifies");
        assert_eq!(f.diags("is stale"), Vec::<String>::new());
        for unrelated in [
            rules.replace("Second rule.", "Second rule, edited."),
            rules.replacen("status = \"active\"", "status = \"retired\"", 1),
        ] {
            f.write("docs/RULES.md", &unrelated);
            assert_eq!(f.diags("is stale"), Vec::<String>::new(), "{unrelated}");
        }
        f.write(
            "docs/RULES.md",
            &rules.replace("First rule.", "First rule, edited."),
        );
        assert_eq!(f.diags("is stale").len(), 1);
    }

    /// ADR-0042: a `#jstream-meta` cover stays fresh through manifest edits outside the
    /// jstream view, goes stale on edits inside it, and a file also covered plainly is
    /// hashed whole. An unknown view fails `docs check`.
    #[test]
    fn jstream_meta_covers_hash_the_manifest_view() {
        let manifest = "[package]\nname = \"jstream-x\"\n\n[package.metadata.jstream]\nlayer = 1\n\n[dependencies]\njstream-types.workspace = true\n";
        let view = doc("V", "active", "v", "crates/*/Cargo.toml#jstream-meta");
        let both = "<!-- @doc\nid = \"W\"\n-->\n<a id=\"w\"></a>\n## W\n<!-- @section\ncovers = [\"crates/*/Cargo.toml#jstream-meta\", \"crates/x/**\"]\n-->\nWhole.\n";
        let mut files = REGISTRIES.to_vec();
        files.extend([
            ("V.md", view.as_str()),
            ("W.md", both),
            ("crates/x/Cargo.toml", manifest),
        ]);
        let f = Fixture::new("jstream-meta", &files);
        verify(&f.0, "V#v", "agent:test", NOTE).expect("V#v verifies");
        verify(&f.0, "W#w", "agent:test", NOTE).expect("W#w verifies");
        assert_eq!(f.diags("is stale"), Vec::<String>::new());
        let stale_anchors = || -> Vec<String> {
            f.diags("is stale")
                .iter()
                .map(|d| d.split('`').nth(1).unwrap_or_default().to_owned())
                .collect()
        };
        // A third-party dependency and a comment: only the plain cover sees them.
        f.write(
            "crates/x/Cargo.toml",
            &format!("# Note.\n{manifest}serde.workspace = true\n"),
        );
        assert_eq!(stale_anchors(), vec!["w"]);
        // The layer: both go stale.
        f.write(
            "crates/x/Cargo.toml",
            &manifest.replace("layer = 1", "layer = 2"),
        );
        assert_eq!(stale_anchors(), vec!["v", "w"]);
        f.write(
            "Y.md",
            &doc("Y", "active", "y", "crates/*/Cargo.toml#other"),
        );
        f.git(&["add", "Y.md"]);
        let bad = f.diags("`#` is reserved");
        assert!(
            !bad.is_empty() && bad.iter().all(|d| d.starts_with("Y.md")),
            "{bad:?}"
        );
    }

    /// ADR-0039: re-verifying section A, in a file that section B covers, leaves B fresh;
    /// a content edit to that file still stales B.
    #[test]
    fn reverify_does_not_cascade_but_content_edits_do() {
        let mut files = REGISTRIES.to_vec();
        let x = doc("X", "active", "a", "src/**");
        let y = "<!-- @doc\nid = \"Y\"\n-->\n<a id=\"b\"></a>\n## B\n<!-- @section\ncovers = [\"X.md\"]\nverified = { hash = \"b3:0\", by = \"agent:t\", note = \"n\" }\n-->\nAbout X.\n";
        files.extend([("X.md", x.as_str()), ("Y.md", y), ("src/a.rs", "a")]);
        let f = Fixture::new("cascade", &files);
        verify(&f.0, "X#a", "agent:test", NOTE).expect("X#a verifies");
        verify(&f.0, "Y#b", "agent:test", NOTE).expect("Y#b verifies");
        assert_eq!(f.diags("is stale"), Vec::<String>::new());
        // Re-verify A after a change to its covered code: its `verified` line changes.
        f.write("src/a.rs", "a2");
        verify(&f.0, "X#a", "agent:other", &format!("{NOTE}, again")).expect("X#a verifies");
        assert_eq!(f.diags("is stale"), Vec::<String>::new());
        // A prose edit to X.md stales B.
        let text = std::fs::read_to_string(f.0.join("X.md")).expect("invariant: written");
        f.write("X.md", &text.replace("Text.", "Changed text."));
        let stale = f.diags("is stale");
        assert_eq!(stale.len(), 2, "{stale:?}");
        assert!(stale[1].contains("section `b`"), "{stale:?}");
    }

    /// `docs review` writes `reviewed` into the `@doc` block (replacing an older one),
    /// refuses ADRs, docs without a TTL, sections and an unchanged line; `docs log` then
    /// finds each review and verification in git, and each body change as a revision.
    #[test]
    fn review_and_log() {
        let t = "<!-- @doc\nid = \"T\"\nkind = \"guide\"\nreview_every = \"90d\"\n-->\nIntro.\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n";
        let adr = "<!-- @doc\nid = \"ADR-0001\"\nkind = \"adr\"\n-->\nDecision.\n";
        let mut files = REGISTRIES.to_vec();
        files.extend([
            ("T.md", t),
            ("ADR.md", adr),
            ("G.md", "<!-- @doc\nid = \"G\"\n-->\n"),
            ("src/a.rs", "a"),
        ]);
        let f = Fixture::new("review-log", &files);
        let commit = |msg: &str| {
            f.git(&["add", "-A"]);
            f.git(&[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                msg,
            ]);
        };
        commit("docs: add T (#1)");
        for (target, why) in [
            ("ADR-0001", "ADRs have no TTL"),
            ("G", "no `review_every`"),
            ("T#a", "stamps a whole doc"),
            ("NOPE", "no tracked doc"),
        ] {
            let err = review(&f.0, target, "agent:test", NOTE).expect_err(target);
            assert!(format!("{err:#}").contains(why), "{target}: {err:#}");
        }
        review(&f.0, "T", "agent:test", NOTE).expect("T reviews");
        let err = review(&f.0, "T", "agent:test", NOTE).expect_err("same line");
        assert!(format!("{err:#}").contains("already records"), "{err:#}");
        commit("docs: review T (#2)");
        verify(&f.0, "T#a", "agent:test", NOTE).expect("T#a verifies");
        commit("docs: verify T#a (#3)");
        let text = std::fs::read_to_string(f.0.join("T.md")).expect("invariant: written");
        f.write("T.md", &text.replace("Text.", "New text."));
        verify(&f.0, "T#a", "agent:edit", &format!("{NOTE}, edited")).expect("T#a verifies");
        commit("docs: edit T#a (#4)");
        review(&f.0, "T.md", "human:j", &format!("{NOTE}, again")).expect("T reviews again");
        commit("docs: review T again (#5)");
        assert_eq!(f.diags("is stale"), Vec::<String>::new());
        let written = std::fs::read_to_string(f.0.join("T.md")).expect("invariant: written");
        assert_eq!(written.matches("reviewed =").count(), 1, "{written}");
        assert!(written.starts_with("<!-- @doc\nid = \"T\"\nkind = \"guide\"\nreview_every = \"90d\"\nreviewed = { by = \"human:j\""), "{written}");

        let l = load(&f.0).expect("invariant: the fixture is readable");
        let changes = |target: &str| -> Vec<(Option<u32>, String)> {
            history::log(&f.0, &l.docs, target, "HEAD")
                .expect(target)
                .events
                .into_iter()
                .map(|e| {
                    let what = match e.change {
                        history::Change::Revision(n) => format!("r{n}"),
                        history::Change::Stamped(s) => s.by,
                        other => format!("{other:?}"),
                    };
                    (e.commit.pr(), what)
                })
                .collect()
        };
        assert_eq!(
            changes("T"),
            vec![
                (Some(1), "r1".into()),
                (Some(2), "agent:test".into()),
                (Some(4), "r2".into()),
                (Some(5), "human:j".into()),
            ]
        );
        assert_eq!(
            changes("T.md#a"),
            vec![
                (Some(1), "r1".into()),
                (Some(3), "agent:test".into()),
                (Some(4), "r2".into()),
                (Some(4), "agent:edit".into()),
            ]
        );
        let err = history::log(&f.0, &l.docs, "T#nope", "HEAD").expect_err("no anchor");
        assert!(
            format!("{err:#}").contains("no section or rule `nope`"),
            "{err:#}"
        );
    }

    /// A fixture `origin` on `main` and a clone of it, for `docs merge-main`.
    fn origin_and_clone(name: &str, files: &[(&str, &str)]) -> (Fixture, Fixture) {
        let origin = Fixture::new(&format!("{name}-origin"), files);
        origin.git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        origin.git(&["config", "user.name", "t"]);
        origin.git(&["config", "user.email", "t@t"]);
        origin.git(&["add", "-A"]);
        origin.git(&["commit", "-q", "-m", "base"]);
        let work = Fixture(
            origin
                .0
                .with_file_name(format!("xtask-{name}-work-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&work.0);
        let cloned = Command::new("git")
            .args(["clone", "-q"])
            .arg(&origin.0)
            .arg(&work.0)
            .status()
            .is_ok_and(|s| s.success());
        assert!(cloned, "git clone failed");
        work.git(&["config", "user.name", "t"]);
        work.git(&["config", "user.email", "t@t"]);
        (origin, work)
    }

    fn commit_all(f: &Fixture, msg: &str) {
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", msg]);
    }

    /// ADR-0055: `docs merge-main --from wave/<name>` merges the wave branch a PR targets
    /// instead of main, and any other branch name is refused.
    #[test]
    fn merge_main_from_a_wave_branch() {
        let mut files = REGISTRIES.to_vec();
        files.push(("src/a.rs", "a\n"));
        let (origin, work) = origin_and_clone("merge-wave", &files);
        origin.git(&["checkout", "-q", "-b", "wave/m1"]);
        origin.write("src/w.rs", "wave\n");
        commit_all(&origin, "wave: w");
        origin.git(&["checkout", "-q", "main"]);
        for bad in ["dev", "wave/", "-x"] {
            let err = merge::merge_main(&work.0, bad).expect_err("refused");
            assert!(format!("{err:#}").contains("can only merge"), "{err:#}");
        }
        let m = merge::merge_main(&work.0, "wave/m1").expect("merges");
        assert!(m.committed && m.refused.is_empty(), "{m:?}");
        assert!(work.0.join("src/w.rs").exists());
    }

    /// Two branches that each mark a different roadmap item done and regenerate the README
    /// conflict only inside its progress block: `docs merge-main` takes the wave's side,
    /// regenerates the block from the merged `docs/progress.toml` and commits.
    #[test]
    fn merge_main_regenerates_conflicting_generated_blocks() {
        let readme = "<!-- @doc\nid = \"README\"\ntitle = \"R\"\nkind = \"normative\"\nstatus = \"active\"\nreview_every = \"90d\"\n-->\n\n# R\n\n<!-- @generated begin progress -->\n<!-- @generated end progress -->\n";
        let item = |id: &str, status: &str| {
            let prs = if status == "done" { "[1]" } else { "[]" };
            format!(
                "[[item]]\nid = \"{id}\"\nmilestone = \"M0\"\ntitle = \"{id}\"\nstatus = \"{status}\"\nprs = {prs}\n\n"
            )
        };
        let progress = |a: &str, b: &str| {
            format!(
                "[[milestone]]\nid = \"M0\"\ntitle = \"Base\"\nroadmap = \"ROADMAP.md#m0\"\n\n{}{}{}",
                item("A", a),
                item("B", b),
                item("C", "planned")
            )
        };
        let base = progress("planned", "planned");
        let mut files = REGISTRIES.to_vec();
        files.extend([("README.md", readme), ("docs/progress.toml", base.as_str())]);
        let (origin, work) = origin_and_clone("merge-generated", &files);
        let generated = |f: &Fixture| {
            let g = write_generated(&f.0).expect("generates");
            assert!(g.problems.is_empty(), "{:?}", g.problems);
        };
        generated(&origin);
        commit_all(&origin, "main: table");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
        origin.git(&["checkout", "-q", "-b", "wave/m1"]);
        origin.write("docs/progress.toml", &progress("done", "planned"));
        generated(&origin);
        commit_all(&origin, "wave: A done");
        origin.git(&["checkout", "-q", "main"]);
        work.write("docs/progress.toml", &progress("planned", "done"));
        generated(&work);
        commit_all(&work, "work: B done");

        let m = merge::merge_main(&work.0, "wave/m1").expect("merges");
        assert!(m.committed && m.refused.is_empty(), "{m:?}");
        assert_eq!(m.generated, vec!["README.md".to_owned()]);
        let text = std::fs::read_to_string(work.0.join("README.md")).expect("readme");
        assert!(text.contains("2 of 3 items are merged"), "{text}");
        assert!(!text.contains("<<<<<<<"), "{text}");
        assert!(write_generated(&work.0).expect("again").files.is_empty());
    }

    /// ADR-0053 with `docs merge-main`: two branches that change different covered files
    /// of one section and re-verify it merge without a conflict, and the section stays
    /// fresh; changing the same covered file conflicts on its ledger entry, which takes
    /// main's and leaves the section stale; any other conflict is left alone and the
    /// merge stays open; a dirty tree is refused.
    #[test]
    fn merge_main_resolves_only_stamp_conflicts() {
        let sec = |id: &str, glob: &str| {
            format!(
                "<!-- @doc\nid = \"{id}\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"{glob}\"]\n-->\nText.\n"
            )
        };
        let mut files = REGISTRIES.to_vec();
        let (s, t) = (sec("S", "src/**"), sec("T", "lib/**"));
        files.extend([
            ("S.md", s.as_str()),
            ("T.md", t.as_str()),
            ("src/a.rs", "a1\n\n\n\n\na5\n"),
            ("src/b.rs", "b\n"),
            ("lib/c.rs", "c\n"),
        ]);
        let (origin, work) = origin_and_clone("merge-main", &files);
        verify(&origin.0, "S#a", "agent:test", NOTE).expect("S verifies");
        verify(&origin.0, "T#a", "agent:test", NOTE).expect("T verifies");
        commit_all(&origin, "stamps");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);

        // Different covered files of S: no conflict at all, and S is fresh.
        origin.write("src/a.rs", "a1 main\n\n\n\n\na5\n");
        verify(&origin.0, "S#a", "agent:main", NOTE).expect("S verifies on main");
        commit_all(&origin, "main: a");
        work.write("src/b.rs", "b work\n");
        verify(&work.0, "S#a", "agent:work", NOTE).expect("S verifies on work");
        commit_all(&work, "work: b");
        work.write("src/b.rs", "dirty\n");
        let err = merge::merge_main(&work.0, "main").expect_err("dirty tree");
        assert!(format!("{err:#}").contains("uncommitted"), "{err:#}");
        work.git(&["checkout", "--", "src/b.rs"]);
        let m = merge::merge_main(&work.0, "main").expect("merges");
        assert!(
            m.committed && m.stamps.is_empty() && m.refused.is_empty(),
            "{m:?}"
        );
        assert!(m.stale.is_empty() && m.migrated.is_none(), "{m:?}");
        assert!(work.diags("is stale").is_empty());

        // The same covered file, merged cleanly by git: its entry conflicts, takes main's,
        // and S is stale.
        origin.write("src/a.rs", "a1 main\n\n\n\n\na5 main\n");
        verify(&origin.0, "S#a", "agent:main", NOTE).expect("S verifies on main");
        commit_all(&origin, "main: a again");
        work.write("src/a.rs", "a1 work\n\n\n\n\na5\n");
        let work_note = "a1 is the branch's change to a.rs and the text still describes it";
        verify(&work.0, "S#a", "agent:work", work_note).expect("S verifies on work");
        commit_all(&work, "work: a");
        let m = merge::merge_main(&work.0, "main").expect("merges");
        assert!(m.committed, "{m:?}");
        assert_eq!(m.stamps, vec!["S#a".to_owned()]);
        assert_eq!(m.stale, vec!["S#a".to_owned()]);
        // The stale input is reported with the branch's note on it and the merged commit
        // that changed it; b.rs, which the merge left alone, isn't.
        let i = &m.inputs["S#a"];
        assert_eq!(i.len(), 1, "{i:?}");
        assert_eq!(
            (i[0].input.as_str(), i[0].note.as_deref()),
            ("file:src/a.rs", Some(work_note))
        );
        assert_eq!(subjects(&i[0].commits), vec!["main: a again"]);
        let ledger =
            std::fs::read_to_string(work.0.join(ledger::PATH)).expect("invariant: written");
        assert!(
            !ledger.contains("<<<<<<<") && ledger.contains("agent:main"),
            "{ledger}"
        );
        assert_eq!(work.diags("is stale").len(), 1);

        // Now S's text conflicts too (and T's entry for lib/c.rs alone): S.md and
        // lib/c.rs are refused, the ledger is resolved and staged, nothing is committed.
        verify(&work.0, "S#a", "agent:work", NOTE).expect("S re-verifies");
        commit_all(&work, "work: re-verify S");
        for (f, who) in [(&origin, "main"), (&work, "work")] {
            let text = std::fs::read_to_string(f.0.join("S.md")).expect("invariant: written");
            f.write("S.md", &text.replace("Text.", &format!("Text from {who}.")));
            f.write("lib/c.rs", &format!("c {who}\n"));
            f.git(&["add", "-A"]);
            verify(&f.0, "T#a", &format!("agent:{who}"), NOTE).expect("T verifies");
            verify(&f.0, "S#a", &format!("agent:{who}"), NOTE).expect("S verifies");
            commit_all(f, who);
        }
        let m = merge::merge_main(&work.0, "main").expect("runs");
        assert!(!m.committed, "{m:?}");
        assert_eq!(m.stamps, vec!["S#a".to_owned(), "T#a".to_owned()]);
        let refused: Vec<&str> = m.refused.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(refused, vec!["S.md", "lib/c.rs"], "{m:?}");
        let s = std::fs::read_to_string(work.0.join("S.md")).expect("invariant: written");
        assert!(
            s.contains("<<<<<<<"),
            "S.md must be left as git wrote it: {s}"
        );
        let unmerged = Command::new("git")
            .args(["diff", "--name-only", "--diff-filter=U"])
            .current_dir(&work.0)
            .output()
            .expect("invariant: git runs");
        assert_eq!(
            String::from_utf8_lossy(&unmerged.stdout),
            "S.md\nlib/c.rs\n"
        );
        let err = merge::merge_main(&work.0, "main").expect_err("merge in progress");
        assert!(
            format!("{err:#}").contains("already in progress"),
            "{err:#}"
        );
    }

    /// #697: after `docs merge-main`, an entry the branch holds at the same hash as main's,
    /// but with a note of its own (as an older `verify --merge` left on inputs a PR into a
    /// wave doesn't change, #607 review B5), takes main's entry; one the branch verified at
    /// other content keeps the branch's note. The change is part of the merge commit, so
    /// nothing follows it, and a merge with nothing new adopts nothing.
    #[test]
    fn merge_main_takes_main_entries_at_the_same_hash() {
        let mut files = REGISTRIES.to_vec();
        let s = "<!-- @doc\nid = \"S\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n";
        let t = s.replace("\"S\"", "\"T\"").replace("src/**", "lib/**");
        files.extend([
            ("S.md", s),
            ("T.md", t.as_str()),
            ("src/a.rs", "a\n"),
            ("src/b.rs", "b\n"),
            ("lib/c.rs", "c\n"),
        ]);
        let (origin, work) = origin_and_clone("merge-adopt", &files);
        verify(&origin.0, "S#a", "agent:main", NOTE).expect("S verifies");
        verify(&origin.0, "T#a", "agent:main", NOTE).expect("T verifies");
        commit_all(&origin, "stamps");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);

        // The branch changes b.rs and verifies it, then carries its own note on a.rs,
        // which it leaves alone, at main's hash.
        work.write("src/b.rs", "b work\n");
        let work_note = "b is the branch's change to b.rs and the text still describes it";
        verify(&work.0, "S#a", "agent:work", work_note).expect("S verifies on work");
        let mut l = ledger::Ledger::read(&work.0).expect("reads");
        let a = l
            .sections
            .get_mut("S#a")
            .and_then(|s| s.get_mut("file:src/a.rs"))
            .expect("invariant: a.rs is stamped");
        let main_a = a.clone();
        a.by = Some("agent:work".into());
        a.note = Some("a note of the branch's on an input it leaves alone".into());
        l.write(&work.0).expect("writes");
        commit_all(&work, "work: b");
        let before = work.rev("HEAD");
        // Main moves on elsewhere, so the merge is a real one with no ledger conflict.
        origin.write("lib/c.rs", "c main\n");
        verify(&origin.0, "T#a", "agent:main", NOTE).expect("T verifies on main");
        commit_all(&origin, "main: c");

        let m = merge::merge_main(&work.0, "main").expect("merges");
        assert!(
            m.committed && m.refused.is_empty() && m.stamps.is_empty(),
            "{m:?}"
        );
        assert_eq!(m.adopted, vec!["S#a".to_owned()]);
        assert!(m.stale.is_empty(), "{m:?}");
        let s = &ledger::Ledger::read(&work.0).expect("reads").sections["S#a"];
        assert_eq!(s["file:src/a.rs"], main_a);
        assert_eq!(s["file:src/b.rs"].note.as_deref(), Some(work_note));
        // In the merge commit itself: HEAD is the merge, its first parent the branch.
        assert_eq!(work.rev("HEAD^1"), before);
        assert_eq!(work.rev("HEAD^2"), origin.rev("HEAD"));
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&work.0)
            .output()
            .expect("invariant: git runs");
        assert!(status.stdout.is_empty(), "{status:?}");
        assert!(work.diags("is stale").is_empty());

        // Nothing new on main: no merge, nothing adopted.
        let m = merge::merge_main(&work.0, "main").expect("up to date");
        assert!(m.committed && m.adopted.is_empty(), "{m:?}");
        assert_eq!(work.rev("HEAD^1"), before);
    }

    /// The subjects of `<hash> <subject>` lines.
    fn subjects(commits: &[String]) -> Vec<&str> {
        commits
            .iter()
            .filter_map(|c| c.split_once(' ').map(|(_, s)| s))
            .collect()
    }

    /// DOCS.md#notes: after `docs merge-main`, `docs verify --merge` keeps the note the
    /// branch had written on each entry the merge changed and adds what the merge changed.
    /// It also repairs an entry a plain re-verify after the merge gave a merge-only note
    /// (#259 B4, #321 B2), and a later merge replaces the earlier merge's addendum.
    #[test]
    fn verify_merge_keeps_the_branch_notes() {
        let mut files = REGISTRIES.to_vec();
        let s = "<!-- @doc\nid = \"S\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n";
        files.extend([
            ("S.md", s),
            ("src/a.rs", "a1\n\n\n\n\na5\n"),
            ("src/b.rs", "b\n"),
        ]);
        let (origin, work) = origin_and_clone("verify-merge", &files);
        verify(&origin.0, "S#a", "agent:test", NOTE).expect("S verifies");
        commit_all(&origin, "stamps");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
        let err = verify_merge(&work.0, "S#a", "agent:work", NOTE).expect_err("no merge yet");
        assert!(
            format!("{err:#}").contains("no merge on this branch"),
            "{err:#}"
        );

        // The branch changes a.rs and b.rs; main changes a.rs too, so a.rs's entry
        // conflicts and takes main's.
        work.write("src/a.rs", "a1 work\n\n\n\n\na5\n");
        work.write("src/b.rs", "b work\n");
        let pr = "the branch's a1 and b edits, which the section text still describes";
        verify(&work.0, "S#a", "agent:work", pr).expect("S verifies on work");
        commit_all(&work, "work: a and b");
        let main_edit = |n: &str| {
            origin.write("src/a.rs", &format!("a1\n\n\n\n\na5 main {n}\n"));
            verify(&origin.0, "S#a", "agent:main", NOTE).expect("S verifies on main");
            commit_all(&origin, &format!("main: a5 {n} (#{n})"));
        };
        main_edit("1");
        let m = merge::merge_main(&work.0, "main").expect("merges");
        assert_eq!(m.stale, vec!["S#a".to_owned()], "{m:?}");
        let entries = || {
            ledger::Ledger::read(&work.0)
                .expect("invariant: parses")
                .sections["S#a"]
                .clone()
        };
        let note = |input: &str| entries()[input].note.clone().unwrap_or_default();
        let joined = |merge: &str| format!("{pr}{}{merge}", ledger::MERGE_MARK);

        // A plain re-verify replaces the branch's note on a.rs with a merge-only one.
        let stock = "merged main: a5 edit, the section text still holds";
        verify(&work.0, "S#a", "agent:work", stock).expect("S verifies");
        assert_eq!(note("file:src/a.rs"), stock);
        let b = entries()["file:src/b.rs"].clone();

        // `--merge` puts the branch's note back on a.rs, with what the merge changed, and
        // leaves b.rs (which the merge didn't change) and the body as they were.
        let first = "main's a5 edit (#1) is outside what the text says about a.rs";
        verify_merge(&work.0, "S#a", "agent:work", first).expect("S verifies after the merge");
        assert_eq!(note("file:src/a.rs"), joined(first));
        assert_eq!(entries()["file:src/b.rs"], b);
        assert!(work.diags("is stale").is_empty());
        commit_all(&work, "work: re-verify S after the merge");

        // The next merge reports the note with the first addendum, and `--merge` replaces
        // that addendum with its own.
        main_edit("2");
        let m = merge::merge_main(&work.0, "main").expect("merges again");
        let i = &m.inputs["S#a"];
        assert_eq!(i[0].note.as_deref(), Some(joined(first).as_str()), "{i:?}");
        assert_eq!(subjects(&i[0].commits), vec!["main: a5 2 (#2)"]);
        let second = "main's second a5 edit (#2) is outside it too";
        verify_merge(&work.0, "S#a", "agent:work", second).expect("S verifies");
        assert_eq!(note("file:src/a.rs"), joined(second));
        assert!(work.diags("is stale").is_empty());
    }

    /// #596: on an input whose hash after the merge equals the merged side's entry,
    /// `docs verify --merge` keeps that entry byte for byte, even when the branch had
    /// written its own entry there before the merge (#539 N3, #323). The branch's note is
    /// still kept and extended on the input the branch changed.
    #[test]
    fn verify_merge_keeps_the_merged_entry_on_inputs_the_branch_leaves_alone() {
        let mut files = REGISTRIES.to_vec();
        let s = "<!-- @doc\nid = \"S\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n";
        files.extend([
            ("S.md", s),
            ("src/a.rs", "a1\n\n\n\n\na5\n"),
            ("src/b.rs", "b1\n\n\n\n\nb5\n"),
        ]);
        let (origin, work) = origin_and_clone("verify-merge-theirs", &files);
        verify(&origin.0, "S#a", "agent:test", NOTE).expect("S verifies");
        commit_all(&origin, "stamps");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);

        // The branch changes a.rs, and makes an edit to b.rs that the wave also makes (as
        // when part of the branch's work lands in the wave through another PR).
        work.write("src/a.rs", "a1 work\n\n\n\n\na5\n");
        work.write("src/b.rs", "b1 new\n\n\n\n\nb5\n");
        let pr = "the branch's a1 and b1 edits, which the section text still describes";
        verify(&work.0, "S#a", "agent:work", pr).expect("S verifies on work");
        commit_all(&work, "work: a1 and b1");
        // The wave changes a5, and b.rs to a version that contains the branch's edit, and
        // restamps both with its own note.
        origin.write("src/a.rs", "a1\n\n\n\n\na5 wave\n");
        origin.write("src/b.rs", "b1 new\n\n\n\n\nb5 wave\n");
        let wave = "the wave's a5 and b edits, which the section text still describes";
        verify(&origin.0, "S#a", "agent:wave-owner", wave).expect("S verifies on the wave");
        commit_all(&origin, "wave: a5 and b");
        let theirs = ledger::Ledger::read(&origin.0)
            .expect("invariant: parses")
            .sections["S#a"]
            .clone();
        merge::merge_main(&work.0, "main").expect("merges");
        // Git on Windows checks the merge out with CRLF line endings.
        let merged_b = std::fs::read_to_string(work.0.join("src/b.rs"))
            .expect("invariant: written")
            .replace("\r\n", "\n");
        assert_eq!(merged_b, "b1 new\n\n\n\n\nb5 wave\n", "b.rs is the wave's");

        let first = "the wave's a5 edit is outside what the text says about a.rs";
        verify_merge(&work.0, "S#a", "agent:work", first).expect("S verifies after the merge");
        let entries = ledger::Ledger::read(&work.0)
            .expect("invariant: parses")
            .sections["S#a"]
            .clone();
        assert_eq!(entries["file:src/b.rs"], theirs["file:src/b.rs"]);
        let a = &entries["file:src/a.rs"];
        assert_eq!(
            a.note.as_deref(),
            Some(format!("{pr}{}{first}", ledger::MERGE_MARK).as_str())
        );
        assert_eq!(a.by.as_deref(), Some("agent:work"));
        assert!(work.diags("is stale").is_empty());
        // The ledger's text on b.rs is the wave's, byte for byte.
        let text = |root: &Path| {
            let t = std::fs::read_to_string(root.join(ledger::PATH)).expect("invariant: written");
            t.lines()
                .skip_while(|l| !l.contains("file:src/b.rs"))
                .take_while(|l| !l.trim().is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(text(&work.0), text(&origin.0));
        assert!(!text(&work.0).is_empty());
    }

    /// #525: `docs verify --merge` refuses a section when the file of an input it would
    /// rewrite changed after the merge, committed or not, since the branch's earlier note
    /// doesn't cover that edit; `--note` still verifies it. A change after the merge to a
    /// file outside the rewritten inputs, or the `docs migrate` commit `merge-main` makes
    /// right after the merge, doesn't refuse.
    #[test]
    fn verify_merge_refuses_inputs_changed_after_the_merge() {
        let mut files = REGISTRIES.to_vec();
        let s = "<!-- @doc\nid = \"S\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\ndepends_on = [\"D.md#d\"]\n-->\nText.\n";
        let d = "<!-- @doc\nid = \"D\"\n-->\n<a id=\"d\"></a>\n## D\nDep.\n";
        files.extend([
            ("S.md", s),
            ("D.md", d),
            ("src/a.rs", "a1\n\n\n\n\na5\n"),
            ("other.txt", "o\n"),
        ]);
        let (origin, work) = origin_and_clone("verify-merge-later", &files);
        verify(&origin.0, "S#a", "agent:test", NOTE).expect("S verifies");
        commit_all(&origin, "stamps");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
        work.write("src/a.rs", "a1 work\n\n\n\n\na5\n");
        verify(&work.0, "S#a", "agent:work", "the branch's a1 edit").expect("S verifies");
        commit_all(&work, "work: a1");
        origin.write("src/a.rs", "a1\n\n\n\n\na5 main\n");
        origin.write("D.md", &d.replace("Dep.", "Dep, from main."));
        verify(&origin.0, "S#a", "agent:main", NOTE).expect("S verifies on main");
        commit_all(&origin, "main: a5 and D");
        let m = merge::merge_main(&work.0, "main").expect("merges");
        assert_eq!(m.stale, vec!["S#a".to_owned()], "{m:?}");
        let refused = |what: &str| {
            let err = verify_merge(&work.0, "S#a", "agent:work", "main's a5 and D edits")
                .expect_err(what);
            format!("{err:#}")
        };

        // A commit after the merge with the subject `merge-main` gives its `docs migrate`
        // commit counts as part of the merge.
        work.write("src/a.rs", "a1 work\n\n\n\n\na5 main\n\n");
        work.git(&["add", "-A"]);
        work.git(&["commit", "-q", "-m", merge::MIGRATE_SUBJECT]);
        verify_merge(&work.0, "S#a", "agent:work", "main's a5 and D edits")
            .expect("the migrate commit is part of the merge");
        work.git(&["checkout", "-q", "--", ledger::PATH]);
        // The rest of the branch's later work refuses each rewritten input it touched, by
        // the file it lives in: a covered file committed after the merge, the dependency's
        // doc edited without a commit.
        work.write("src/a.rs", "a1 work later\n\n\n\n\na5 main\n\n");
        commit_all(&work, "work: a1 again");
        work.write("D.md", &d.replace("Dep.", "Dep, from main, and later."));
        let err = refused("a.rs and D.md changed after the merge");
        for want in [
            "S#a",
            "file:src/a.rs (src/a.rs)",
            "dep:D.md#d (D.md)",
            "--note",
        ] {
            assert!(err.contains(want), "{want}: {err}");
        }
        assert!(!err.contains("body"), "{err}");
        // So does the section's own text, edited after the merge.
        work.git(&["checkout", "-q", "--", "D.md"]);
        work.write("S.md", &s.replace("Text.", "Text, later."));
        let err = refused("S.md changed after the merge");
        assert!(err.contains("body (S.md)"), "{err}");
        assert!(work.diags("is stale").len() == 1, "nothing was written");
        // `--note` covers the later edits.
        verify(
            &work.0,
            "S#a",
            "agent:work",
            "a1 again, the text, and main's a5 and D",
        )
        .expect("--note verifies");
        assert!(work.diags("is stale").is_empty());
        commit_all(&work, "work: a1 and text after the merge");

        // Accepted: back on the merge, with only a file outside the section changed since.
        work.git(&["reset", "-q", "--hard", "HEAD~3"]);
        work.write("other.txt", "o later\n");
        commit_all(&work, "work: other");
        verify_merge(&work.0, "S#a", "agent:work", "main's a5 and D edits")
            .expect("nothing the section reads changed after the merge");
        let entries = ledger::Ledger::read(&work.0)
            .expect("invariant: parses")
            .sections["S#a"]
            .clone();
        assert_eq!(
            entries["file:src/a.rs"].note.as_deref(),
            Some(
                format!(
                    "the branch's a1 edit{}main's a5 and D edits",
                    ledger::MERGE_MARK
                )
                .as_str()
            )
        );
        assert!(work.diags("is stale").is_empty());
    }

    /// `docs verify --renote` replaces the note on the entries this branch wrote on a fresh
    /// section, keeping their hashes and every other entry (#521, #523); it refuses a stale
    /// section, a section the branch never stamped, and a branch that isn't main or a wave.
    #[test]
    fn verify_renote_rewrites_a_fresh_note() {
        let mut files = REGISTRIES.to_vec();
        let s = "<!-- @doc\nid = \"S\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n";
        files.extend([("S.md", s), ("src/a.rs", "a\n"), ("src/b.rs", "b\n")]);
        let (origin, work) = origin_and_clone("verify-renote", &files);
        verify(&origin.0, "S#a", "agent:main", NOTE).expect("S verifies");
        commit_all(&origin, "stamps");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
        let better = "a.rs now returns early; the section's sentence on a.rs still says so";
        let err = verify_renote(&work.0, "S#a", "agent:work", better, "main")
            .expect_err("nothing written yet");
        assert!(format!("{err:#}").contains("nothing to renote"), "{err:#}");

        work.write("src/a.rs", "a work\n");
        let entries = || {
            ledger::Ledger::read(&work.0)
                .expect("invariant: parses")
                .sections["S#a"]
                .clone()
        };
        let err = verify_renote(&work.0, "S#a", "agent:work", better, "main").expect_err("stale");
        assert!(format!("{err:#}").contains("is stale"), "{err:#}");
        verify(
            &work.0,
            "S#a",
            "agent:work",
            "a.rs changed and the text holds",
        )
        .expect("verifies");
        let before = entries();
        verify_renote(&work.0, "S#a", "agent:review", better, "main").expect("renotes");
        let after = entries();
        let a = &after["file:src/a.rs"];
        assert_eq!(a.hash, before["file:src/a.rs"].hash);
        assert_eq!(
            (a.by.as_deref(), a.note.as_deref()),
            (Some("agent:review"), Some(better))
        );
        assert_eq!(after["file:src/b.rs"], before["file:src/b.rs"]);
        assert_eq!(after["body"], before["body"]);
        assert!(work.diags("is stale").is_empty());
        // The refusal fits --renote, which merges nothing (#534).
        let err = verify_renote(&work.0, "S#a", "agent:work", better, "-x").expect_err("branch");
        let err = format!("{err:#}");
        assert!(
            err.contains("can only compare against `main`, a `wave/<name>` branch or a stacked"),
            "{err}"
        );
        assert!(!err.contains("merge"), "{err}");
    }

    /// #715: `docs verify --renote --from <base>` on a PR stacked on another PR's branch
    /// rewrites only the entries written above that base, keeping the base's own; it
    /// refuses a base that isn't on the remote or isn't an ancestor of `HEAD`.
    #[test]
    fn verify_renote_takes_a_stacked_base() {
        let mut files = REGISTRIES.to_vec();
        let s = "<!-- @doc\nid = \"S\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n";
        files.extend([("S.md", s), ("src/a.rs", "a\n"), ("src/b.rs", "b\n")]);
        let (origin, work) = origin_and_clone("verify-renote-stacked", &files);
        verify(&origin.0, "S#a", "agent:main", NOTE).expect("S verifies");
        commit_all(&origin, "stamps");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
        // The base PR changes a.rs; the stacked PR above it changes b.rs.
        work.git(&["checkout", "-q", "-b", "base-pr"]);
        work.write("src/a.rs", "a base\n");
        verify(&work.0, "S#a", "agent:base", "the base PR's a.rs edit").expect("verifies");
        commit_all(&work, "base PR");
        work.git(&["push", "-q", "origin", "base-pr"]);
        work.git(&["checkout", "-q", "-b", "stacked-pr"]);
        work.write("src/b.rs", "b stacked\n");
        verify(
            &work.0,
            "S#a",
            "agent:stacked",
            "the stacked PR's b.rs edit",
        )
        .expect("verifies");
        commit_all(&work, "stacked PR");
        let better = "b.rs now returns early; the section's sentence on b.rs still says so";
        verify_renote(&work.0, "S#a", "agent:review", better, "base-pr").expect("renotes");
        let after = ledger::Ledger::read(&work.0)
            .expect("invariant: parses")
            .sections["S#a"]
            .clone();
        assert_eq!(after["file:src/b.rs"].note.as_deref(), Some(better));
        assert_eq!(
            after["file:src/a.rs"].note.as_deref(),
            Some("the base PR's a.rs edit"),
            "the base PR's entry keeps its note"
        );

        let err = verify_renote(&work.0, "S#a", "agent:review", better, "gone")
            .expect_err("not on the remote");
        assert!(
            format!("{err:#}").contains("`origin/gone` doesn't exist"),
            "{err:#}"
        );
        // The base PR moves on; until the stacked PR merges it, it isn't an ancestor.
        commit_all(&work, "stacked PR: renoted");
        work.git(&["checkout", "-q", "base-pr"]);
        work.write("src/a.rs", "a base 2\n");
        commit_all(&work, "base PR, round 2");
        work.git(&["push", "-q", "origin", "base-pr"]);
        work.git(&["checkout", "-q", "stacked-pr"]);
        let err = verify_renote(&work.0, "S#a", "agent:review", better, "base-pr")
            .expect_err("not an ancestor");
        assert!(format!("{err:#}").contains("isn't an ancestor"), "{err:#}");
    }

    /// `docs verify --renote` fails when the ledger where the branch left main doesn't
    /// parse, rather than reading it as empty and taking every entry for the branch's own
    /// (#534).
    #[test]
    fn verify_renote_refuses_an_unparsable_base_ledger() {
        let mut files = REGISTRIES.to_vec();
        let s = "<!-- @doc\nid = \"S\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n";
        files.extend([("S.md", s), ("src/a.rs", "a\n")]);
        let (origin, work) = origin_and_clone("verify-renote-bad-base", &files);
        origin.write(ledger::PATH, "this is [not a ledger\n");
        commit_all(&origin, "broken ledger");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
        std::fs::remove_file(work.0.join(ledger::PATH)).expect("invariant: pulled");
        verify(&work.0, "S#a", "agent:work", NOTE).expect("S verifies");
        commit_all(&work, "work: a fresh ledger");
        let before = std::fs::read_to_string(work.0.join(ledger::PATH)).expect("written");
        let better = "a.rs is unchanged and the section's sentence on it still holds";
        let err = verify_renote(&work.0, "S#a", "agent:review", better, "main")
            .expect_err("the base ledger doesn't parse");
        let err = format!("{err:#}");
        assert!(err.contains("doesn't parse"), "{err}");
        assert!(err.contains(ledger::PATH), "{err}");
        let after = std::fs::read_to_string(work.0.join(ledger::PATH)).expect("written");
        assert_eq!(before, after, "nothing was written");
    }

    /// #551: `docs verify --merge` fails when the ledger at the merge base doesn't parse,
    /// rather than taking every entry the branch had before its merge for its own.
    #[test]
    fn verify_merge_refuses_an_unparsable_base_ledger() {
        let mut files = REGISTRIES.to_vec();
        let s = "<!-- @doc\nid = \"S\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n";
        files.extend([("S.md", s), ("src/a.rs", "a\n")]);
        let (origin, work) = origin_and_clone("verify-merge-bad-base", &files);
        origin.write(ledger::PATH, "this is [not a ledger\n");
        commit_all(&origin, "broken ledger");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
        std::fs::remove_file(work.0.join(ledger::PATH)).expect("invariant: pulled");
        verify(&work.0, "S#a", "agent:work", NOTE).expect("S verifies");
        commit_all(&work, "work: a fresh ledger");
        origin.write("src/a.rs", "a main\n");
        commit_all(&origin, "main: a");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
        let before = std::fs::read_to_string(work.0.join(ledger::PATH)).expect("written");
        let err = verify_merge(
            &work.0,
            "S#a",
            "agent:work",
            "main's a.rs edit is outside what the text says",
        )
        .expect_err("the base ledger doesn't parse");
        let err = format!("{err:#}");
        assert!(err.contains("at the merge's base"), "{err}");
        assert!(err.contains("doesn't parse"), "{err}");
        assert!(err.contains(ledger::PATH), "{err}");
        let after = std::fs::read_to_string(work.0.join(ledger::PATH)).expect("written");
        assert_eq!(before, after, "nothing was written");
    }

    /// #551: `docs verify --merge` fails when the ledger on the branch before its merge
    /// doesn't parse, rather than finding no entries of the branch's own. Here the branch
    /// committed a broken ledger and repaired it after the merge, uncommitted.
    #[test]
    fn verify_merge_refuses_an_unparsable_branch_ledger() {
        let mut files = REGISTRIES.to_vec();
        let s = "<!-- @doc\nid = \"S\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n";
        files.extend([("S.md", s), ("src/a.rs", "a\n")]);
        let (origin, work) = origin_and_clone("verify-merge-bad-branch", &files);
        verify(&origin.0, "S#a", "agent:test", NOTE).expect("S verifies");
        commit_all(&origin, "stamps");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
        work.write(ledger::PATH, "this is [not a ledger\n");
        commit_all(&work, "work: broken ledger");
        origin.write("src/a.rs", "a main\n");
        commit_all(&origin, "main: a");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);
        work.git(&["checkout", "origin/main", "--", ledger::PATH]);
        let before = std::fs::read_to_string(work.0.join(ledger::PATH)).expect("written");
        let err = verify_merge(
            &work.0,
            "S#a",
            "agent:work",
            "main's a.rs edit is outside what the text says",
        )
        .expect_err("the branch's ledger doesn't parse");
        let err = format!("{err:#}");
        assert!(err.contains("on this branch before its merge"), "{err}");
        assert!(err.contains("doesn't parse"), "{err}");
        assert!(err.contains(ledger::PATH), "{err}");
        let after = std::fs::read_to_string(work.0.join(ledger::PATH)).expect("written");
        assert_eq!(before, after, "nothing was written");
    }

    /// #551 N1: `docs merge-main` moves inline stamps and lists the branch's notes after it
    /// commits the merge, so a ledger that doesn't parse fails there; the error says the
    /// merge is committed rather than reading as a failed merge.
    #[test]
    fn merge_main_says_the_merge_is_committed_when_the_notes_cannot_be_listed() {
        let mut files = REGISTRIES.to_vec();
        let s = "<!-- @doc\nid = \"S\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n-->\nText.\n";
        files.extend([("S.md", s), ("src/a.rs", "a\n")]);
        let (origin, work) = origin_and_clone("merge-main-bad-branch", &files);
        work.write(ledger::PATH, "this is [not a ledger\n");
        commit_all(&work, "work: broken ledger");
        origin.write("src/a.rs", "a main\n");
        commit_all(&origin, "main: a");
        let err = merge::merge_main(&work.0, "main").expect_err("the notes can't be listed");
        let err = format!("{err:#}");
        assert!(err.contains("the merge is committed"), "{err}");
        assert!(err.contains(ledger::PATH), "{err}");
        let out = Command::new("git")
            .args(["rev-list", "--parents", "-n", "1", "HEAD"])
            .current_dir(&work.0)
            .output()
            .expect("git runs");
        let parents = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            parents.split_whitespace().count(),
            3,
            "HEAD is the merge: {parents}"
        );
    }

    /// ADR-0053 transition: a branch from before the ledger re-stamped a section inline and
    /// added a new section with an inline stamp; main moved every stamp to the ledger.
    /// `docs merge-main` takes main's side of the inline conflict, then moves the new
    /// section's stamp to the ledger in a second commit.
    #[test]
    fn merge_main_moves_a_pre_ledger_branch_to_the_ledger() {
        let inline = |id: &str, glob: &str, text: &str| {
            format!(
                "<!-- @doc\nid = \"{id}\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"{glob}\"]\nverified = {{ hash = \"H\", by = \"agent:old\", note = \"note\" }}\n-->\n{text}\n"
            )
        };
        // Writes the pre-ADR-0053 hash into `path`'s inline stamp, as the old verify did.
        let stamp_inline = |f: &Fixture, path: &str, glob: &str| {
            let l = load(&f.0).expect("invariant: the fixture is readable");
            let by_path: BTreeMap<&str, &parse::Doc> =
                l.docs.iter().map(|d| (d.path.as_str(), d)).collect();
            let files = hash::Files::new(&f.0, &l.files);
            let d = by_path[path];
            let h = hash::legacy_section_hash(d, "a", &[glob.to_owned()], &[], &files, &by_path)
                .expect("invariant: hashable");
            let text = d.text.lines().map(|l| {
                if l.starts_with("verified") {
                    format!("verified = {{ hash = \"{h}\", by = \"agent:old\", note = \"note\" }}")
                } else {
                    l.to_owned()
                }
            });
            f.write(path, &(text.collect::<Vec<_>>().join("\n") + "\n"));
        };
        let mut files = REGISTRIES.to_vec();
        let s = inline("S", "src/**", "Text.");
        files.extend([("S.md", s.as_str()), ("src/a.rs", "a\n")]);
        let (origin, work) = origin_and_clone("merge-pre-ledger", &files);
        stamp_inline(&origin, "S.md", "src/**");
        commit_all(&origin, "inline stamp");
        work.git(&["pull", "-q", "--no-rebase", "origin", "main"]);

        migrate(&origin.0).expect("main migrates");
        commit_all(&origin, "main: ledger");
        work.write("src/a.rs", "a work\n");
        stamp_inline(&work, "S.md", "src/**");
        work.write("N.md", &inline("N", "lib/**", "New."));
        work.write("lib/n.rs", "n\n");
        work.git(&["add", "-A"]);
        stamp_inline(&work, "N.md", "lib/**");
        commit_all(&work, "work: old-style stamps");

        let m = merge::merge_main(&work.0, "main").expect("merges");
        assert!(m.committed && m.refused.is_empty(), "{m:?}");
        assert_eq!(m.stamps, vec!["S#a".to_owned()]);
        let mig = m.migrated.expect("the new section's stamp moved");
        assert_eq!(mig.moved, vec!["N#a".to_owned()]);
        assert_eq!(m.stale, vec!["S#a".to_owned()]);
        for path in ["S.md", "N.md"] {
            let text = std::fs::read_to_string(work.0.join(path)).expect("invariant: written");
            assert!(
                !text.contains("verified") && !text.contains("<<<<<<<"),
                "{text}"
            );
        }
        assert!(work.diags("inline `verified`").is_empty());
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&work.0)
            .output()
            .expect("invariant: git runs");
        assert!(
            status.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&status.stdout)
        );
    }

    const MAP: &str = "<!-- @doc\nid = \"M\"\n-->\n<a id=\"map\"></a>\n## Map\n<!-- @section\n-->\n| L | Crate |\n|---|---|\n| 0 | `a` |\n| 1 | `b` |\n";

    /// ADR-0039: a row-level dependency stales only when its own row changes, and must
    /// name exactly one row.
    #[test]
    fn row_dependency_stales_only_on_its_row() {
        let mut files = REGISTRIES.to_vec();
        let p = |dep: &str| {
            format!(
                "<!-- @doc\nid = \"P\"\n-->\n<a id=\"p\"></a>\n## P\n<!-- @section\ndepends_on = [\"{dep}\"]\nverified = {{ hash = \"b3:0\", by = \"agent:t\", note = \"n\" }}\n-->\nPurpose.\n"
            )
        };
        let (pa, pz) = (p("M.md#map/a"), p("M.md#map/z"));
        files.extend([("M.md", MAP), ("P.md", pa.as_str()), ("Z.md", "")]);
        let f = Fixture::new("rows", &files);
        f.write(
            "Z.md",
            &pz.replace("id = \"P\"", "id = \"Z\"")
                .replace("id=\"p\"", "id=\"z\""),
        );
        f.git(&["add", "-A"]);
        assert_eq!(
            f.diags("`depends_on` row"),
            vec!["Z.md:5: `depends_on` row `M.md#map/z`: no table row has a cell `` `z` ``"]
        );
        verify(&f.0, "P#p", "agent:test", NOTE).expect("P#p verifies");
        assert!(f.diags("section `p` is stale").is_empty());
        f.write("M.md", &MAP.replace("| 1 | `b` |", "| 2 | `b` |"));
        assert!(f.diags("section `p` is stale").is_empty());
        f.write("M.md", &MAP.replace("| 0 | `a` |", "| 0 | `a` | new |"));
        assert_eq!(f.diags("section `p` is stale").len(), 1);
        f.write("M.md", &format!("{MAP}| 3 | `a` |\n"));
        assert_eq!(
            f.diags("`depends_on` row `M.md#map/a`"),
            vec!["P.md:5: `depends_on` row `M.md#map/a`: several table rows have a cell `` `a` ``"]
        );
    }

    /// ADR-0053 migration: `docs migrate` moves an inline stamp that still holds into the
    /// ledger (its by and note on `body`), drops one that doesn't without moving it, drops
    /// ledger stamps of sections that are gone, and a second run changes nothing.
    #[test]
    fn migrate_moves_holding_inline_stamps_only() {
        let sec = |id: &str, anchor: &str, text: &str| {
            format!(
                "<!-- @doc\nid = \"{id}\"\n-->\n<a id=\"{anchor}\"></a>\n## S\n<!-- @section\ncovers = [\"src/**\"]\nverified = {{ hash = \"H-{anchor}\", by = \"agent:orig-{anchor}\", note = \"note {anchor}\" }}\n-->\n{text}\n"
            )
        };
        let mut files = REGISTRIES.to_vec();
        let (a, s) = (sec("A", "a", "About src."), sec("S", "s", "Stale."));
        let orphan = "# x\n\n[\"GONE#x\".body]\nstamp = { hash = \"b3:1\" }\n";
        files.extend([
            ("src/x.rs", "x"),
            ("A.md", a.as_str()),
            ("S.md", s.as_str()),
            (ledger::PATH, orphan),
        ]);
        let f = Fixture::new("migrate", &files);
        // A's inline stamp holds (the pre-ADR-0053 hash of the tree); S's does not.
        let l = load(&f.0).expect("invariant: the fixture is readable");
        let by_path: BTreeMap<&str, &parse::Doc> =
            l.docs.iter().map(|d| (d.path.as_str(), d)).collect();
        let files = hash::Files::new(&f.0, &l.files);
        let src = vec!["src/**".to_owned()];
        let h = hash::legacy_section_hash(by_path["A.md"], "a", &src, &[], &files, &by_path)
            .expect("invariant: hashable");
        f.write("A.md", &a.replace("H-a", &h));
        assert_eq!(f.diags("inline `verified`").len(), 2);
        let got = migrate(&f.0).expect("migrate runs");
        assert_eq!(
            got,
            Migrate {
                moved: vec!["A#a".into()],
                stale: vec!["S#s".into()],
                pruned: vec!["GONE#x".into()],
            }
        );
        for path in ["A.md", "S.md"] {
            let text = std::fs::read_to_string(f.0.join(path)).expect("invariant: written");
            assert!(!text.contains("verified"), "{text}");
        }
        assert!(f.diags("inline `verified`").is_empty());
        assert!(f.diags("canonical").is_empty());
        let stale = f.diags("is stale");
        assert_eq!(stale.len(), 1, "{stale:?}");
        assert!(
            stale[0].starts_with("S.md") && stale[0].contains("no stamp"),
            "{stale:?}"
        );
        let led = ledger::Ledger::read(&f.0).expect("invariant: written");
        let body = &led.sections["A#a"][hash::BODY];
        assert_eq!(
            (body.by.as_deref(), body.note.as_deref()),
            (Some("agent:orig-a"), Some("note a"))
        );
        assert_eq!(led.sections["A#a"]["file:src/x.rs"].by, None);
        let again = migrate(&f.0).expect("migrate runs");
        assert_eq!(again, Migrate::default());
    }

    /// R-DOC-012: unmatched `covers` fail in active docs, warn in drafts, and are skipped
    /// in retired docs (review N1 on #12).
    #[test]
    fn unmatched_covers_by_status() {
        let mut files = REGISTRIES.to_vec();
        let (active, draft, retired) = (
            doc("A", "active", "a", "nope/**"),
            doc("D", "draft", "d", "nope/**"),
            doc("R", "retired", "r", "nope/**"),
        );
        files.extend([("A.md", &*active), ("D.md", &*draft), ("R.md", &*retired)]);
        let f = Fixture::new("globs", &files);
        let all = check_repo(&f.0).expect("invariant: the fixture is readable");
        let unmatched: Vec<(&str, bool)> = all
            .iter()
            .filter(|d| d.msg.contains("matches no tracked file"))
            .map(|d| (d.path.as_str(), d.warning))
            .collect();
        assert_eq!(unmatched, vec![("A.md", false), ("D.md", true)]);
    }

    /// R-DOC-008: crate roots list exactly the sections covering the crate; whole-doc
    /// references (ADRs) are allowed anywhere; every reference must resolve; retired docs
    /// create no obligations.
    #[test]
    fn back_references_both_directions() {
        let mut files = REGISTRIES.to_vec();
        let (a, b, r) = (
            doc("A", "active", "a", "crates/x/**"),
            doc("B", "active", "b", "crates/y/**"),
            doc("R", "retired", "r", "crates/x/**"),
        );
        let adr = "<!-- @doc\nid = \"ADR-0001\"\nkind = \"adr\"\nstatus = \"accepted\"\n-->\n";
        let old = "<!-- @doc\nid = \"ADR-0002\"\nkind = \"adr\"\nstatus = \"superseded\"\n-->\n";
        files.extend([
            ("A.md", &*a),
            ("B.md", &*b),
            ("R.md", &*r),
            ("adr/0001.md", adr),
            ("adr/0002.md", old),
            ("crates/x/Cargo.toml", ""),
            (
                "crates/x/src/lib.rs",
                "//! X.\n//!\n//! Docs: A.md#a, adr/0001.md\n",
            ),
            (
                "crates/x/src/m.rs",
                "//! Docs: A.md#a, A.md#gone, adr/0002.md, R.md#r\n",
            ),
            ("crates/y/Cargo.toml", ""),
            ("crates/y/src/main.rs", "//! Docs: A.md#a\n"),
            ("crates/z/Cargo.toml", ""),
            ("crates/z/src/lib.rs", "//! Uncovered, so no line needed.\n"),
        ]);
        let f = Fixture::new("backrefs", &files);
        let got = f.diags("`Docs:`");
        assert_eq!(
            got,
            vec![
                "crates/x/src/m.rs:1: `Docs:` reference `A.md#gone`: no anchor `gone` in A.md",
                "crates/x/src/m.rs:1: `Docs:` reference `R.md#r` cites a retired doc",
                "crates/x/src/m.rs:1: `Docs:` reference `adr/0002.md` cites a superseded doc",
                "crates/y/src/main.rs:1: `Docs:` is missing `B.md#b`, whose `covers` match this crate",
                "crates/y/src/main.rs:1: `Docs:` lists `A.md#a`, which does not cover this crate",
            ],
            "{got:#?}"
        );
        f.write("crates/y/src/main.rs", "fn main() {}\n");
        assert_eq!(
            f.diags("no `//! Docs:` line"),
            vec![
                "crates/y/src/main.rs:1: crate root has no `//! Docs:` line; sections covering this crate: B.md#b"
            ]
        );
    }

    /// `docs for`: sections by `covers`, live rules by `applies_to`, and live ADRs cited
    /// by matched sections and rules or by the file's and its crate root's `Docs:` lines.
    #[test]
    fn docs_for_collects_sections_rules_and_adrs() {
        let adr = |id: &str, status: &str| {
            format!("<!-- @doc\nid = \"{id}\"\nkind = \"adr\"\nstatus = \"{status}\"\n-->\n")
        };
        let arch = "<!-- @doc\nid = \"A\"\n-->\n<a id=\"x\"></a>\n## X\n<!-- @section\ncovers = [\"crates/x/**\"]\ndepends_on = [\"adr/1.md\"]\n-->\nSee [ADR 4](adr/4.md).\n\n<a id=\"y\"></a>\n## Y\n<!-- @section\ncovers = [\"crates/y/**\"]\n-->\nSee [ADR 5](adr/5.md).\n";
        let rules = "<!-- @doc\nid = \"R\"\n-->\n<a id=\"R-A-001\"></a>\n### R-A-001: Live\n<!-- @rule\nstatus = \"active\"\napplies_to = [\"crates/**\"]\n-->\nPer [ADR 2](adr/2.md) and [old](adr/3.md).\n\n<a id=\"R-A-002\"></a>\n### R-A-002: Gone\n<!-- @rule\nstatus = \"retired\"\napplies_to = [\"crates/**\"]\n-->\nRetired.\n";
        let (a1, a2, a3, a4, a5, a6) = (
            adr("ADR-1", "accepted"),
            adr("ADR-2", "accepted"),
            adr("ADR-3", "superseded"),
            adr("ADR-4", "accepted"),
            adr("ADR-5", "accepted"),
            adr("ADR-6", "accepted"),
        );
        let f = Fixture::new(
            "for",
            &[
                ("A.md", arch),
                ("R.md", rules),
                ("adr/1.md", &a1),
                ("adr/2.md", &a2),
                ("adr/3.md", &a3),
                ("adr/4.md", &a4),
                ("adr/5.md", &a5),
                ("adr/6.md", &a6),
                ("crates/x/Cargo.toml", ""),
                ("crates/x/src/lib.rs", "//! Docs: A.md#x, adr/6.md\n"),
            ],
        );
        let l = load(&f.0).expect("invariant: the fixture is readable");
        let g = govern::governing(&f.0, &l.docs, &l.files, "crates/x/src/new.rs");
        let set = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(g.sections, set(&["A.md#x"]));
        assert_eq!(g.rules, set(&["R.md#R-A-001"]));
        assert_eq!(
            g.adrs,
            set(&["adr/1.md", "adr/2.md", "adr/4.md", "adr/6.md"])
        );
        let text = govern::render("crates/x/src/new.rs", &g, &l.docs);
        assert!(text.contains("R.md#R-A-001  (R-A-001: Live)"), "{text}");
        assert_eq!(
            repo_path(&f.0, "./crates/x/../x/a.rs").ok().as_deref(),
            Some("crates/x/a.rs")
        );
        assert!(repo_path(&f.0, "../outside").is_err());
        // Absolute paths, including ones that don't exist yet, resolve inside the repo.
        let abs = f.0.join("crates/x/src/not-yet/new.rs");
        assert_eq!(
            repo_path(&f.0, &abs.to_string_lossy()).ok().as_deref(),
            Some("crates/x/src/not-yet/new.rs")
        );
        // `..` is resolved before the walk, so escaping through a missing directory is
        // reported as outside the repository (review N1 on #15).
        let escape = f.0.join("nope/../../etc/passwd");
        let err = repo_path(&f.0, &escape.to_string_lossy()).expect_err("outside the repo");
        assert!(
            format!("{err:#}").contains("is outside the repository"),
            "{err:#}"
        );
        let inside = f.0.join("nope/../crates/x/a.rs");
        assert_eq!(
            repo_path(&f.0, &inside.to_string_lossy()).ok().as_deref(),
            Some("crates/x/a.rs")
        );
    }

    /// DOCS#gate reports, without failing: crates no section covers, and `applies_to`
    /// globs naming a crate that neither exists nor is planned in ARCHITECTURE#crate-map,
    /// keyed by directory: a `crates/` name under `apps/` warns, and the reverse (review N2
    /// on #15).
    #[test]
    fn uncovered_crates_and_applies_to_typos_warn() {
        let arch = "<!-- @doc\nid = \"ARCH\"\n-->\n<a id=\"crate-map\"></a>\n## Crate map\n<!-- @section\n-->\n| `jstream-planned` | later |\n| 6 | `apps/*` | `jstream-app` (links `jstream-planned`) |\n";
        let rules = "<!-- @doc\nid = \"R\"\n-->\n<a id=\"R-A-001\"></a>\n### R-A-001: Globs\n<!-- @rule\nstatus = \"active\"\napplies_to = [\"crates/jstream-x/**\", \"crates/jstream-planned/**\", \"crates/jstrem-typo/**\", \"crates/*/src/**\", \"apps/jstream-app/**\", \"apps/jstream-x/**\", \"crates/jstream-app/**\"]\n-->\nText.\n";
        let f = Fixture::new(
            "warnings",
            &[
                ("docs/external.toml", ""),
                ("docs/facts.toml", ""),
                ("docs/RULES.md", rules),
                ("docs/ARCHITECTURE.md", arch),
                ("crates/jstream-x/Cargo.toml", ""),
                ("crates/jstream-x/src/lib.rs", "//! X.\n"),
            ],
        );
        let all = check_repo(&f.0).expect("invariant: the fixture is readable");
        let warnings: Vec<String> = all
            .iter()
            .filter(|d| d.warning)
            .map(|d| format!("{}: {}", d.path, d.msg))
            .collect();
        assert_eq!(
            warnings,
            vec![
                "crates/jstream-x/src/lib.rs: crate `crates/jstream-x` is not covered by any section",
                "docs/RULES.md: rule `R-A-001`: `applies_to` names crate `apps/jstream-x`, which neither exists nor appears in ARCHITECTURE#crate-map (typo?)",
                "docs/RULES.md: rule `R-A-001`: `applies_to` names crate `crates/jstream-app`, which neither exists nor appears in ARCHITECTURE#crate-map (typo?)",
                "docs/RULES.md: rule `R-A-001`: `applies_to` names crate `crates/jstrem-typo`, which neither exists nor appears in ARCHITECTURE#crate-map (typo?)",
            ]
        );
    }

    /// The repository's own docs must pass. This is what the `docs` CI job enforces.
    #[test]
    fn repository_docs_pass() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let diags = check_repo(&root).expect("invariant: the repo is readable");
        let report: Vec<String> = diags
            .iter()
            .filter(|d| !d.warning)
            .map(|d| format!("{}:{}: {}", d.path, d.line, d.msg))
            .collect();
        assert!(
            report.is_empty(),
            "docs check failed:\n{}",
            report.join("\n")
        );
    }
}
