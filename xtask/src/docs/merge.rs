//! `docs merge-main`: merges `origin/main` (or, with `--from`, the wave branch a PR targets;
//! ADR-0055) and resolves conflicts that are only between stamps, by taking that branch's
//! side (docs/DOCS.md#verification).
//!
//! Parallel PRs that re-verify the same input of a section conflict on its ledger entry
//! (ADR-0053). Neither side re-read the combination, so after the merge the section has to
//! be re-read anyway; which entry the merge keeps doesn't matter, only that the section is
//! reported as stale. A branch from before ADR-0053 also conflicts on the inline
//! `verified` lines main moved to the ledger; main's side (no line) is taken the same way,
//! and inline stamps the branch added are then moved by `docs migrate`.
//!
//! A conflict wholly inside `@generated` blocks, such as two PRs each regenerating the
//! README's progress table, is resolved by taking that branch's side and rewriting every
//! block from the merged sources, as `docs generate` would (ADR-0038). Every other conflict
//! is left untouched for a person.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

use super::{hash, is_tracked, ledger, load, parse};

/// The remote `merge-main` fetches from, and the branch it merges unless given `--from`.
const REMOTE: &str = "origin";
pub(crate) const BRANCH: &str = "main";

/// What `docs merge-main` did.
#[derive(Debug, Default)]
pub(crate) struct Merge {
    /// Sections whose conflicting stamp was resolved to main's, as `DOC#anchor`.
    pub(crate) stamps: Vec<String>,
    /// What `docs migrate` did after the merge, when the branch still had inline stamps.
    pub(crate) migrated: Option<super::Migrate>,
    /// Files whose conflict was only inside `@generated` blocks, rewritten from the merged
    /// sources.
    pub(crate) generated: Vec<String>,
    /// Conflicted files left for a person, with the reason.
    pub(crate) refused: Vec<(String, String)>,
    /// Whether the merge was committed.
    pub(crate) committed: bool,
    /// Sections stale after the merge, as `DOC#anchor`; empty unless `committed`.
    pub(crate) stale: Vec<String>,
    /// For each stale section, the notes this branch had written on it before the merge
    /// (on entries that differ from the merge base's), so the re-verify can carry them
    /// forward (docs/DOCS.md#notes).
    pub(crate) notes: BTreeMap<String, Vec<String>>,
}

fn git(root: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("running `git {}`", args.join(" ")))
}

