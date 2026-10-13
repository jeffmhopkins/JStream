//! Code back-references (docs/DOCS.md#back-references, R-DOC-008). Every crate root
//! (`src/lib.rs`, else `src/main.rs`) carries a `//! Docs:` line listing exactly the sections
//! whose `covers` match files in that crate. Module-level `Docs:` lines are optional; when
//! present, each reference must resolve to an existing doc and anchor.

use std::collections::{BTreeMap, BTreeSet};

use super::check::Repo;
use super::parse::{Diag, Doc};

fn diag(path: &str, line: usize, msg: String) -> Diag {
    Diag {
        path: path.to_owned(),
        line,
        msg,
        warning: false,
    }
}

/// The references on the `Docs:` line of a file's leading `//!` comment, with its line
/// number. Only the leading comment counts, so a `Docs:` in later code is ignored.
fn docs_line(text: &str) -> Option<(usize, Vec<String>)> {
    for (i, l) in text.lines().enumerate() {
        let t = l.trim_start();
        let Some(rest) = t.strip_prefix("//!") else {
            if t.is_empty() {
                continue;
            }
            break;
        };
        if let Some(refs) = rest.trim_start().strip_prefix("Docs:") {
            let refs = refs
                .split(',')
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(str::to_owned)
                .collect();
            return Some((i + 1, refs));
        }
    }
    None
}

/// Every crate in the repository: `(directory, root file)`, from tracked `Cargo.toml`s
/// below the workspace root.
fn crates(paths: &[String]) -> Vec<(&str, &str)> {
    let tracked: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    paths
        .iter()
        .filter_map(|p| p.strip_suffix("/Cargo.toml"))
        .filter_map(|dir| {
            ["lib.rs", "main.rs"].iter().find_map(|f| {
                let root = format!("{dir}/src/{f}");
                tracked.get(root.as_str()).map(|r| (dir, *r))
            })
        })
        .collect()
}

/// Runs the back-reference checks.
pub(crate) fn check(repo: &Repo<'_>) -> Vec<Diag> {
    let mut out = Vec::new();
    let paths = repo.files.paths();
    let by_path: BTreeMap<&str, &Doc> = repo.docs.iter().map(|d| (d.path.as_str(), d)).collect();

    // Files matched by each section's `covers`, keyed by `path#anchor`. Retired docs
    // describe nothing current, so they create no obligations (R-DOC-012).
    let mut covered: Vec<(String, Vec<&str>)> = Vec::new();
    for doc in repo
        .docs
        .iter()
        .filter(|d| d.meta_str("status") != Some("retired"))
    {
        for s in &doc.sections {
            let globs: Vec<String> = s
                .meta
                .as_ref()
                .and_then(|m| m.get("covers"))
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if globs.is_empty() {
                continue;
            }
            // Invalid globs are reported by the section checks.
            if let Ok(m) = repo.files.matching(&globs) {
                covered.push((format!("{}#{}", doc.path, s.anchor), m));
            }
        }
    }

    let roots: BTreeSet<&str> = crates(paths).iter().map(|(_, r)| *r).collect();
    for (dir, root) in crates(paths) {
        let prefix = format!("{dir}/");
        let expected: BTreeSet<&str> = covered
            .iter()
            .filter(|(_, files)| files.iter().any(|f| f.starts_with(&prefix)))
            .map(|(r, _)| r.as_str())
            .collect();
        let text = match std::fs::read_to_string(repo.root.join(root)) {
            Ok(t) => t,
            Err(e) => {
                out.push(diag(root, 1, format!("reading crate root: {e}")));
                continue;
            }
        };
        let Some((line, listed)) = docs_line(&text) else {
            if !expected.is_empty() {
                out.push(diag(
                    root,
                    1,
                    format!(
                        "crate root has no `//! Docs:` line; sections covering this crate: {}",
                        expected.iter().copied().collect::<Vec<_>>().join(", ")
                    ),
                ));
            }
            continue;
        };
        let listed: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
        for missing in expected.difference(&listed) {
            out.push(diag(
                root,
                line,
                format!("`Docs:` is missing `{missing}`, whose `covers` match this crate"),
            ));
        }
        for extra in listed.difference(&expected) {
            out.push(diag(
                root,
                line,
                format!("`Docs:` lists `{extra}`, which does not cover this crate"),
            ));
        }
    }

    // Module-level lines: optional, but every reference must resolve.
    for p in paths
        .iter()
        .filter(|p| p.ends_with(".rs") && !roots.contains(p.as_str()))
    {
        let Ok(text) = std::fs::read_to_string(repo.root.join(p)) else {
            continue;
        };
        let Some((line, refs)) = docs_line(&text) else {
            continue;
        };
        for r in refs {
            let resolves = r.split_once('#').is_some_and(|(file, anchor)| {
                by_path.get(file).is_some_and(|d| d.has_anchor(anchor))
            });
            if !resolves {
                out.push(diag(
                    p,
                    line,
                    format!("`Docs:` reference `{r}` is not a tracked doc section"),
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docs_line_reads_only_the_leading_comment() {
        let t = "//! Crate.\n//!\n//! Docs: a.md#x, b.md#y\n\nfn f() {}\n";
        assert_eq!(
            docs_line(t),
            Some((3, vec!["a.md#x".to_owned(), "b.md#y".to_owned()]))
        );
        assert_eq!(docs_line("//! Crate.\nfn f() {}\n//! Docs: a.md#x\n"), None);
    }

    #[test]
    fn crate_roots_prefer_lib() {
        let paths: Vec<String> = [
            "Cargo.toml",
            "crates/a/Cargo.toml",
            "crates/a/src/lib.rs",
            "crates/a/src/main.rs",
            "xtask/Cargo.toml",
            "xtask/src/main.rs",
            "crates/b/Cargo.toml",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            crates(&paths),
            vec![
                ("crates/a", "crates/a/src/lib.rs"),
                ("xtask", "xtask/src/main.rs")
            ]
        );
    }
}
