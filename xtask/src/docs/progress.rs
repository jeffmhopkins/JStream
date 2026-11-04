//! The roadmap progress block in `README.md`, generated from `docs/progress.toml`
//! (docs/DOCS.md#generated). `docs generate` writes it, `docs check` reports drift, and
//! `docs progress` lists the entries that the merged history says are out of date.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::generate::{begin, escape, with_block};
use super::parse::{Diag, Doc};

/// The data file: milestones and roadmap items with their status and PRs.
pub(crate) const DATA: &str = "docs/progress.toml";
/// The doc that carries the block.
const TARGET: &str = "README.md";
/// The name of the generated block.
pub(crate) const BLOCK: &str = "progress";
/// Each status, with the label the item table shows for it.
const STATUSES: &[(&str, &str)] = &[
    ("done", "✅ merged"),
    ("hardware", "🧪 merged, Windows run pending"),
    ("review", "🔍 in review"),
    ("planned", "⏳ planned"),
];
/// The steps an unfinished item can still need (its `needs`), with the label the
/// "What's left" table shows for each.
const NEEDS: &[(&str, &str)] = &[
    ("build", "🔨 build"),
    ("wave", "🌊 its wave lands"),
    ("hardware", "🧪 Windows run"),
    ("maintainer", "👤 maintainer"),
];
/// The step shown for an unfinished item whose `needs` isn't set yet.
const UNSET: &str = "unset";
/// Cells in the progress bar.
const BAR: usize = 10;

/// A milestone of `ROADMAP.md`.
#[derive(Debug)]
struct Milestone {
    id: String,
    title: String,
    /// The link to its roadmap section, relative to the repository root.
    roadmap: String,
}

/// A release: a tagged alpha that shipped, or the next one being gathered.
#[derive(Debug)]
struct Release {
    id: String,
    /// The version it ships as, e.g. `0.1.0-alpha.4`.
    version: String,
    /// What the release is for, in a sentence.
    goal: String,
    /// The git tag, once it has shipped.
    tag: Option<String>,
}

/// One roadmap item: a PR-sized piece of a milestone.
#[derive(Debug)]
struct Item {
    id: String,
    milestone: String,
    title: String,
    status: String,
    prs: Vec<u64>,
    note: Option<String>,
    /// The release it first ships in, when that is known.
    release: Option<String>,
    /// Its release waits for it.
    gate: bool,
    /// The steps still to do before it is `done`, in order (`NEEDS`).
    needs: Vec<String>,
}

impl Item {
    /// Its PRs are on `main` (it may still wait on a hardware run).
    fn merged(&self) -> bool {
        matches!(self.status.as_str(), "done" | "hardware")
    }

    /// The steps it still needs: its `needs`, or for a `hardware` item that lists none,
    /// its Windows run, or `unset` for another unfinished item that lists none. Empty
    /// once it is `done`.
    fn steps(&self) -> Vec<&str> {
        match self.status.as_str() {
            "done" => Vec::new(),
            _ if !self.needs.is_empty() => self.needs.iter().map(String::as_str).collect(),
            "hardware" => vec!["hardware"],
            _ => vec![UNSET],
        }
    }
}

/// The parsed data file.
#[derive(Debug)]
struct Progress {
    releases: Vec<Release>,
    milestones: Vec<Milestone>,
    items: Vec<Item>,
}

/// A required string key of `t`.
fn string(t: &toml::Table, key: &str, what: &str, errs: &mut Vec<String>) -> String {
    match t.get(key).and_then(toml::Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.to_owned(),
        _ => {
            errs.push(format!("{what}: missing string `{key}`"));
            String::new()
        }
    }
}

/// Reports keys of `t` that are not in `known`.
fn unknown_keys(t: &toml::Table, known: &[&str], what: &str, errs: &mut Vec<String>) {
    for k in t.keys().filter(|k| !known.contains(&k.as_str())) {
        errs.push(format!("{what}: unknown key `{k}`"));
    }
}

/// The `[[name]]` tables of `doc`.
fn tables<'t>(doc: &'t toml::Table, name: &str, errs: &mut Vec<String>) -> Vec<&'t toml::Table> {
    match doc.get(name) {
        None => Vec::new(),
        Some(v) => match v.as_array() {
            Some(a) if a.iter().all(toml::Value::is_table) => {
                a.iter().filter_map(toml::Value::as_table).collect()
            }
            _ => {
                errs.push(format!(
                    "`{name}` must be an array of tables (`[[{name}]]`)"
                ));
                Vec::new()
            }
        },
    }
}