/// Runs `git <args>` and fails with its stderr unless it succeeds.
fn git_ok(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = git(root, args)?;
    if !out.status.success() {
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Fetches and merges `origin/<branch>` into the current branch, resolves stamp-only
/// conflicts, and commits the merge when nothing else conflicts. `branch` is `main`, or a
/// `wave/*` branch for a PR into a wave.
pub(crate) fn merge_main(root: &Path, branch: &str) -> Result<Merge> {
    if branch != BRANCH && !branch.strip_prefix("wave/").is_some_and(|w| !w.is_empty()) {
        bail!("can only merge `{BRANCH}` or a `wave/<name>` branch, not `{branch}`");
    }
    if git(root, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])?
        .status
        .success()
    {
        bail!("a merge is already in progress; finish or abort it first");
    }
    if !git_ok(root, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        bail!("the working tree has uncommitted changes; commit or stash them first");
    }
    git_ok(root, &["fetch", "-q", REMOTE, branch])?;
    let upstream = format!("{REMOTE}/{branch}");
    // A fixed conflict style, whatever the user's config, so the markers parse.
    let out = git(
        root,
        &[
            "-c",
            "merge.conflictStyle=merge",
            "merge",
            "--no-edit",
            "-q",
            &upstream,
        ],
    )?;
    let mut report = Merge::default();
    if out.status.success() {
        report.committed = true;
        report.migrated = migrate_inline(root)?;
        report.stale = stale_sections(root)?;
        report.notes = branch_notes(root, &report.stale)?;
        return Ok(report);
    }
    if !git(root, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])?
        .status
        .success()
    {
        bail!(
            "`git merge {upstream}` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let unmerged = String::from_utf8(git_ok(
        root,
        &["diff", "--name-only", "-z", "--diff-filter=U"],
    )?)?;
    for path in unmerged.split('\0').filter(|p| !p.is_empty()) {
        // Both sides must have modified the file: delete/modify and similar conflicts
        // are not stamp conflicts.
        let stages = String::from_utf8(git_ok(root, &["ls-files", "-u", "-z", "--", path])?)?;
        let has = |n: &str| {
            stages
                .split('\0')
                .any(|e| e.split_once('\t').is_some_and(|(m, _)| m.ends_with(n)))
        };
        let resolved = if has(" 2") && has(" 3") {
            std::fs::read_to_string(root.join(path))
                .map_err(|e| format!("not readable as text: {e}"))
                .and_then(|text| {
                    if path == ledger::PATH {
                        resolve_ledger(&text)
                    } else {
                        resolve(path, &text).or_else(|why| {
                            let text = resolve_generated(path, &text).map_err(|_| why)?;
                            report.generated.push(path.to_owned());
                            Ok((text, Vec::new()))
                        })
                    }
                })
        } else {
            Err("not modified on both sides".to_owned())
        };
        match resolved {
            Ok((text, stamps)) => {
                std::fs::write(root.join(path), text).with_context(|| format!("writing {path}"))?;
                git_ok(root, &["add", "--", path])?;
                report.stamps.extend(stamps);
            }
            Err(why) => report.refused.push((path.to_owned(), why)),
        }
    }
    report.stamps.sort();
    report.stamps.dedup();
    if report.refused.is_empty() && !report.generated.is_empty() {
        let g = super::write_generated(root)?;
        for d in &g.problems {
            report
                .refused
                .push((d.path.clone(), format!("`docs generate`: {}", d.msg)));
        }
        for (path, _, _) in &g.files {
            git_ok(root, &["add", "--", path])?;
        }
    }
    if report.refused.is_empty() {
        git_ok(root, &["commit", "-q", "--no-edit"])?;
        report.committed = true;
        report.migrated = migrate_inline(root)?;
        report.stale = stale_sections(root)?;
        report.notes = branch_notes(root, &report.stale)?;
    }
    Ok(report)
}

/// The ledger as it was at `rev`; empty when the file is missing or doesn't parse there.
fn ledger_at(root: &Path, rev: &str) -> Result<ledger::Ledger> {
    let out = git(root, &["show", &format!("{rev}:{}", ledger::PATH)])?;
    if !out.status.success() {
        return Ok(ledger::Ledger::default());
    }
    Ok(ledger::Ledger::parse(&String::from_utf8_lossy(&out.stdout)).unwrap_or_default())
}

/// For each of `stale`, the distinct notes the branch wrote on it: notes on entries of the
/// merge's first parent that differ from the merge base's entry. Called after the merge
/// commit, so `HEAD^1` is the branch and `HEAD^2` what was merged. A merge-main that only
/// ran `docs migrate` on top leaves the merge at `HEAD~1`; both are tried.
fn branch_notes(root: &Path, stale: &[String]) -> Result<BTreeMap<String, Vec<String>>> {
    let mut out = BTreeMap::new();
    if stale.is_empty() {
        return Ok(out);
    }
    let merge = ["HEAD", "HEAD~1"].into_iter().find(|rev| {
        git(root, &["rev-parse", "-q", "--verify", &format!("{rev}^2")])
            .is_ok_and(|o| o.status.success())
    });
    let Some(merge) = merge else { return Ok(out) };
    let (ours, theirs) = (format!("{merge}^1"), format!("{merge}^2"));
    let base = git_ok(root, &["merge-base", &ours, &theirs])?;
    let base = String::from_utf8_lossy(&base).trim().to_owned();
    let (branch, base) = (ledger_at(root, &ours)?, ledger_at(root, &base)?);
    for key in stale {
        let Some(stamp) = branch.sections.get(key) else {
            continue;
        };
        let before = base.sections.get(key);
        let mut notes: Vec<String> = Vec::new();
        for (input, e) in stamp {
            if before.and_then(|b| b.get(input)) == Some(e) {
                continue;
            }
            if let Some(n) = &e.note
                && !notes.contains(n)
            {
                notes.push(n.clone());
            }
        }
        if !notes.is_empty() {
            out.insert(key.clone(), notes);
        }
    }
    Ok(out)
}

