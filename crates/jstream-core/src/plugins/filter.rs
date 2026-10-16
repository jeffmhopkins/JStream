//! One CLAP filter in a source's audio chain (ADR-0035): its [`HostSupervisor`], and
//! its settings sent to the plugin as parameter changes.
//!
//! The chain ([`crate::filters::Chain`]) owns one [`ClapFilter`] per CLAP filter it
//! runs, keyed by filter ID, and ticks it on every pass of the `jstream-audio-sources`
//! thread. Once the host is ready the chain adds the mixer's CLAP filter on its block;
//! while the host is down the mixer passes the dry signal (ADR-0035 › 3). A chain
//! rebuilt around it (a filter added, removed or moved) adds a new mixer filter on the
//! same block, so the host keeps running.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use jstream_platform::{HostBlock, HostRequest, PluginHosts};
use jstream_plugin_proto::{Config, DEFAULT_DEPTH, ParamEvent};
use jstream_protocol::Settings;
use serde_json::Value;

use super::kinds::plugin_text;
use super::supervisor::{HostEvent, HostSupervisor, SupervisorPolicy};

/// What runs the CLAP filter kinds a scan found: the host starter, and each kind's
/// plugin. Given to the audio thread by [`crate::Engine::scan_clap_plugins`].
pub(crate) struct ClapCatalog {
    pub(crate) hosts: Arc<dyn PluginHosts>,
    /// Each `clap/` kind's plugin file and CLAP id.
    pub(crate) plugins: BTreeMap<String, (PathBuf, String)>,
    pub(crate) policy: SupervisorPolicy,
}

impl std::fmt::Debug for ClapCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClapCatalog")
            .field("plugins", &self.plugins)
            .finish_non_exhaustive()
    }
}

/// One CLAP filter's host, and what its plugin has been told.
pub(crate) struct ClapFilter {
    supervisor: HostSupervisor,
    /// The settings sent to the running host's plugin. Cleared when a new host gets
    /// ready, so it is sent every setting.
    sent: Settings,
}

/// What a [`ClapFilter::tick`] means for the chain.
#[derive(Default)]
pub(crate) struct Ticked {
    /// The filter's block changed (a host got ready, or the filter was disabled): the
    /// chain is rebuilt on the current block.
    pub(crate) block_changed: bool,
    /// What to log.
    pub(crate) notes: Vec<String>,
}

impl ClapFilter {
    /// A filter of `kind` whose host starts at the next tick, for a mixer at `rate`
    /// with blocks of `block_frames`. `None` when the catalog has no such kind.
    pub(crate) fn new(
        catalog: &ClapCatalog,
        kind: &str,
        rate: u32,
        block_frames: usize,
        now: Instant,
    ) -> Option<Self> {
        let (file, plugin_id) = catalog.plugins.get(kind)?;
        let request = HostRequest {
            file: file.clone(),
            plugin_id: plugin_id.clone(),
            config: Config {
                sample_rate: rate,
                block_frames: u32::try_from(block_frames).unwrap_or(u32::MAX),
                depth: DEFAULT_DEPTH,
            },
            state: None,
        };
        Some(Self {
            supervisor: HostSupervisor::new(
                Arc::clone(&catalog.hosts),
                request,
                catalog.policy,
                now,
            ),
            sent: Settings::new(),
        })
    }

    /// The running host's block, while it is ready.
    pub(crate) fn block(&self) -> Option<&HostBlock> {
        self.supervisor.block()
    }

    /// Advances the supervisor to `now`.
    pub(crate) fn tick(&mut self, kind: &str, now: Instant) -> Ticked {
        let mut ticked = Ticked::default();
        for event in self.supervisor.tick(now) {
            match event {
                HostEvent::Ready { .. } => {
                    self.sent = Settings::new();
                    ticked.block_changed = true;
                }
                HostEvent::Crashed { reason, retry_in } => ticked.notes.push(format!(
                    "{kind} filter's plugin host crashed ({reason}); bypassed, restarting in {} ms",
                    retry_in.as_millis()
                )),
                HostEvent::Disabled { reason } => {
                    ticked.block_changed = true;
                    ticked
                        .notes
                        .push(format!("{kind} filter is disabled and bypassed: {reason}"));
                }
                HostEvent::Error(message) => ticked.notes.push(format!(
                    "{kind} filter's plugin host reported: {}",
                    plugin_text(&message)
                )),
                HostEvent::Degraded => ticked.notes.push(format!(
                    "{kind} filter's plugin host is missing blocks; they pass dry"
                )),
                HostEvent::Recovered => ticked
                    .notes
                    .push(format!("{kind} filter's plugin host keeps up again")),
                // The plugin's own parameter changes and saved state aren't written back
                // to the filter's settings yet.
                HostEvent::Params(_) | HostEvent::ParamChanged(_) | HostEvent::StateSaved => {}
            }
        }
        ticked
    }

