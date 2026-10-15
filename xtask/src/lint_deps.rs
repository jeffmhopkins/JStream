//! `cargo xtask lint-deps`: checks the workspace's crate graph against the dependency and
//! layering rules whose `enforced_by` names `ci:lint-deps` (docs/RULES.md).
//!
//! - R-ARCH-005: every member declares `[package.metadata.jstream]` (`layer`, `summary`,
//!   `unsafe_allowed`).
//! - R-ARCH-002: a member depends only on members in strictly lower layers.
//! - R-ARCH-003: the clients never reach `jstream-core`, directly or through other members.
//! - R-UNSAFE-001: only the declared crates set `unsafe_allowed = true`; every other crate
//!   root carries `#![forbid(unsafe_code)]`.
//! - R-DEP-002: every resolved package with a `links =` key or a C-compiling build
//!   dependency (directly, or through a build helper one level down) is on
//!   [`NATIVE_ALLOWLIST`].
//! - R-DEP-003: member manifests take every dependency from `[workspace.dependencies]`.
//!
//! Docs: docs/DOCS.md#dependency-lint, docs/ARCHITECTURE.md#crate-map

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// Crates that may set `unsafe_allowed = true` (R-UNSAFE-001), besides any `*-sys` crate.
const UNSAFE_CRATES: &[&str] = &[
    "jstream-platform-windows",
    "jstream-platform-macos",
    "jstream-platform-linux",
    "jstream-vcam-windows",
];

/// Clients that never depend on the engine (R-ARCH-003).
const CLIENTS: &[&str] = &["jstream-ui", "jstream-mcp", "jstream-server", "jstream-cli"];

/// The engine crate the clients must not reach (R-ARCH-003).
const ENGINE: &str = "jstream-core";

/// Build dependencies that mean a build script compiles C or C++ (R-DEP-002).
const C_BUILD_TOOLS: &[&str] = &["cc", "cmake", "autotools", "nasm-rs"];

/// Native-code packages allowed by R-DEP-002: (package, feature that must be enabled, why).
/// Adding an entry needs the same justification as a new dependency (R-DEP-004).
const NATIVE_ALLOWLIST: &[(&str, Option<&str>, &str)] = &[
    (
        "blake3",
        Some("pure"),
        "build-depends on `cc` for SIMD code; with the `pure` feature its build script compiles no C",
    ),
    (
        "ring",
        None,
        "R-DEP-006 names it as rustls' crypto provider: its build script compiles BoringSSL-derived \
         C and assembly for the primitives (no media code). Used by jstream-outputs for rtmps://",
    ),
    (
        "prettyplease",
        None,
        "`links = \"prettyplease03\"` only enforces one version per graph; its build script prints \
         cfg declarations and checks that key, and compiles nothing. Reached through leptos_macro \
         (jstream-ui, ADR-0045)",
    ),
    (
        "wasm-bindgen-shared",
        None,
        "`links = \"wasm_bindgen\"` only enforces one version per graph; its build script sets \
         environment variables and compiles nothing. Reached through wgpu's wasm32-only dependencies",
    ),
    // Tauri 2 (jstream-desktop, ADR-0045), a dependency on Windows only. `cargo metadata`
    // lists every platform's packages, so most of these are never built for Windows.
    (
        "tauri",
        None,
        "`links = \"Tauri\"` only enforces one version per graph; its build script writes the \
         ACL and permission files and compiles nothing (jstream-desktop)",
    ),
    (
        "vswhom-sys",
        None,
        "build-depends on `cc` for a small C++ file that asks Windows where Visual Studio is, \
         so embed-resource can run the resource compiler; under tauri-build, at build time only. \
         Nothing of it is linked into the app (jstream-desktop)",
    ),
    (
        "atk-sys",
        None,
        "`links = \"atk-1.0\"`: bindings to the system's ATK through pkg-config, compiling no C. \
         Linux only, under Tauri's WebKitGTK webview; never built (jstream-desktop uses Tauri on Windows only)",
    ),
    (
        "gdk-sys",
        None,
        "`links = \"gdk-3\"`: bindings to the system's GDK through pkg-config, compiling no C. \
         Linux only, under Tauri's WebKitGTK webview; never built (jstream-desktop uses Tauri on Windows only)",
    ),
    (
        "gtk-sys",
        None,
        "`links = \"gtk-3\"`: bindings to the system's GTK through pkg-config, compiling no C. \
         Linux only, under Tauri's WebKitGTK webview; never built (jstream-desktop uses Tauri on Windows only)",
    ),
    (
        "webkit2gtk-sys",
        None,
        "`links = \"web_kit2\"`: bindings to the system's WebKitGTK through pkg-config, compiling \
         no C. Linux only, under Tauri; never built (jstream-desktop uses Tauri on Windows only)",
    ),
    (
        "objc2-exception-helper",
        None,
        "build-depends on `cc` for a few lines of Objective-C that catch exceptions. macOS only, \
         under Tauri's tao and wry; never built (jstream-desktop uses Tauri on Windows only)",
    ),
    (
        "iana-time-zone-haiku",
        None,
        "build-depends on `cc` for a small C++ shim. Haiku only, under chrono (serde_with under \
         tauri-utils); never built",
    ),
    (
        "defmt",
        None,
        "`links = \"defmt\"` only enforces one version per graph; an optional logging backend of \
         jiff for embedded targets, listed through serde_with under tauri-utils; compiles nothing \
         and is never built here",
    ),
];