/// Parses and validates the data file; returns every problem found.
fn parse(text: &str) -> Result<Progress, Vec<String>> {
    let doc: toml::Table = text
        .parse()
        .map_err(|e| vec![format!("not valid TOML: {e}")])?;
    let mut errs = Vec::new();
    unknown_keys(
        &doc,
        &["release", "milestone", "item"],
        "top level",
        &mut errs,
    );
    let mut releases = Vec::new();
    for t in tables(&doc, "release", &mut errs) {
        let id = string(t, "id", "release", &mut errs);
        let what = format!("release `{id}`");
        unknown_keys(t, &["id", "version", "goal", "tag"], &what, &mut errs);
        let tag = match t.get("tag") {
            None => None,
            Some(v) => match v.as_str() {
                Some(s) if !s.trim().is_empty() => Some(s.to_owned()),
                _ => {
                    errs.push(format!("{what}: `tag` must be a non-empty string"));
                    None
                }
            },
        };
        let r = Release {
            version: string(t, "version", &what, &mut errs),
            goal: string(t, "goal", &what, &mut errs),
            tag,
            id,
        };
        if releases.iter().any(|o: &Release| o.id == r.id) {
            errs.push(format!("duplicate release id `{}`", r.id));
        }
        if r.tag.is_some() && releases.iter().any(|o: &Release| o.tag.is_none()) {
            errs.push(format!(
                "{what}: a tagged release must come before every untagged one"
            ));
        }
        releases.push(r);
    }
    let mut milestones = Vec::new();
    for t in tables(&doc, "milestone", &mut errs) {
        let id = string(t, "id", "milestone", &mut errs);
        let what = format!("milestone `{id}`");
        unknown_keys(t, &["id", "title", "roadmap"], &what, &mut errs);
        let m = Milestone {
            title: string(t, "title", &what, &mut errs),
            roadmap: string(t, "roadmap", &what, &mut errs),
            id,
        };
        if milestones.iter().any(|o: &Milestone| o.id == m.id) {
            errs.push(format!("duplicate milestone id `{}`", m.id));
        }
        milestones.push(m);
    }
    let mut items = Vec::new();
    for t in tables(&doc, "item", &mut errs) {
        let id = string(t, "id", "item", &mut errs);
        let what = format!("item `{id}`");
        unknown_keys(
            t,
            &[
                "id",
                "milestone",
                "title",
                "status",
                "prs",
                "note",
                "needs",
                "release",
                "gate",
            ],
            &what,
            &mut errs,
        );
        let prs = match t.get("prs") {
            None => Vec::new(),
            Some(v) => {
                let nums: Option<Vec<u64>> = v.as_array().and_then(|a| {
                    a.iter()
                        .map(|x| x.as_integer().and_then(|n| u64::try_from(n).ok()))
                        .collect()
                });
                nums.filter(|n| n.iter().all(|&p| p > 0))
                    .unwrap_or_else(|| {
                        errs.push(format!("{what}: `prs` must be an array of PR numbers"));
                        Vec::new()
                    })
            }
        };
        let note = match t.get("note") {
            None => None,
            Some(v) => match v.as_str() {
                Some(s) if !s.trim().is_empty() => Some(s.to_owned()),
                _ => {
                    errs.push(format!("{what}: `note` must be a non-empty string"));
                    None
                }
            },
        };
        let needs = match t.get("needs") {
            None => Vec::new(),
            Some(v) => v
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .map(|x| x.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| {
                    errs.push(format!(
                        "{what}: `needs` must be a non-empty array of steps"
                    ));
                    Vec::new()
                }),
        };
        for n in &needs {
            if !NEEDS.iter().any(|(s, _)| s == n) {
                let names: Vec<&str> = NEEDS.iter().map(|(s, _)| *s).collect();
                errs.push(format!(
                    "{what}: unknown step `{n}` in `needs` (one of {})",
                    names.join(", ")
                ));
            }
        }
        let release = match t.get("release") {
            None => None,
            Some(v) => match v.as_str() {
                Some(s) if !s.trim().is_empty() => Some(s.to_owned()),
                _ => {
                    errs.push(format!("{what}: `release` must be a release id"));
                    None
                }
            },
        };
        let gate = match t.get("gate") {
            None => false,
            Some(v) => v.as_bool().unwrap_or_else(|| {
                errs.push(format!("{what}: `gate` must be true or false"));
                false
            }),
        };
        if gate && release.is_none() {
            errs.push(format!(
                "{what}: `gate` needs the `release` that waits for it"
            ));
        }
        let item = Item {
            milestone: string(t, "milestone", &what, &mut errs),
            title: string(t, "title", &what, &mut errs),
            status: string(t, "status", &what, &mut errs),
            prs,
            note,
            release,
            gate,
            needs,
            id,
        };
        if let Some(r) = &item.release {
            match releases.iter().find(|o| &o.id == r) {
                None => errs.push(format!(
                    "{what}: unknown release `{r}`; list it as a `[[release]]` first"
                )),
                Some(o) if o.tag.is_some() && !item.merged() && !item.status.is_empty() => {
                    errs.push(format!(
                        "{what}: release `{r}` has shipped, so the item must be merged (`done` or `hardware`)"
                    ));
                }
                Some(_) => {}
            }
        }
        if !item.milestone.is_empty() && !milestones.iter().any(|m| m.id == item.milestone) {
            errs.push(format!(
                "{what}: unknown milestone `{}`; list it as a `[[milestone]]` first",
                item.milestone
            ));
        }
        if !item.status.is_empty() && !STATUSES.iter().any(|(s, _)| *s == item.status) {
            let names: Vec<&str> = STATUSES.iter().map(|(s, _)| *s).collect();
            errs.push(format!(
                "{what}: unknown status `{}` (one of {})",
                item.status,
                names.join(", ")
            ));
        }
        if item.status != "planned" && !item.status.is_empty() && item.prs.is_empty() {
            errs.push(format!(
                "{what}: status `{}` needs the PRs that delivered it in `prs`",
                item.status
            ));
        }
        if item.status == "hardware" && item.note.is_none() {
            errs.push(format!(
                "{what}: status `hardware` needs a `note` naming the pending run"
            ));
        }
        if item.status == "hardware"
            && !item.needs.is_empty()
            && !item.needs.iter().any(|n| n == "hardware")
        {
            errs.push(format!(
                "{what}: status `hardware` needs `hardware` in its `needs`"
            ));
        }
        let mut seen = BTreeSet::new();
        if item.needs.iter().any(|n| !seen.insert(n)) {
            errs.push(format!("{what}: a step is listed twice in `needs`"));
        }
        let mut seen = BTreeSet::new();
        if item.prs.iter().any(|p| !seen.insert(p)) {
            errs.push(format!("{what}: a PR is listed twice in `prs`"));
        }
        if items.iter().any(|o: &Item| o.id == item.id) {
            errs.push(format!("duplicate item id `{}`", item.id));
        }
        items.push(item);
    }
    for m in &milestones {
        if !items.iter().any(|i| i.milestone == m.id) {
            errs.push(format!("milestone `{}` has no items", m.id));
        }
    }
    if errs.is_empty() {
        Ok(Progress {
            releases,
            milestones,
            items,
        })
    } else {
        Err(errs)
    }
}

