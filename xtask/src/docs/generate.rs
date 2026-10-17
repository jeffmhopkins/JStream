//! Generated content derived from ADR metadata (docs/DOCS.md#generated, ADR-0038): the
//! `adr-status` banner under every ADR's title, and the ADR index `docs/adr/README.md`.
//! `docs generate` writes both; `docs check` regenerates them in memory and reports drift.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Component, Path};

use super::parse::{Diag, Doc};

/// The name of the status banner block in every ADR.
pub(crate) const STATUS_BLOCK: &str = "adr-status";
/// The name of the table block in the ADR index.
pub(crate) const INDEX_BLOCK: &str = "adr-index";
/// The generated ADR index.
pub(crate) const INDEX_PATH: &str = "docs/adr/README.md";
/// Where ADRs live; only these are listed in the index.
const ADR_DIR: &str = "docs/adr/";

/// The line that opens a generated block.
pub(crate) fn begin(name: &str) -> String {
    format!("<!-- @generated begin {name} -->")
}

/// The line that closes a generated block.
pub(crate) fn end(name: &str) -> String {
    format!("<!-- @generated end {name} -->")
}

/// `to` as a link relative to the directory of `from` (both repo paths).
fn relative(from: &str, to: &str) -> String {
    let dir: Vec<Component<'_>> = Path::new(from)
        .parent()
        .map(|p| p.components().collect())
        .unwrap_or_default();
    let target: Vec<Component<'_>> = Path::new(to).components().collect();
    let common = dir.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".to_owned(); dir.len() - common];
    parts.extend(
        target[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// Text that is safe inside a Markdown link label or table cell.
pub(crate) fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('|', "\\|")
}

/// The number in an `ADR-NNNN` id.
fn number(doc: &Doc) -> Option<u32> {
    doc.meta_str("id")?.strip_prefix("ADR-")?.parse().ok()
}

/// The ADR that `doc`'s `superseded_by` names, or why it can't be used.
fn successor<'d>(doc: &Doc, by_path: &BTreeMap<&str, &'d Doc>) -> Result<&'d Doc, String> {
    let target = doc
        .meta_str("superseded_by")
        .ok_or("a superseded ADR needs `superseded_by`")?;
    match by_path.get(target) {
        Some(d) if d.is_adr() && d.meta_str("id").is_some() => Ok(d),
        _ => Err(format!(
            "`superseded_by` `{target}` is not a tracked ADR (a repo path such as `docs/adr/NNNN-….md`)"
        )),
    }
}

/// The blockquote line of `doc`'s status banner.
fn status_line(doc: &Doc, by_path: &BTreeMap<&str, &Doc>) -> Result<String, String> {
    match doc.meta_str("status") {
        Some("superseded") => {
            let next = successor(doc, by_path)?;
            let id = next.meta_str("id").unwrap_or_default();
            let title = next.meta_str("title").unwrap_or_default();
            Ok(format!(
                "> **Superseded** by [{}]({}). This decision no longer applies.",
                escape(&format!("{id}: {title}")),
                relative(&doc.path, &next.path)
            ))
        }
        Some(status) => Ok(format!("> **Status:** {status}")),
        None => Err("the `@doc` block has no `status`".into()),
    }
}

/// Lines of `text` that are not inside a fenced code block, as 0-based indices.
pub(crate) fn outside_fences(lines: &[&str]) -> Vec<bool> {
    let mut fence: Option<&str> = None;
    lines
        .iter()
        .map(|l| {
            let t = l.trim_start();
            if let Some(f) = fence {
                if t.starts_with(f) {
                    fence = None;
                }
                return false;
            }
            if t.starts_with("```") || t.starts_with("~~~") {
                fence = Some(&t[..3]);
                return false;
            }
            true
        })
        .collect()
}

/// Removes every `name` block outside code fences, from its begin line through its end
/// line, and one of the two blank lines that then meet. With `strict`, only blocks whose
/// inside is exactly one `> ` line are removed: the shape the generator writes.
fn strip_lines(lines: &mut Vec<&str>, name: &str, strict: bool) {
    let (b, e) = (begin(name), end(name));
    let mut open = outside_fences(lines);
    let mut i = 0;
    while i < lines.len() {
        let close = (open[i] && lines[i].trim_end() == b)
            .then(|| (i + 1..lines.len()).find(|&k| lines[k].trim_end() == e))
            .flatten()
            .filter(|&k| !strict || (k == i + 2 && lines[i + 1].starts_with("> ")));
        let Some(k) = close else {
            i += 1;
            continue;
        };
        lines.drain(i..=k);
        if i > 0 && i < lines.len() && lines[i].trim().is_empty() && lines[i - 1].trim().is_empty()
        {
            lines.remove(i);
        }
        open = outside_fences(lines);
    }
}

