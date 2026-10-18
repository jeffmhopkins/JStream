//! The workspace's tests as `cargo test -- --list` names them, read from the source
//! without building, so that `docs check` can resolve `test:` entries in `enforced_by`
//! (docs/DOCS.md#rules).
//!
//! A test is a `fn` with an attribute whose path ends in `test`, with or without
//! arguments (`#[test]`, `#[tokio::test(flavor = "…")]`), declared directly in a module,
//! named by its module path from its
//! target's root: `src/lib.rs`, `src/main.rs`, `src/bin/*`, or `tests/*`. Modules are
//! followed through `mod x;` (to `x.rs` or `x/mod.rs`, or a `#[path]`) and `mod x { … }`.
//! Tests that a macro generates aren't seen.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Every test of every crate.
pub(crate) struct TestList {
    /// By the crate's name with `-` written `_`: every test across its targets.
    by_crate: BTreeMap<String, BTreeSet<String>>,
    /// By integration test target (`tests/<name>.rs`): that target's tests.
    by_target: BTreeMap<String, BTreeSet<String>>,
}

/// How a `test:` entry resolved.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// A test, or a module that holds one, exists at that path.
    Found,
    /// Its first segment names no crate or test target.
    NoTarget,
    /// The crate or target exists, but nothing in it has that path.
    NoTest,
}

impl TestList {
    /// Reads every crate under `root` that `crates` lists, as `(directory, package name)`.
    pub(crate) fn read(root: &Path, crates: &[(String, String)], paths: &[String]) -> Self {
        let tracked: BTreeSet<&str> = paths.iter().map(String::as_str).collect();
        let mut list = Self {
            by_crate: BTreeMap::new(),
            by_target: BTreeMap::new(),
        };
        for (dir, name) in crates {
            let mut all = BTreeSet::new();
            for (target, file) in targets(dir, &tracked) {
                let mut tests = BTreeSet::new();
                walk(root, &file, true, &[], &mut tests, 0);
                all.extend(tests.iter().cloned());
                if let Some(t) = target {
                    list.by_target.entry(t).or_default().extend(tests);
                }
            }
            list.by_crate
                .entry(name.replace('-', "_"))
                .or_default()
                .extend(all);
        }
        list
    }

    /// Resolves `path` (the part after `test:`): its first segment is a crate or an
    /// integration test target, the rest a test, or a module of tests, in it.
    pub(crate) fn resolve(&self, path: &str) -> Resolved {
        let Some((first, rest)) = path.split_once("::") else {
            return Resolved::NoTest;
        };
        let sets: Vec<&BTreeSet<String>> = [self.by_crate.get(first), self.by_target.get(first)]
            .into_iter()
            .flatten()
            .collect();
        if sets.is_empty() {
            return Resolved::NoTarget;
        }
        let module = format!("{rest}::");
        let found = sets.iter().any(|s| {
            s.contains(rest)
                || s.range(module.clone()..)
                    .next()
                    .is_some_and(|t| t.starts_with(&module))
        });
        if found {
            Resolved::Found
        } else {
            Resolved::NoTest
        }
    }
}

/// A crate's test-bearing targets: `(integration test target name, root file)`.
fn targets(dir: &str, tracked: &BTreeSet<&str>) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    for f in ["src/lib.rs", "src/main.rs"] {
        let p = format!("{dir}/{f}");
        if tracked.contains(p.as_str()) {
            out.push((None, p));
        }
    }
    for kind in ["src/bin", "tests"] {
        let prefix = format!("{dir}/{kind}/");
        for p in tracked
            .range(prefix.as_str()..)
            .take_while(|p| p.starts_with(&prefix))
        {
            let rest = &p[prefix.len()..];
            let name = match rest.split_once('/') {
                None => rest.strip_suffix(".rs"),
                Some((d, "main.rs")) => Some(d),
                Some(_) => None,
            };
            if let Some(name) = name {
                let target = (kind == "tests").then(|| name.to_owned());
                out.push((target, (*p).to_owned()));
            }
        }
    }
    out
}

