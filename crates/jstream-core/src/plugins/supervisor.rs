//! `HostSupervisor`: keeps one CLAP filter instance's host process running (ADR-0035 ›
//! 3). It runs on a control thread, never the audio thread, and is driven by `tick`.
//!
//! - A host is started, then ready once the block says so. The caller then adds the
//!   mixer's CLAP filter on the host's block (removing any older one).
//! - A host that exits, stops beating its heartbeat for `LIVENESS_MS`, or doesn't get
//!   ready in time has crashed. It is killed, and started again after a backoff that
//!   doubles, with the plugin's last saved state. The mixer bypasses the filter
//!   meanwhile, so nothing else stops.
//! - After `max_crashes` crashes without a stable run between them, or a failure that a
//!   restart can't fix (the plugin won't load, isn't an effect, or won't activate), the
//!   filter is disabled, and stays so until [`HostSupervisor::enable`].
//! - The supervisor asks for the plugin's state whenever the plugin marks it changed, and
//!   keeps the latest, which a restart restores and the caller stores in the filter's
//!   settings.
//! - A run of consecutive late blocks in the mixer marks the filter degraded, and its end
//!   recovered.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use jstream_platform::{HostBlock, HostRequest, PluginHost, PluginHosts};
use jstream_plugin_proto::control::{FromHost, ParamDesc, ToHost};
use jstream_plugin_proto::{
    ERROR_ACTIVATE, ERROR_BAD_CONFIG, ERROR_LOAD, ERROR_NOT_AN_EFFECT, LIVENESS_MS, ParamEvent,
    STATE_FAILED, Shared,
};

/// How long [`HostSupervisor::stop`] waits for a host to stop by itself.
pub const STOP_GRACE: Duration = Duration::from_millis(500);

/// When the supervisor restarts and gives up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupervisorPolicy {
    /// The wait before the first restart. Each crash in a row doubles it.
    pub first_retry: Duration,
    /// The longest wait.
    pub max_retry: Duration,
    /// Crashes in a row that disable the filter.
    pub max_crashes: u32,
    /// How long a host must run for its earlier crashes to be forgotten.
    pub stable_after: Duration,
    /// How long a started host may take to get ready.
    pub start_timeout: Duration,
    /// Consecutive late blocks that mark the filter degraded.
    pub degraded_after: u32,
}

impl Default for SupervisorPolicy {
    fn default() -> Self {
        Self {
            first_retry: Duration::from_secs(1),
            max_retry: Duration::from_secs(30),
            max_crashes: 5,
            stable_after: Duration::from_secs(60),
            start_timeout: Duration::from_secs(10),
            degraded_after: 50,
        }
    }
}

/// Where the supervised host is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostStatus {
    /// Started, not ready yet.
    Starting,
    /// Ready: the filter's block is live.
    Running,
    /// Crashed, waiting to restart.
    Waiting,
    /// Given up on until re-enabled.
    Disabled,
    /// Stopped by the caller.
    Stopped,
}

/// What happened in a `tick`, for the caller to act on and report.
#[derive(Clone)]
pub enum HostEvent {
    /// A host is ready on `block`, with this plugin latency in frames. Put the mixer's
    /// CLAP filter on it, replacing any older one.
    Ready {
        /// The new host's block.
        block: HostBlock,
        /// The plugin's own latency, frames.
        latency: u32,
    },
    /// The plugin's parameters, after it starts and after it restarts itself.
    Params(Vec<ParamDesc>),
    /// The plugin changed one of its parameters (its own editor).
    ParamChanged(ParamEvent),
    /// A new saved state is in [`HostSupervisor::saved_state`].
    StateSaved,
    /// The host crashed, and restarts after `retry_in`.
    Crashed {
        /// What went wrong.
        reason: String,
        /// The wait before the restart.
        retry_in: Duration,
    },
    /// The filter is disabled.
    Disabled {
        /// Why.
        reason: String,
    },
    /// The host reported an error it keeps running through (a state it refused).
    Error(String),
    /// The mixer has missed `degraded_after` blocks in a row.
    Degraded,
    /// The mixer has the host's blocks again.
    Recovered,
}