/// `n` of `total` as a bar of `BAR` cells, rounded to the nearest cell.
fn bar(n: usize, total: usize) -> String {
    let filled = (n * BAR * 2 + total) / (total * 2);
    format!("`{}{}`", "█".repeat(filled), "░".repeat(BAR - filled))
}

/// The PR list cell.
fn prs(item: &Item) -> String {
    let list: Vec<String> = item.prs.iter().map(|p| format!("#{p}")).collect();
    list.join(", ")
}

/// The status label of an item.
fn label(i: &Item) -> String {
    let label = STATUSES
        .iter()
        .find(|(s, _)| *s == i.status)
        .map_or("", |(_, l)| l);
    match &i.note {
        Some(n) => format!("{label}: {}", escape(n)),
        None => label.to_owned(),
    }
}

/// The progress cells of a group of items: bar and percentage, merged, in review and
/// planned.
fn cells(items: &[&Item]) -> String {
    let merged = items.iter().filter(|i| i.merged()).count();
    let review = items.iter().filter(|i| i.status == "review").count();
    if items.is_empty() {
        return "no items | 0 | 0 | 0".to_owned();
    }
    format!(
        "{} {}% | {merged} | {review} | {}",
        bar(merged, items.len()),
        (merged * 200 + items.len()) / (items.len() * 2),
        items.len() - merged - review,
    )
}

/// The release part of the block: one row per release, and the items of each.
fn releases(p: &Progress, out: &mut String) {
    if p.releases.is_empty() {
        return;
    }
    let next = p.releases.iter().position(|r| r.tag.is_none());
    out.push_str(
        "### By release\n\n\
         An item counts toward the release it first ships in. A release's gate items are \
         the ones it waits for; the others are merged work it carries.\n\n\
         | Release | Goal | Status | Progress | Merged | In review | Planned |\n\
         |---|---|---|---|---|---|---|\n",
    );
    let of = |id: Option<&str>| -> Vec<&Item> {
        p.items
            .iter()
            .filter(|i| i.release.as_deref() == id)
            .collect()
    };
    for (n, r) in p.releases.iter().enumerate() {
        let status = match (&r.tag, next) {
            (Some(tag), _) => format!("📦 released as `{}`", escape(tag)),
            (None, Some(k)) if k == n => {
                let gate: Vec<&Item> = of(Some(&r.id)).into_iter().filter(|i| i.gate).collect();
                if gate.is_empty() {
                    "🔨 next".to_owned()
                } else {
                    format!(
                        "🔨 next: waits on {} gate items, {} merged",
                        gate.len(),
                        gate.iter().filter(|i| i.merged()).count()
                    )
                }
            }
            (None, _) => "⏳ later".to_owned(),
        };
        let _ = writeln!(
            out,
            "| `{}` | {} | {status} | {} |",
            escape(&r.version),
            escape(&r.goal),
            cells(&of(Some(&r.id)))
        );
    }
    let rest = of(None);
    if !rest.is_empty() {
        let _ = writeln!(
            out,
            "| Not scheduled | Roadmap items no release is gathering yet | ⏳ later | {} |",
            cells(&rest)
        );
    }
    for (n, r) in p.releases.iter().enumerate() {
        let mut items = of(Some(&r.id));
        if items.is_empty() {
            continue;
        }
        // Unfinished work first, so what the release still waits on leads.
        let rank = |i: &Item| STATUSES.iter().rev().position(|(s, _)| *s == i.status);
        items.sort_by_key(|i| (rank(i), !i.gate));
        let merged = items.iter().filter(|i| i.merged()).count();
        let open = if next == Some(n) { " open" } else { "" };
        let _ = write!(
            out,
            "\n<details{open}>\n<summary><b>{}</b>: {merged} of {} merged</summary>\n\n\
             | ID | Item | Gate | Milestone | Status | PRs |\n|---|---|---|---|---|---|\n",
            escape(&r.version),
            items.len()
        );
        for i in items {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} | {} |",
                escape(&i.id),
                escape(&i.title),
                if i.gate { "🚧" } else { "" },
                escape(&i.milestone),
                label(i),
                prs(i)
            );
        }
        out.push_str("\n</details>\n");
    }
    out.push_str("\n### By goal\n\nEach goal is a milestone of the roadmap.\n\n");
}

