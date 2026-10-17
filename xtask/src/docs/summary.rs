//! The `ui-summary` block in `docs/UI-FEATURES.md` (docs/DOCS.md#generated): for each
//! area section, how many rows its feature table has in each status, counted from the
//! table's `Status` column. `docs generate` writes it and `docs check` reports drift, a
//! row whose status is missing or not one of the five, and a block that finds no area. A
//! doc without the block is left alone.

use std::fmt::Write as _;

use super::generate::{begin, escape, outside_fences, with_block};
use super::parse::{Diag, Doc};

/// The doc that carries the block.
pub(crate) const TARGET: &str = "docs/UI-FEATURES.md";
/// The name of the generated block.
pub(crate) const BLOCK: &str = "ui-summary";
/// The statuses a row can have, in the order the summary's columns show them
/// (docs/UI-FEATURES.md#how-to-update).
const STATUSES: &[&str] = &["✅", "🟡", "⬜", "🔒", "⏸"];
/// The header cell of the column that is counted.
const STATUS_COLUMN: &str = "Status";

/// An area: a section with a feature table, and its count per status.
struct Area {
    anchor: String,
    title: String,
    counts: [usize; STATUSES.len()],
}

/// The cells of a Markdown table row, trimmed; an escaped `\|` stays inside its cell.
fn cells(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|');
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cell.push(c);
                cell.extend(chars.next());
            }
            '|' => out.push(std::mem::take(&mut cell).trim().to_owned()),
            _ => cell.push(c),
        }
    }
    if !cell.trim().is_empty() {
        out.push(cell.trim().to_owned());
    }
    out
}

/// Whether `line` is a table's delimiter row (`|---|:--:|`).
fn is_delimiter(line: &str) -> bool {
    let c = cells(line);
    !c.is_empty()
        && c.iter()
            .all(|x| !x.is_empty() && x.chars().all(|ch| matches!(ch, '-' | ':')))
}

/// Counts the rows of every table in `lines[start..stop]` that has a `Status` column.
/// Returns `None` when there is no such table, and pushes a problem (with its 0-based
/// line) for each row whose status is not a known one.
fn count(
    lines: &[&str],
    open: &[bool],
    (start, stop): (usize, usize),
    problems: &mut Vec<(usize, String)>,
) -> Option<[usize; STATUSES.len()]> {
    let mut counts = [0; STATUSES.len()];
    let mut found = false;
    let mut k = start;
    while k + 1 < stop {
        let column =
            (open[k] && lines[k].trim_start().starts_with('|') && is_delimiter(lines[k + 1]))
                .then(|| cells(lines[k]).iter().position(|c| c == STATUS_COLUMN))
                .flatten();
        let Some(column) = column else {
            k += 1;
            continue;
        };
        found = true;
        k += 2;
        while k < stop && open[k] && lines[k].trim_start().starts_with('|') {
            let row = cells(lines[k]);
            let status = row
                .get(column)
                .map_or("", |s| s.trim_end_matches('\u{fe0f}'));
            match STATUSES.iter().position(|s| *s == status) {
                Some(i) => counts[i] += 1,
                None if status.is_empty() => problems.push((
                    k,
                    format!("the row has no `{STATUS_COLUMN}` (the `{BLOCK}` block counts it)"),
                )),
                None => problems.push((
                    k,
                    format!(
                        "status `{status}` is not one of {} (the `{BLOCK}` block counts them)",
                        STATUSES.join(" ")
                    ),
                )),
            }
            k += 1;
        }
    }
    found.then_some(counts)
}

/// The areas of `doc`: every section other than the one holding the block whose body has
/// a table with a `Status` column, in document order.
fn areas(doc: &Doc, block_line: usize, problems: &mut Vec<(usize, String)>) -> Vec<Area> {
    let lines: Vec<&str> = doc.text.lines().collect();
    let open = outside_fences(&lines);
    doc.sections
        .iter()
        .filter(|s| !(s.body.0..s.body.1).contains(&block_line))
        .filter_map(|s| {
            let counts = count(&lines, &open, s.body, problems)?;
            let title = lines
                .get(s.line.wrapping_sub(1))
                .and_then(|l| l.strip_prefix("## "))
                .unwrap_or(&s.anchor)
                .trim()
                .to_owned();
            Some(Area {
                anchor: s.anchor.clone(),
                title,
                counts,
            })
        })
        .collect()
}