/// After a merge commit: runs `docs migrate` and commits what it changed, if anything, so
/// inline stamps a branch from before ADR-0053 added reach the ledger.
fn migrate_inline(root: &Path) -> Result<Option<super::Migrate>> {
    let m = super::migrate(root)?;
    git_ok(root, &["add", "-u"])?;
    if root.join(ledger::PATH).exists() {
        git_ok(root, &["add", "--", ledger::PATH])?;
    }
    if git(root, &["diff", "--cached", "--quiet"])?
        .status
        .success()
    {
        return Ok(None);
    }
    git_ok(
        root,
        &[
            "commit",
            "-q",
            "-m",
            "docs: move inline verified stamps to docs/verified.toml (ADR-0053)",
        ],
    )?;
    Ok(Some(m))
}

/// One conflict hunk: the 0-based line ranges its two sides occupy once resolved to that
/// side.
struct Hunk {
    ours: (usize, usize),
    theirs: (usize, usize),
}

/// The two sides of a conflicted file, each as if resolved wholly to that side, and the
/// line ranges the conflict hunks occupy in each; or why the markers can't be read.
fn sides(text: &str) -> Result<(String, String, Vec<Hunk>), String> {
    let (mut ours, mut theirs) = (Vec::new(), Vec::new());
    let mut hunks = Vec::new();
    // Which side the current line belongs to: 0 both, 1 ours, 2 base (dropped), 3 theirs.
    let mut side = 0;
    let mut start = (0, 0);
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        let marker =
            |m: &str| bare == m || bare.strip_prefix(m).is_some_and(|r| r.starts_with(' '));
        match side {
            0 if marker("<<<<<<<") => {
                side = 1;
                start = (ours.len(), theirs.len());
            }
            1 if marker("|||||||") => side = 2,
            1 | 2 if bare == "=======" => side = 3,
            3 if marker(">>>>>>>") => {
                side = 0;
                hunks.push(Hunk {
                    ours: (start.0, ours.len()),
                    theirs: (start.1, theirs.len()),
                });
            }
            _ if marker("<<<<<<<") || marker(">>>>>>>") => {
                return Err("malformed conflict markers".to_owned());
            }
            0 => {
                ours.push(line);
                theirs.push(line);
            }
            1 => ours.push(line),
            2 => {}
            _ => theirs.push(line),
        }
    }
    if side != 0 {
        return Err("malformed conflict markers".to_owned());
    }
    if hunks.is_empty() {
        return Err("no conflict markers".to_owned());
    }
    Ok((ours.concat(), theirs.concat(), hunks))
}

/// Resolves every conflict in the ledger to main's side (ADR-0053). Any conflict there is
/// between stamps, and a section whose entry is taken from main is stale whenever the
/// branch changed that input, so it is reported for a re-read. The result must parse; it is
/// rewritten canonically. Returns it and the sections whose stamp differs from the
/// branch's.
pub(super) fn resolve_ledger(text: &str) -> Result<(String, Vec<String>), String> {
    let (ours, theirs, _) = sides(text)?;
    let parse = |t: &str, side: &str| {
        ledger::Ledger::parse(t).map_err(|e| format!("{side} doesn't parse: {e}"))
    };
    let (ours, theirs) = (
        parse(&ours, "this branch's side")?,
        parse(&theirs, "main's side")?,
    );
    let stamps = theirs
        .sections
        .iter()
        .filter(|(k, v)| ours.sections.get(*k) != Some(v))
        .map(|(k, _)| k.clone())
        .chain(
            ours.sections
                .keys()
                .filter(|k| !theirs.sections.contains_key(*k))
                .cloned(),
        )
        .collect();
    Ok((theirs.render(), stamps))
}