/// Collects the tests of the module in `file` (and its submodules) into `out`, each
/// prefixed with `module`. `mod_rs` says whether `file` owns its directory (a target
/// root or a `mod.rs`), which decides where its `mod x;` files are.
fn walk(
    root: &Path,
    file: &str,
    mod_rs: bool,
    module: &[String],
    out: &mut BTreeSet<String>,
    depth: usize,
) {
    // A cycle through `#[path]` would otherwise recurse forever.
    if depth > 32 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(root.join(file)) else {
        return;
    };
    let path = Path::new(file);
    let parent = path.parent().unwrap_or(Path::new(""));
    let base = if mod_rs {
        parent.to_path_buf()
    } else {
        parent.join(path.file_stem().unwrap_or_default())
    };
    let toks = tokens(&text);
    // Each open inline module: its name and the brace depth inside it.
    let mut inline: Vec<(String, usize)> = Vec::new();
    let mut braces = 0usize;
    let mut attrs = Attrs::default();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Punct('#') => {
                i = attrs.read(&toks, i + 1);
                continue;
            }
            Tok::Punct('{') => {
                braces += 1;
                attrs = Attrs::default();
            }
            Tok::Punct('}') => {
                if inline.last().is_some_and(|(_, d)| *d == braces) {
                    inline.pop();
                }
                braces = braces.saturating_sub(1);
                attrs = Attrs::default();
            }
            Tok::Punct(';') => attrs = Attrs::default(),
            // Items count only directly in a module, not in a fn, impl or macro body.
            Tok::Ident(kw) if braces == inline.last().map_or(0, |(_, d)| *d) => {
                let name = match toks.get(i + 1) {
                    Some(Tok::Ident(n)) => Some(n.trim_start_matches("r#").to_owned()),
                    _ => None,
                };
                match (kw.as_str(), name) {
                    ("fn", Some(name)) => {
                        if attrs.test {
                            let mut p: Vec<&str> = module.iter().map(String::as_str).collect();
                            p.extend(inline.iter().map(|(n, _)| n.as_str()));
                            p.push(&name);
                            out.insert(p.join("::"));
                        }
                        attrs = Attrs::default();
                        i += 2;
                        continue;
                    }
                    ("mod", Some(name)) => match toks.get(i + 2) {
                        Some(Tok::Punct(';')) => {
                            let dir: PathBuf =
                                inline.iter().fold(base.clone(), |d, (n, _)| d.join(n));
                            let mut sub: Vec<String> = module.to_vec();
                            sub.extend(inline.iter().map(|(n, _)| n.clone()));
                            sub.push(name.clone());
                            let found = match attrs.path.take() {
                                Some(p) => {
                                    let from = if inline.is_empty() { parent } else { &dir };
                                    // rustc treats a `#[path]` file as a `mod.rs`: its
                                    // own `mod x;` files are its siblings.
                                    Some((from.join(p), true))
                                }
                                None => [
                                    (dir.join(format!("{name}.rs")), false),
                                    (dir.join(&name).join("mod.rs"), true),
                                ]
                                .into_iter()
                                .find(|(p, _)| root.join(p).is_file()),
                            };
                            if let Some((p, own)) = found
                                && let Some(p) = p.to_str()
                            {
                                walk(root, p, own, &sub, out, depth + 1);
                            }
                            attrs = Attrs::default();
                            i += 3;
                            continue;
                        }
                        Some(Tok::Punct('{')) => {
                            braces += 1;
                            inline.push((name, braces));
                            attrs = Attrs::default();
                            i += 3;
                            continue;
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// The attributes read since the last item.
#[derive(Default)]
struct Attrs {
    test: bool,
    path: Option<String>,
}

impl Attrs {
    /// Reads the attribute starting after `#` at `i`; returns the index after it.
    fn read(&mut self, toks: &[Tok], mut i: usize) -> usize {
        let inner = matches!(toks.get(i), Some(Tok::Punct('!')));
        if inner {
            i += 1;
        }
        if !matches!(toks.get(i), Some(Tok::Punct('['))) {
            return i;
        }
        let start = i + 1;
        let mut depth = 0usize;
        let mut end = start;
        while let Some(t) = toks.get(end) {
            match t {
                Tok::Punct('[' | '(' | '{') => depth += 1,
                Tok::Punct(']' | ')' | '}') if depth == 0 => break,
                Tok::Punct(']' | ')' | '}') => depth -= 1,
                _ => {}
            }
            end += 1;
        }
        if !inner {
            // The attribute's path: idents joined by `::`, up to anything else.
            let mut last = None;
            let mut j = start;
            while let Some(Tok::Ident(n)) = toks.get(j) {
                last = Some(n.as_str());
                j += 1;
                if matches!(
                    (toks.get(j), toks.get(j + 1)),
                    (Some(Tok::Punct(':')), Some(Tok::Punct(':')))
                ) {
                    j += 2;
                } else {
                    break;
                }
            }
            match last {
                // `#[test]`, or one with arguments: `#[tokio::test(flavor = "…")]`.
                Some("test") if j == end || closes_at(toks, j) == Some(end) => {
                    self.test = true;
                }
                Some("path") => {
                    if let (Some(Tok::Punct('=')), Some(Tok::Str(s))) =
                        (toks.get(j), toks.get(j + 1))
                    {
                        self.path = Some(s.clone());
                    }
                }
                _ => {}
            }
        }
        end + 1
    }
}

/// The index just past the `( … )` group that opens at `j`, if one does.
fn closes_at(toks: &[Tok], j: usize) -> Option<usize> {
    if toks.get(j) != Some(&Tok::Punct('(')) {
        return None;
    }
    let mut depth = 0usize;
    for (k, t) in toks.iter().enumerate().skip(j) {
        match t {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(k + 1);
                }
            }
            _ => {}
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Punct(char),
    Str(String),
}

/// Splits Rust source into identifiers, punctuation and string literals, skipping
/// comments, char literals, lifetimes and numbers. Good enough to find modules,
/// attributes and fns; not a full lexer.
fn tokens(src: &str) -> Vec<Tok> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            // Block comments nest.
            let mut depth = 0;
            while i < c.len() {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if let Some((s, next)) = raw_string(&c, i) {
            out.push(Tok::Str(s));
            i = next;
        } else if ch == '"' || (ch == 'b' && c.get(i + 1) == Some(&'"')) {
            i += if ch == 'b' { 2 } else { 1 };
            let mut s = String::new();
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' {
                    i += 1;
                }
                if let Some(&x) = c.get(i) {
                    s.push(x);
                }
                i += 1;
            }
            out.push(Tok::Str(s));
            i += 1;
        } else if ch == '\'' {
            // A char literal ('x', '\n', '\u{..}') or a lifetime ('a).
            if c.get(i + 1) == Some(&'\\') {
                // Past the backslash and the escaped char, so `'\''` ends at its own quote.
                i += 3;
                while i < c.len() && c[i] != '\'' {
                    i += 1;
                }
                i += 1;
            } else if c.get(i + 2) == Some(&'\'') {
                i += 3;
            } else {
                i += 1;
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
            }
        } else if ch.is_alphabetic() || ch == '_' {
            let start = i;
            if ch == 'r' && c.get(i + 1) == Some(&'#') {
                i += 2;
            }
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(c[start..i].iter().collect()));
        } else if ch.is_ascii_digit() {
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '.') {
                i += 1;
            }
        } else {
            out.push(Tok::Punct(ch));
            i += 1;
        }
    }
    out
}