    /// Sends the running plugin each of `settings` that changed since it was last sent,
    /// through the block's parameter ring. A setting the ring can't take now is sent
    /// on a later pass. A setting that isn't a CLAP parameter id with a finite number
    /// is never sent.
    pub(crate) fn send(&mut self, settings: &Settings) {
        let Some(block) = self.supervisor.block() else {
            return;
        };
        let ring = &(**block).as_ref().params_in;
        for (id, value) in settings.iter() {
            if self.sent.get(id) == Some(value) {
                continue;
            }
            let Some(event) = param_event(id, value) else {
                continue;
            };
            if !ring.push(event) {
                return;
            }
            self.sent.insert(id.to_owned(), value.clone());
        }
    }
}

/// The parameter change for setting `id` = `value`: `id` is a CLAP parameter id in
/// decimal (the kind's parameters are named so) and `value` a finite number.
fn param_event(id: &str, value: &Value) -> Option<ParamEvent> {
    let id = id.parse().ok()?;
    let value = value.as_f64().filter(|v| v.is_finite())?;
    Some(ParamEvent { id, value })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_platform::plugin_host::fake::FakePluginHosts;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn only_numeric_ids_with_finite_values_are_parameters() {
        assert_eq!(
            param_event("7", &json!(0.5)),
            Some(ParamEvent { id: 7, value: 0.5 })
        );
        assert_eq!(
            param_event("7", &json!(3)),
            Some(ParamEvent { id: 7, value: 3.0 })
        );
        assert_eq!(param_event("x", &json!(0.5)), None);
        assert_eq!(param_event("-1", &json!(0.5)), None);
        assert_eq!(param_event("7", &json!("0.5")), None);
        assert_eq!(param_event("7", &Value::Null), None);
    }

    /// Settings reach the plugin once its host is ready, each once, and only changes
    /// after that.
    #[test]
    fn settings_are_sent_once_the_host_is_ready() {
        let fake = FakePluginHosts::new();
        let catalog = ClapCatalog {
            hosts: Arc::new(fake.clone()),
            plugins: BTreeMap::from([(
                "clap/org.example.gain".to_owned(),
                ("a.clap".into(), "org.example.gain".to_owned()),
            )]),
            policy: SupervisorPolicy::default(),
        };
        assert!(ClapFilter::new(&catalog, "clap/other", 48_000, 64, Instant::now()).is_none());
        let mut filter = ClapFilter::new(
            &catalog,
            "clap/org.example.gain",
            48_000,
            64,
            Instant::now(),
        )
        .expect("a known kind");
        let settings: Settings = [("1", json!(0.25)), ("label", json!("x"))]
            .into_iter()
            .collect();
        filter.send(&settings);
        let deadline = Instant::now() + Duration::from_secs(5);
        while filter.block().is_none() {
            assert!(Instant::now() < deadline, "the host never got ready");
            let ticked = filter.tick("clap/org.example.gain", Instant::now());
            assert!(ticked.notes.is_empty(), "{:?}", ticked.notes);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(fake.hosts()[0].request.config.block_frames, 64);
        filter.send(&settings);
        filter.send(&settings);
        let block = Arc::clone(filter.block().expect("ready"));
        let ring = &(*block).as_ref().params_in;
        assert_eq!(ring.pop(), Some(ParamEvent { id: 1, value: 0.25 }));
        assert_eq!(ring.pop(), None);
        let louder: Settings = [("1", json!(0.75))].into_iter().collect();
        filter.send(&louder);
        assert_eq!(ring.pop(), Some(ParamEvent { id: 1, value: 0.75 }));
        assert_eq!(ring.pop(), None);
    }
}