impl std::fmt::Debug for HostEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready { latency, .. } => f
                .debug_struct("Ready")
                .field("latency", latency)
                .finish_non_exhaustive(),
            Self::Params(params) => f.debug_tuple("Params").field(params).finish(),
            Self::ParamChanged(event) => f.debug_tuple("ParamChanged").field(event).finish(),
            Self::StateSaved => f.write_str("StateSaved"),
            Self::Crashed { reason, retry_in } => f
                .debug_struct("Crashed")
                .field("reason", reason)
                .field("retry_in", retry_in)
                .finish(),
            Self::Disabled { reason } => {
                f.debug_struct("Disabled").field("reason", reason).finish()
            }
            Self::Error(message) => f.debug_tuple("Error").field(message).finish(),
            Self::Degraded => f.write_str("Degraded"),
            Self::Recovered => f.write_str("Recovered"),
        }
    }
}

/// Keeps one CLAP filter's host running. See the module docs.
pub struct HostSupervisor {
    hosts: Arc<dyn PluginHosts>,
    request: HostRequest,
    policy: SupervisorPolicy,
    status: HostStatus,
    host: Option<Box<dyn PluginHost>>,
    block: Option<HostBlock>,
    /// Crashes since the last stable run.
    crashes: u32,
    /// When the current host started, or when a waiting one restarts.
    since: Instant,
    /// The heartbeat last seen, and when it last changed.
    beat: (u64, Instant),
    degraded: bool,
}

impl std::fmt::Debug for HostSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostSupervisor")
            .field("request", &self.request)
            .field("status", &self.status)
            .field("crashes", &self.crashes)
            .finish_non_exhaustive()
    }
}

impl HostSupervisor {
    /// A supervisor that starts its host at the first `tick`. `request.state` is the
    /// state the filter's settings hold, restored at each start until the plugin saves a
    /// newer one.
    pub fn new(
        hosts: Arc<dyn PluginHosts>,
        request: HostRequest,
        policy: SupervisorPolicy,
        now: Instant,
    ) -> Self {
        Self {
            hosts,
            request,
            policy,
            status: HostStatus::Waiting,
            host: None,
            block: None,
            crashes: 0,
            since: now,
            beat: (0, now),
            degraded: false,
        }
    }

    /// Where the host is.
    pub fn status(&self) -> HostStatus {
        self.status
    }

    /// Crashes since the last stable run.
    pub fn crashes(&self) -> u32 {
        self.crashes
    }

    /// The latest state the plugin saved, or the one it started with.
    pub fn saved_state(&self) -> Option<&[u8]> {
        self.request.state.as_deref()
    }

    /// The running host's block, while it is ready.
    pub fn block(&self) -> Option<&HostBlock> {
        self.block.as_ref()
    }

    /// Starts a disabled or stopped filter again, at the next `tick`, with no crashes
    /// counted.
    pub fn enable(&mut self, now: Instant) {
        if matches!(self.status, HostStatus::Disabled | HostStatus::Stopped) {
            self.crashes = 0;
            self.since = now;
            self.status = HostStatus::Waiting;
        }
    }

    /// Asks the running host for the plugin's state; `StateSaved` follows.
    pub fn request_state(&mut self) {
        if let Some(host) = &mut self.host {
            host.send(&ToHost::SaveState);
        }
    }

