//! Parses one tracked Markdown doc into its metadata, anchors, sections, rules, links and
//! fact markers. Structure (anchors, headings, metadata blocks) is scanned line by line, as
//! the format in docs/DOCS.md is strict; links come from a CommonMark parser so that text
//! inside code spans and fenced blocks is never mistaken for a reference.

use pulldown_cmark::{Event, Parser, Tag};

/// A problem found in a doc, reported as `path:line: message`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Diag {
    pub(crate) path: String,
    pub(crate) line: usize,
    pub(crate) msg: String,
}

/// A `##` section with its anchor and `@section` metadata.
#[derive(Debug)]
pub(crate) struct Section {
    pub(crate) anchor: String,
    pub(crate) line: usize,
    pub(crate) meta: Option<toml::Table>,
}

/// A `### R-…` rule with its `@rule` metadata.
#[derive(Debug)]
pub(crate) struct Rule {
    pub(crate) id: String,
    pub(crate) line: usize,
    pub(crate) meta: Option<toml::Table>,
}

/// A Markdown link destination, outside code.
#[derive(Debug)]
pub(crate) struct Link {
    pub(crate) dest: String,
    pub(crate) line: usize,
}

/// A `value<!--fact:id-->` marker: the fact id and the text on its line before the marker.
#[derive(Debug)]
pub(crate) struct FactRef {
    pub(crate) id: String,
    pub(crate) before: String,
    pub(crate) line: usize,
}

/// One parsed doc.
#[derive(Debug)]
pub(crate) struct Doc {
    pub(crate) path: String,
    pub(crate) meta: Option<toml::Table>,
    pub(crate) anchors: Vec<(String, usize)>,
    pub(crate) sections: Vec<Section>,
    pub(crate) rules: Vec<Rule>,
    pub(crate) links: Vec<Link>,
    pub(crate) facts: Vec<FactRef>,
    pub(crate) bare_urls: Vec<(String, usize)>,
}

impl Doc {
    /// Whether the doc's metadata declares `kind = "adr"`.
    pub(crate) fn is_adr(&self) -> bool {
        self.meta_str("kind") == Some("adr")
    }

    /// A string value from the `@doc` block.
    pub(crate) fn meta_str(&self, key: &str) -> Option<&str> {
        self.meta.as_ref()?.get(key)?.as_str()
    }

    /// Whether `anchor` is defined in this doc.
    pub(crate) fn has_anchor(&self, anchor: &str) -> bool {
        self.anchors.iter().any(|(a, _)| a == anchor)
    }
}