/// One rule violation, reported as `<where>: <rule>: <what>`.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Finding {
    pub(crate) at: String,
    pub(crate) rule: &'static str,
    pub(crate) msg: String,
}

/// `cargo xtask lint-deps`. Returns whether the workspace is clean.
pub(crate) fn run(args: &[String], root: &Path) -> Result<bool> {
    if !args.is_empty() {
        bail!("usage: cargo xtask lint-deps (takes no arguments)");
    }
    let meta = cargo_metadata(root, true)?;
    let findings = check(&meta)?;
    for f in &findings {
        eprintln!("{}: {}: {}", f.at, f.rule, f.msg);
    }
    let members = meta["workspace_members"].as_array().map_or(0, Vec::len);
    if findings.is_empty() {
        eprintln!("lint-deps: ok ({members} workspace crate(s))");
        Ok(true)
    } else {
        eprintln!("lint-deps: {} problem(s)", findings.len());
        Ok(false)
    }
}

/// `cargo metadata` for the workspace at `root`, with the full resolve graph.
fn cargo_metadata(root: &Path, locked: bool) -> Result<Value> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["metadata", "--format-version", "1"])
        .current_dir(root);
    if locked {
        cmd.arg("--locked");
    }
    let out = cmd.output().context("running `cargo metadata`")?;
    if !out.status.success() {
        bail!(
            "`cargo metadata` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    serde_json::from_slice(&out.stdout).context("parsing `cargo metadata` output")
}

/// A workspace member with its declared metadata.
struct Member<'a> {
    pkg: &'a Value,
    name: &'a str,
    /// Manifest path relative to the workspace root, for messages.
    manifest: String,
    layer: Option<i64>,
    unsafe_allowed: Option<bool>,
}

/// Runs every check over parsed `cargo metadata` output.
pub(crate) fn check(meta: &Value) -> Result<Vec<Finding>> {
    let ws_root = meta["workspace_root"]
        .as_str()
        .context("`cargo metadata`: no workspace_root")?;
    let member_ids: BTreeSet<&str> = strs(&meta["workspace_members"]).collect();
    let packages = meta["packages"]
        .as_array()
        .context("`cargo metadata`: no packages")?;
    let mut findings = Vec::new();

    let mut members: BTreeMap<&str, Member<'_>> = BTreeMap::new();
    for pkg in packages {
        if !member_ids.contains(pkg["id"].as_str().unwrap_or_default()) {
            continue;
        }
        let name = pkg["name"].as_str().context("package without a name")?;
        let manifest_abs = pkg["manifest_path"].as_str().unwrap_or_default();
        let manifest = rel(ws_root, manifest_abs);
        let m = declared(pkg, name, manifest, &mut findings);
        members.insert(name, m);
    }

    for m in members.values() {
        layering(m, &members, &mut findings);
        unsafe_decl(ws_root, m, &mut findings)?;
        workspace_deps(m, &mut findings)?;
    }
    clients(&members, &mut findings);
    native(meta, packages, &mut findings);

    findings.sort();
    findings.dedup();
    Ok(findings)
}

