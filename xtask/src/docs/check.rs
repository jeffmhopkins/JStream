//! Cross-document checks over the parsed docs (docs/DOCS.md#gate). This module covers
//! identity, metadata schema, references, rules, facts, hash-bound verification and glob
//! coverage. Code back-references live in `backrefs.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use super::hash::{self, Files};
use super::parse::{Diag, Doc};

const DOC_KEYS: &[&str] = &[
    "id",
    "title",
    "kind",
    "status",
    "review_every",
    "reviewed",
    "replaced_by",
    "retired_sections",
    "tier",
    "superseded_by",
];
const SECTION_KEYS: &[&str] = &["covers", "depends_on", "verified"];
const RULE_KEYS: &[&str] = &["status", "enforced_by", "applies_to", "replaced_by"];
const KINDS: &[&str] = &[
    "normative",
    "adr",
    "guide",
    "plan",
    "reference",
    "generated",
];
const DOC_STATUSES: &[&str] = &["active", "draft", "retired"];
const ADR_STATUSES: &[&str] = &["proposed", "accepted", "superseded", "rejected"];
const EXT_STATUSES: &[&str] = &["ok", "unverified", "broken"];
/// ADRs numbered from here on must declare a decision `tier` (ADR-0017).
const TIER_REQUIRED_FROM: u32 = 17;

/// Everything the cross-doc checks need, loaded from the repository.
pub(crate) struct Repo<'a> {
    pub(crate) root: &'a Path,
    pub(crate) docs: &'a [Doc],
    /// `docs/external.toml`, parsed.
    pub(crate) external: toml::Table,
    /// `docs/facts.toml`, parsed.
    pub(crate) facts: toml::Table,
    /// Raw text of `docs/RULES.md`, for the R-GIT-003 check table.
    pub(crate) rules_md: String,
    /// Every tracked file, for `covers` globs and hashing.
    pub(crate) files: Files<'a>,
}