/// Parses `text` (the contents of `path`). Problems found while parsing go to `diags`.
pub(crate) fn parse(path: &str, text: &str, diags: &mut Vec<Diag>) -> Doc {
    let mut d = Doc {
        path: path.to_owned(),
        meta: None,
        anchors: Vec::new(),
        sections: Vec::new(),
        rules: Vec::new(),
        links: Vec::new(),
        facts: Vec::new(),
        bare_urls: Vec::new(),
    };
    let mut err = |line: usize, msg: String| {
        diags.push(Diag {
            path: path.to_owned(),
            line,
            msg,
        });
    };

    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    // The @doc block must be the very first bytes of the file (DOCS#identity).
    if text.starts_with("<!-- @doc") {
        match read_block(&lines, 0) {
            Some((body, end)) => {
                d.meta = parse_toml(&body, 1, &mut err);
                i = end + 1;
            }
            None => err(1, "unterminated `<!-- @doc` block (missing `-->`)".into()),
        }
    } else {
        err(
            1,
            "file must start with `<!-- @doc` (no BOM, blank lines or text before it)".into(),
        );
    }
    let is_adr = d.is_adr();

    let mut fence: Option<&str> = None;
    while i < lines.len() {
        let line = lines[i];
        let n = i + 1;
        let trimmed = line.trim_start();
        if let Some(f) = fence {
            if trimmed.starts_with(f) {
                fence = None;
            }
            i += 1;
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence = Some(&trimmed[..3]);
            i += 1;
            continue;
        }
        if let Some(anchor) = anchor_of(line) {
            d.anchors.push((anchor.to_owned(), n));
        }
        if let Some(title) = line.strip_prefix("## ") {
            let anchor = i.checked_sub(1).and_then(|p| anchor_of(lines[p]));
            let Some(anchor) = anchor else {
                if title.trim() != "Revision history" {
                    err(
                        n,
                        format!(
                            "`## {}` has no `<a id=\"…\"></a>` anchor on the line before it",
                            title.trim()
                        ),
                    );
                } else {
                    err(n, "revision history tables are not allowed; history comes from git (R-DOC-005)".into());
                }
                i += 1;
                continue;
            };
            let mut meta = None;
            if !is_adr {
                if lines
                    .get(i + 1)
                    .is_some_and(|l| l.starts_with("<!-- @section"))
                {
                    match read_block(&lines, i + 1) {
                        Some((body, end)) => {
                            meta = parse_toml(&body, n + 1, &mut err);
                            i = end;
                        }
                        None => err(n + 1, "unterminated `<!-- @section` block".into()),
                    }
                } else {
                    err(
                        n,
                        format!("section `{anchor}` must be followed by a `<!-- @section` block"),
                    );
                }
            }
            d.sections.push(Section {
                anchor: anchor.to_owned(),
                line: n,
                meta,
            });
        } else if let Some(rest) = line.strip_prefix("### R-") {
            let id = format!("R-{}", rest.split(':').next().unwrap_or_default().trim());
            if i.checked_sub(1).and_then(|p| anchor_of(lines[p])) != Some(id.as_str()) {
                err(
                    n,
                    format!("rule `{id}` must be preceded by `<a id=\"{id}\"></a>`"),
                );
            }
            let mut meta = None;
            if lines
                .get(i + 1)
                .is_some_and(|l| l.starts_with("<!-- @rule"))
            {
                match read_block(&lines, i + 1) {
                    Some((body, end)) => {
                        meta = parse_toml(&body, n + 1, &mut err);
                        i = end;
                    }
                    None => err(n + 1, "unterminated `<!-- @rule` block".into()),
                }
            } else {
                err(
                    n,
                    format!("rule `{id}` must be followed by a `<!-- @rule` block"),
                );
            }
            d.rules.push(Rule { id, line: n, meta });
        }
        i += 1;
    }
    if fence.is_some() {
        err(lines.len(), "unterminated code fence".into());
    }

    scan_inline(text, &mut d);
    d
}

/// The id of an `<a id="…"></a>` line, if `line` is one.
fn anchor_of(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("<a id=\"")?
        .strip_suffix("\"></a>")
}

/// Reads a metadata block starting at `start` (the `<!-- @kind` line). Returns the TOML body
/// and the index of the closing `-->` line.
fn read_block(lines: &[&str], start: usize) -> Option<(String, usize)> {
    let mut body = String::new();
    for (k, l) in lines.iter().enumerate().skip(start + 1) {
        if l.trim() == "-->" {
            return Some((body, k));
        }
        body.push_str(l);
        body.push('\n');
    }
    None
}

fn parse_toml(body: &str, line: usize, err: &mut impl FnMut(usize, String)) -> Option<toml::Table> {
    match body.parse::<toml::Table>() {
        Ok(t) => Some(t),
        Err(e) => {
            err(line, format!("metadata is not valid TOML: {}", e.message()));
            None
        }
    }
}