/// R-ARCH-005: reads `[package.metadata.jstream]`, reporting what is missing or mistyped.
fn declared<'a>(
    pkg: &'a Value,
    name: &'a str,
    manifest: String,
    findings: &mut Vec<Finding>,
) -> Member<'a> {
    let decl = &pkg["metadata"]["jstream"];
    let mut problem = |msg: String| {
        findings.push(Finding {
            at: manifest.clone(),
            rule: "R-ARCH-005",
            msg,
        });
    };
    if !decl.is_object() {
        problem(format!(
            "`{name}` has no `[package.metadata.jstream]` (layer, summary, unsafe_allowed)"
        ));
    }
    let layer = decl["layer"].as_i64().filter(|l| *l >= 0);
    if decl.is_object() && layer.is_none() {
        problem(format!(
            "`{name}`: `layer` must be a non-negative integer (ARCH crate map)"
        ));
    }
    if decl.is_object() && decl["summary"].as_str().is_none_or(|s| s.trim().is_empty()) {
        problem(format!("`{name}`: `summary` must be a non-empty string"));
    }
    let unsafe_allowed = decl["unsafe_allowed"].as_bool();
    if decl.is_object() && unsafe_allowed.is_none() {
        problem(format!("`{name}`: `unsafe_allowed` must be true or false"));
    }
    Member {
        pkg,
        name,
        manifest,
        layer,
        unsafe_allowed,
    }
}

/// Names of the workspace members `m` depends on, of any kind (normal, build, dev).
fn member_deps<'a>(m: &Member<'a>, members: &BTreeMap<&str, Member<'_>>) -> BTreeSet<&'a str> {
    m.pkg["dependencies"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["path"].is_string())
        .filter_map(|d| d["name"].as_str())
        .filter(|n| members.contains_key(n))
        .collect()
}

/// R-ARCH-002: every member dependency is in a strictly lower layer.
fn layering(m: &Member<'_>, members: &BTreeMap<&str, Member<'_>>, findings: &mut Vec<Finding>) {
    let Some(layer) = m.layer else { return };
    for dep in member_deps(m, members) {
        let Some(dep_layer) = members.get(dep).and_then(|d| d.layer) else {
            continue; // already reported under R-ARCH-005
        };
        if dep_layer >= layer {
            findings.push(Finding {
                at: m.manifest.clone(),
                rule: "R-ARCH-002",
                msg: format!(
                    "`{}` (layer {layer}) depends on `{dep}` (layer {dep_layer}); \
                     dependencies must be in strictly lower layers",
                    m.name
                ),
            });
        }
    }
}

/// R-ARCH-003: no client reaches the engine, directly or through other members.
fn clients(members: &BTreeMap<&str, Member<'_>>, findings: &mut Vec<Finding>) {
    for client in CLIENTS {
        let Some(m) = members.get(client) else {
            continue;
        };
        // Walk member dependencies, remembering how each was first reached.
        let mut via: BTreeMap<&str, &str> = BTreeMap::new();
        let mut queue = vec![m.name];
        while let Some(cur) = queue.pop() {
            let Some(cm) = members.get(cur) else { continue };
            for dep in member_deps(cm, members) {
                if dep != m.name && !via.contains_key(dep) {
                    via.insert(dep, cur);
                    queue.push(dep);
                }
            }
        }
        if via.contains_key(ENGINE) {
            let mut path = vec![ENGINE];
            let mut cur = ENGINE;
            while let Some(prev) = via.get(cur) {
                path.push(prev);
                cur = prev;
            }
            path.reverse();
            findings.push(Finding {
                at: m.manifest.clone(),
                rule: "R-ARCH-003",
                msg: format!(
                    "client `{client}` depends on the engine: {}",
                    path.join(" -> ")
                ),
            });
        }
    }
}