/// Runs every check; returns the diagnostics found.
pub(crate) fn check(repo: &Repo<'_>) -> Vec<Diag> {
    let mut out = Vec::new();
    let by_path: BTreeMap<&str, &Doc> = repo.docs.iter().map(|d| (d.path.as_str(), d)).collect();
    let ext_urls = check_external(repo, &mut out);
    let facts = check_facts_registry(repo, &mut out);
    let ci_checks = ci_check_names(&repo.rules_md);
    if ci_checks.is_empty() {
        out.push(diag(
            "docs/RULES.md",
            1,
            "could not find the R-GIT-003 check table".into(),
        ));
    }

    let mut ids: BTreeMap<&str, &str> = BTreeMap::new();
    for doc in repo.docs {
        check_doc_meta(doc, &mut out);
        if let Some(id) = doc.meta_str("id")
            && let Some(other) = ids.insert(id, &doc.path)
        {
            out.push(diag(
                &doc.path,
                1,
                format!("duplicate doc id `{id}` (also in {other})"),
            ));
        }
        let mut seen = BTreeSet::new();
        for (a, line) in &doc.anchors {
            if !seen.insert(a.as_str()) {
                out.push(diag(&doc.path, *line, format!("duplicate anchor `{a}`")));
            }
        }
        for s in &doc.sections {
            if let Some(meta) = &s.meta {
                check_section_meta(repo, doc, s.line, &s.anchor, meta, &by_path, &mut out);
            }
        }
        for r in &doc.rules {
            if let Some(meta) = &r.meta {
                check_rule_meta(doc, r.line, &r.id, meta, &ci_checks, &mut out);
            }
        }
        for link in &doc.links {
            check_link(
                repo, doc, &link.dest, link.line, &ext_urls, &by_path, &mut out,
            );
        }
        for (url, line) in &doc.bare_urls {
            out.push(diag(
                &doc.path,
                *line,
                format!(
                    "bare URL `{url}`: write it as a Markdown link registered in docs/external.toml"
                ),
            ));
        }
        for f in &doc.facts {
            match facts.get(f.id.as_str()) {
                None => out.push(diag(&doc.path, f.line, format!("unknown fact `{}`", f.id))),
                Some(value) if !f.before.trim_end().ends_with(value.as_str()) => out.push(diag(
                    &doc.path,
                    f.line,
                    format!(
                        "fact `{}` must be written right before its marker as `{value}`",
                        f.id
                    ),
                )),
                Some(_) => {}
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn diag(path: &str, line: usize, msg: String) -> Diag {
    Diag {
        path: path.to_owned(),
        line,
        msg,
        warning: false,
    }
}

fn check_doc_meta(doc: &Doc, out: &mut Vec<Diag>) {
    let Some(meta) = &doc.meta else { return };
    for k in meta.keys() {
        if !DOC_KEYS.contains(&k.as_str()) {
            out.push(diag(&doc.path, 1, format!("unknown `@doc` key `{k}`")));
        }
    }
    for k in ["id", "title", "kind", "status"] {
        if doc.meta_str(k).is_none() {
            out.push(diag(&doc.path, 1, format!("`@doc` needs a string `{k}`")));
        }
    }
    let kind = doc.meta_str("kind").unwrap_or_default();
    if !KINDS.contains(&kind) {
        out.push(diag(&doc.path, 1, format!("unknown kind `{kind}`")));
    }
    let status = doc.meta_str("status").unwrap_or_default();
    let allowed = if kind == "adr" {
        ADR_STATUSES
    } else {
        DOC_STATUSES
    };
    if !allowed.contains(&status) {
        out.push(diag(
            &doc.path,
            1,
            format!("status `{status}` is not valid for kind `{kind}`"),
        ));
    }
    if kind != "adr" && kind != "generated" && doc.meta_str("review_every").is_none() {
        out.push(diag(&doc.path, 1, "`@doc` needs `review_every`".into()));
    }
    if kind == "adr" {
        let number = doc
            .meta_str("id")
            .and_then(|id| id.strip_prefix("ADR-"))
            .and_then(|n| n.parse::<u32>().ok());
        let tier = doc.meta_str("tier");
        if number.is_some_and(|n| n >= TIER_REQUIRED_FROM) && tier.is_none() {
            out.push(diag(
                &doc.path,
                1,
                "ADRs from ADR-0017 on need `tier` (R-GOV-005)".into(),
            ));
        }
        if let Some(t) = tier
            && t != "maintainer"
            && t != "agent"
        {
            out.push(diag(
                &doc.path,
                1,
                format!("tier `{t}` must be `maintainer` or `agent`"),
            ));
        }
        if (status == "superseded") != doc.meta_str("superseded_by").is_some() {
            out.push(diag(
                &doc.path,
                1,
                "`superseded_by` is required exactly when status is `superseded`".into(),
            ));
        }
    }
}

fn check_section_meta(
    repo: &Repo<'_>,
    doc: &Doc,
    line: usize,
    anchor: &str,
    meta: &toml::Table,
    by_path: &BTreeMap<&str, &Doc>,
    out: &mut Vec<Diag>,
) {
    for k in meta.keys() {
        if !SECTION_KEYS.contains(&k.as_str()) {
            out.push(diag(
                &doc.path,
                line,
                format!("section `{anchor}`: unknown key `{k}`"),
            ));
        }
    }
    let covers = string_array(meta, "covers", doc, line, out);
    let deps = string_array(meta, "depends_on", doc, line, out);
    // R-DOC-012: a section can't describe code that doesn't exist. Drafts only warn, and
    // retired docs describe nothing current, so they are exempt.
    let status = doc.meta_str("status");
    for g in covers.iter().filter(|_| status != Some("retired")) {
        match repo.files.matching(std::slice::from_ref(g)) {
            Ok(m) if m.is_empty() => out.push(Diag {
                warning: status == Some("draft"),
                ..diag(
                    &doc.path,
                    line,
                    format!("section `{anchor}`: `covers` glob `{g}` matches no tracked file"),
                )
            }),
            Ok(_) => {}
            Err(e) => out.push(diag(&doc.path, line, format!("section `{anchor}`: {e:#}"))),
        }
    }
    if !covers.is_empty() || !deps.is_empty() {
        match meta.get("verified").and_then(|v| v.as_table()) {
            None => out.push(diag(
                &doc.path,
                line,
                format!("section `{anchor}` has covers/depends_on but no `verified`"),
            )),
            Some(v) => {
                for k in ["hash", "by", "note"] {
                    if v.get(k).and_then(|x| x.as_str()).is_none_or(str::is_empty) {
                        out.push(diag(
                            &doc.path,
                            line,
                            format!(
                                "section `{anchor}`: `verified.{k}` must be a non-empty string"
                            ),
                        ));
                    }
                }
                let stored = v.get("hash").and_then(|x| x.as_str()).unwrap_or_default();
                match hash::section_hash(doc, anchor, &covers, &deps, &repo.files, by_path) {
                    Ok(computed) if computed == stored => {}
                    Ok(computed) => out.push(diag(
                        &doc.path,
                        line,
                        format!(
                            "section `{anchor}` is stale (stored {stored}, now {computed}): re-read it \
                             against your change; if it still holds run `cargo xtask docs verify \
                             {}#{anchor} --by <who> --note \"<what you checked>\"`, otherwise edit it",
                            doc.meta_str("id").unwrap_or(&doc.path)
                        ),
                    )),
                    Err(e) => out.push(diag(&doc.path, line, format!("section `{anchor}`: {e:#}"))),
                }
            }
        }
    }
    for dep in deps {
        let (file, frag) = dep.split_once('#').unwrap_or((dep.as_str(), ""));
        if let Some(target) = file.strip_prefix("ext:") {
            out.push(diag(
                &doc.path,
                line,
                format!("`depends_on` must name repo docs, not `ext:{target}`"),
            ));
            continue;
        }
        match by_path.get(file) {
            None => out.push(diag(
                &doc.path,
                line,
                format!("`depends_on` target `{dep}` is not a tracked doc"),
            )),
            Some(t) if !frag.is_empty() && !t.has_anchor(frag) => {
                out.push(diag(
                    &doc.path,
                    line,
                    format!("`depends_on` anchor `{dep}` does not exist"),
                ));
            }
            Some(t) if !doc.is_adr() && t.meta_str("status") == Some("superseded") => {
                out.push(diag(
                    &doc.path,
                    line,
                    format!("`depends_on` points at superseded `{file}`"),
                ));
            }
            Some(_) => {}
        }
    }
}

fn check_rule_meta(
    doc: &Doc,
    line: usize,
    id: &str,
    meta: &toml::Table,
    ci_checks: &BTreeSet<String>,
    out: &mut Vec<Diag>,
) {
    for k in meta.keys() {
        if !RULE_KEYS.contains(&k.as_str()) {
            out.push(diag(
                &doc.path,
                line,
                format!("rule `{id}`: unknown key `{k}`"),
            ));
        }
    }
    match meta.get("status").and_then(|v| v.as_str()) {
        Some("active" | "retired") => {}
        other => out.push(diag(
            &doc.path,
            line,
            format!("rule `{id}`: status must be `active` or `retired`, got {other:?}"),
        )),
    }
    // R-DOC-012: `applies_to` may name paths that don't exist yet, but must be valid globs.
    for g in string_array(meta, "applies_to", doc, line, out) {
        if let Err(e) = globset::Glob::new(&g) {
            out.push(diag(
                &doc.path,
                line,
                format!("rule `{id}`: invalid `applies_to` glob `{g}`: {e}"),
            ));
        }
    }
    for e in string_array(meta, "enforced_by", doc, line, out) {
        let ok = match e.split_once(':') {
            Some(("ci", name)) => ci_checks.contains(name),
            Some(("lint" | "deny" | "test", rest)) => !rest.is_empty(),
            None => e == "review" || e == "human",
            _ => false,
        };
        if !ok {
            out.push(diag(
                &doc.path,
                line,
                format!("rule `{id}`: invalid `enforced_by` entry `{e}`"),
            ));
        }
    }
}

fn string_array(
    meta: &toml::Table,
    key: &str,
    doc: &Doc,
    line: usize,
    out: &mut Vec<Diag>,
) -> Vec<String> {
    match meta.get(key) {
        None => Vec::new(),
        Some(v) => match v.as_array() {
            Some(a) if a.iter().all(toml::Value::is_str) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect(),
            _ => {
                out.push(diag(
                    &doc.path,
                    line,
                    format!("`{key}` must be an array of strings"),
                ));
                Vec::new()
            }
        },
    }
}

fn check_link(
    repo: &Repo<'_>,
    doc: &Doc,
    dest: &str,
    line: usize,
    ext_urls: &BTreeSet<String>,
    by_path: &BTreeMap<&str, &Doc>,
    out: &mut Vec<Diag>,
) {
    if dest.starts_with("http://") || dest.starts_with("https://") {
        if !ext_urls.contains(dest) {
            out.push(diag(
                &doc.path,
                line,
                format!("external URL `{dest}` is not registered in docs/external.toml"),
            ));
        }
        return;
    }
    if dest.starts_with("mailto:") {
        return;
    }
    let (file, frag) = dest.split_once('#').unwrap_or((dest, ""));
    let target_path = if file.is_empty() {
        doc.path.clone()
    } else {
        let base = Path::new(&doc.path).parent().unwrap_or(Path::new(""));
        match normalize(&base.join(file)) {
            Some(p) => p,
            None => {
                out.push(diag(
                    &doc.path,
                    line,
                    format!("link `{dest}` escapes the repository"),
                ));
                return;
            }
        }
    };
    if !repo.root.join(&target_path).exists() {
        out.push(diag(
            &doc.path,
            line,
            format!("link `{dest}`: `{target_path}` does not exist"),
        ));
        return;
    }
    let Some(target) = by_path.get(target_path.as_str()) else {
        return;
    };
    if !frag.is_empty() && !target.has_anchor(frag) {
        out.push(diag(
            &doc.path,
            line,
            format!("link `{dest}`: no anchor `{frag}` in {target_path}"),
        ));
    }
    if !doc.is_adr() && target.is_adr() && target.meta_str("status") == Some("superseded") {
        out.push(diag(
            &doc.path,
            line,
            format!("link `{dest}` cites superseded {target_path}; cite its replacement"),
        ));
    }
}

/// Resolves `.` and `..` lexically; `None` if the path escapes the root.
pub(crate) fn normalize(p: &Path) -> Option<String> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other),
        }
    }
    Some(out.to_string_lossy().replace('\\', "/"))
}

