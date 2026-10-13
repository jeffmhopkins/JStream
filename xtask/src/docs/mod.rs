//! `cargo xtask docs`: checks and maintains the documentation graph (docs/DOCS.md).
//!
//! Docs: docs/DOCS.md#bootstrap, docs/DOCS.md#gate, docs/DOCS.md#verification

mod check;
mod hash;
mod parse;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

pub(crate) use parse::Diag;

/// Files that look like docs but are not tracked (docs/DOCS.md#identity).
fn is_tracked(path: &str) -> bool {
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
    let out = Command::new("git")
        .args(["ls-files", "-z"])
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

fn read_toml(root: &Path, p: &str) -> Result<toml::Table> {
    let text = std::fs::read_to_string(root.join(p)).with_context(|| format!("reading {p}"))?;
    text.parse().with_context(|| format!("parsing {p}"))
}

/// Runs every check over the repository at `root` and returns the diagnostics.
pub(crate) fn check_repo(root: &Path) -> Result<Vec<Diag>> {
    let l = load(root)?;
    let repo = check::Repo {
        root,
        docs: &l.docs,
        external: read_toml(root, "docs/external.toml")?,
        facts: read_toml(root, "docs/facts.toml")?,
        rules_md: std::fs::read_to_string(root.join("docs/RULES.md"))
            .context("reading docs/RULES.md")?,
        files: hash::Files::new(root, &l.files),
    };
    let mut diags = l.parse_diags;
    diags.extend(check::check(&repo));
    diags.sort();
    diags.dedup();
    Ok(diags)
}

/// `docs verify <DOC-ID#anchor | path#anchor> --by <who> --note <text>`: records that a
/// section was re-read and still holds, by writing its current hash into `verified`.
fn verify(root: &Path, target: &str, by: &str, note: &str) -> Result<()> {
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
    let (Some(meta), Some((start, end))) = (&section.meta, section.block) else {
        bail!("section `{anchor}` has no `@section` block");
    };
    let strings = |k: &str| -> Vec<String> {
        meta.get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let files = hash::Files::new(root, &l.files);
    let h = hash::section_hash(
        doc,
        anchor,
        &strings("covers"),
        &strings("depends_on"),
        &files,
        &by_path,
    )?;
    let line = format!(
        "verified = {{ hash = {}, by = {}, note = {} }}",
        hash::toml_str(&h),
        hash::toml_str(by),
        hash::toml_str(note)
    );

    let mut lines: Vec<String> = doc.text.lines().map(str::to_owned).collect();
    match (start + 1..end).find(|&k| lines[k].trim_start().starts_with("verified")) {
        Some(k) => lines[k] = line,
        None => lines.insert(end, line),
    }
    let mut out = lines.join("\n");
    if doc.text.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(root.join(&doc.path), out).with_context(|| format!("writing {}", doc.path))?;
    eprintln!(
        "verified {}#{anchor}: {h}",
        doc.meta_str("id").unwrap_or(&doc.path)
    );
    Ok(())
}

/// Entry point for `cargo xtask docs <args>`.
pub(crate) fn run(args: &[String], root: &Path) -> Result<bool> {
    match args.first().map(String::as_str) {
        Some("check") => {
            let diags = check_repo(root)?;
            for d in &diags {
                eprintln!("{}:{}: {}", d.path, d.line, d.msg);
            }
            if diags.is_empty() {
                eprintln!("docs check: ok");
            } else {
                eprintln!("docs check: {} problem(s)", diags.len());
            }
            Ok(diags.is_empty())
        }
        Some("verify") => {
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
                    "usage: cargo xtask docs verify <DOC#anchor> --by <human:..|agent:..> --note \"<what you checked>\""
                );
            };
            verify(root, target, by, note)?;
            Ok(true)
        }
        _ => bail!("usage: cargo xtask docs check | verify <DOC#anchor> --by <who> --note <text>"),
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

    /// The repository's own docs must pass. This is what the `docs` CI job enforces.
    #[test]
    fn repository_docs_pass() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let diags = check_repo(&root).expect("invariant: the repo is readable");
        let report: Vec<String> = diags
            .iter()
            .map(|d| format!("{}:{}: {}", d.path, d.line, d.msg))
            .collect();
        assert!(
            diags.is_empty(),
            "docs check failed:\n{}",
            report.join("\n")
        );
    }
}