/// Whether `name` may declare `unsafe_allowed = true` (R-UNSAFE-001).
fn may_use_unsafe(name: &str) -> bool {
    UNSAFE_CRATES.contains(&name) || name.ends_with("-sys")
}

/// R-UNSAFE-001: the declaration is allowed, and undeclared crates forbid `unsafe` in
/// every library and binary crate root.
fn unsafe_decl(ws_root: &str, m: &Member<'_>, findings: &mut Vec<Finding>) -> Result<()> {
    match m.unsafe_allowed {
        Some(true) if !may_use_unsafe(m.name) => findings.push(Finding {
            at: m.manifest.clone(),
            rule: "R-UNSAFE-001",
            msg: format!(
                "`{}` declares `unsafe_allowed = true` but is not a platform, vcam or `*-sys` crate",
                m.name
            ),
        }),
        Some(false) => {
            for target in m.pkg["targets"].as_array().into_iter().flatten() {
                let is_root = strs(&target["kind"]).any(|k| {
                    matches!(
                        k,
                        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" | "bin"
                    )
                });
                let Some(src) = target["src_path"].as_str().filter(|_| is_root) else {
                    continue;
                };
                let text = std::fs::read_to_string(src).with_context(|| format!("reading {src}"))?;
                if !forbids_unsafe(&text) {
                    findings.push(Finding {
                        at: rel(ws_root, src),
                        rule: "R-UNSAFE-001",
                        msg: format!(
                            "`{}` declares `unsafe_allowed = false`, so this crate root needs \
                             `#![forbid(unsafe_code)]`",
                            m.name
                        ),
                    });
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Length of the (possibly nested) block comment at the start of `s`.
fn block_comment_len(s: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut k = 0;
    while k < s.len() {
        let rest = &s[k..];
        if rest.starts_with("/*") {
            depth += 1;
            k += 2;
        } else if rest.starts_with("*/") {
            depth -= 1;
            k += 2;
            if depth == 0 {
                return Some(k);
            }
        } else {
            k += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// Whether a crate root carries `#![forbid(unsafe_code)]` (possibly among other lints, and
/// wrapped over several lines) among its leading inner attributes, as in `docs_line` in
/// xtask/src/docs/backrefs.rs: only comments, whitespace and inner attributes may come
/// before it, so one inside a comment or later in the file doesn't count.
fn forbids_unsafe(text: &str) -> bool {
    let mut s = text.strip_prefix('\u{feff}').unwrap_or(text);
    loop {
        s = s.trim_start();
        if s.starts_with("//") {
            s = s.split_once('\n').map_or("", |(_, rest)| rest);
        } else if s.starts_with("/*") {
            let Some(n) = block_comment_len(s) else {
                return false;
            };
            s = &s[n..];
        } else if s.starts_with("#!") && s[2..].trim_start().starts_with('[') {
            let Some(n) = crate::governance::attr_len(s) else {
                return false;
            };
            let attr: String = s[..n].chars().filter(|c| !c.is_whitespace()).collect();
            let forbids = attr
                .strip_prefix("#![forbid(")
                .and_then(|rest| rest.strip_suffix(")]"))
                .is_some_and(|lints| lints.split(',').any(|x| x == "unsafe_code"));
            if forbids {
                return true;
            }
            s = &s[n..];
        } else {
            return false;
        }
    }
}

/// R-DEP-003: every dependency table entry in a member manifest is `workspace = true`.
fn workspace_deps(m: &Member<'_>, findings: &mut Vec<Finding>) -> Result<()> {
    let path = m.pkg["manifest_path"].as_str().unwrap_or_default();
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", m.manifest))?;
    let manifest: toml::Table = text
        .parse()
        .with_context(|| format!("parsing {}", m.manifest))?;

    let mut tables: Vec<(String, &toml::Table)> = Vec::new();
    let kinds = ["dependencies", "dev-dependencies", "build-dependencies"];
    for kind in kinds {
        if let Some(t) = manifest.get(kind).and_then(toml::Value::as_table) {
            tables.push((kind.to_owned(), t));
        }
    }
    for (cfg, t) in manifest
        .get("target")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
    {
        for kind in kinds {
            if let Some(deps) = t.get(kind).and_then(toml::Value::as_table) {
                tables.push((format!("target.{cfg}.{kind}"), deps));
            }
        }
    }
    for (table, deps) in tables {
        for (dep, spec) in deps {
            let inherited = spec
                .as_table()
                .and_then(|s| s.get("workspace"))
                .and_then(toml::Value::as_bool)
                == Some(true);
            if !inherited {
                findings.push(Finding {
                    at: m.manifest.clone(),
                    rule: "R-DEP-003",
                    msg: format!(
                        "[{table}] `{dep}` must be `{dep}.workspace = true`, with its version \
                         in the root `[workspace.dependencies]`"
                    ),
                });
            }
        }
    }
    Ok(())
}

/// R-DEP-002: every resolved package that links native code or compiles C in its build
/// script is on [`NATIVE_ALLOWLIST`], with its required feature enabled.
fn native(meta: &Value, packages: &[Value], findings: &mut Vec<Finding>) {
    let by_id: BTreeMap<&str, &Value> = packages
        .iter()
        .filter_map(|p| Some((p["id"].as_str()?, p)))
        .collect();
    let nodes: BTreeMap<&str, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();
    let name_of = |id: &str| {
        by_id
            .get(id)
            .and_then(|p| p["name"].as_str())
            .unwrap_or_default()
    };
    for node in nodes.values() {
        let Some(pkg) = node["id"].as_str().and_then(|id| by_id.get(id)) else {
            continue;
        };
        let name = pkg["name"].as_str().unwrap_or_default();
        let version = pkg["version"].as_str().unwrap_or_default();
        let mut why = Vec::new();
        if let Some(links) = pkg["links"].as_str() {
            why.push(format!("`links = \"{links}\"`"));
        }
        for build_dep in deps_of(node, Some("build")) {
            let dep_name = name_of(build_dep);
            if C_BUILD_TOOLS.contains(&dep_name) {
                why.push(format!("build-depends on `{dep_name}`"));
                continue;
            }
            // One level down: a build helper such as `cxx-build` that uses `cc` itself.
            let Some(dep_node) = nodes.get(build_dep) else {
                continue;
            };
            for tool in deps_of(dep_node, None).into_iter().map(name_of) {
                if C_BUILD_TOOLS.contains(&tool) {
                    why.push(format!(
                        "build-depends on `{dep_name}`, which depends on `{tool}`"
                    ));
                }
            }
        }
        if why.is_empty() {
            continue;
        }
        let at = format!("{name} {version}");
        match NATIVE_ALLOWLIST.iter().find(|(n, _, _)| *n == name) {
            None => findings.push(Finding {
                at,
                rule: "R-DEP-002",
                msg: format!(
                    "native code ({}) is not on the lint-deps allowlist (ADR-0031)",
                    why.join(", ")
                ),
            }),
            Some((_, Some(feature), _)) if !strs(&node["features"]).any(|f| f == *feature) => {
                findings.push(Finding {
                    at,
                    rule: "R-DEP-002",
                    msg: format!(
                        "allowlisted only with feature `{feature}`, which is not enabled ({})",
                        why.join(", ")
                    ),
                });
            }
            Some(_) => {}
        }
    }
}

/// The package ids a resolve node depends on with the given kind (`None`: normal).
fn deps_of<'v>(node: &'v Value, kind: Option<&str>) -> Vec<&'v str> {
    node["deps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| {
            d["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].as_str() == kind)
        })
        .filter_map(|d| d["pkg"].as_str())
        .collect()
}

/// The strings in a JSON array (nothing for anything else).
fn strs(v: &Value) -> impl Iterator<Item = &str> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str)
}

/// `path` relative to the workspace root, with `/` separators, for messages.
fn rel(ws_root: &str, path: &str) -> String {
    Path::new(path).strip_prefix(ws_root).map_or_else(
        |_| path.to_owned(),
        |p| p.to_string_lossy().replace('\\', "/"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway Cargo workspace, removed on drop even when an assertion fails.
    struct Fixture(std::path::PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl Fixture {
        /// A workspace named after the test with `crates/*` members, plus `files`.
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let dir =
                std::env::temp_dir().join(format!("xtask-lint-deps-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let f = Self(dir);
            f.write(
                "Cargo.toml",
                "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n\
                 [workspace.dependencies]\n\
                 a = { path = \"crates/a\" }\nb = { path = \"crates/b\" }\n\
                 c = { path = \"crates/c\" }\njstream-core = { path = \"crates/jstream-core\" }\n\
                 native = { path = \"crates/native\" }\n",
            );
            for (path, text) in files {
                f.write(path, text);
            }
            f
        }

        fn write(&self, path: &str, text: &str) {
            let p = self.0.join(path);
            std::fs::create_dir_all(p.parent().expect("invariant: files live in the fixture"))
                .expect("invariant: temp dir is writable");
            std::fs::write(p, text).expect("invariant: temp dir is writable");
        }

        /// `rule: msg` for every finding, in order.
        fn findings(&self) -> Vec<String> {
            let meta = cargo_metadata(&self.0, false).expect("invariant: fixture is a workspace");
            check(&meta)
                .expect("invariant: the fixture is readable")
                .into_iter()
                .map(|f| format!("{}: {}: {}", f.at, f.rule, f.msg))
                .collect()
        }
    }

    /// A member crate: its manifest and a crate root, as fixture files.
    fn krate(name: &str, meta: &str, deps: &str, root: &str) -> [(String, String); 2] {
        [
            (
                format!("crates/{name}/Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
                     {meta}\n[dependencies]\n{deps}"
                ),
            ),
            (format!("crates/{name}/src/lib.rs"), root.to_owned()),
        ]
    }

    fn meta(layer: i64, unsafe_allowed: bool) -> String {
        format!(
            "[package.metadata.jstream]\nlayer = {layer}\nsummary = \"s\"\nunsafe_allowed = {unsafe_allowed}\n"
        )
    }

    const SAFE: &str = "#![forbid(unsafe_code)]\n";

    fn fixture(name: &str, crates: &[[(String, String); 2]]) -> Fixture {
        let files: Vec<(&str, &str)> = crates
            .iter()
            .flatten()
            .map(|(p, t)| (p.as_str(), t.as_str()))
            .collect();
        Fixture::new(name, &files)
    }

    #[test]
    fn clean_workspace_passes() {
        let f = fixture(
            "clean",
            &[
                krate("a", &meta(0, false), "", SAFE),
                krate("b", &meta(1, false), "a.workspace = true\n", SAFE),
            ],
        );
        assert_eq!(f.findings(), Vec::<String>::new());
    }

    #[test]
    fn same_or_higher_layer_dependency_fails() {
        let f = fixture(
            "layers",
            &[
                krate("a", &meta(1, false), "", SAFE),
                krate("b", &meta(1, false), "a.workspace = true\n", SAFE),
                krate("c", &meta(0, false), "b.workspace = true\n", SAFE),
            ],
        );
        let got = f.findings();
        assert_eq!(got.len(), 2, "{got:#?}");
        assert!(got[0].contains("R-ARCH-002: `b` (layer 1) depends on `a` (layer 1)"));
        assert!(got[1].contains("R-ARCH-002: `c` (layer 0) depends on `b` (layer 1)"));
    }

    #[test]
    fn missing_or_malformed_metadata_fails() {
        let f = fixture(
            "metadata",
            &[
                krate("a", "", "", SAFE),
                krate(
                    "b",
                    "[package.metadata.jstream]\nlayer = \"one\"\nsummary = \"\"\n",
                    "",
                    SAFE,
                ),
            ],
        );
        let got = f.findings();
        assert_eq!(got.len(), 4, "{got:#?}");
        assert!(got[0].contains("crates/a/Cargo.toml: R-ARCH-005: `a` has no"));
        assert!(got.iter().any(|g| g.contains("`b`: `layer` must be")));
        assert!(got.iter().any(|g| g.contains("`b`: `summary` must be")));
        assert!(
            got.iter()
                .any(|g| g.contains("`b`: `unsafe_allowed` must be"))
        );
    }

    #[test]
    fn client_reaching_engine_fails_even_transitively() {
        // `jstream-mcp` -> `a` -> `jstream-core`; the layers are made consistent on purpose so
        // only R-ARCH-003 fires.
        let f = fixture(
            "clients",
            &[
                krate("jstream-core", &meta(0, false), "", SAFE),
                krate(
                    "a",
                    &meta(1, false),
                    "jstream-core.workspace = true\n",
                    SAFE,
                ),
                krate("jstream-mcp", &meta(2, false), "a.workspace = true\n", SAFE),
            ],
        );
        let got = f.findings();
        assert_eq!(
            got,
            [
                "crates/jstream-mcp/Cargo.toml: R-ARCH-003: client `jstream-mcp` depends on \
                 the engine: jstream-mcp -> a -> jstream-core"
            ]
        );
    }

    #[test]
    fn unsafe_declarations_are_checked() {
        let f = fixture(
            "unsafe",
            &[
                krate("a", &meta(0, true), "", ""),
                krate("b", &meta(0, false), "", "//! No forbid here.\n"),
                krate("c-sys", &meta(0, true), "", ""),
                krate("jstream-platform-windows", &meta(0, true), "", ""),
            ],
        );
        let got = f.findings();
        assert_eq!(got.len(), 2, "{got:#?}");
        assert!(got[0].starts_with("crates/a/Cargo.toml: R-UNSAFE-001: `a` declares"));
        assert!(got[1].starts_with("crates/b/src/lib.rs: R-UNSAFE-001:"));
    }

    #[test]
    fn forbid_attribute_forms() {
        assert!(forbids_unsafe("//! Doc.\n\n#![forbid(unsafe_code)]\n"));
        assert!(forbids_unsafe("#![forbid(missing_docs, unsafe_code)]\n"));
        assert!(!forbids_unsafe("#![deny(unsafe_code)]\n"));
        assert!(!forbids_unsafe(
            "// #![forbid(unsafe_code)] is commented out\n"
        ));
        // rustfmt wraps long attribute lists over several lines. (`\x23` is `#`, so the
        // governance check's text scan doesn't report this string as a suppression.)
        assert!(forbids_unsafe(
            "//! Doc.\n\x23![allow(\n    clippy::x,\n    reason = \"]\"\n)]\n#![forbid(\n    unsafe_code,\n)]\n"
        ));
        assert!(forbids_unsafe(
            "/* licence\n /* nested */ */\n#![forbid(missing_docs,\n unsafe_code)]\n"
        ));
        // Only leading inner attributes count (review N3 on #16).
        assert!(!forbids_unsafe(
            "/*\n#![forbid(unsafe_code)]\n*/\nfn f() {}\n"
        ));
        assert!(!forbids_unsafe(
            "//! Doc.\nfn f() {}\n#![forbid(unsafe_code)]\n"
        ));
        assert!(!forbids_unsafe(
            "const S: &str = \"\n#![forbid(unsafe_code)]\n\";\n"
        ));
        assert!(!forbids_unsafe("#![forbid(unsafe_code_lookalike)]\n"));
    }

    #[test]
    fn dependencies_not_from_the_workspace_fail() {
        let f = fixture(
            "workspace-deps",
            &[
                krate("a", &meta(0, false), "", SAFE),
                krate(
                    "b",
                    &meta(1, false),
                    "a = { path = \"../a\" }\n\n[target.'cfg(unix)'.dev-dependencies]\n\
                     c = { path = \"../c\" }\n",
                    SAFE,
                ),
                krate("c", &meta(0, false), "", SAFE),
            ],
        );
        let got = f.findings();
        assert_eq!(got.len(), 2, "{got:#?}");
        assert!(got[0].contains("R-DEP-003: [dependencies] `a` must be `a.workspace = true`"));
        assert!(got[1].contains("[target.cfg(unix).dev-dependencies] `c`"));
    }

    #[test]
    fn native_links_needs_the_allowlist() {
        let mut native = krate("native", &meta(0, false), "", SAFE);
        native[0].1 = native[0].1.replace(
            "edition = \"2024\"\n",
            "edition = \"2024\"\nlinks = \"foo\"\nbuild = \"build.rs\"\n",
        );
        let f = fixture(
            "native",
            &[
                native,
                krate("b", &meta(1, false), "native.workspace = true\n", SAFE),
            ],
        );
        f.write("crates/native/build.rs", "fn main() {}\n");
        let got = f.findings();
        assert_eq!(
            got,
            [
                "native 0.0.0: R-DEP-002: native code (`links = \"foo\"`) is not on the \
              lint-deps allowlist (ADR-0031)"
            ]
        );
    }

    #[test]
    fn c_build_script_needs_the_allowlisted_feature() {
        let mut b3 = krate("blake3", &meta(1, false), "", SAFE);
        b3[0]
            .1
            .push_str("\n[build-dependencies]\ncc.workspace = true\n\n[features]\npure = []\n");
        let f = fixture(
            "c-build",
            &[
                b3,
                krate("cc", &meta(0, false), "", SAFE),
                krate("b", &meta(2, false), "blake3.workspace = true\n", SAFE),
            ],
        );
        f.write("crates/blake3/build.rs", "fn main() {}\n");
        let root = "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n\
                    [workspace.dependencies]\nblake3 = { path = \"crates/blake3\" }\n\
                    cc = { path = \"crates/cc\" }\n";
        f.write("Cargo.toml", root);
        assert_eq!(
            f.findings(),
            [
                "blake3 0.0.0: R-DEP-002: allowlisted only with feature `pure`, which is not \
              enabled (build-depends on `cc`)"
            ]
        );

        f.write(
            "Cargo.toml",
            &root.replace(
                "crates/blake3\" }",
                "crates/blake3\", features = [\"pure\"] }",
            ),
        );
        assert_eq!(f.findings(), Vec::<String>::new());
    }

    /// A build helper that uses `cc` itself (`cxx-build` -> `cc`) means the build script
    /// compiles C too, one level down (review N4 on #16). The helper's own normal
    /// dependency on `cc` is not a finding.
    #[test]
    fn c_build_helper_one_level_down_needs_the_allowlist() {
        let mut x = krate("x", &meta(2, false), "", SAFE);
        x[0].1
            .push_str("\n[build-dependencies]\ncxx-build.workspace = true\n");
        let f = fixture(
            "c-helper",
            &[
                x,
                krate("cc", &meta(0, false), "", SAFE),
                krate("cxx-build", &meta(1, false), "cc.workspace = true\n", SAFE),
            ],
        );
        f.write("crates/x/build.rs", "fn main() {}\n");
        f.write(
            "Cargo.toml",
            "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n\n\
             [workspace.dependencies]\ncc = { path = \"crates/cc\" }\n\
             cxx-build = { path = \"crates/cxx-build\" }\n",
        );
        assert_eq!(
            f.findings(),
            [
                "x 0.0.0: R-DEP-002: native code (build-depends on `cxx-build`, which depends \
                 on `cc`) is not on the lint-deps allowlist (ADR-0031)"
            ]
        );
    }
}