/// The inside of the block: one row per area, then the totals.
fn render(areas: &[Area]) -> String {
    let mut out = format!(
        "| Area | Rows | {} |\n|---|---|{}\n",
        STATUSES.join(" | "),
        "---|".repeat(STATUSES.len())
    );
    let mut total = [0; STATUSES.len()];
    for a in areas {
        for (t, c) in total.iter_mut().zip(a.counts) {
            *t += c;
        }
        let counts: Vec<String> = a.counts.iter().map(usize::to_string).collect();
        let _ = writeln!(
            out,
            "| [{}](#{}) | {} | {} |",
            escape(&a.title),
            a.anchor,
            a.counts.iter().sum::<usize>(),
            counts.join(" | ")
        );
    }
    let totals: Vec<String> = total.iter().map(|c| format!("**{c}**")).collect();
    let _ = writeln!(
        out,
        "| **Total** | **{}** | {} |",
        total.iter().sum::<usize>(),
        totals.join(" | ")
    );
    out
}

/// What the generator would write to `docs/UI-FEATURES.md` (path, line of the block, new
/// text), when it differs from what is committed, and the problems that stop it.
pub(crate) fn generate(docs: &[Doc]) -> (Option<(String, usize, String)>, Vec<Diag>) {
    let Some(doc) = docs.iter().find(|d| d.path == TARGET) else {
        return (None, Vec::new());
    };
    let lines: Vec<&str> = doc.text.lines().collect();
    let open = outside_fences(&lines);
    let b = begin(BLOCK);
    let Some(block_line) = (0..lines.len()).find(|&k| open[k] && lines[k].trim_end() == b) else {
        return (None, Vec::new());
    };
    let problem = |line: usize, msg: String| Diag {
        path: TARGET.to_owned(),
        line,
        msg,
        warning: false,
    };
    let mut found = Vec::new();
    let areas = areas(doc, block_line, &mut found);
    let mut problems: Vec<Diag> = found
        .into_iter()
        .map(|(k, msg)| problem(k + 1, msg))
        .collect();
    if areas.is_empty() {
        problems.push(problem(
            block_line + 1,
            format!("the `{BLOCK}` block found no section with a `{STATUS_COLUMN}` table to count"),
        ));
    }
    if !problems.is_empty() {
        return (None, problems);
    }
    match with_block(doc, BLOCK, &render(&areas)) {
        Ok((new, _)) if new == doc.text => (None, Vec::new()),
        Ok((new, line)) => (Some((TARGET.to_owned(), line, new)), Vec::new()),
        Err(msg) => (None, vec![problem(block_line + 1, msg)]),
    }
}

