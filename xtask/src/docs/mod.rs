//! `cargo xtask docs`: checks and maintains the documentation graph (docs/DOCS.md).
//!
//! Docs: docs/DOCS.md#bootstrap, docs/DOCS.md#gate, docs/DOCS.md#verification, docs/DOCS.md#back-references, docs/DOCS.md#generated

mod backrefs;
mod check;
pub(crate) mod generate;
mod govern;
mod hash;
pub(crate) mod parse;

use std::collections::BTreeMap;
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
    diags.extend(generate::check(&l.docs));
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

/// What `docs rehash` did.
#[derive(Debug, Default, PartialEq, Eq)]
struct Rehash {
    /// Sections whose hash was rewritten, as `DOC#anchor`.
    restamped: Vec<String>,
    /// Sections already fresh under the current rule.
    fresh: usize,
    /// Sections left stale, as `DOC#anchor`: fresh under neither rule.
    stale: Vec<String>,
}

/// `docs rehash`: the one-time migration to ADR-0039's hashing rule. A section's stored
/// hash is replaced by its current-rule hash only when it is what the old rule computes for
/// this tree (metadata hashed, row references widened to their section), or what the
/// current rule computes with row references widened (a section verified against a whole
/// table, then narrowed to its row). Only the `hash` value changes; `by` and `note` stay.
/// Every decision is made against the unmodified tree before anything is written, so
/// sections cannot affect each other. A stale section is never written.
fn rehash(root: &Path) -> Result<Rehash> {
    let l = load(root)?;
    let by_path: BTreeMap<&str, &parse::Doc> =
        l.docs.iter().map(|d| (d.path.as_str(), d)).collect();
    let files = hash::Files::new(root, &l.files);
    let untracked = git_ls(root, &["--others", "--exclude-standard"])?;
    let untracked = hash::Files::new(root, &untracked);
    let legacy = hash::Mode {
        strip_metadata: false,
        widen_rows: true,
    };
    let narrowed = hash::Mode {
        strip_metadata: true,
        widen_rows: true,
    };
    let mut report = Rehash::default();
    // Per doc: (line index, stored hash, new hash).
    let mut edits: BTreeMap<&str, Vec<(usize, String, String)>> = BTreeMap::new();
    for doc in &l.docs {
        for s in &doc.sections {
            let (Some(meta), Some((start, end))) = (&s.meta, s.block) else {
                continue;
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
            let Some(stored) = meta
                .get("verified")
                .and_then(|v| v.get("hash"))
                .and_then(|h| h.as_str())
            else {
                continue;
            };
            let (covers, deps) = (strings("covers"), strings("depends_on"));
            let name = format!("{}#{}", doc.meta_str("id").unwrap_or(&doc.path), s.anchor);
            let hash_by = |mode| {
                hash::section_hash_by(mode, doc, &s.anchor, &covers, &deps, &files, &by_path)
            };
            let now = hash_by(hash::Mode::CURRENT)?;
            if stored == now {
                report.fresh += 1;
                continue;
            }
            let was_fresh = stored == hash_by(legacy)? || stored == hash_by(narrowed)?;
            // As in `verify`: CI hashes only the committed tree (review B1 on #10).
            let line = (start + 1..end).find(|&k| {
                let l = doc.text.lines().nth(k).unwrap_or_default();
                l.trim_start().starts_with("verified") && l.matches(stored).count() == 1
            });
            match line {
                Some(k) if was_fresh && untracked.matching(&covers)?.is_empty() => {
                    edits
                        .entry(doc.path.as_str())
                        .or_default()
                        .push((k, stored.to_owned(), now));
                    report.restamped.push(name);
                }
                _ => report.stale.push(name),
            }
        }
    }
    for (path, list) in edits {
        let doc = by_path[path];
        let mut lines: Vec<String> = doc.text.lines().map(str::to_owned).collect();
        for (k, old, new) in list {
            lines[k] = lines[k].replacen(&old, &new, 1);
        }
        let mut out = lines.join("\n");
        if doc.text.ends_with('\n') {
            out.push('\n');
        }
        std::fs::write(root.join(path), out).with_context(|| format!("writing {path}"))?;
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
            let l = load(root)?;
            let g = generate::generate(&l.docs);
            for d in &g.problems {
                eprintln!("{}:{}: {}", d.path, d.line, d.msg);
            }
            for (path, _, text) in &g.files {
                std::fs::write(root.join(path), text).with_context(|| format!("writing {path}"))?;
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
        Some("rehash") => {
            let [_] = args else {
                bail!("usage: cargo xtask docs rehash");
            };
            let r = rehash(root)?;
            for s in &r.restamped {
                eprintln!("restamped {s}");
            }
            for s in &r.stale {
                eprintln!("stale, left for a re-read: {s}");
            }
            eprintln!(
                "docs rehash: {} restamped, {} already fresh, {} stale",
                r.restamped.len(),
                r.fresh,
                r.stale.len()
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
        _ => bail!(
            "usage: cargo xtask docs check | generate | verify <DOC#anchor> --by <who> --note <text> | for <path> | rehash"
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

    const NOTE: &str = "checked the section text against its inputs in this test";

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

    /// ADR-0039 migration: `docs rehash` restamps only sections fresh under the old rule
    /// (or fresh before being narrowed to a row), keeps `by` and `note`, and never writes a
    /// stale section.
    #[test]
    fn rehash_restamps_fresh_sections_only() {
        let sec = |id: &str, anchor: &str, meta: &str, text: &str| {
            format!(
                "<!-- @doc\nid = \"{id}\"\n-->\n<a id=\"{anchor}\"></a>\n## S\n<!-- @section\n{meta}\nverified = {{ hash = \"H-{anchor}\", by = \"agent:orig-{anchor}\", note = \"note {anchor}\" }}\n-->\n{text}\n"
            )
        };
        let mut files = REGISTRIES.to_vec();
        let a = sec("A", "a", "covers = [\"src/**\"]", "About src.");
        let b = sec("B", "b", "covers = [\"A.md\"]", "About A.");
        let s = sec("S", "s", "covers = [\"src/**\"]", "Stale.");
        let r = sec("R", "r", "depends_on = [\"M.md#map/a\"]", "Row.");
        files.extend([
            ("src/x.rs", "x"),
            ("M.md", MAP),
            ("A.md", a.as_str()),
            ("B.md", b.as_str()),
            ("S.md", s.as_str()),
            ("R.md", r.as_str()),
        ]);
        let f = Fixture::new("rehash", &files);
        // Stamp each section with its old-rule hash, in dependency order (B covers A.md).
        let legacy = hash::Mode {
            strip_metadata: false,
            widen_rows: true,
        };
        for (path, anchor) in [("A.md", "a"), ("B.md", "b"), ("S.md", "s"), ("R.md", "r")] {
            let l = load(&f.0).expect("invariant: the fixture is readable");
            let by_path: BTreeMap<&str, &parse::Doc> =
                l.docs.iter().map(|d| (d.path.as_str(), d)).collect();
            let d = by_path[path];
            let meta = d.sections[0]
                .meta
                .as_ref()
                .expect("invariant: has metadata");
            let get = |k: &str| -> Vec<String> {
                meta.get(k)
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let files = hash::Files::new(&f.0, &l.files);
            let h = hash::section_hash_by(
                legacy,
                d,
                anchor,
                &get("covers"),
                &get("depends_on"),
                &files,
                &by_path,
            )
            .expect("invariant: hashable");
            f.write(path, &d.text.replace(&format!("H-{anchor}"), &h));
        }
        // S goes stale under the old rule: its covered code changes after the stamp.
        f.write("src/x.rs", "x2");
        let before = std::fs::read_to_string(f.0.join("S.md")).expect("invariant: written");
        let (a_before, b_before) = (
            std::fs::read_to_string(f.0.join("A.md")).expect("invariant: written"),
            std::fs::read_to_string(f.0.join("B.md")).expect("invariant: written"),
        );
        let got = rehash(&f.0).expect("rehash runs");
        assert_eq!(got.restamped, vec!["B#b", "R#r"], "{got:?}");
        assert_eq!(got.stale, vec!["A#a", "S#s"], "{got:?}");
        // Stale sections are untouched; restamped ones keep `by` and `note`.
        assert_eq!(
            std::fs::read_to_string(f.0.join("S.md")).expect("invariant: written"),
            before
        );
        assert_eq!(
            std::fs::read_to_string(f.0.join("A.md")).expect("invariant: written"),
            a_before
        );
        let b_after = std::fs::read_to_string(f.0.join("B.md")).expect("invariant: written");
        assert_ne!(b_after, b_before);
        assert!(b_after.contains("by = \"agent:orig-b\", note = \"note b\""));
        let stale: Vec<String> = f.diags("is stale");
        assert_eq!(stale.len(), 2, "{stale:?}");
        assert!(
            stale
                .iter()
                .all(|d| d.starts_with("A.md") || d.starts_with("S.md"))
        );
        // A second run changes nothing: B and R are now fresh, A and S still refused.
        let again = rehash(&f.0).expect("rehash runs");
        assert_eq!((again.restamped.len(), again.fresh), (0, 2), "{again:?}");
        assert_eq!(again.stale, vec!["A#a", "S#s"]);
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