/// The inside of the block: the releases, then a summary table of the milestones, then
/// each milestone's items.
fn render(p: &Progress) -> String {
    let mut out = String::new();
    releases(p, &mut out);
    out.push_str(
        "| Milestone | Progress | Merged | In review | Planned |\n|---|---|---|---|---|\n",
    );
    let of =
        |m: &Milestone| -> Vec<&Item> { p.items.iter().filter(|i| i.milestone == m.id).collect() };
    for m in &p.milestones {
        let items = of(m);
        let merged = items.iter().filter(|i| i.merged()).count();
        let review = items.iter().filter(|i| i.status == "review").count();
        let _ = writeln!(
            out,
            "| [{}: {}]({}) | {} {}% | {merged} | {review} | {} |",
            escape(&m.id),
            escape(&m.title),
            m.roadmap,
            bar(merged, items.len()),
            (merged * 200 + items.len()) / (items.len() * 2),
            items.len() - merged - review,
        );
    }
    let hardware = p.items.iter().filter(|i| i.status == "hardware").count();
    let _ = writeln!(
        out,
        "\n{} of {} items are merged; {hardware} of those still wait on a run on Windows hardware.",
        p.items.iter().filter(|i| i.merged()).count(),
        p.items.len(),
    );
    left(p, &mut out);
    for m in &p.milestones {
        let items = of(m);
        let merged = items.iter().filter(|i| i.merged()).count();
        let _ = write!(
            out,
            "\n<details>\n<summary><b>{}</b> {}: {merged} of {} merged</summary>\n\n\
             | ID | Item | Status | PRs |\n|---|---|---|---|\n",
            escape(&m.id),
            escape(&m.title),
            items.len()
        );
        for i in items {
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} |",
                escape(&i.id),
                escape(&i.title),
                label(i),
                prs(i)
            );
        }
        out.push_str("\n</details>\n");
    }
    out
}

/// The "What's left" part of the block: every item not yet `done` with the steps it still
/// needs, in milestone order.
fn left(p: &Progress, out: &mut String) {
    let open: Vec<&Item> = p
        .milestones
        .iter()
        .flat_map(|m| p.items.iter().filter(move |i| i.milestone == m.id))
        .filter(|i| !i.steps().is_empty())
        .collect();
    if open.is_empty() {
        out.push_str("\nNothing is left: every item is done.\n");
        return;
    }
    let label = |s: &str| {
        NEEDS
            .iter()
            .find(|(n, _)| *n == s)
            .map_or("❔ not set", |(_, l)| l)
    };
    let counts: Vec<String> = NEEDS
        .iter()
        .chain([&(UNSET, "❔ not set")])
        .filter_map(|(n, l)| {
            let c = open.iter().filter(|i| i.steps().first() == Some(n)).count();
            (c > 0).then(|| format!("{l} {c}"))
        })
        .collect();
    let _ = write!(
        out,
        "\n<details open>\n<summary><b>What's left</b>: {} items, by next step: {}</summary>\n\n\
         The order these land in, and what done means, is in \
         [ROADMAP › Endgame](ROADMAP.md#endgame).\n\n\
         | ID | Item | Milestone | Needs, in order | Note | PRs |\n|---|---|---|---|---|---|\n",
        open.len(),
        counts.join(", "),
    );
    for i in open {
        let steps: Vec<&str> = i.steps().into_iter().map(label).collect();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} |",
            escape(&i.id),
            escape(&i.title),
            escape(&i.milestone),
            steps.join(", then "),
            i.note.as_deref().map(escape).unwrap_or_default(),
            prs(i)
        );
    }
    out.push_str("\n</details>\n");
}

/// Reads the data file, if there is one.
fn read(root: &Path) -> Result<Option<String>> {
    let path = root.join(DATA);
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read_to_string(&path)
        .map(Some)
        .with_context(|| format!("reading {DATA}"))
}

