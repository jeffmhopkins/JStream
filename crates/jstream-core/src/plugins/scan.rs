//! What the engine knows of its CLAP plugin scan, for `plugin.clap_scan` and
//! `plugin.clap_rescan` (protocol 0.95, ADR-0035): the plugin hosts the host gave it,
//! why there are none when it knows, and the last finished scan. A scan holds
//! [`ClapScans::scan_lock`] while it runs, so a rescan that arrives during one waits for
//! it, then scans again. None of these locks is taken with the collection lock held.

use super::kinds::ClapScan;
use jstream_platform::PluginHosts;
use jstream_protocol::{ClapFailed, ClapFolder, ClapFound, ClapScanInfo};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// The engine's CLAP scan state.
#[derive(Default)]
pub(crate) struct ClapScans {
    /// The plugin hosts, once the host gives them.
    hosts: Mutex<Option<Arc<dyn PluginHosts>>>,
    /// Why the engine hosts no CLAP plugins, when the host said.
    unavailable: Mutex<Option<String>>,
    /// The last finished scan.
    last: Mutex<Option<ClapScan>>,
    /// How many scans are running or waiting to run.
    scanning: AtomicUsize,
    /// Held while a scan runs.
    pub(crate) scan_lock: Mutex<()>,
}

/// Counts a scan as running from its creation until it is dropped.
pub(crate) struct Scanning<'a>(&'a AtomicUsize);

impl Drop for Scanning<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The most of a failure reason `plugin.clap_scan` reports, in characters. A plugin
/// host's error can carry a long stderr; the whole of it is in the log.
const REASON_MAX: usize = 300;

/// `reason`, cut to [`REASON_MAX`] characters with an ellipsis when longer.
fn clipped(reason: &str) -> String {
    match reason.char_indices().nth(REASON_MAX) {
        Some((end, _)) => format!("{}…", &reason[..end]),
        None => reason.to_owned(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ClapScans {
    /// Keeps `hosts` for later rescans.
    pub(crate) fn set_hosts(&self, hosts: Arc<dyn PluginHosts>) {
        *lock(&self.hosts) = Some(hosts);
        *lock(&self.unavailable) = None;
    }

    /// The hosts, if the host gave them.
    pub(crate) fn hosts(&self) -> Option<Arc<dyn PluginHosts>> {
        lock(&self.hosts).clone()
    }

    /// Records why the engine hosts no CLAP plugins.
    pub(crate) fn set_unavailable(&self, reason: String) {
        if lock(&self.hosts).is_none() {
            *lock(&self.unavailable) = Some(reason);
        }
    }

    /// Marks a scan as running (or waiting for the one running) until the guard drops.
    pub(crate) fn begin(&self) -> Scanning<'_> {
        self.scanning.fetch_add(1, Ordering::SeqCst);
        Scanning(&self.scanning)
    }

    /// Keeps `scan` as the last finished one.
    pub(crate) fn finished(&self, scan: &ClapScan) {
        *lock(&self.last) = Some(scan.clone());
    }

    /// What `plugin.clap_scan` answers. Reads whether each search folder exists, so it
    /// touches the file system, never under the collection lock.
    pub(crate) fn info(&self) -> ClapScanInfo {
        let Some(hosts) = self.hosts() else {
            return ClapScanInfo::unavailable(lock(&self.unavailable).clone());
        };
        let folders = hosts
            .folders()
            .into_iter()
            .map(|path| {
                let exists = path.is_dir();
                ClapFolder::new(path.display().to_string(), exists)
            })
            .collect();
        let last = lock(&self.last).clone();
        let scanned = last.is_some();
        let scan = last.unwrap_or_default();
        let found = scan
            .kinds
            .iter()
            .map(|k| {
                ClapFound::new(
                    k.descriptor.kind_id.clone(),
                    k.descriptor.name.clone(),
                    k.file.display().to_string(),
                )
            })
            .collect();
        let failed = scan
            .failed
            .iter()
            .map(|(file, reason)| ClapFailed::new(file.display().to_string(), clipped(reason)))
            .collect();
        ClapScanInfo::available(
            folders,
            self.scanning.load(Ordering::SeqCst) > 0,
            scanned,
            found,
            failed,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_failure_reason_is_clipped() {
        assert_eq!(clipped("short"), "short");
        let long = "é".repeat(REASON_MAX + 5);
        let clipped = clipped(&long);
        assert_eq!(clipped.chars().count(), REASON_MAX + 1);
        assert!(clipped.ends_with('…'));
    }
    use crate::plugins::kinds::ClapKind;
    use jstream_platform::PlatformError;
    use jstream_plugin_proto::control::PluginDesc;
    use jstream_protocol::{KindCategory, KindDescriptor};
    use std::path::{Path, PathBuf};

    struct Hosts(Vec<PathBuf>);

    impl PluginHosts for Hosts {
        fn search_paths(&self) -> Vec<PathBuf> {
            self.0.iter().filter(|p| p.is_dir()).cloned().collect()
        }
        fn folders(&self) -> Vec<PathBuf> {
            self.0.clone()
        }
        fn scan(&self, _: &Path) -> Result<Vec<PluginDesc>, PlatformError> {
            Ok(Vec::new())
        }
        fn start(
            &self,
            _: &jstream_platform::HostRequest,
        ) -> Result<Box<dyn jstream_platform::PluginHost>, PlatformError> {
            Err(PlatformError::Unsupported {
                what: "test".into(),
            })
        }
    }

    #[test]
    fn without_hosts_the_scan_is_unavailable_with_the_reason() {
        let scans = ClapScans::default();
        assert_eq!(scans.info(), ClapScanInfo::unavailable(None));
        scans.set_unavailable("no host".into());
        assert_eq!(
            scans.info(),
            ClapScanInfo::unavailable(Some("no host".into()))
        );
    }

    #[test]
    fn the_info_names_each_folder_the_last_scan_and_a_running_one() {
        let dir = std::env::temp_dir();
        let missing = dir.join("jstream-no-such-clap-folder");
        let scans = ClapScans::default();
        scans.set_unavailable("ignored once hosts exist".into());
        scans.set_hosts(Arc::new(Hosts(vec![dir.clone(), missing.clone()])));
        let info = scans.info();
        assert!(info.available && !info.scanned && !info.scanning);
        assert_eq!(
            info.folders,
            vec![
                ClapFolder::new(dir.display().to_string(), true),
                ClapFolder::new(missing.display().to_string(), false),
            ]
        );
        {
            let _running = scans.begin();
            assert!(scans.info().scanning);
        }
        assert!(!scans.info().scanning);
        let descriptor = KindDescriptor {
            kind_id: "clap/com.example.verb".into(),
            category: KindCategory::AudioFilter,
            name: "Verb".into(),
            version: 1,
            video: false,
            audio: true,
            params: Vec::new(),
            max_outset_px: None,
        };
        scans.finished(&ClapScan {
            kinds: vec![ClapKind {
                descriptor,
                file: dir.join("Verb.clap"),
                plugin_id: "com.example.verb".into(),
            }],
            failed: vec![(dir.join("Bad.clap"), "it crashed".into())],
        });
        let info = scans.info();
        assert!(info.scanned);
        assert_eq!(
            info.found,
            vec![ClapFound::new(
                "clap/com.example.verb",
                "Verb",
                dir.join("Verb.clap").display().to_string()
            )]
        );
        assert_eq!(
            info.failed,
            vec![ClapFailed::new(
                dir.join("Bad.clap").display().to_string(),
                "it crashed"
            )]
        );
    }
}
