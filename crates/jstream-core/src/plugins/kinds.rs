//! CLAP filter kinds (ADR-0035 › 6): each audio effect a scan finds becomes an
//! audio-filter kind, `clap/<plugin id>`, whose parameters are the plugin's.

use std::path::{Path, PathBuf};

use jstream_platform::PluginHosts;
use jstream_plugin_proto::control::{
    PARAM_BYPASS, PARAM_HIDDEN, PARAM_READ_ONLY, PARAM_STEPPED, ParamDesc, PluginDesc,
};
use jstream_protocol::{KindCategory, KindDescriptor, ParamDescriptor, ParamType};
use serde_json::json;

/// The prefix of every CLAP kind's ID.
pub const CLAP_KIND_PREFIX: &str = "clap/";

/// The most characters of a plugin's text that reach clients.
const MAX_PLUGIN_TEXT: usize = 128;

/// Text a plugin chose (a name, a parameter's name or module), as clients and agents
/// may read it (#351): control characters become spaces, it is trimmed, and cut to
/// [`MAX_PLUGIN_TEXT`] characters. Clients render it as text only.
pub(crate) fn plugin_text(text: &str) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_PLUGIN_TEXT)
        .collect();
    text.trim().to_owned()
}

/// How deep a scan looks below each search directory, as CLAP's search is recursive.
const MAX_SCAN_DEPTH: usize = 8;

/// One CLAP filter kind a scan found: its descriptor, and what starts its host.
#[derive(Clone, Debug, PartialEq)]
pub struct ClapKind {
    /// The kind, `clap/<plugin id>`, an audio filter.
    pub descriptor: KindDescriptor,
    /// The plugin file it is in.
    pub file: PathBuf,
    /// The plugin's CLAP id within the file.
    pub plugin_id: String,
}

/// What [`Engine::scan_clap_plugins`](crate::Engine::scan_clap_plugins) found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClapScan {
    /// Every hostable effect, ordered by kind ID. When two files offer the same plugin
    /// id, the first file found wins.
    pub kinds: Vec<ClapKind>,
    /// Each plugin file that couldn't be scanned, and why.
    pub failed: Vec<(PathBuf, String)>,
}

/// Scans every `.clap` file under `hosts`' search directories, each in its own
/// short-lived host process.
pub fn scan(hosts: &dyn PluginHosts) -> ClapScan {
    let mut files = Vec::new();
    for dir in hosts.search_paths() {
        find_clap_files(&dir, 0, &mut files);
    }
    scan_files(hosts, &files)
}

/// Scans `files`, in order.
pub fn scan_files(hosts: &dyn PluginHosts, files: &[PathBuf]) -> ClapScan {
    let mut scan = ClapScan::default();
    for file in files {
        match hosts.scan(file) {
            Ok(plugins) => {
                for plugin in &plugins {
                    let kind = ClapKind {
                        descriptor: descriptor(plugin),
                        file: file.clone(),
                        plugin_id: plugin.id.clone(),
                    };
                    if !scan
                        .kinds
                        .iter()
                        .any(|k| k.descriptor.kind_id == kind.descriptor.kind_id)
                    {
                        scan.kinds.push(kind);
                    }
                }
            }
            Err(e) => scan.failed.push((file.clone(), e.to_string())),
        }
    }
    scan.kinds
        .sort_by(|a, b| a.descriptor.kind_id.cmp(&b.descriptor.kind_id));
    scan
}

/// Collects the `.clap` entries under `dir`, in name order. A `.clap` entry is a plugin
/// (a file on Windows and Linux, a bundle directory on macOS), so it isn't entered.
fn find_clap_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        let is_clap = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("clap"));
        if is_clap {
            out.push(path);
        } else if depth < MAX_SCAN_DEPTH && path.is_dir() {
            find_clap_files(&path, depth + 1, out);
        }
    }
}