/// Validates `docs/external.toml`; returns the registered URLs.
fn check_external(repo: &Repo<'_>, out: &mut Vec<Diag>) -> BTreeSet<String> {
    const P: &str = "docs/external.toml";
    let mut urls = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let Some(entries) = repo.external.get("external").and_then(|v| v.as_array()) else {
        out.push(diag(P, 1, "expected `[[external]]` entries".into()));
        return urls;
    };
    for e in entries {
        let get = |k: &str| e.get(k).and_then(|v| v.as_str());
        let id = get("id").unwrap_or("?");
        for k in [
            "id",
            "url",
            "relied_on",
            "review_every",
            "status",
            "checked_note",
        ] {
            if get(k).is_none() {
                out.push(diag(P, 1, format!("external `{id}`: missing string `{k}`")));
            }
        }
        if !ids.insert(id.to_owned()) {
            out.push(diag(P, 1, format!("duplicate external id `{id}`")));
        }
        if let Some(s) = get("status")
            && !EXT_STATUSES.contains(&s)
        {
            out.push(diag(P, 1, format!("external `{id}`: unknown status `{s}`")));
        }
        if let Some(u) = get("url") {
            urls.insert(u.to_owned());
        }
    }
    urls
}

/// Validates `docs/facts.toml` (including file-sourced values); returns id → value.
fn check_facts_registry(repo: &Repo<'_>, out: &mut Vec<Diag>) -> BTreeMap<String, String> {
    const P: &str = "docs/facts.toml";
    let mut facts = BTreeMap::new();
    let Some(entries) = repo.facts.get("fact").and_then(|v| v.as_array()) else {
        out.push(diag(P, 1, "expected `[[fact]]` entries".into()));
        return facts;
    };
    for f in entries {
        let (Some(id), Some(value)) = (
            f.get("id").and_then(|v| v.as_str()),
            f.get("value").and_then(|v| v.as_str()),
        ) else {
            out.push(diag(
                P,
                1,
                "every fact needs string `id` and `value`".into(),
            ));
            continue;
        };
        if facts.insert(id.to_owned(), value.to_owned()).is_some() {
            out.push(diag(P, 1, format!("duplicate fact `{id}`")));
        }
        if f.get("check").and_then(|v| v.as_str()) != Some("file") {
            continue;
        }
        let src = f.get("source").and_then(|v| v.as_table());
        let file = src.and_then(|s| s.get("file")).and_then(|v| v.as_str());
        let key = src.and_then(|s| s.get("key")).and_then(|v| v.as_str());
        let (Some(file), Some(key)) = (file, key) else {
            out.push(diag(
                P,
                1,
                format!("fact `{id}`: file facts need `source.file` and `source.key`"),
            ));
            continue;
        };
        let actual = std::fs::read_to_string(repo.root.join(file))
            .ok()
            .and_then(|t| t.parse::<toml::Table>().ok())
            .and_then(|t| lookup(&t, key).map(str::to_owned));
        if actual.as_deref() != Some(value) {
            out.push(diag(
                P,
                1,
                format!("fact `{id}` is `{value}` but {file} `{key}` is {actual:?}"),
            ));
        }
    }
    facts
}

