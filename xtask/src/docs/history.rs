//! `docs log`: a doc's or section's history, derived from git (docs/DOCS.md#history).
//! Nothing about history is stored in the files: revisions are the first-parent commits
//! that changed the normalized body, verifications and reviews are the commits that
//! changed the `verified` or `reviewed` line.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::{hash, parse};

/// A commit as `docs log` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Commit {
    pub(crate) sha: String,
    /// Committer time, seconds since the Unix epoch: when the change reached the branch.
    pub(crate) time: i64,
    pub(crate) author: String,
    pub(crate) subject: String,
}

impl Commit {
    /// The PR a squash-merge subject names, as its trailing `(#n)`.
    pub(crate) fn pr(&self) -> Option<u32> {
        let rest = self.subject.trim_end().strip_suffix(')')?;
        let (_, n) = rest.rsplit_once("(#")?;
        n.parse().ok()
    }
}

/// A `verified` or `reviewed` stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub(crate) by: String,
    pub(crate) note: String,
    /// The section hash, for `verified` stamps.
    pub(crate) hash: Option<String>,
}

/// What a commit did to the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// The n-th change to the normalized body (the first is the target's creation).
    Revision(usize),
    /// The `verified` (section) or `reviewed` (doc) line changed to this stamp.
    Stamped(Stamp),
    /// Only the stored hash changed, by and note kept (as `docs rehash` does).
    Restamped,
    /// The target no longer exists after this commit.
    Removed,
}

/// One entry of a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Event {
    pub(crate) commit: Commit,
    pub(crate) change: Change,
}

/// The log of one doc or section.
#[derive(Debug)]
pub(crate) struct Log {
    /// `DOC` or `DOC#anchor`.
    pub(crate) name: String,
    pub(crate) path: String,
    /// Whether the target is a whole doc (its stamp is `reviewed`).
    pub(crate) whole_doc: bool,
    /// The doc's `review_every`, for whole docs that have one.
    pub(crate) review_every: Option<String>,
    /// Oldest first.
    pub(crate) events: Vec<Event>,
    /// Whether the clone is shallow, so older history is missing.
    pub(crate) shallow: bool,
    /// Whether the working tree differs from `rev` for this file.
    pub(crate) uncommitted: bool,
}

/// The target's state in one version of its file.
#[derive(Debug, PartialEq, Eq)]
struct State {
    body: Option<String>,
    stamp: Option<Stamp>,
}

fn git(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("running `git {}`", args.join(" ")))
}

fn stamp_of(v: Option<&toml::Value>) -> Option<Stamp> {
    let t = v?.as_table()?;
    let s = |k: &str| t.get(k).and_then(|x| x.as_str()).map(str::to_owned);
    Some(Stamp {
        by: s("by").unwrap_or_default(),
        note: s("note").unwrap_or_default(),
        hash: s("hash"),
    })
}

/// The target's state in `text`, the contents of `path`: `anchor = None` is the whole doc.
fn state(path: &str, text: &str, anchor: Option<&str>) -> State {
    let doc = parse::parse(path, text, &mut Vec::new());
    let stamp = match anchor {
        None => stamp_of(doc.meta.as_ref().and_then(|m| m.get("reviewed"))),
        Some(a) => stamp_of(
            doc.sections
                .iter()
                .find(|s| s.anchor == a)
                .and_then(|s| s.meta.as_ref())
                .and_then(|m| m.get("verified")),
        ),
    };
    State {
        body: hash::body(&doc, anchor),
        stamp,
    }
}

/// Folds successive states of the target into events. `states` is oldest first; `None`
/// means the file did not exist at that commit.
fn events(states: Vec<(Commit, Option<State>)>) -> Vec<Event> {
    let mut out = Vec::new();
    let mut revisions = 0;
    let mut prev = State {
        body: None,
        stamp: None,
    };
    for (commit, st) in states {
        let st = st.unwrap_or(State {
            body: None,
            stamp: None,
        });
        let mut push = |change| {
            out.push(Event {
                commit: commit.clone(),
                change,
            });
        };
        match (&prev.body, &st.body) {
            (Some(_), None) => push(Change::Removed),
            (old, Some(new)) if old.as_ref() != Some(new) => {
                revisions += 1;
                push(Change::Revision(revisions));
            }
            _ => {}
        }
        if st.stamp != prev.stamp
            && let Some(s) = &st.stamp
        {
            match &prev.stamp {
                Some(p) if p.by == s.by && p.note == s.note => push(Change::Restamped),
                _ => push(Change::Stamped(s.clone())),
            }
        }
        prev = st;
    }
    out
}