    /// Stops the host: asks it to stop, so the plugin is deactivated cleanly, and ends it
    /// if it hasn't within [`STOP_GRACE`]. This waits, so it is for the control thread.
    pub fn stop(&mut self) {
        if let Some(mut host) = self.host.take() {
            if host.send(&ToHost::Stop) {
                let deadline = Instant::now() + STOP_GRACE;
                while !host.exited() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            host.kill();
        }
        self.block = None;
        self.status = HostStatus::Stopped;
    }

    /// Advances the supervisor to `now`, returning what happened, oldest first.
    pub fn tick(&mut self, now: Instant) -> Vec<HostEvent> {
        let mut events = Vec::new();
        if self.status == HostStatus::Waiting && now >= self.since {
            self.start(now, &mut events);
        }
        if matches!(self.status, HostStatus::Starting | HostStatus::Running) {
            self.drain(&mut events);
            self.watch(now, &mut events);
        }
        events
    }

    fn start(&mut self, now: Instant, events: &mut Vec<HostEvent>) {
        match self.hosts.start(&self.request) {
            Ok(host) => {
                let heartbeat = shared(&host.block())
                    .status
                    .heartbeat
                    .load(Ordering::Acquire);
                self.host = Some(host);
                self.status = HostStatus::Starting;
                self.since = now;
                self.beat = (heartbeat, now);
                self.degraded = false;
            }
            Err(e) => self.crash(format!("the plugin host didn't start: {e}"), now, events),
        }
    }

    fn drain(&mut self, events: &mut Vec<HostEvent>) {
        let Some(host) = &mut self.host else {
            return;
        };
        while let Some(message) = host.try_recv() {
            match message {
                FromHost::Params(params) => events.push(HostEvent::Params(params)),
                FromHost::ParamChanged(event) => events.push(HostEvent::ParamChanged(event)),
                FromHost::State(state) => {
                    self.request.state = Some(state);
                    events.push(HostEvent::StateSaved);
                }
                FromHost::StateDirty => {
                    host.send(&ToHost::SaveState);
                }
                FromHost::Error(message) => events.push(HostEvent::Error(message)),
                FromHost::Plugins(_) => {}
            }
        }
    }

    fn watch(&mut self, now: Instant, events: &mut Vec<HostEvent>) {
        let Some(host) = &mut self.host else {
            return;
        };
        let block = host.block();
        let status = &shared(&block).status;
        if status.host_state.load(Ordering::Acquire) == STATE_FAILED {
            let reason = failure(status.host_error.load(Ordering::Acquire));
            self.disable(reason, events);
            return;
        }
        if host.exited() {
            self.crash("the plugin host exited".into(), now, events);
            return;
        }
        let heartbeat = status.heartbeat.load(Ordering::Acquire);
        if heartbeat != self.beat.0 {
            self.beat = (heartbeat, now);
        } else if self.status == HostStatus::Running
            && now.saturating_duration_since(self.beat.1) > Duration::from_millis(LIVENESS_MS)
        {
            // Only a running host must beat: one that is starting is still loading and
            // activating the plugin, which can take seconds, and `start_timeout` bounds it.
            self.crash("the plugin host stopped responding".into(), now, events);
            return;
        }
        match self.status {
            HostStatus::Starting => {
                if let Some(latency) = shared(&block).ready_latency() {
                    self.status = HostStatus::Running;
                    self.since = now;
                    self.beat = (heartbeat, now);
                    self.block = Some(Arc::clone(&block));
                    events.push(HostEvent::Ready { block, latency });
                } else if now.saturating_duration_since(self.since) > self.policy.start_timeout {
                    self.crash("the plugin host didn't get ready".into(), now, events);
                }
            }
            HostStatus::Running => {
                if self.crashes > 0
                    && now.saturating_duration_since(self.since) >= self.policy.stable_after
                {
                    self.crashes = 0;
                }
                let run = status.mixer_miss_run.load(Ordering::Acquire);
                if !self.degraded && run >= u64::from(self.policy.degraded_after) {
                    self.degraded = true;
                    events.push(HostEvent::Degraded);
                } else if self.degraded && run == 0 {
                    self.degraded = false;
                    events.push(HostEvent::Recovered);
                }
            }
            _ => {}
        }
    }

    fn end_host(&mut self) {
        if let Some(mut host) = self.host.take() {
            host.kill();
        }
        self.block = None;
    }

    fn crash(&mut self, reason: String, now: Instant, events: &mut Vec<HostEvent>) {
        self.end_host();
        self.crashes += 1;
        if self.crashes >= self.policy.max_crashes {
            self.disable(
                format!("{reason}; disabled after {} crashes in a row", self.crashes),
                events,
            );
            return;
        }
        let doublings = self.crashes.saturating_sub(1).min(16);
        let retry_in = self
            .policy
            .first_retry
            .saturating_mul(1 << doublings)
            .min(self.policy.max_retry);
        self.status = HostStatus::Waiting;
        self.since = now + retry_in;
        events.push(HostEvent::Crashed { reason, retry_in });
    }

    fn disable(&mut self, reason: String, events: &mut Vec<HostEvent>) {
        self.end_host();
        self.status = HostStatus::Disabled;
        events.push(HostEvent::Disabled { reason });
    }
}

impl Drop for HostSupervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn shared(block: &HostBlock) -> &Shared {
    (**block).as_ref()
}

/// A failure a restart can't fix, in words.
fn failure(code: u32) -> String {
    match code {
        ERROR_BAD_CONFIG => "the plugin host refused the engine's configuration",
        ERROR_LOAD => "the plugin failed to load",
        ERROR_NOT_AN_EFFECT => "the plugin is not a stereo audio effect",
        ERROR_ACTIVATE => "the plugin failed to start at the engine's rate and block size",
        _ => "the plugin host failed",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_platform::plugin_host::fake::{FAKE_LATENCY, FakePluginHosts, fake_param};
    use jstream_plugin_proto::Config;

    fn request() -> HostRequest {
        HostRequest {
            file: "a.clap".into(),
            plugin_id: "org.example.gain".into(),
            config: Config {
                sample_rate: 48_000,
                block_frames: 64,
                depth: 2,
            },
            state: Some(vec![1]),
        }
    }

    struct Rig {
        hosts: FakePluginHosts,
        sup: HostSupervisor,
        now: Instant,
    }

    /// How long, on the wall clock, a test waits for the fake host's thread.
    const READY_WAIT: Duration = Duration::from_secs(20);

    impl Rig {
        fn new(policy: SupervisorPolicy) -> Self {
            let hosts = FakePluginHosts::new();
            let now = Instant::now();
            let sup = HostSupervisor::new(Arc::new(hosts.clone()), request(), policy, now);
            Self { hosts, sup, now }
        }

        /// Advances by `by`, letting the fake host's thread run a little first.
        fn tick(&mut self, by: Duration) -> Vec<HostEvent> {
            std::thread::sleep(Duration::from_millis(5));
            self.now += by;
            self.sup.tick(self.now)
        }

        /// Ticks until the host is ready, returning everything seen. It waits on the
        /// wall clock, not a count of ticks: the fake host is a thread, and a loaded CI
        /// runner can leave it unscheduled for seconds. Each tick still moves the
        /// supervisor's clock by only 1 ms, so even the whole wait stays well inside the
        /// default 10 s `start_timeout`.
        fn until_ready(&mut self) -> Vec<HostEvent> {
            let mut seen = Vec::new();
            let deadline = Instant::now() + READY_WAIT;
            while Instant::now() < deadline {
                let events = self.tick(Duration::from_millis(1));
                let ready = events.iter().any(|e| matches!(e, HostEvent::Ready { .. }));
                seen.extend(events);
                if ready {
                    return seen;
                }
            }
            panic!("never ready: {seen:?}");
        }
    }

    #[test]
    fn a_host_starts_with_the_saved_state_and_reports_ready_and_params() {
        let mut rig = Rig::new(SupervisorPolicy::default());
        let seen = rig.until_ready();
        assert!(seen.iter().any(|e| matches!(
            e,
            HostEvent::Ready { latency, .. } if *latency == FAKE_LATENCY
        )));
        let seen: Vec<HostEvent> = [seen, rig.tick(Duration::from_millis(1))].concat();
        assert!(
            seen.iter()
                .any(|e| matches!(e, HostEvent::Params(p) if *p == vec![fake_param()]))
        );
        assert_eq!(rig.sup.status(), HostStatus::Running);
        assert!(rig.sup.block().is_some());
        assert_eq!(rig.hosts.hosts()[0].request.state, Some(vec![1]));
    }

    #[test]
    fn saved_state_is_kept_and_restored_after_a_crash_with_doubling_backoff() {
        let mut rig = Rig::new(SupervisorPolicy::default());
        rig.until_ready();
        rig.hosts.hosts()[0].set_state(vec![9, 9]);
        rig.sup.request_state();
        let mut saved = false;
        let deadline = Instant::now() + READY_WAIT;
        while Instant::now() < deadline {
            saved |= rig
                .tick(Duration::from_millis(1))
                .iter()
                .any(|e| matches!(e, HostEvent::StateSaved));
            if saved {
                break;
            }
        }
        assert!(saved);
        assert_eq!(rig.sup.saved_state(), Some(&[9, 9][..]));

        let mut waits = Vec::new();
        for n in 0..3 {
            rig.hosts.hosts()[n].crash();
            let events = rig.tick(Duration::from_millis(1));
            let Some(HostEvent::Crashed { retry_in, .. }) = events.last() else {
                panic!("{events:?}");
            };
            waits.push(*retry_in);
            assert_eq!(rig.sup.status(), HostStatus::Waiting);
            assert!(rig.sup.block().is_none());
            // Not before the wait is over.
            assert!(rig.tick(*retry_in - Duration::from_millis(2)).is_empty());
            assert_eq!(rig.hosts.hosts().len(), n + 1);
            rig.until_ready();
            assert_eq!(rig.hosts.hosts()[n + 1].request.state, Some(vec![9, 9]));
        }
        assert_eq!(
            waits,
            [1, 2, 4].map(Duration::from_secs),
            "the backoff doubles"
        );
        assert_eq!(rig.sup.crashes(), 3);
        // A stable run forgets the crashes.
        rig.tick(Duration::from_secs(61));
        assert_eq!(rig.sup.crashes(), 0);
    }

    #[test]
    fn a_hung_host_is_killed_and_restarted() {
        let mut rig = Rig::new(SupervisorPolicy::default());
        rig.until_ready();
        let first = Arc::clone(&rig.hosts.hosts()[0]);
        first.hang();
        rig.tick(Duration::from_millis(1));
        assert!(rig.tick(Duration::from_millis(500)).is_empty());
        let events = rig.tick(Duration::from_millis(600));
        assert!(
            matches!(&events[..], [HostEvent::Crashed { reason, .. }] if reason.contains("stopped responding")),
            "{events:?}"
        );
        assert!(first.has_exited(), "the hung host was killed");
        rig.tick(Duration::from_secs(1));
        rig.until_ready();
        assert_eq!(rig.hosts.hosts().len(), 2);
    }

    #[test]
    fn repeated_crashes_disable_the_filter_until_it_is_enabled() {
        let mut rig = Rig::new(SupervisorPolicy {
            max_crashes: 2,
            ..SupervisorPolicy::default()
        });
        rig.hosts.refuse_starts(true);
        let events = rig.tick(Duration::ZERO);
        assert!(
            matches!(&events[..], [HostEvent::Crashed { reason, .. }] if reason.contains("didn't start")),
            "{events:?}"
        );
        let events = rig.tick(Duration::from_secs(1));
        assert!(
            matches!(&events[..], [HostEvent::Disabled { reason }] if reason.contains("2 crashes")),
            "{events:?}"
        );
        assert_eq!(rig.sup.status(), HostStatus::Disabled);
        assert!(rig.tick(Duration::from_secs(3600)).is_empty());

        rig.hosts.refuse_starts(false);
        rig.sup.enable(rig.now);
        assert_eq!(rig.sup.crashes(), 0);
        rig.until_ready();
    }

    #[test]
    fn a_host_that_never_gets_ready_counts_as_a_crash() {
        let mut rig = Rig::new(SupervisorPolicy {
            start_timeout: Duration::from_millis(200),
            ..SupervisorPolicy::default()
        });
        // Hung from the start, it never reads `Start` or beats.
        rig.hosts.start_hung(true);
        assert!(rig.tick(Duration::ZERO).is_empty());
        assert!(rig.tick(Duration::from_millis(200)).is_empty());
        let events = rig.tick(Duration::from_millis(1));
        assert!(
            matches!(&events[..], [HostEvent::Crashed { reason, .. }] if reason.contains("didn't get ready")),
            "{events:?}"
        );
        assert!(rig.hosts.hosts()[0].has_exited());
    }

    #[test]
    fn a_host_may_load_for_longer_than_the_liveness_window() {
        let mut rig = Rig::new(SupervisorPolicy::default());
        // It reads `Start` but doesn't beat or get ready while the plugin loads.
        rig.hosts.start_loading(true);
        assert!(rig.tick(Duration::ZERO).is_empty());
        assert!(rig.tick(Duration::from_secs(3)).is_empty());
        assert_eq!(rig.sup.status(), HostStatus::Starting);
        rig.hosts.hosts()[0].finish_loading();
        rig.until_ready();
        assert_eq!(rig.hosts.hosts().len(), 1, "no restart");
        // Running, it must beat: a hang is caught as before.
        rig.hosts.hosts()[0].hang();
        rig.tick(Duration::from_millis(1));
        let events = rig.tick(Duration::from_millis(1100));
        assert!(
            matches!(&events[..], [HostEvent::Crashed { reason, .. }] if reason.contains("stopped responding")),
            "{events:?}"
        );
    }

    #[test]
    fn a_failure_a_restart_cant_fix_disables_at_once() {
        let mut rig = Rig::new(SupervisorPolicy::default());
        rig.until_ready();
        let block = rig.sup.block().cloned().unwrap();
        let status = &shared(&block).status;
        status.host_error.store(ERROR_LOAD, Ordering::Release);
        status.host_state.store(STATE_FAILED, Ordering::Release);
        let events = rig.tick(Duration::from_millis(1));
        assert!(
            matches!(&events[..], [HostEvent::Disabled { reason }] if reason.contains("failed to load")),
            "{events:?}"
        );
        assert_eq!(rig.sup.crashes(), 0);
    }

    #[test]
    fn a_run_of_misses_marks_the_filter_degraded_until_blocks_return() {
        let mut rig = Rig::new(SupervisorPolicy::default());
        rig.until_ready();
        let block = rig.sup.block().cloned().unwrap();
        let status = &shared(&block).status;
        status.mixer_miss_run.store(49, Ordering::Release);
        assert!(rig.tick(Duration::from_millis(1)).is_empty());
        status.mixer_miss_run.store(50, Ordering::Release);
        assert!(matches!(
            &rig.tick(Duration::from_millis(1))[..],
            [HostEvent::Degraded]
        ));
        assert!(rig.tick(Duration::from_millis(1)).is_empty());
        status.mixer_miss_run.store(0, Ordering::Release);
        assert!(matches!(
            &rig.tick(Duration::from_millis(1))[..],
            [HostEvent::Recovered]
        ));
    }

    #[test]
    fn stopping_ends_the_host() {
        let mut rig = Rig::new(SupervisorPolicy::default());
        rig.until_ready();
        let host = Arc::clone(&rig.hosts.hosts()[0]);
        rig.sup.stop();
        assert_eq!(rig.sup.status(), HostStatus::Stopped);
        assert!(host.has_exited());
        assert!(host.received().contains(&ToHost::Stop));
        assert!(rig.tick(Duration::from_secs(60)).is_empty());
        rig.sup.enable(rig.now);
        rig.until_ready();
    }
}
