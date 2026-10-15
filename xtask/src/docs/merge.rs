//! `docs merge-main`: merges `origin/main` and resolves conflicts that are only between
//! two `verified` stamps, by taking main's stamp (docs/DOCS.md#verification).
//!
//! Parallel PRs that re-verify the same section conflict on its `verified` line. The hash
//! covers the section's inputs, so after the merge the section has to be re-read against
//! the combined change anyway; which stamp the merge keeps doesn't matter, only that the
//! section is reported as stale. Every other conflict is left untouched for a person.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

use super::{hash, is_tracked, load, parse};

/// The branch `merge-main` merges, and the remote it is fetched from.
const REMOTE: &str = "origin";
const BRANCH: &str = "main";

/// What `docs merge-main` did.
#[derive(Debug, Default)]
pub(crate) struct Merge {
    /// Sections whose conflicting stamp was resolved to main's, as `DOC#anchor`.
    pub(crate) stamps: Vec<String>,
    /// Conflicted files left for a person, with the reason.
    pub(crate) refused: Vec<(String, String)>,
    /// Whether the merge was committed.
    pub(crate) committed: bool,
    /// Sections stale after the merge, as `DOC#anchor`; empty unless `committed`.
    pub(crate) stale: Vec<String>,
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

/// Fetches and merges `origin/main` into the current branch, resolves stamp-only
/// conflicts, and commits the merge when nothing else conflicts.
pub(crate) fn merge_main(root: &Path) -> Result<Merge> {
    if git(root, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])?
        .status
        .success()
    {
        bail!("a merge is already in progress; finish or abort it first");
    }
    if !git_ok(root, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        bail!("the working tree has uncommitted changes; commit or stash them first");
    }
    git_ok(root, &["fetch", "-q", REMOTE, BRANCH])?;
    let upstream = format!("{REMOTE}/{BRANCH}");
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
        report.stale = stale_sections(root)?;
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
                .and_then(|text| resolve(path, &text))
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
    if report.refused.is_empty() {
        git_ok(root, &["commit", "-q", "--no-edit"])?;
        report.committed = true;
        report.stale = stale_sections(root)?;
    }
    Ok(report)
}

/// One conflict hunk: the 0-based line ranges its two sides occupy once resolved to that
/// side.
struct Hunk {
    ours: (usize, usize),
    theirs: (usize, usize),
}

/// Resolves every conflict in `text` (the conflicted contents of `path`) to main's side,
/// provided each conflict is only between `verified` lines of `@section` blocks on both
/// sides. Returns the resolved text and the sections whose stamp was taken from main, or
/// why the file is left alone.
fn resolve(path: &str, text: &str) -> Result<(String, Vec<String>), String> {
    if !is_tracked(path) {
        return Err("not a tracked doc".to_owned());
    }
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
    let (ours, theirs) = (ours.concat(), theirs.concat());
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
                    Some(s) if stamp => {
                        if is_main {
                            let id = doc.meta_str("id").unwrap_or(&doc.path);
                            stamps.push(format!("{id}#{}", s.anchor));
                        }
                    }
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
    stamps.dedup();
    Ok((theirs, stamps))
}

/// Sections whose `verified` stamp doesn't match the tree, or that need one and have
/// none, as `DOC#anchor`.
pub(crate) fn stale_sections(root: &Path) -> Result<Vec<String>> {
    let l = load(root)?;
    let by_path: BTreeMap<&str, &parse::Doc> =
        l.docs.iter().map(|d| (d.path.as_str(), d)).collect();
    let files = hash::Files::new(root, &l.files);
    let mut stale = Vec::new();
    for doc in &l.docs {
        for s in &doc.sections {
            let Some(meta) = &s.meta else { continue };
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
            let (covers, deps) = (strings("covers"), strings("depends_on"));
            if covers.is_empty() && deps.is_empty() {
                continue;
            }
            let stored = meta
                .get("verified")
                .and_then(|v| v.get("hash"))
                .and_then(|h| h.as_str());
            let now = hash::section_hash(doc, &s.anchor, &covers, &deps, &files, &by_path).ok();
            if stored.is_none() || stored != now.as_deref() {
                stale.push(format!(
                    "{}#{}",
                    doc.meta_str("id").unwrap_or(&doc.path),
                    s.anchor
                ));
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