/// Resolves every conflict in `text` (the conflicted contents of `path`) to main's side,
/// provided each conflict is only between inline `verified` lines of `@section` blocks
/// (either side may have none, as when main moved them to the ledger). Returns the
/// resolved text and the sections whose stamp was in conflict, or why the file is left
/// alone.
fn resolve(path: &str, text: &str) -> Result<(String, Vec<String>), String> {
    if !is_tracked(path) {
        return Err("not a tracked doc".to_owned());
    }
    let (ours, theirs, hunks) = sides(text)?;
    let mut stamps = Vec::new();
    for (text, is_main) in [(&ours, false), (&theirs, true)] {
        let doc = parse::parse(path, text, &mut Vec::new());
        let lines: Vec<&str> = text.lines().collect();
        for h in &hunks {
            let (a, b) = if is_main { h.theirs } else { h.ours };
            for (k, line) in lines.iter().enumerate().take(b).skip(a) {
                let stamp = line
                    .trim_start()
                    .strip_prefix("verified")
                    .is_some_and(|r| r.trim_start().starts_with('='));
                let section = doc.sections.iter().find(|s| {
                    s.meta.is_some() && s.block.is_some_and(|(open, close)| open < k && k < close)
                });
                match section {
                    Some(s) if stamp => stamps.push(super::section_key(&doc, &s.anchor)),
                    _ => {
                        return Err(format!(
                            "a conflict outside `@section` `verified` lines (line {} on {})",
                            k + 1,
                            if is_main {
                                "main's side"
                            } else {
                                "this branch's side"
                            }
                        ));
                    }
                }
            }
        }
    }
    stamps.sort();
    stamps.dedup();
    Ok((theirs, stamps))
}

/// Resolves every conflict in `text` (the conflicted contents of `path`) to the merged
/// branch's side, provided each conflict on both sides lies strictly inside a
/// `@generated` block that `docs generate` writes. The caller then rewrites the blocks from the merged sources, so the
/// side taken doesn't matter. Returns the resolved text, or why the file is left alone.
fn resolve_generated(path: &str, text: &str) -> Result<String, String> {
    if !is_tracked(path) {
        return Err("not a tracked doc".to_owned());
    }
    let (ours, theirs, hunks) = sides(text)?;
    for (side, is_main) in [(&ours, false), (&theirs, true)] {
        // Only blocks a generator owns: a conflict in any other block would keep the
        // merged branch's side with nothing to rewrite it.
        let names = [
            super::generate::STATUS_BLOCK,
            super::generate::INDEX_BLOCK,
            super::progress::BLOCK,
            super::summary::BLOCK,
        ];
        let mut inside = Vec::new();
        let mut open = false;
        for line in side.lines() {
            let line = line.trim_end();
            if names.iter().any(|n| line == super::generate::end(n)) {
                open = false;
            }
            inside.push(open);
            if names.iter().any(|n| line == super::generate::begin(n)) {
                open = true;
            }
        }
        for h in &hunks {
            let (a, b) = if is_main { h.theirs } else { h.ours };
            if (a..b).any(|k| !inside.get(k).copied().unwrap_or(false)) {
                return Err("a conflict outside `@generated` blocks".to_owned());
            }
        }
    }
    Ok(theirs)
}