/// Collects links, fact markers and bare URLs, skipping code.
fn scan_inline(text: &str, d: &mut Doc) {
    let line_of = |off: usize| text[..off].bytes().filter(|&b| b == b'\n').count() + 1;
    for (ev, range) in Parser::new(text).into_offset_iter() {
        match ev {
            Event::Start(Tag::Link { dest_url, .. }) => {
                d.links.push(Link {
                    dest: dest_url.to_string(),
                    line: line_of(range.start),
                });
            }
            Event::InlineHtml(html) | Event::Html(html) => {
                if let Some(id) = html
                    .trim()
                    .strip_prefix("<!--fact:")
                    .and_then(|s| s.strip_suffix("-->"))
                {
                    let line_start = text[..range.start].rfind('\n').map_or(0, |p| p + 1);
                    d.facts.push(FactRef {
                        id: id.to_owned(),
                        before: text[line_start..range.start].to_owned(),
                        line: line_of(range.start),
                    });
                }
            }
            Event::Text(t) => {
                for word in t.split_whitespace() {
                    if word.starts_with("http://") || word.starts_with("https://") {
                        d.bare_urls.push((word.to_owned(), line_of(range.start)));
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> (Doc, Vec<Diag>) {
        let mut diags = Vec::new();
        let d = parse("t.md", text, &mut diags);
        (d, diags)
    }

    const HEAD: &str = "<!-- @doc\nid = \"T\"\ntitle = \"T\"\nkind = \"normative\"\nstatus = \"active\"\nreview_every = \"90d\"\n-->\n";

    #[test]
    fn parses_doc_section_rule_and_links() {
        let text = format!(
            "{HEAD}\n# T\n\n<a id=\"s\"></a>\n## Sec\n<!-- @section\ncovers = [\"a/**\"]\n-->\n\nSee [x](other.md#y) and `[no](code.md)`.\n\n<a id=\"R-X-001\"></a>\n### R-X-001: Thing\n<!-- @rule\nstatus = \"active\"\n-->\nbuild 22000<!--fact:min-windows-build-->\n"
        );
        let (d, diags) = p(&text);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(d.meta_str("id"), Some("T"));
        assert_eq!(d.sections.len(), 1);
        assert_eq!(d.sections[0].anchor, "s");
        assert!(d.sections[0].meta.as_ref().unwrap().contains_key("covers"));
        assert_eq!(d.rules[0].id, "R-X-001");
        assert_eq!(d.links.len(), 1, "link inside code must be ignored");
        assert_eq!(d.links[0].dest, "other.md#y");
        assert_eq!(d.facts[0].id, "min-windows-build");
        assert!(d.facts[0].before.ends_with("22000"));
    }

    #[test]
    fn requires_doc_block_first() {
        let (_, diags) = p(&format!("\n{HEAD}"));
        assert!(diags.iter().any(|d| d.msg.contains("must start with")));
    }

    #[test]
    fn heading_without_anchor_or_section_block() {
        let (_, diags) = p(&format!(
            "{HEAD}\n## Lonely\n\n<a id=\"x\"></a>\n## Bare\ntext\n"
        ));
        assert!(diags.iter().any(|d| d.msg.contains("no `<a id")));
        assert!(
            diags
                .iter()
                .any(|d| d.msg.contains("`<!-- @section` block"))
        );
    }

    #[test]
    fn headings_inside_fences_are_ignored() {
        let (d, diags) = p(&format!("{HEAD}\n```markdown\n## Not a section\n```\n"));
        assert!(diags.is_empty(), "{diags:?}");
        assert!(d.sections.is_empty());
    }

    #[test]
    fn revision_history_is_rejected() {
        let (_, diags) = p(&format!("{HEAD}\n## Revision history\n"));
        assert!(diags.iter().any(|d| d.msg.contains("R-DOC-005")));
    }

    #[test]
    fn bad_toml_is_reported() {
        let (_, diags) = p("<!-- @doc\nid = \n-->\n");
        assert!(diags.iter().any(|d| d.msg.contains("not valid TOML")));
    }

    #[test]
    fn bare_urls_are_found_outside_code() {
        let (d, _) = p(&format!(
            "{HEAD}\nsee https://example.com now, not `https://code.example`\n"
        ));
        assert_eq!(d.bare_urls.len(), 1);
    }
}