/// `docs log <DOC | path>[#anchor] [--rev <rev>]`: the target's history on the
/// first-parent line of `rev` (squash merges on `main`, by default `HEAD`). Docs are
/// followed by their current path, so history from before a rename is not shown.
pub(crate) fn log(root: &Path, docs: &[parse::Doc], target: &str, rev: &str) -> Result<Log> {
    let (doc_ref, anchor) = match target.split_once('#') {
        Some((d, a)) => (d, Some(a)),
        None => (target, None),
    };
    let doc = docs
        .iter()
        .find(|d| d.path == doc_ref || d.meta_str("id") == Some(doc_ref))
        .with_context(|| format!("no tracked doc `{doc_ref}`"))?;
    let id = doc.meta_str("id").unwrap_or(&doc.path);
    let name = anchor.map_or_else(|| id.to_owned(), |a| format!("{id}#{a}"));

    let out = git(
        root,
        &[
            "log",
            "--first-parent",
            "--reverse",
            "--format=%H%x1f%ct%x1f%an%x1f%s%x1e",
            rev,
            "--",
            &doc.path,
        ],
    )?;
    if !out.status.success() {
        bail!("`git log` failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let mut states = Vec::new();
    for rec in String::from_utf8(out.stdout)?.split('\x1e') {
        let f: Vec<&str> = rec.trim_matches('\n').split('\x1f').collect();
        let [sha, time, author, subject] = f[..] else {
            continue;
        };
        let commit = Commit {
            sha: sha.to_owned(),
            time: time
                .parse()
                .with_context(|| format!("commit time `{time}`"))?,
            author: author.to_owned(),
            subject: subject.to_owned(),
        };
        let show = git(root, &["show", &format!("{sha}:{}", doc.path)])?;
        let st = show.status.success().then(|| {
            state(
                &doc.path,
                &String::from_utf8_lossy(&show.stdout).replace("\r\n", "\n"),
                anchor,
            )
        });
        states.push((commit, st));
    }
    let events = events(states);
    if let Some(a) = anchor
        && events.is_empty()
        && !doc.has_anchor(a)
    {
        bail!("no section or rule `{a}` in {}", doc.path);
    }
    let shallow = git(root, &["rev-parse", "--is-shallow-repository"])
        .is_ok_and(|o| o.stdout.starts_with(b"true"));
    let uncommitted = git(root, &["diff", "--quiet", rev, "--", &doc.path])
        .is_ok_and(|o| o.status.code() == Some(1));
    Ok(Log {
        name,
        path: doc.path.clone(),
        whole_doc: anchor.is_none(),
        review_every: anchor
            .is_none()
            .then(|| doc.meta_str("review_every").map(str::to_owned))
            .flatten(),
        events,
        shallow,
        uncommitted,
    })
}

/// Days in a `review_every` value such as `"90d"`.
pub(crate) fn ttl_days(s: &str) -> Option<i64> {
    s.strip_suffix('d')?.parse().ok().filter(|&d| d > 0)
}

/// `YYYY-MM-DD` (UTC) for seconds since the Unix epoch.
pub(crate) fn date(secs: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The log as text; `now` is seconds since the Unix epoch, for the TTL line.
pub(crate) fn render(log: &Log, now: i64) -> String {
    use std::fmt::Write as _;
    let mut s = format!("{} ({})\n", log.name, log.path);
    let stamp = if log.whole_doc {
        "reviewed"
    } else {
        "verified"
    };
    let (mut revisions, mut stamps) = (0, 0);
    let mut last_ttl = None;
    for e in &log.events {
        let c = &e.commit;
        let pr = c.pr().map_or_else(String::new, |n| format!("#{n}"));
        let what = match &e.change {
            Change::Revision(n) => {
                revisions += 1;
                last_ttl = Some(c.time);
                let created = if *n == 1 { "  (created)" } else { "" };
                format!("r{n:<9}{}: {}{created}", c.author, c.subject)
            }
            Change::Stamped(st) => {
                stamps += 1;
                last_ttl = Some(c.time);
                format!("{stamp:<10}by {}: {}", st.by, st.note)
            }
            Change::Restamped => "restamped hash only, by and note kept".to_owned(),
            Change::Removed => format!("removed   {}: {}", c.author, c.subject),
        };
        let sha = c.sha.get(..9).unwrap_or(&c.sha);
        let _ = writeln!(s, "  {}  {sha}  {pr:<6} {what}", date(c.time));
    }
    if log.events.is_empty() {
        s.push_str("  (no commits)\n");
    }
    let _ = writeln!(s, "{revisions} revision(s), {stamps} {stamp} stamp(s)");
    if let (Some(every), Some(t)) = (&log.review_every, last_ttl)
        && let Some(days) = ttl_days(every)
    {
        let due = t + days * 86_400;
        let state = if due <= now { "due now" } else { "not due" };
        let _ = writeln!(
            s,
            "review every {every}: clock restarted {}, due {} ({state})",
            date(t),
            date(due)
        );
    }
    if log.uncommitted {
        s.push_str("note: the working tree has uncommitted changes to this file, not shown\n");
    }
    if log.shallow {
        s.push_str(
            "note: this clone is shallow, so older history is missing \
             (`git fetch --unshallow`)\n",
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(n: i64, subject: &str) -> Commit {
        Commit {
            sha: format!("{n:040}"),
            time: n * 86_400,
            author: "a".into(),
            subject: subject.into(),
        }
    }

    fn st(body: Option<&str>, stamp: Option<(&str, &str, &str)>) -> Option<State> {
        Some(State {
            body: body.map(str::to_owned),
            stamp: stamp.map(|(by, note, h)| Stamp {
                by: by.into(),
                note: note.into(),
                hash: Some(h.into()),
            }),
        })
    }

    #[test]
    fn pr_from_squash_subject() {
        assert_eq!(commit(0, "feat: x (#135)").pr(), Some(135));
        assert_eq!(commit(0, "fix (#12) and more").pr(), None);
        assert_eq!(commit(0, "Merge branch main").pr(), None);
    }

    #[test]
    fn dates_and_ttl() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(951_782_400), "2000-02-29");
        assert_eq!(date(1_791_936_000), "2026-10-14");
        assert_eq!(date(-86_400), "1969-12-31");
        assert_eq!(ttl_days("90d"), Some(90));
        assert_eq!(ttl_days("0d"), None);
        assert_eq!(ttl_days("3w"), None);
    }

    /// Revisions count body changes only; stamps are reported when by or note change, a
    /// hash-only change is a restamp, and a commit that touches neither is skipped.
    #[test]
    fn folds_states_into_events() {
        let ev = events(vec![
            (commit(1, "add"), st(Some("A"), None)),
            (
                commit(2, "verify"),
                st(Some("A"), Some(("agent:x", "n1", "h1"))),
            ),
            (
                commit(3, "other section"),
                st(Some("A"), Some(("agent:x", "n1", "h1"))),
            ),
            (
                commit(4, "edit"),
                st(Some("B"), Some(("agent:x", "n1", "h1"))),
            ),
            (
                commit(5, "rehash"),
                st(Some("B"), Some(("agent:x", "n1", "h2"))),
            ),
            (
                commit(6, "edit+verify"),
                st(Some("C"), Some(("agent:y", "n2", "h3"))),
            ),
            (commit(7, "drop section"), st(None, None)),
            (commit(8, "file deleted"), None),
            (commit(9, "back"), st(Some("C"), None)),
        ]);
        let got: Vec<(i64, Change)> = ev
            .into_iter()
            .map(|e| (e.commit.time / 86_400, e.change))
            .collect();
        let stamp = |by: &str, note: &str, h: &str| {
            Change::Stamped(Stamp {
                by: by.into(),
                note: note.into(),
                hash: Some(h.into()),
            })
        };
        assert_eq!(
            got,
            vec![
                (1, Change::Revision(1)),
                (2, stamp("agent:x", "n1", "h1")),
                (4, Change::Revision(2)),
                (5, Change::Restamped),
                (6, Change::Revision(3)),
                (6, stamp("agent:y", "n2", "h3")),
                (7, Change::Removed),
                (9, Change::Revision(4)),
            ]
        );
    }

    #[test]
    fn renders_ttl_for_docs() {
        let log = Log {
            name: "T".into(),
            path: "T.md".into(),
            whole_doc: true,
            review_every: Some("30d".into()),
            events: vec![
                Event {
                    commit: commit(1, "docs: add T (#7)"),
                    change: Change::Revision(1),
                },
                Event {
                    commit: commit(10, "docs: review T (#9)"),
                    change: Change::Stamped(Stamp {
                        by: "human:j".into(),
                        note: "re-read".into(),
                        hash: None,
                    }),
                },
            ],
            shallow: false,
            uncommitted: false,
        };
        let text = render(&log, 50 * 86_400);
        assert!(
            text.contains("1970-01-02  000000000  #7     r1        a: docs: add T (#7)  (created)"),
            "{text}"
        );
        assert!(
            text.contains("#9     reviewed  by human:j: re-read"),
            "{text}"
        );
        assert!(
            text.contains("1 revision(s), 1 reviewed stamp(s)"),
            "{text}"
        );
        assert!(
            text.contains("clock restarted 1970-01-11, due 1970-02-10 (due now)"),
            "{text}"
        );
    }
}