/// Sections whose ledger stamp doesn't match their inputs, or that need one and have
/// none, as `DOC#anchor`.
pub(crate) fn stale_sections(root: &Path) -> Result<Vec<String>> {
    let l = load(root)?;
    let by_path: BTreeMap<&str, &parse::Doc> =
        l.docs.iter().map(|d| (d.path.as_str(), d)).collect();
    let files = hash::Files::new(root, &l.files);
    // A ledger that doesn't parse holds no stamps: everything is stale.
    let ledger = ledger::Ledger::read(root).unwrap_or_default();
    let mut stale = Vec::new();
    for doc in &l.docs {
        for s in &doc.sections {
            let Some(meta) = &s.meta else { continue };
            let (covers, deps) = (
                super::strings(meta, "covers"),
                super::strings(meta, "depends_on"),
            );
            if covers.is_empty() && deps.is_empty() {
                continue;
            }
            let key = super::section_key(doc, &s.anchor);
            let fresh = hash::inputs(doc, &s.anchor, &covers, &deps, &files, &by_path)
                .is_ok_and(|i| ledger::diff(ledger.sections.get(&key), &i).is_empty());
            if !fresh {
                stale.push(key);
            }
        }
    }
    Ok(stale)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "<!-- @doc\nid = \"T\"\n-->\n<a id=\"a\"></a>\n## A\n<!-- @section\ncovers = [\"src/**\"]\n";

    #[test]
    fn stamp_only_conflicts_take_main() {
        let text = format!(
            "{DOC}<<<<<<< HEAD\nverified = {{ hash = \"b3:1\", by = \"agent:x\", note = \"ours\" }}\n=======\nverified = {{ hash = \"b3:2\", by = \"agent:y\", note = \"main\" }}\n>>>>>>> origin/main\n-->\nText.\n"
        );
        let (out, stamps) = resolve("T.md", &text).expect("stamp-only");
        assert_eq!(
            out,
            format!(
                "{DOC}verified = {{ hash = \"b3:2\", by = \"agent:y\", note = \"main\" }}\n-->\nText.\n"
            )
        );
        assert_eq!(stamps, vec!["T#a".to_owned()]);
        // diff3 style: the base side is dropped.
        let diff3 = text.replace(
            "=======\n",
            "||||||| base\nverified = { hash = \"b3:0\", by = \"a:z\", note = \"base\" }\n=======\n",
        );
        assert_eq!(resolve("T.md", &diff3).expect("diff3").0, out);
    }

    #[test]
    fn generated_only_conflicts_take_main() {
        let block = |o: &str, t: &str| {
            format!(
                "# R\n\n<!-- @generated begin progress -->\n| a |\n<<<<<<< HEAD\n{o}=======\n{t}>>>>>>> origin/wave/m1\n<!-- @generated end progress -->\n\nAfter.\n"
            )
        };
        let text = block("7 left.\n", "8 left.\n");
        assert_eq!(
            resolve_generated("README.md", &text).expect("generated-only"),
            "# R\n\n<!-- @generated begin progress -->\n| a |\n8 left.\n<!-- @generated end progress -->\n\nAfter.\n"
        );
        // The UI features summary is generator-owned too.
        let ui = text.replace("progress -->", "ui-summary -->");
        assert_eq!(
            resolve_generated("docs/UI-FEATURES.md", &ui).expect("generated-only"),
            "# R\n\n<!-- @generated begin ui-summary -->\n| a |\n8 left.\n<!-- @generated end ui-summary -->\n\nAfter.\n"
        );
        let outside = text.replace(
            "\nAfter.\n",
            "\n<<<<<<< HEAD\nOurs.\n=======\nMain.\n>>>>>>> m\n",
        );
        let marker = "# R\n\n<<<<<<< HEAD\n<!-- @generated begin progress -->\n=======\n>>>>>>> m\nx\n<!-- @generated end progress -->\n";
        let unowned = text.replace("progress -->", "other -->");
        for (path, text) in [
            ("README.md", unowned.as_str()),
            ("README.md", outside.as_str()),
            ("README.md", marker),
            ("a.rs", text.as_str()),
        ] {
            assert!(resolve_generated(path, text).is_err(), "{text}");
        }
    }

    #[test]
    fn other_conflicts_are_refused() {
        let stamp =
            |n: &str| format!("verified = {{ hash = \"b3:{n}\", by = \"a:x\", note = \"n\" }}\n");
        let conflict =
            |o: &str, t: &str| format!("<<<<<<< HEAD\n{o}=======\n{t}>>>>>>> origin/main\n");
        let cases = [
            (
                "T.md",
                format!(
                    "{DOC}{}-->\nText.\n",
                    conflict(&format!("{}Ours.\n", stamp("1")), &stamp("2"))
                ),
                "outside",
            ),
            (
                "T.md",
                format!("{DOC}-->\n{}", conflict("Ours.\n", "Main.\n")),
                "outside",
            ),
            // A `verified` look-alike in the body is content, not metadata.
            (
                "T.md",
                format!("{DOC}-->\n{}", conflict(&stamp("1"), &stamp("2"))),
                "outside",
            ),
            (
                "src/a.rs",
                conflict(&stamp("1"), &stamp("2")),
                "not a tracked doc",
            ),
            ("T.md", format!("{DOC}-->\nText.\n"), "no conflict markers"),
            (
                "T.md",
                format!("{DOC}<<<<<<< HEAD\n{}-->\n", stamp("1")),
                "malformed",
            ),
        ];
        for (path, text, why) in cases {
            let err = resolve(path, &text).expect_err(&text);
            assert!(err.contains(why), "{err}\n{text}");
        }
    }
}
