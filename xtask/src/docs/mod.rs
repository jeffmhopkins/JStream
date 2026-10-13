//! `cargo xtask docs`: checks the documentation graph described in docs/DOCS.md.
//!
//! Docs: docs/DOCS.md#bootstrap, docs/DOCS.md#gate

mod check;
mod parse;

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

/// Runs every check over the repository at `root` and returns the diagnostics.
pub(crate) fn check_repo(root: &Path) -> Result<Vec<Diag>> {
    let out = Command::new("git")
        .args(["ls-files", "-z", "*.md"])
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
        .filter(|p| is_tracked(p))
        .map(str::to_owned)
        .collect();
    paths.sort();

    let mut diags = Vec::new();
    let mut docs = Vec::new();
    for p in &paths {
        let text = std::fs::read_to_string(root.join(p)).with_context(|| format!("reading {p}"))?;
        docs.push(parse::parse(p, &text, &mut diags));
    }
    let read_toml = |p: &str| -> Result<toml::Table> {
        let text = std::fs::read_to_string(root.join(p)).with_context(|| format!("reading {p}"))?;
        text.parse().with_context(|| format!("parsing {p}"))
    };
    let repo = check::Repo {
        root,
        docs: &docs,
        external: read_toml("docs/external.toml")?,
        facts: read_toml("docs/facts.toml")?,
        rules_md: std::fs::read_to_string(root.join("docs/RULES.md"))
            .context("reading docs/RULES.md")?,
    };
    diags.extend(check::check(&repo));
    diags.sort();
    diags.dedup();
    Ok(diags)
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
        _ => bail!("usage: cargo xtask docs check"),
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