/// What the generator would write to `README.md` (path, line of the block, new text),
/// when it differs from what is committed, and the problems that stop it.
pub(crate) fn generate(root: &Path, docs: &[Doc]) -> (Option<(String, usize, String)>, Vec<Diag>) {
    let problem = |path: &str, msg: String| Diag {
        path: path.to_owned(),
        line: 1,
        msg,
        warning: false,
    };
    let target = docs.iter().find(|d| d.path == TARGET);
    let has_block = target.is_some_and(|d| d.text.lines().any(|l| l.trim_end() == begin(BLOCK)));
    let text = match read(root) {
        Ok(Some(t)) => t,
        Ok(None) if has_block => {
            return (
                None,
                vec![problem(TARGET, format!("the `{BLOCK}` block needs {DATA}"))],
            );
        }
        Ok(None) => return (None, Vec::new()),
        Err(e) => return (None, vec![problem(DATA, format!("{e:#}"))]),
    };
    let p = match parse(&text) {
        Ok(p) => p,
        Err(errs) => return (None, errs.into_iter().map(|m| problem(DATA, m)).collect()),
    };
    let Some(doc) = target else {
        return (
            None,
            vec![problem(DATA, format!("{TARGET} is not tracked"))],
        );
    };
    match with_block(doc, BLOCK, &render(&p)) {
        Ok((new, _)) if new == doc.text => (None, Vec::new()),
        Ok((new, line)) => (Some((TARGET.to_owned(), line, new)), Vec::new()),
        Err(msg) => (None, vec![problem(TARGET, msg)]),
    }
}

/// Drift diagnostics for `docs check` (R-DOC-006).
pub(crate) fn check(root: &Path, docs: &[Doc]) -> Vec<Diag> {
    let (file, mut out) = generate(root, docs);
    if let Some((path, line, _)) = file {
        out.push(Diag {
            path,
            line,
            msg: format!(
                "the generated `{BLOCK}` block is out of date with {DATA}: run `cargo xtask docs generate` (R-DOC-006)"
            ),
            warning: false,
        });
    }
    out
}

/// A merged PR as its squash commit on the first-parent history records it.
pub(crate) struct Merged {
    pub(crate) number: u64,
    /// The words of the subject's other parenthesised groups, such as `A5` in
    /// `feat(cli): … (A5) (#166)`.
    names: BTreeSet<String>,
}

/// Parses a squash-merge subject: its trailing `(#N)`, and the other `(…)` groups after
/// the type's scope.
fn merged_from(subject: &str) -> Option<Merged> {
    let rest = subject.trim_end().strip_suffix(')')?;
    let (head, num) = rest.rsplit_once("(#")?;
    let number = num.parse().ok()?;
    let title = head.split_once(": ").map_or(head, |(_, t)| t);
    let mut names = BTreeSet::new();
    for group in title.split('(').skip(1) {
        if let Some((inside, _)) = group.split_once(')') {
            for part in inside.split(',').map(str::trim) {
                // `(CL1 part 1)` names `CL1`: keep the whole group and its first word.
                names.extend(part.split_whitespace().next().map(str::to_owned));
                names.insert(part.to_owned());
            }
        }
    }
    Some(Merged { number, names })
}

/// The merged PRs in `git log --format=%B%x00` output: each commit's subject, and for a
/// wave's landing squash (`chore(wave): land …`), the `* <subject> (#N)` lines of its
/// body. That body is GitHub's list of every commit on the wave branch, so its lines
/// also name PRs earlier waves landed and issues builder commits end with; see
/// `merge::Commit::new` for the filtering a merged-PR list needs (#775 N4).
pub(crate) fn merged_in(log: &str) -> Vec<Merged> {
    let mut out = Vec::new();
    for message in log.split('\0') {
        let mut lines = message.trim_start_matches('\n').lines();
        let Some(subject) = lines.next() else {
            continue;
        };
        out.extend(merged_from(subject));
        if subject.starts_with("chore(wave): land ") {
            out.extend(
                lines
                    .filter_map(|l| l.strip_prefix("* "))
                    .filter_map(merged_from),
            );
        }
    }
    out
}