/// Drift diagnostics for `docs check` (R-DOC-006).
pub(crate) fn check(docs: &[Doc]) -> Vec<Diag> {
    let (file, mut out) = generate(docs);
    if let Some((path, line, _)) = file {
        out.push(Diag {
            path,
            line,
            msg: format!(
                "the generated `{BLOCK}` block is out of date with the area tables: run `cargo xtask docs generate` (R-DOC-006)"
            ),
            warning: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::parse::parse;

    const HEAD: &str = "<!-- @doc\nid = \"UI-FEATURES\"\ntitle = \"U\"\nkind = \"reference\"\nstatus = \"active\"\nreview_every = \"30d\"\n-->\n\n# U\n\n";

    fn doc(body: &str) -> Doc {
        parse(TARGET, &format!("{HEAD}{body}"), &mut Vec::new())
    }

    const SUMMARY: &str = "<a id=\"summary\"></a>\n## Summary\n<!-- @section\n-->\n\n<!-- @generated begin ui-summary -->\nold\n<!-- @generated end ui-summary -->\n\nAfter.\n\n";

    const AREAS: &str = "<a id=\"theme\"></a>\n## Theme and shared controls\n<!-- @section\n-->\n\n\
        | ID | Feature | Requirement | Status | Notes |\n|---|---|---|---|---|\n\
        | WS-01 | Dark | Dark \\| only. | ✅ | |\n| WS-02 | Fonts | Plex. | ⏸️ | Licence. |\n\
        | WS-03 | Bars | Thin. | 🟡 | |\n\n\
        <a id=\"header\"></a>\n## Header bar\n<!-- @section\n-->\n\n\
        | ID | Feature | Requirement | Status | Notes |\n|---|---|---|---|---|\n\
        | HD-01 | Menu | Rows. | ⬜ | |\n| HD-02 | Privacy | Blur. | 🔒 | E2b. |\n\n\
        ```\n| ID | Status |\n|---|---|\n| X-01 | ✅ |\n```\n\n\
        <a id=\"beyond\"></a>\n## Beyond the prototype\n<!-- @section\n-->\n\nNo table.\n";

    #[test]
    fn cells_keep_escaped_pipes() {
        assert_eq!(cells("| a | b \\| c |  |"), ["a", "b \\| c", ""]);
        assert!(is_delimiter("|---|:--:|"));
        assert!(!is_delimiter("| a | b |"));
    }

    #[test]
    fn counts_each_area_and_the_total() {
        let d = doc(&format!("{SUMMARY}{AREAS}"));
        let (file, problems) = generate(std::slice::from_ref(&d));
        assert!(problems.is_empty(), "{problems:?}");
        let (path, line, text) = file.expect("invariant: the block changes");
        assert_eq!((path.as_str(), line), (TARGET, 16));
        assert!(
            text.contains(
                "<!-- @generated begin ui-summary -->\n\
                 | Area | Rows | ✅ | 🟡 | ⬜ | 🔒 | ⏸ |\n|---|---|---|---|---|---|---|\n\
                 | [Theme and shared controls](#theme) | 3 | 1 | 1 | 0 | 0 | 1 |\n\
                 | [Header bar](#header) | 2 | 0 | 0 | 1 | 1 | 0 |\n\
                 | **Total** | **5** | **1** | **1** | **1** | **1** | **1** |\n\
                 <!-- @generated end ui-summary -->\n\nAfter.\n"
            ),
            "{text}"
        );
        // Regenerating changes nothing.
        let again = parse(TARGET, &text, &mut Vec::new());
        assert_eq!(generate(&[again]), (None, Vec::new()));
    }

    #[test]
    fn a_doc_without_the_block_is_left_alone() {
        let d = doc(AREAS);
        assert_eq!(generate(std::slice::from_ref(&d)), (None, Vec::new()));
        assert!(check(&[d]).is_empty());
        assert_eq!(generate(&[]), (None, Vec::new()));
    }

    #[test]
    fn unknown_statuses_and_drift_are_reported() {
        let bad = doc(&format!("{SUMMARY}{}", AREAS.replace("| 🔒 |", "| done |")));
        let msgs: Vec<String> = check(&[bad])
            .into_iter()
            .map(|d| format!("{}:{}: {}", d.path, d.line, d.msg))
            .collect();
        assert_eq!(
            msgs,
            [
                "docs/UI-FEATURES.md:41: status `done` is not one of ✅ 🟡 ⬜ 🔒 ⏸ (the `ui-summary` block counts them)"
            ]
        );
        let short = doc(&format!(
            "{SUMMARY}{}",
            AREAS.replace("| HD-01 | Menu | Rows. | ⬜ | |", "| HD-01 | Menu |")
        ));
        let msgs: Vec<String> = check(&[short]).into_iter().map(|d| d.msg).collect();
        assert_eq!(
            msgs,
            ["the row has no `Status` (the `ui-summary` block counts it)"]
        );
        let stale = doc(&format!("{SUMMARY}{AREAS}"));
        let msgs: Vec<String> = check(&[stale]).into_iter().map(|d| d.msg).collect();
        assert_eq!(
            msgs,
            [
                "the generated `ui-summary` block is out of date with the area tables: run `cargo xtask docs generate` (R-DOC-006)"
            ]
        );
        let empty = doc(SUMMARY);
        let msgs: Vec<String> = check(&[empty]).into_iter().map(|d| d.msg).collect();
        assert_eq!(
            msgs,
            ["the `ui-summary` block found no section with a `Status` table to count"]
        );
        let open = doc(&format!(
            "{}{AREAS}",
            SUMMARY.replace("<!-- @generated end ui-summary -->\n", "")
        ));
        let msgs: Vec<String> = check(&[open]).into_iter().map(|d| d.msg).collect();
        assert_eq!(
            msgs,
            ["the `ui-summary` block is not closed by `<!-- @generated end ui-summary -->`"]
        );
    }
}
