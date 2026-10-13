//! `cargo xtask docs`: checks and maintains the documentation graph (docs/DOCS.md).
//!
//! Docs: docs/DOCS.md#bootstrap, docs/DOCS.md#gate, docs/DOCS.md#verification, docs/DOCS.md#back-references

mod backrefs;
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
    diags.extend(backrefs::check(&repo));
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
    // Hashes only cover tracked files, so an untracked covered file would make this hash
    // differ from the one CI computes after the commit (review B1 on #10).
    let untracked = git_ls(root, &["--others", "--exclude-standard"])?;
    let pending = hash::Files::new(root, &untracked).matching(&strings("covers"))?;
    if !pending.is_empty() {
        bail!(
            "untracked files match this section's `covers`; `git add` them first: {}",
            pending.join(", ")
        );
    }
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
        let written = std::fs::read_to_string(f.0.join("T.md")).expect("invariant: just written");
        assert!(written.contains("hash = \"b3:"), "{written}");
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
                "//! Docs: A.md#a, A.md#gone, adr/0002.md\n",
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
                "crates/x/src/m.rs:1: `Docs:` reference `adr/0002.md` cites a superseded ADR",
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