/// Joins lines, keeping `like`'s final newline.
pub(crate) fn join(lines: &[&str], like: &str) -> String {
    let mut out = lines.join("\n");
    if like.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// `doc`'s text with the inside of its `name` block replaced by `body`, and the 1-based
/// line of the begin marker. Used by the generators that own one block in a hand-written
/// doc (the README's progress table, the UI features summary).
pub(crate) fn with_block(doc: &Doc, name: &str, body: &str) -> Result<(String, usize), String> {
    let lines: Vec<&str> = doc.text.lines().collect();
    let open = outside_fences(&lines);
    let (b, e) = (begin(name), end(name));
    let start = (0..lines.len())
        .find(|&k| open[k] && lines[k].trim_end() == b)
        .ok_or_else(|| format!("no `{b}` line to generate the `{name}` block into"))?;
    let stop = (start + 1..lines.len())
        .find(|&k| lines[k].trim_end() == e)
        .ok_or_else(|| format!("the `{name}` block is not closed by `{e}`"))?;
    let mut out: Vec<&str> = lines[..=start].to_vec();
    out.extend(body.lines());
    out.extend(&lines[stop..]);
    Ok((join(&out, &doc.text), start + 1))
}

/// `text` without its well-formed `adr-status` blocks: what R-DOC-009 compares in an
/// accepted ADR (ADR-0038). Used by the governance check.
pub(crate) fn strip_status(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    strip_lines(&mut lines, STATUS_BLOCK, true);
    join(&lines, text)
}

/// `doc`'s text with its status banner regenerated: any `adr-status` block removed, then
/// one written under the `# ` title line, between single blank lines.
fn with_status(doc: &Doc, line: &str) -> Result<String, String> {
    let mut lines: Vec<&str> = doc.text.lines().collect();
    strip_lines(&mut lines, STATUS_BLOCK, false);
    let open = outside_fences(&lines);
    let title = (doc.content_start..lines.len())
        .find(|&k| open[k] && lines[k].starts_with("# "))
        .ok_or("no `# ` title line to put the status banner under")?;
    let rest = (title + 1..lines.len())
        .find(|&k| !lines[k].trim().is_empty())
        .unwrap_or(lines.len());
    let (b, e) = (begin(STATUS_BLOCK), end(STATUS_BLOCK));
    let mut out: Vec<&str> = lines[..=title].to_vec();
    out.extend(["", &b, line, &e]);
    if rest < lines.len() {
        out.push("");
        out.extend(&lines[rest..]);
    }
    Ok(join(&out, &doc.text))
}

/// The full text of `docs/adr/README.md`, listing `adrs` (sorted by number).
fn index(adrs: &[&Doc], by_path: &BTreeMap<&str, &Doc>) -> String {
    let mut out = String::from(
        "<!-- @doc\nid = \"ADR-INDEX\"\ntitle = \"Architecture decision records\"\nkind = \"generated\"\nstatus = \"active\"\n-->\n\n\
         # Architecture decision records\n\n\
         Every decision recorded in `docs/adr/`, with its status. This file is generated from each\n\
         ADR's `@doc` block by `cargo xtask docs generate`; never edit it by hand\n\
         ([DOCS › Generated content](../DOCS.md#generated)). New ADRs start from\n\
         [`TEMPLATE.md`](TEMPLATE.md).\n\n",
    );
    out.push_str(&begin(INDEX_BLOCK));
    out.push_str("\n| ADR | Title | Status | Tier | Superseded by |\n|---|---|---|---|---|\n");
    for d in adrs {
        let id = d.meta_str("id").unwrap_or_default();
        let next = match successor(d, by_path) {
            Ok(n) if d.meta_str("status") == Some("superseded") => format!(
                "[{}]({})",
                escape(n.meta_str("id").unwrap_or_default()),
                relative(INDEX_PATH, &n.path)
            ),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "| [{}]({}) | {} | {} | {} | {} |",
            escape(id),
            relative(INDEX_PATH, &d.path),
            escape(d.meta_str("title").unwrap_or_default()),
            d.meta_str("status").unwrap_or_default(),
            d.meta_str("tier").unwrap_or("—"),
            next
        );
    }
    out.push_str(&end(INDEX_BLOCK));
    out.push('\n');
    out
}

/// What the generator would write.
pub(crate) struct Generated {
    /// Every file whose generated text differs from what is committed: its path, the
    /// 1-based line to report drift at (`None`: not a tracked doc yet), and the new text.
    pub(crate) files: Vec<(String, Option<usize>, String)>,
    /// Problems that stop a file from being generated.
    pub(crate) problems: Vec<Diag>,
}

/// Regenerates every banner and the index from `docs`.
pub(crate) fn generate(docs: &[Doc]) -> Generated {
    let by_path: BTreeMap<&str, &Doc> = docs.iter().map(|d| (d.path.as_str(), d)).collect();
    let mut g = Generated {
        files: Vec::new(),
        problems: Vec::new(),
    };
    let mut adrs: Vec<&Doc> = Vec::new();
    for d in docs.iter().filter(|d| d.is_adr()) {
        if d.path.starts_with(ADR_DIR) {
            adrs.push(d);
        }
        match status_line(d, &by_path).and_then(|l| with_status(d, &l)) {
            Ok(text) if text == d.text => {}
            Ok(text) => {
                let line = d
                    .text
                    .lines()
                    .position(|l| l.starts_with("# "))
                    .map_or(1, |k| k + 1);
                g.files.push((d.path.clone(), Some(line), text));
            }
            Err(msg) => g.problems.push(Diag {
                path: d.path.clone(),
                line: 1,
                msg: format!("cannot generate the `{STATUS_BLOCK}` banner: {msg}"),
                warning: false,
            }),
        }
    }
    adrs.sort_by_key(|d| (number(d).unwrap_or(u32::MAX), d.path.clone()));
    let existing = by_path.get(INDEX_PATH);
    if !adrs.is_empty() || existing.is_some() {
        let text = index(&adrs, &by_path);
        if existing.is_none_or(|d| d.text != text) {
            g.files
                .push((INDEX_PATH.to_owned(), existing.map(|_| 1), text));
        }
    }
    g
}

/// Drift diagnostics for `docs check`: generated content that differs from what is
/// committed (R-DOC-006).
pub(crate) fn check(docs: &[Doc]) -> Vec<Diag> {
    let g = generate(docs);
    let mut out = g.problems;
    for (path, line, _) in g.files {
        let msg = if path == INDEX_PATH {
            match line {
                Some(_) => "the ADR index is out of date",
                None => "the ADR index is missing (or not tracked by git)",
            }
        } else {
            "the generated `adr-status` banner is missing or out of date"
        };
        out.push(Diag {
            path,
            line: line.unwrap_or(1),
            msg: format!("{msg}: run `cargo xtask docs generate` (R-DOC-006, ADR-0038)"),
            warning: false,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::parse::parse;

    fn adr(path: &str, id: &str, status: &str, extra: &str, body: &str) -> Doc {
        let text = format!(
            "<!-- @doc\nid = \"{id}\"\ntitle = \"Title {id}\"\nkind = \"adr\"\nstatus = \"{status}\"\n{extra}-->\n\n# {id}: Title\n\n{body}"
        );
        parse(path, &text, &mut Vec::new())
    }

    const BODY: &str = "<a id=\"context\"></a>\n## Context\nWhy.\n";

    #[test]
    fn relative_links() {
        assert_eq!(relative("docs/adr/a.md", "docs/adr/b.md"), "b.md");
        assert_eq!(relative("docs/adr/README.md", "docs/DOCS.md"), "../DOCS.md");
        assert_eq!(relative("docs/adr/a.md", "x/b.md"), "../../x/b.md");
    }

    #[test]
    fn banners_for_each_status() {
        let docs = vec![
            adr(
                "docs/adr/0001-a.md",
                "ADR-0001",
                "superseded",
                "superseded_by = \"docs/adr/0002-b.md\"\n",
                BODY,
            ),
            adr(
                "docs/adr/0002-b.md",
                "ADR-0002",
                "accepted",
                "tier = \"agent\"\n",
                BODY,
            ),
            adr("docs/adr/0003-c.md", "ADR-0003", "rejected", "", "Text.\n"),
        ];
        let g = generate(&docs);
        assert!(g.problems.is_empty(), "{:?}", g.problems);
        let text = |p: &str| {
            g.files
                .iter()
                .find(|(path, _, _)| path == p)
                .map(|(_, _, t)| t.clone())
                .expect("invariant: generated")
        };
        assert!(text("docs/adr/0001-a.md").ends_with(
            "# ADR-0001: Title\n\n<!-- @generated begin adr-status -->\n> **Superseded** by [ADR-0002: Title ADR-0002](0002-b.md). This decision no longer applies.\n<!-- @generated end adr-status -->\n\n<a id=\"context\"></a>\n## Context\nWhy.\n"
        ));
        assert!(text("docs/adr/0002-b.md").contains("\n\n<!-- @generated begin adr-status -->\n> **Status:** accepted\n<!-- @generated end adr-status -->\n\n<a id"));
        assert!(
            text("docs/adr/0003-c.md")
                .contains("> **Status:** rejected\n<!-- @generated end adr-status -->\n\nText.\n")
        );
        let index = text(INDEX_PATH);
        assert!(index.contains("kind = \"generated\""), "{index}");
        assert!(index.contains(
            "| [ADR-0001](0001-a.md) | Title ADR-0001 | superseded | — | [ADR-0002](0002-b.md) |\n| [ADR-0002](0002-b.md) | Title ADR-0002 | accepted | agent |  |\n"
        ), "{index}");
    }

    /// Generating is idempotent, and a status change rewrites only the banner.
    #[test]
    fn regeneration_is_stable_and_replaces_the_banner() {
        let first = adr("docs/adr/0001-a.md", "ADR-0001", "proposed", "", BODY);
        let g = generate(&[first]);
        let (_, _, once) = &g.files[0];
        let again = parse("docs/adr/0001-a.md", once, &mut Vec::new());
        let g2 = generate(std::slice::from_ref(&again));
        assert!(
            g2.files.iter().all(|(p, _, _)| p == INDEX_PATH),
            "{:?}",
            g2.files
        );
        let accepted = parse(
            "docs/adr/0001-a.md",
            &once.replace("\"proposed\"", "\"accepted\""),
            &mut Vec::new(),
        );
        let g3 = generate(std::slice::from_ref(&accepted));
        let (_, _, updated) = &g3.files[0];
        assert_eq!(*updated, once.replace("proposed", "accepted"));
        // What R-DOC-009 compares is the ADR as it was before it had a banner.
        let bare = adr("docs/adr/0001-a.md", "ADR-0001", "accepted", "", BODY).text;
        assert_eq!(strip_status(updated), bare);
        assert_eq!(strip_status(&accepted.text), bare);
    }

    /// The strict strip used by governance leaves blocks of any other shape, and blocks in
    /// code fences, in place.
    #[test]
    fn strict_strip_only_removes_generator_shaped_blocks() {
        let odd = "# T\n\n<!-- @generated begin adr-status -->\n> **Status:** accepted\nMore.\n<!-- @generated end adr-status -->\n\nX\n";
        assert_eq!(strip_status(odd), odd);
        let fenced = "# T\n\n```\n<!-- @generated begin adr-status -->\n> q\n<!-- @generated end adr-status -->\n```\n";
        assert_eq!(strip_status(fenced), fenced);
        let good = "# T\n\n<!-- @generated begin adr-status -->\n> q\n<!-- @generated end adr-status -->\n\nX\n";
        assert_eq!(strip_status(good), "# T\n\nX\n");
    }

    #[test]
    fn drift_and_bad_successor_are_reported() {
        let docs = vec![
            adr(
                "docs/adr/0001-a.md",
                "ADR-0001",
                "superseded",
                "superseded_by = \"docs/adr/0009-gone.md\"\n",
                BODY,
            ),
            adr("docs/adr/0002-b.md", "ADR-0002", "accepted", "", BODY),
        ];
        let msgs: Vec<String> = check(&docs)
            .into_iter()
            .map(|d| format!("{}:{}: {}", d.path, d.line, d.msg))
            .collect();
        assert_eq!(
            msgs,
            vec![
                "docs/adr/0001-a.md:1: cannot generate the `adr-status` banner: `superseded_by` `docs/adr/0009-gone.md` is not a tracked ADR (a repo path such as `docs/adr/NNNN-….md`)",
                "docs/adr/0002-b.md:8: the generated `adr-status` banner is missing or out of date: run `cargo xtask docs generate` (R-DOC-006, ADR-0038)",
                "docs/adr/README.md:1: the ADR index is missing (or not tracked by git): run `cargo xtask docs generate` (R-DOC-006, ADR-0038)",
            ]
        );
    }

    /// GitHub renders the banner as a blockquote, and the end marker stays outside it.
    #[test]
    fn banner_renders_as_a_blockquote() {
        use pulldown_cmark::{Event, Parser, Tag, TagEnd};
        let docs = [adr("docs/adr/0001-a.md", "ADR-0001", "accepted", "", BODY)];
        let (_, _, text) = &generate(&docs).files[0];
        let events: Vec<Event<'_>> = Parser::new(text).collect();
        let quote = events
            .iter()
            .position(|e| matches!(e, Event::Start(Tag::BlockQuote(_))))
            .expect("invariant: a blockquote");
        let quote_end = events
            .iter()
            .position(|e| matches!(e, Event::End(TagEnd::BlockQuote(_))))
            .expect("invariant: closed");
        let end_marker = events
            .iter()
            .position(|e| matches!(e, Event::Html(h) if h.contains("@generated end")))
            .expect("invariant: the end marker is an HTML block");
        assert!(quote < quote_end && quote_end < end_marker);
    }
}