/// What the merged history says is out of date in `p`, as Markdown list lines.
fn stale(p: &Progress, merged: &[Merged]) -> Vec<String> {
    let numbers: BTreeSet<u64> = merged.iter().map(|m| m.number).collect();
    let mut out = Vec::new();
    for i in &p.items {
        // A `planned` item may list the merged first part of work still to come.
        if i.status == "review" && i.prs.iter().all(|n| numbers.contains(n)) {
            out.push(format!(
                "- **{}** is `{}`, but all its PRs ({}) are merged: set `done`, or `hardware` with a note.",
                i.id,
                i.status,
                prs(i)
            ));
        }
        if matches!(i.status.as_str(), "planned" | "review") && i.needs.is_empty() {
            out.push(format!(
                "- **{}** is `{}` but has no `needs`: list the steps it still needs.",
                i.id, i.status
            ));
        }
        if i.status == "done" && !i.needs.is_empty() {
            out.push(format!(
                "- **{}** is `done` but still lists `needs`: remove it.",
                i.id
            ));
        }
        if i.merged() {
            let open: Vec<String> = i
                .prs
                .iter()
                .filter(|n| !numbers.contains(n))
                .map(|n| format!("#{n}"))
                .collect();
            if !open.is_empty() {
                out.push(format!(
                    "- **{}** is `{}`, but {} is not merged.",
                    i.id,
                    i.status,
                    open.join(", ")
                ));
            }
        }
        let unlisted: Vec<String> = merged
            .iter()
            .filter(|m| m.names.contains(&i.id) && !i.prs.contains(&m.number))
            .map(|m| format!("#{}", m.number))
            .collect();
        if !unlisted.is_empty() {
            out.push(format!(
                "- **{}**: merged PRs that name it are not in its `prs`: {}.",
                i.id,
                unlisted.join(", ")
            ));
        }
    }
    out
}

