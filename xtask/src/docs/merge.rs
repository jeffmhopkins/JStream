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
    /// For each stale section, its stale inputs: the note this branch had written on each
    /// before the merge and the merged commits that changed it, so the re-verify can keep
    /// the one and name the other (docs/DOCS.md#notes).
    pub(crate) inputs: BTreeMap<String, Vec<StaleInput>>,
}

/// One stale input of a section after a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaleInput {
    /// The input, keyed as in the ledger (`body`, `file:<path>`, `dep:<reference>`).
    pub(crate) input: String,
    /// This branch's note on it before the merge, when the branch had written its own entry.
    pub(crate) note: Option<String>,
    /// The merged commits that changed the file the input lives in, as `<hash> <subject>`.
    pub(crate) commits: Vec<String>,
}

/// The merge `docs merge-main` made last on this branch: the latest merge on its
/// first-parent chain that no `origin/main` or `origin/wave/*` branch contains.
#[derive(Debug, Clone)]
pub(crate) struct MergePoint {
    /// The branch before the merge (the merge's first parent).
    ours: String,
    /// What was merged (the merge's second parent).
    theirs: String,
    /// Their merge base.
    base: String,
    /// The commit `docs merge-main` left the branch at: the merge, or the `docs migrate`
    /// commit it made right after it. What changed since is the branch's own later work.
    after: String,
}

/// The subject of the commit `docs merge-main` makes after a merge for what `docs migrate`
/// moved.
pub(crate) const MIGRATE_SUBJECT: &str =
    "docs: move inline verified stamps to docs/verified.toml (ADR-0053)";

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
    check_branch(branch, "can only merge")?;
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
        report.inputs = stale_report(root)?;
        report.stale = report.inputs.keys().cloned().collect();
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
        report.inputs = stale_report(root)?;
        report.stale = report.inputs.keys().cloned().collect();
    }
    Ok(report)
}

/// The ledger as it was at `rev`; empty when the file is missing or doesn't parse there.
fn ledger_at(root: &Path, rev: &str) -> Result<ledger::Ledger> {
    Ok(ledger_text_at(root, rev)?
        .and_then(|text| ledger::Ledger::parse(&text).ok())
        .unwrap_or_default())
}