/// The kind for `plugin`. Its parameters are the plugin's own, minus the hidden,
/// read-only and bypass ones and any without a finite range; each is identified by its
/// CLAP parameter id, in decimal, and applies live.
pub fn descriptor(plugin: &PluginDesc) -> KindDescriptor {
    KindDescriptor {
        kind_id: format!("{CLAP_KIND_PREFIX}{}", plugin.id),
        category: KindCategory::AudioFilter,
        name: match plugin_text(&plugin.name) {
            name if name.is_empty() => plugin_text(&plugin.id),
            name => name,
        },
        version: 1,
        video: false,
        audio: true,
        max_outset_px: None,
        params: plugin.params.iter().filter_map(param).collect(),
    }
}

fn param(p: &ParamDesc) -> Option<ParamDescriptor> {
    if p.flags & (PARAM_HIDDEN | PARAM_READ_ONLY | PARAM_BYPASS) != 0
        || !(p.min.is_finite() && p.max.is_finite() && p.default.is_finite())
        || p.min > p.max
    {
        return None;
    }
    let (ty, default) = if p.flags & PARAM_STEPPED != 0 {
        let (min, max) = (p.min.ceil(), p.max.floor());
        if min > max || min < i64::MIN as f64 || max > i64::MAX as f64 {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "whole numbers checked to be within i64's range"
        )]
        let (min, max, default) = (
            min as i64,
            max as i64,
            p.default.round().clamp(min, max) as i64,
        );
        (
            ParamType::Int {
                min: Some(min),
                max: Some(max),
                step: Some(1),
                unit: None,
            },
            json!(default),
        )
    } else {
        (
            ParamType::Float {
                min: Some(p.min),
                max: Some(p.max),
                step: None,
                unit: None,
            },
            json!(p.default.clamp(p.min, p.max)),
        )
    };
    Some(ParamDescriptor {
        id: p.id.to_string(),
        ty,
        default,
        label: match plugin_text(&p.name) {
            label if label.is_empty() => format!("Parameter {}", p.id),
            label => label,
        },
        description: None,
        group: Some(plugin_text(&p.module)).filter(|group| !group.is_empty()),
        advanced: false,
        visible_if: None,
        live: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_platform::PlatformError;
    use jstream_platform::plugin_host::fake::{FakePluginHosts, fake_param};
    use jstream_plugin_proto::control::PARAM_ENUM;

    fn p(id: u32, flags: u32, min: f64, max: f64, default: f64) -> ParamDesc {
        ParamDesc {
            id,
            name: format!("P{id}"),
            module: if id == 2 { "Out".into() } else { String::new() },
            min,
            max,
            default,
            flags,
        }
    }

    fn plugin(id: &str, params: Vec<ParamDesc>) -> PluginDesc {
        PluginDesc {
            id: id.into(),
            name: String::new(),
            vendor: "Example".into(),
            version: "1.0".into(),
            description: String::new(),
            params,
        }
    }

    #[test]
    fn plugin_parameters_become_settings() {
        let kind = descriptor(&plugin(
            "org.example.gain",
            vec![
                p(1, 0, -24.0, 24.0, 0.0),
                p(2, PARAM_STEPPED | PARAM_ENUM, 0.2, 3.7, 9.0),
                p(3, PARAM_HIDDEN, 0.0, 1.0, 0.0),
                p(4, PARAM_READ_ONLY, 0.0, 1.0, 0.0),
                p(5, PARAM_BYPASS | PARAM_STEPPED, 0.0, 1.0, 0.0),
                p(6, 0, 0.0, f64::INFINITY, 0.0),
                p(7, 0, 1.0, 0.0, 0.5),
                p(8, PARAM_STEPPED, 0.2, 0.8, 0.5),
            ],
        ));
        assert_eq!(kind.kind_id, "clap/org.example.gain");
        assert_eq!(kind.name, "org.example.gain");
        assert_eq!(kind.category, KindCategory::AudioFilter);
        assert!(kind.audio && !kind.video);
        let ids: Vec<&str> = kind.params.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["1", "2"]);
        assert_eq!(
            kind.params[0].ty,
            ParamType::Float {
                min: Some(-24.0),
                max: Some(24.0),
                step: None,
                unit: None
            }
        );
        assert_eq!(
            kind.params[1].ty,
            ParamType::Int {
                min: Some(1),
                max: Some(3),
                step: Some(1),
                unit: None
            }
        );
        assert_eq!(kind.params[1].default, json!(3));
        assert_eq!(kind.params[1].group.as_deref(), Some("Out"));
        for p in &kind.params {
            assert!(p.live);
            assert_eq!(p.check(&p.default), Ok(()), "{}", p.id);
        }
    }

    /// A plugin's text reaches clients without control characters and bounded.
    #[test]
    fn plugin_text_is_bounded_and_plain() {
        let mut gain = plugin("org.example.gain", vec![p(1, 0, 0.0, 1.0, 0.0)]);
        gain.name = format!("Gain\n\u{1b}[2J{}", "x".repeat(500));
        gain.params[0].name = "\t\r\n".into();
        gain.params[0].module = " \u{7}Out ".into();
        let kind = descriptor(&gain);
        assert_eq!(kind.name.chars().count(), MAX_PLUGIN_TEXT);
        assert!(kind.name.starts_with("Gain  [2J"), "{}", kind.name);
        assert!(!kind.name.chars().any(char::is_control));
        assert_eq!(kind.params[0].label, "Parameter 1");
        assert_eq!(kind.params[0].group.as_deref(), Some("Out"));
    }

    #[test]
    fn a_scan_lists_each_plugin_once_and_reports_failures() {
        let hosts = FakePluginHosts::new();
        let mut gain = plugin("org.example.gain", vec![fake_param()]);
        gain.name = "Gain".into();
        hosts.add_file("b.clap", vec![gain.clone()]);
        hosts.add_file(
            "a.clap",
            vec![plugin("org.example.zeta", vec![]), gain.clone()],
        );
        let scan = scan_files(
            &hosts,
            &["a.clap".into(), "b.clap".into(), "missing.clap".into()],
        );
        let found: Vec<(&str, &Path)> = scan
            .kinds
            .iter()
            .map(|k| (k.descriptor.kind_id.as_str(), k.file.as_path()))
            .collect();
        assert_eq!(
            found,
            [
                ("clap/org.example.gain", Path::new("a.clap")),
                ("clap/org.example.zeta", Path::new("a.clap"))
            ]
        );
        assert_eq!(scan.kinds[0].plugin_id, "org.example.gain");
        assert_eq!(scan.kinds[0].descriptor.name, "Gain");
        assert_eq!(scan.failed.len(), 1);
        assert_eq!(scan.failed[0].0, Path::new("missing.clap"));
    }

    struct Dirs(Vec<PathBuf>, FakePluginHosts);

    impl PluginHosts for Dirs {
        fn search_paths(&self) -> Vec<PathBuf> {
            self.0.clone()
        }
        fn scan(&self, file: &Path) -> Result<Vec<PluginDesc>, PlatformError> {
            self.1.scan(file)
        }
        fn start(
            &self,
            request: &jstream_platform::HostRequest,
        ) -> Result<Box<dyn jstream_platform::PluginHost>, PlatformError> {
            self.1.start(request)
        }
    }

    #[test]
    fn a_scan_walks_the_search_directories_for_clap_files() {
        let root = std::env::temp_dir().join(format!("jstream-core-clap-{}", std::process::id()));
        let nested = root.join("Vendor").join("Sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(root.join("Bundle.CLAP").join("Contents")).unwrap();
        for f in [
            root.join("b.clap"),
            nested.join("a.clap"),
            root.join("x.dll"),
        ] {
            std::fs::write(f, b"").unwrap();
        }
        let fake = FakePluginHosts::new();
        for (file, id) in [
            (root.join("b.clap"), "org.b"),
            (nested.join("a.clap"), "org.a"),
            (root.join("Bundle.CLAP"), "org.bundle"),
        ] {
            fake.add_file(file, vec![plugin(id, vec![])]);
        }
        let found = scan(&Dirs(vec![root.clone(), root.join("missing")], fake));
        let ids: Vec<&str> = found
            .kinds
            .iter()
            .map(|k| k.descriptor.kind_id.as_str())
            .collect();
        assert_eq!(ids, ["clap/org.a", "clap/org.b", "clap/org.bundle"]);
        assert!(found.failed.is_empty(), "{:?}", found.failed);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