fn lookup<'t>(t: &'t toml::Table, dotted: &str) -> Option<&'t str> {
    let mut parts = dotted.split('.');
    let mut cur = t.get(parts.next()?)?;
    for p in parts {
        cur = cur.as_table()?.get(p)?;
    }
    cur.as_str()
}

/// The CI check names from the R-GIT-003 table in docs/RULES.md.
pub(crate) fn ci_check_names(rules_md: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Some(start) = rules_md.find("<a id=\"R-GIT-003\"></a>") else {
        return names;
    };
    let mut in_table = false;
    for line in rules_md[start..].lines() {
        if line.starts_with('|') {
            in_table = true;
            if let Some(name) = line.strip_prefix("| `").and_then(|r| r.split('`').next()) {
                names.insert(name.to_owned());
            }
        } else if in_table {
            break;
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_names_come_from_the_rgit003_table() {
        let md = "x\n<a id=\"R-GIT-003\"></a>\n### R-GIT-003\n\n| Check | Runs |\n|---|---|\n| `fmt` | x |\n| `docs` | y |\n\nafter\n| `not` | z |\n";
        let names = ci_check_names(md);
        assert_eq!(names.into_iter().collect::<Vec<_>>(), vec!["docs", "fmt"]);
    }

    #[test]
    fn normalize_resolves_dots_and_rejects_escape() {
        assert_eq!(
            normalize(Path::new("docs/../README.md")).as_deref(),
            Some("README.md")
        );
        assert_eq!(
            normalize(Path::new("docs/./adr/x.md")).as_deref(),
            Some("docs/adr/x.md")
        );
        assert_eq!(normalize(Path::new("../outside.md")), None);
    }

    #[test]
    fn dotted_lookup() {
        let t: toml::Table = "[toolchain]\nchannel = \"1.97.0\"\n".parse().unwrap();
        assert_eq!(lookup(&t, "toolchain.channel"), Some("1.97.0"));
        assert_eq!(lookup(&t, "toolchain.nope"), None);
    }
}