/// `docs progress`: the stale-entry report, in Markdown, for the weekly sweep.
pub(crate) fn report(root: &Path) -> Result<String> {
    let Some(text) = read(root)? else {
        bail!("{DATA} does not exist");
    };
    let p = parse(&text).map_err(|errs| anyhow::anyhow!("{DATA}: {}", errs.join("; ")))?;
    let shallow = Command::new("git")
        .args(["rev-parse", "--is-shallow-repository"])
        .current_dir(root)
        .output()
        .context("running `git rev-parse`")?;
    let log = Command::new("git")
        .args(["log", "--first-parent", "--format=%B%x00", "HEAD"])
        .current_dir(root)
        .output()
        .context("running `git log`")?;
    if !log.status.success() {
        bail!("`git log` failed: {}", String::from_utf8_lossy(&log.stderr));
    }
    let merged = merged_in(&String::from_utf8_lossy(&log.stdout));
    let mut out = String::new();
    if String::from_utf8_lossy(&shallow.stdout).trim() == "true" {
        out.push_str("Shallow clone: PRs merged before its first commit look unmerged.\n\n");
    }
    let lines = stale(&p, &merged);
    if lines.is_empty() {
        let _ = writeln!(
            out,
            "Every entry in `{DATA}` agrees with the merged history."
        );
    } else {
        let _ = writeln!(
            out,
            "Update these entries in `{DATA}`, then run `cargo xtask docs generate` (docs/DOCS.md#generated)."
        );
        out.push('\n');
        for l in lines {
            out.push_str(&l);
            out.push('\n');
        }
    }
    let by_milestone: BTreeMap<&str, usize> = p.items.iter().fold(BTreeMap::new(), |mut m, i| {
        *m.entry(i.milestone.as_str()).or_default() += 1;
        m
    });
    let _ = writeln!(
        out,
        "\n{} items in {} milestones.",
        p.items.len(),
        by_milestone.len()
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::parse::parse as parse_doc;

    const DATA_OK: &str = r#"
[[milestone]]
id = "M1"
title = "First"
roadmap = "ROADMAP.md#m1"

[[item]]
id = "A1"
milestone = "M1"
title = "Done thing"
status = "done"
prs = [10, 11]

[[item]]
id = "A2"
milestone = "M1"
title = "Box | pipe"
status = "hardware"
prs = [12]
note = "Run on Windows"

[[item]]
id = "A3"
milestone = "M1"
title = "Open"
status = "review"
prs = [13]
needs = ["build", "hardware"]

[[item]]
id = "A4"
milestone = "M1"
title = "Later"
status = "planned"
needs = ["maintainer"]
"#;

    #[test]
    fn renders_summary_and_items() {
        let p = parse(DATA_OK).expect("invariant: valid");
        let r = render(&p);
        assert!(
            r.contains("| [M1: First](ROADMAP.md#m1) | `█████░░░░░` 50% | 2 | 1 | 1 |\n"),
            "{r}"
        );
        assert!(
            r.contains("2 of 4 items are merged; 1 of those still wait"),
            "{r}"
        );
        assert!(
            r.contains("<summary><b>M1</b> First: 2 of 4 merged</summary>"),
            "{r}"
        );
        assert!(
            r.contains("| A1 | Done thing | ✅ merged | #10, #11 |\n"),
            "{r}"
        );
        assert!(
            r.contains(
                "| A2 | Box \\| pipe | 🧪 merged, Windows run pending: Run on Windows | #12 |\n"
            ),
            "{r}"
        );
        assert!(r.contains("| A4 | Later | ⏳ planned |  |\n"), "{r}");
        assert!(
            r.contains(
                "<summary><b>What's left</b>: 3 items, by next step: 🔨 build 1, 🧪 Windows run 1, 👤 maintainer 1</summary>"
            ),
            "{r}"
        );
        assert!(
            r.contains("| A2 | Box \\| pipe | M1 | 🧪 Windows run | Run on Windows | #12 |\n"),
            "{r}"
        );
        assert!(
            r.contains("| A3 | Open | M1 | 🔨 build, then 🧪 Windows run |  | #13 |\n"),
            "{r}"
        );
        assert!(!r.contains("| A1 | Done thing | M1 |"), "{r}");
        let unset = parse(&DATA_OK.replace("needs = [\"maintainer\"]\n", ""))
            .expect("invariant: needs is optional");
        let r = render(&unset);
        assert!(
            r.contains("| A4 | Later | M1 | ❔ not set |  |  |\n"),
            "{r}"
        );
        assert!(
            r.contains("🧪 Windows run 1, ❔ not set 1</summary>"),
            "{r}"
        );
        let s = stale(&unset, &[]).join("\n");
        assert!(s.contains("**A4** is `planned` but has no `needs`"), "{s}");
    }

    #[test]
    fn nothing_left_once_every_item_is_done() {
        let all_done = DATA_OK
            .replace("status = \"hardware\"", "status = \"done\"")
            .replace("status = \"review\"", "status = \"done\"")
            .replace("status = \"planned\"", "status = \"done\"\nprs = [14]");
        let p = parse(&all_done).expect("invariant: valid");
        assert!(render(&p).contains("Nothing is left: every item is done."));
    }

    #[test]
    fn invalid_data_is_reported() {
        let bad = r#"
[[milestone]]
id = "M1"
title = "First"
roadmap = "ROADMAP.md#m1"

[[item]]
id = "A1"
milestone = "M9"
title = "x"
status = "doing"
prs = [0]
extra = 1

[[item]]
id = "A1"
milestone = "M1"
title = "y"
status = "hardware"
prs = [3, 3]
needs = ["build", "build"]

[[item]]
id = "A3"
milestone = "M1"
title = "w"
status = "planned"
needs = ["soon"]
"#;
        let errs = parse(bad).expect_err("invariant: invalid");
        let all = errs.join("\n");
        for needle in [
            "item `A1`: unknown key `extra`",
            "`prs` must be an array of PR numbers",
            "unknown milestone `M9`",
            "unknown status `doing`",
            "status `hardware` needs a `note`",
            "a PR is listed twice",
            "duplicate item id `A1`",
            "status `hardware` needs `hardware` in its `needs`",
            "a step is listed twice in `needs`",
            "item `A3`: unknown step `soon`",
        ] {
            assert!(all.contains(needle), "missing {needle:?} in:\n{all}");
        }
    }

    const DATA_RELEASES: &str = r#"
[[release]]
id = "a1"
version = "0.1.0-alpha.1"
goal = "First build"
tag = "v0.1.0-alpha.1"

[[release]]
id = "a2"
version = "0.1.0-alpha.2"
goal = "Agents | scenes"

[[release]]
id = "a3"
version = "0.1.0-alpha.3"
goal = "Empty so far"

[[milestone]]
id = "M1"
title = "First"
roadmap = "ROADMAP.md#m1"

[[item]]
id = "A1"
milestone = "M1"
title = "Shipped"
status = "done"
prs = [10]
release = "a1"

[[item]]
id = "A2"
milestone = "M1"
title = "Landed since"
status = "done"
prs = [11]
release = "a2"

[[item]]
id = "A3"
milestone = "M1"
title = "Gating"
status = "review"
prs = [12]
needs = ["build"]
release = "a2"
gate = true

[[item]]
id = "A4"
milestone = "M1"
title = "Someday"
status = "planned"
needs = ["build"]
"#;

    #[test]
    fn renders_releases_then_goals() {
        let p = parse(DATA_RELEASES).expect("invariant: valid");
        let r = render(&p);
        assert!(r.starts_with("### By release\n"), "{r}");
        assert!(
            r.contains("| `0.1.0-alpha.1` | First build | 📦 released as `v0.1.0-alpha.1` | `██████████` 100% | 1 | 0 | 0 |\n"),
            "{r}"
        );
        assert!(
            r.contains(
                "| `0.1.0-alpha.2` | Agents \\| scenes | 🔨 next: waits on 1 gate items, 0 merged | `█████░░░░░` 50% | 1 | 1 | 0 |\n"
            ),
            "{r}"
        );
        assert!(
            r.contains("| `0.1.0-alpha.3` | Empty so far | ⏳ later | no items | 0 | 0 | 0 |\n"),
            "{r}"
        );
        assert!(r.contains("| Not scheduled |"), "{r}");
        // The next release is open, with its unfinished items first.
        let next = r
            .find("<details open>\n<summary><b>0.1.0-alpha.2</b>: 1 of 2 merged</summary>")
            .expect("next release open");
        let gating = r[next..]
            .find("| A3 | Gating | 🚧 | M1 |")
            .expect("A3 listed");
        let landed = r[next..].find("| A2 | Landed since |").expect("A2 listed");
        assert!(gating < landed, "{r}");
        assert!(
            r.contains("<details>\n<summary><b>0.1.0-alpha.1</b>: 1 of 1 merged</summary>"),
            "{r}"
        );
        assert!(r.find("### By goal").expect("goals") > next, "{r}");
        assert!(r.contains("| [M1: First](ROADMAP.md#m1) |"), "{r}");
    }

    #[test]
    fn release_data_is_checked() {
        let bad = r#"
[[release]]
id = "a2"
version = "0.1.0-alpha.2"
goal = "Next"

[[release]]
id = "a1"
version = "0.1.0-alpha.1"
goal = "Old"
tag = "v0.1.0-alpha.1"

[[milestone]]
id = "M1"
title = "First"
roadmap = "ROADMAP.md#m1"

[[item]]
id = "A1"
milestone = "M1"
title = "x"
status = "review"
prs = [1]
needs = ["build"]
release = "a1"

[[item]]
id = "A2"
milestone = "M1"
title = "y"
status = "done"
prs = [2]
release = "a9"

[[item]]
id = "A3"
milestone = "M1"
title = "z"
status = "done"
prs = [3]
gate = true
"#;
        let errs = parse(bad).expect_err("invariant: invalid");
        let all = errs.join("\n");
        for needle in [
            "release `a1`: a tagged release must come before every untagged one",
            "item `A1`: release `a1` has shipped, so the item must be merged",
            "item `A2`: unknown release `a9`",
            "item `A3`: `gate` needs the `release`",
        ] {
            assert!(all.contains(needle), "missing {needle:?} in:\n{all}");
        }
    }

    #[test]
    fn block_is_replaced_and_regeneration_is_stable() {
        let readme = "<!-- @doc\nid = \"README\"\ntitle = \"R\"\nkind = \"normative\"\nstatus = \"active\"\nreview_every = \"90d\"\n-->\n\n# R\n\n<!-- @generated begin progress -->\nold\n<!-- @generated end progress -->\n\nAfter.\n";
        let doc = parse_doc("README.md", readme, &mut Vec::new());
        let p = parse(DATA_OK).expect("invariant: valid");
        let (new, line) = with_block(&doc, BLOCK, &render(&p)).expect("invariant: has a block");
        assert_eq!(line, 11);
        assert!(!new.contains("\nold\n"));
        assert!(new.ends_with("<!-- @generated end progress -->\n\nAfter.\n"));
        let again = parse_doc("README.md", &new, &mut Vec::new());
        let (twice, _) = with_block(&again, BLOCK, &render(&p)).expect("invariant: has a block");
        assert_eq!(twice, new);
        let none = parse_doc("README.md", "# R\n", &mut Vec::new());
        assert!(with_block(&none, BLOCK, "x").is_err());
    }

    #[test]
    fn squash_subjects_and_stale_entries() {
        let m = merged_from("feat(cli): protocol 0.7 outputs (A5) (#166)").expect("invariant");
        assert_eq!(m.number, 166);
        assert!(m.names.contains("A5"));
        let m = merged_from("feat(audio): CLAP (CL1 part 1, ADR-0035) (#272)").expect("invariant");
        assert!(m.names.contains("CL1") && m.names.contains("ADR-0035"));
        let m =
            merged_from("feat(protocol): program scene (B10, ADR-0030) (#201)").expect("invariant");
        assert!(m.names.contains("B10") && m.names.contains("ADR-0030"));
        assert!(!m.names.contains("protocol"));
        assert!(merged_from("Merge branch 'main'").is_none());
        let landed = merged_in(
            "chore(wave): land wave/m2 (#470)\n\n* feat: a (W1) (#315)\n\nbody\n\n\
             * fix: a review round\n\n* feat: b (#466)\n\0\nfeat: c (#469)\n\n* feat: d (#1)\n\0\n",
        );
        let numbers: Vec<u64> = landed.iter().map(|m| m.number).collect();
        assert_eq!(numbers, [470, 315, 466, 469]);
        assert!(landed[1].names.contains("W1"));

        let p = parse(DATA_OK).expect("invariant: valid");
        let merged: Vec<Merged> = [
            "feat: a (A1) (#10)",
            "feat: b (#12)",
            "feat: c (A3) (#13)",
            "feat: d (A4) (#14)",
        ]
        .iter()
        .filter_map(|s| merged_from(s))
        .collect();
        let s = stale(&p, &merged).join("\n");
        assert!(
            s.contains("**A1** is `done`, but #11 is not merged."),
            "{s}"
        );
        assert!(
            s.contains("**A3** is `review`, but all its PRs (#13) are merged"),
            "{s}"
        );
        assert!(
            s.contains("**A4**: merged PRs that name it are not in its `prs`: #14."),
            "{s}"
        );
        assert!(!s.contains("**A2**"), "{s}");
        let leftover =
            parse(&DATA_OK.replace("prs = [10, 11]", "prs = [10, 11]\nneeds = [\"build\"]"))
                .expect("invariant: valid");
        let s = stale(&leftover, &merged).join("\n");
        assert!(
            s.contains("**A1** is `done` but still lists `needs`: remove it."),
            "{s}"
        );
    }
}
