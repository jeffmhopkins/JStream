//! Code back-references (docs/DOCS.md#back-references, R-DOC-008). Every crate root
//! (`src/lib.rs`, else `src/main.rs`) carries a `//! Docs:` line listing exactly the sections
//! whose `covers` match files in that crate. Module-level `Docs:` lines are optional. Every
//! reference, on any line, must resolve (docs/DOCS.md#references): `path#anchor` to a
//! section, or a bare `path` to a whole doc such as an ADR.

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
/// number. Only the leading comment counts, so a `Docs:` in later code is ignored. Blank
/// lines and single-line inner attributes (`#![…]`) may precede or interleave it.
pub(crate) fn docs_line(text: &str) -> Option<(usize, Vec<String>)> {
    for (i, l) in text.lines().enumerate() {
        let t = l.trim_start();
        let Some(rest) = t.strip_prefix("//!") else {
            if t.is_empty() || (t.starts_with("#![") && t.ends_with(']')) {
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
pub(crate) fn crates(paths: &[String]) -> Vec<(&str, &str)> {
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

/// The crate that owns `file`: the deepest crate directory containing it, so files of a
/// nested crate don't count toward the outer one.
pub(crate) fn owner<'c>(file: &str, dirs: &[&'c str]) -> Option<&'c str> {
    dirs.iter()
        .copied()
        .filter(|d| file.starts_with(&format!("{d}/")))
        .max_by_key(|d| d.len())
}

/// Checks one reference: `path#anchor` must name a section of a tracked doc, a bare `path`
/// a tracked doc. Code may not cite a superseded ADR (docs/DOCS.md#references).
fn resolve(r: &str, by_path: &BTreeMap<&str, &Doc>) -> Result<(), String> {
    let (file, anchor) = r.split_once('#').map_or((r, None), |(f, a)| (f, Some(a)));
    let Some(doc) = by_path.get(file) else {
        return Err(format!("`Docs:` reference `{r}` is not a tracked doc"));
    };
    if let Some(a) = anchor
        && !doc.has_anchor(a)
    {
        return Err(format!(
            "`Docs:` reference `{r}`: no anchor `{a}` in {file}"
        ));
    }
    match doc.meta_str("status") {
        Some("superseded") => Err(format!("`Docs:` reference `{r}` cites a superseded doc")),
        // Retired docs create no obligations, so citing them is always stale (#13 N5).
        Some("retired") => Err(format!("`Docs:` reference `{r}` cites a retired doc")),
        _ => Ok(()),
    }
}

/// Crate directories known to the repository, as `crates/<name>` or `apps/<name>`: every
/// existing crate directory, plus every `jstream-…` name in ARCHITECTURE#crate-map
/// (planned crates). Names on the crate map's `apps/*` row are apps; all others are under
/// `crates/`. Used to flag likely typos in `applies_to` globs, which may otherwise name
/// crates that don't exist yet; keying by directory means `apps/jstream-types/**` is
/// flagged too (review N2 on #15).
pub(crate) fn known_crates(docs: &[Doc], paths: &[String]) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = crates(paths).iter().map(|(d, _)| (*d).to_owned()).collect();
    if let Some(doc) = docs.iter().find(|d| d.path == "docs/ARCHITECTURE.md")
        && let Some(body) = super::hash::body(doc, Some("crate-map"))
    {
        for line in body.lines() {
            let dir = if line.contains("`apps/*`") {
                "apps"
            } else {
                "crates"
            };
            for tok in line.split('`').skip(1).step_by(2) {
                if tok.starts_with("jstream-")
                    && tok
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                {
                    names.insert(format!("{dir}/{tok}"));
                }
            }
        }
    }
    names
}

/// The literal crate directory a glob targets (`crates/<name>` or `apps/<name>`), if the
/// name has no glob syntax.
pub(crate) fn glob_crate(glob: &str) -> Option<&str> {
    let prefix = ["crates/", "apps/"]
        .into_iter()
        .find(|p| glob.starts_with(p))?;
    let name = glob[prefix.len()..].split('/').next()?;
    (!name.is_empty() && !name.contains(['*', '?', '[', '{']))
        .then(|| &glob[..prefix.len() + name.len()])
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

    let crates = crates(paths);
    let dirs: Vec<&str> = crates.iter().map(|(d, _)| *d).collect();
    let roots: BTreeSet<&str> = crates.iter().map(|(_, r)| *r).collect();
    for &(dir, root) in &crates {
        let expected: BTreeSet<&str> = covered
            .iter()
            .filter(|(_, files)| files.iter().any(|f| owner(f, &dirs) == Some(dir)))
            .map(|(r, _)| r.as_str())
            .collect();
        let text = match std::fs::read_to_string(repo.root.join(root)) {
            Ok(t) => t,
            Err(e) => {
                out.push(diag(root, 1, format!("reading crate root: {e}")));
                continue;
            }
        };
        // DOCS#gate: report, without failing, crates that no section covers.
        if expected.is_empty() {
            out.push(Diag {
                warning: true,
                ..diag(
                    root,
                    1,
                    format!("crate `{dir}` is not covered by any section"),
                )
            });
        }
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
        for r in &listed {
            if let Err(msg) = resolve(r, &by_path) {
                out.push(diag(root, line, msg));
            }
        }
        // Whole-doc references (ADRs) add context; only sections take part in the
        // two-way check.
        let listed: BTreeSet<&str> = listed
            .iter()
            .map(String::as_str)
            .filter(|r| r.contains('#'))
            .collect();
        for missing in expected.difference(&listed) {
            out.push(diag(
                root,
                line,
                format!("`Docs:` is missing `{missing}`, whose `covers` match this crate"),
            ));
        }
        for extra in listed
            .difference(&expected)
            .filter(|r| resolve(r, &by_path).is_ok())
        {
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
            if let Err(msg) = resolve(&r, &by_path) {
                out.push(diag(p, line, msg));
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
        assert_eq!(
            docs_line("#![forbid(unsafe_code)]\n//! Docs: a.md\n"),
            Some((2, vec!["a.md".to_owned()]))
        );
    }

    #[test]
    fn nested_crates_own_their_files() {
        let dirs = ["crates/a", "crates/a/inner"];
        assert_eq!(owner("crates/a/src/x.rs", &dirs), Some("crates/a"));
        assert_eq!(
            owner("crates/a/inner/src/y.rs", &dirs),
            Some("crates/a/inner")
        );
        assert_eq!(owner("crates/ab/src/z.rs", &dirs), None);
    }

    #[test]
    fn glob_crate_names_literal_targets_only() {
        assert_eq!(
            glob_crate("crates/jstream-core/**"),
            Some("crates/jstream-core")
        );
        assert_eq!(
            glob_crate("apps/jstream-cli/src/main.rs"),
            Some("apps/jstream-cli")
        );
        assert_eq!(glob_crate("crates/jstream-platform-*/src/**"), None);
        assert_eq!(glob_crate("crates/**"), None);
        assert_eq!(glob_crate("docs/**"), None);
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