/// The ledger's text at `rev`; `None` when the file is missing there.
fn ledger_text_at(root: &Path, rev: &str) -> Result<Option<String>> {
    let out = git(root, &["show", &format!("{rev}:{}", ledger::PATH)])?;
    Ok(out
        .status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// The latest merge on this branch's first-parent chain, unless it is already on
/// `origin/main` or an `origin/wave/*` branch (a merge the branch inherited, not one it
/// made); `None` when there is no such merge.
pub(crate) fn last_merge(root: &Path) -> Result<Option<MergePoint>> {
    let merge = git_ok(
        root,
        &["rev-list", "--first-parent", "--merges", "-n1", "HEAD"],
    )?;
    let merge = String::from_utf8_lossy(&merge).trim().to_owned();
    if merge.is_empty() {
        return Ok(None);
    }
    let upstream = git_ok(
        root,
        &[
            "for-each-ref",
            "--contains",
            &merge,
            "--format=%(refname)",
            &format!("refs/remotes/{REMOTE}/{BRANCH}"),
            &format!("refs/remotes/{REMOTE}/wave/"),
        ],
    )?;
    if !upstream.is_empty() {
        return Ok(None);
    }
    let rev = |r: &str| -> Result<String> {
        Ok(String::from_utf8_lossy(&git_ok(root, &["rev-parse", r])?)
            .trim()
            .to_owned())
    };
    let (ours, theirs) = (rev(&format!("{merge}^1"))?, rev(&format!("{merge}^2"))?);
    let base = git_ok(root, &["merge-base", &ours, &theirs])?;
    let base = String::from_utf8_lossy(&base).trim().to_owned();
    // The first commit after the merge on the first-parent chain, if it is the one
    // `merge-main` makes for `docs migrate`, as `<hash> <parent> <subject>`.
    let next = git_ok(
        root,
        &[
            "log",
            "--first-parent",
            "--reverse",
            "--format=%H %P %s",
            &format!("{merge}..HEAD"),
        ],
    )?;
    let after = String::from_utf8_lossy(&next)
        .lines()
        .next()
        .and_then(|l| {
            let (hash, rest) = l.split_once(' ')?;
            let (parent, subject) = rest.split_once(' ')?;
            (parent == merge && subject == MIGRATE_SUBJECT).then(|| hash.to_owned())
        })
        .unwrap_or(merge);
    Ok(Some(MergePoint {
        ours,
        theirs,
        base,
        after,
    }))
}

/// Which of `paths` differ in the working tree, committed or not, from what `docs
/// merge-main` left at `point`: the files this branch changed after its merge.
pub(crate) fn changed_after(
    root: &Path,
    point: &MergePoint,
    paths: &[String],
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for path in paths {
        let diff = git(root, &["diff", "--quiet", &point.after, "--", path])?;
        match diff.status.code() {
            Some(0) => {}
            Some(1) => out.push(path.clone()),
            _ => bail!(
                "`git diff --quiet {} -- {path}` failed: {}",
                point.after,
                String::from_utf8_lossy(&diff.stderr).trim()
            ),
        }
    }
    Ok(out)
}

/// The ledger at the merge base of `HEAD` and `origin/<branch>` (`main`, or the `wave/*`
/// branch a PR targets): the entries this branch didn't write are the ones equal to it.
/// Empty when the ledger doesn't exist there; an error when it doesn't parse, since an
/// empty base would make every entry look like this branch's own.
pub(crate) fn base_ledger(root: &Path, branch: &str) -> Result<ledger::Ledger> {
    check_branch(branch, "can only compare against")?;
    let upstream = format!("{REMOTE}/{branch}");
    let base = git_ok(root, &["merge-base", "HEAD", &upstream])
        .with_context(|| format!("finding where this branch left `{upstream}`"))?;
    let base = String::from_utf8_lossy(&base).trim().to_owned();
    match ledger_text_at(root, &base)? {
        None => Ok(ledger::Ledger::default()),
        Some(text) => ledger::Ledger::parse(&text).map_err(|e| {
            anyhow::anyhow!(
                "{} where this branch left `{upstream}` ({base}) doesn't parse: {e}",
                ledger::PATH
            )
        }),
    }
}

/// Refuses any branch but `main` and `wave/<name>`; `refusal` says what the caller does
/// with it, as in "`refusal` `main` or a `wave/<name>` branch".
fn check_branch(branch: &str, refusal: &str) -> Result<()> {
    if branch != BRANCH && !branch.strip_prefix("wave/").is_some_and(|w| !w.is_empty()) {
        bail!("{refusal} `{BRANCH}` or a `wave/<name>` branch, not `{branch}`");
    }
    Ok(())
}

/// The ledger entries this branch had written itself before `point`'s merge: those of
/// the merge's first parent that differ from the merge base's, by section.
pub(crate) fn branch_entries(
    root: &Path,
    point: &MergePoint,
) -> Result<BTreeMap<String, ledger::Stamp>> {
    let (branch, base) = (ledger_at(root, &point.ours)?, ledger_at(root, &point.base)?);
    Ok(branch
        .sections
        .into_iter()
        .filter_map(|(key, stamp)| {
            let before = base.sections.get(&key);
            let own: ledger::Stamp = stamp
                .into_iter()
                .filter(|(input, e)| before.and_then(|b| b.get(input)) != Some(e))
                .collect();
            (!own.is_empty()).then_some((key, own))
        })
        .collect())
}

/// Each stale section after a merge commit with its stale inputs, each with this branch's
/// earlier note and the merged commits that changed it (none listed when the branch has no
/// merge of its own).
fn stale_report(root: &Path) -> Result<BTreeMap<String, Vec<StaleInput>>> {
    let stale = stale_map(root)?;
    let point = last_merge(root)?;
    let own = match &point {
        Some(p) => branch_entries(root, p)?,
        None => BTreeMap::new(),
    };
    let mut out = BTreeMap::new();
    for (key, inputs) in stale {
        let mut list = Vec::new();
        for (input, path) in inputs {
            let Some(p) = &point else { break };
            let range = format!("{}..{}", p.base, p.theirs);
            let log = git_ok(root, &["log", "--format=%h %s", &range, "--", &path])?;
            list.push(StaleInput {
                note: own
                    .get(&key)
                    .and_then(|s| s.get(&input))
                    .and_then(|e| e.note.clone()),
                commits: String::from_utf8_lossy(&log)
                    .lines()
                    .map(str::to_owned)
                    .collect(),
                input,
            });
        }
        out.insert(key, list);
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
    git_ok(root, &["commit", "-q", "-m", MIGRATE_SUBJECT])?;
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

/// Each stale section (as `DOC#anchor`) with the inputs that differ from its stamp, each
/// with the file it lives in. A section whose inputs can't be computed has none listed.
fn stale_map(root: &Path) -> Result<BTreeMap<String, Vec<(String, String)>>> {
    let l = load(root)?;
    let by_path: BTreeMap<&str, &parse::Doc> =
        l.docs.iter().map(|d| (d.path.as_str(), d)).collect();
    let files = hash::Files::new(root, &l.files);
    // A ledger that doesn't parse holds no stamps: everything is stale.
    let ledger = ledger::Ledger::read(root).unwrap_or_default();
    let mut stale = BTreeMap::new();
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
            let changed = match hash::inputs(doc, &s.anchor, &covers, &deps, &files, &by_path) {
                Ok(i) => ledger::diff(ledger.sections.get(&key), &i),
                Err(_) => {
                    stale.insert(key, Vec::new());
                    continue;
                }
            };
            if !changed.is_empty() {
                let located = changed
                    .into_iter()
                    .map(|input| {
                        let path = input_path(&doc.path, &input);
                        (input, path)
                    })
                    .collect();
                stale.insert(key, located);
            }
        }
    }
    Ok(stale)
}

/// The file a ledger input lives in: the doc itself for `body`, the covered file for
/// `file:<path>[#view]`, and the target doc for `dep:<path>[#anchor[/key]]`.
pub(crate) fn input_path(doc: &str, input: &str) -> String {
    let path = match input.split_once(':') {
        Some(("file" | "dep", rest)) => rest,
        _ => doc,
    };
    path.split('#').next().unwrap_or(path).to_owned()
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