/// A raw string (`r"…"`, `r#"…"#`, `br#"…"#`) starting at `i`: its text and the index
/// after it.
fn raw_string(c: &[char], i: usize) -> Option<(String, usize)> {
    let mut j = i;
    if c.get(j) == Some(&'b') {
        j += 1;
    }
    if c.get(j) != Some(&'r') {
        return None;
    }
    j += 1;
    let mut hashes = 0;
    while c.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    if c.get(j) != Some(&'"') {
        return None;
    }
    j += 1;
    let start = j;
    while j < c.len() {
        if c[j] == '"' && (1..=hashes).all(|k| c.get(j + k) == Some(&'#')) {
            return Some((c[start..j].iter().collect(), j + 1 + hashes));
        }
        j += 1;
    }
    Some((c[start..].iter().collect(), c.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary tree, removed on drop.
    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writes `files` into a fresh tree named after the test and reads its one crate,
    /// `crates/jstream-a`.
    fn list(name: &str, files: &[(&str, &str)]) -> TestList {
        let dir = std::env::temp_dir().join(format!("xtask-tests-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = Fixture(dir);
        for (p, text) in files {
            let p = f.0.join(p);
            std::fs::create_dir_all(p.parent().expect("invariant: files live in the tree"))
                .expect("invariant: temp dir is writable");
            std::fs::write(p, text).expect("invariant: temp dir is writable");
        }
        let mut paths: Vec<String> = files.iter().map(|(p, _)| (*p).to_owned()).collect();
        paths.sort();
        let crates = vec![("crates/jstream-a".to_owned(), "jstream-a".to_owned())];
        TestList::read(&f.0, &crates, &paths)
    }

    #[test]
    fn tests_are_named_by_their_module_path_from_the_target_root() {
        let l = list(
            "module-paths",
            &[
                (
                    "crates/jstream-a/src/lib.rs",
                    "mod engine;\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn inline() {}\n    fn helper() {}\n}\n#[test]\nfn top() {}\n",
                ),
                (
                    "crates/jstream-a/src/engine.rs",
                    "mod rt;\n// #[test] fn commented() {}\nconst S: &str = \"#[test] fn in_a_string() {}\";\n#[tokio::test]\nasync fn asyncly() {}\nconst Q: char = '\\'';\n#[tokio::test(flavor = \"multi_thread\", worker_threads = 2)]\nasync fn with_arguments() {}\n#[test_case(1)]\nfn not_a_test_attribute(_: u8) {}\n",
                ),
                (
                    "crates/jstream-a/src/engine/rt/mod.rs",
                    "#[test]\n#[should_panic(expected = \"x\")]\npub fn deep() { #[test] fn nested() {} }\n",
                ),
                (
                    "crates/jstream-a/tests/scenarios.rs",
                    "mod rt {\n    #[test]\n    fn overload() {}\n}\n#[test]\nfn r#match() {}\n",
                ),
            ],
        );
        let found = |p| l.resolve(p) == Resolved::Found;
        assert!(found("jstream_a::top"));
        assert!(found("jstream_a::tests::inline"));
        assert!(found("jstream_a::engine::asyncly"));
        // A test attribute with arguments (#739 review B1).
        assert!(found("jstream_a::engine::with_arguments"));
        assert!(found("jstream_a::engine::rt::deep"));
        // Integration tests count toward their crate and their own target.
        assert!(found("jstream_a::rt::overload"));
        assert!(found("scenarios::rt::overload"));
        assert!(found("scenarios::match"));
        // A module that holds tests stands for them.
        assert!(found("jstream_a::engine::rt"));
        assert!(found("jstream_a::engine"));
        for missing in [
            "jstream_a::helper",
            "jstream_a::tests::helper",
            "jstream_a::engine::commented",
            "jstream_a::engine::in_a_string",
            "jstream_a::engine::not_a_test_attribute",
            "jstream_a::engine::rt::nested",
            "jstream_a::inline",
            "jstream_a::engine::r",
            "jstream_a",
        ] {
            assert_eq!(l.resolve(missing), Resolved::NoTest, "{missing}");
        }
        assert_eq!(l.resolve("jstream_b::top"), Resolved::NoTarget);
    }

    #[test]
    fn a_path_attribute_and_bin_targets_are_followed() {
        let l = list(
            "path-and-bin",
            &[
                (
                    "crates/jstream-a/src/main.rs",
                    "#[path = \"elsewhere/odd.rs\"]\nmod odd;\n",
                ),
                (
                    "crates/jstream-a/src/elsewhere/odd.rs",
                    "mod sib;\n#[test]\nfn there() {}\n",
                ),
                // A `#[path]` file is a `mod.rs`: its submodule is a sibling (review N1).
                (
                    "crates/jstream-a/src/elsewhere/sib.rs",
                    "#[test]\nfn beside() {}\n",
                ),
                (
                    "crates/jstream-a/src/bin/tool.rs",
                    "#[test]\nfn tooled() {}\n",
                ),
            ],
        );
        assert_eq!(l.resolve("jstream_a::odd::there"), Resolved::Found);
        assert_eq!(l.resolve("jstream_a::odd::sib::beside"), Resolved::Found);
        assert_eq!(l.resolve("jstream_a::tooled"), Resolved::Found);
    }
}
