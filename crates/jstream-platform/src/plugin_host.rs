//! Out-of-process CLAP plugin hosts (ADR-0035): the engine starts one
//! `jstream-plugin-host` process per CLAP filter instance and talks to it through a
//! shared block (`jstream-plugin-proto`) and framed control messages on its stdio. This
//! is the seam: a platform crate creates the block, starts and contains the process,
//! and carries the messages; the engine's supervisor decides when to start, restart or
//! give up.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use jstream_plugin_proto::control::{FromHost, PluginDesc, ToHost};
use jstream_plugin_proto::{Config, Shared};

use crate::PlatformError;

/// A host's shared block, as the mixer's CLAP filter holds it.
pub type HostBlock = Arc<dyn AsRef<Shared> + Send + Sync>;

/// What to run in a new host.
#[derive(Clone, Debug, PartialEq)]
pub struct HostRequest {
    /// The plugin file (`.clap`).
    pub file: PathBuf,
    /// The plugin's CLAP id within the file.
    pub plugin_id: String,
    /// The block's configuration: the engine's rate, block size and pipeline depth.
    pub config: Config,
    /// A state an earlier run saved, restored before the plugin starts.
    pub state: Option<Vec<u8>>,
}

/// Starts plugin hosts and scans plugin files. Implemented by the platform crates (the
/// Windows one first) and by `fake` for tests.
pub trait PluginHosts: Send + Sync {
    /// The CLAP search directories on this platform that exist, in CLAP's order.
    fn search_paths(&self) -> Vec<PathBuf>;

    /// The hostable audio effects in `file`, found by a short-lived host process, so a
    /// plugin that crashes while being scanned costs only that process.
    ///
    /// # Errors
    /// The host can't start, times out, crashes, or reports an error.
    fn scan(&self, file: &Path) -> Result<Vec<PluginDesc>, PlatformError>;

    /// Creates a block initialised with `request.config`, starts a host process on it,
    /// and sends it `Start` with `request.state`. The host is ready once the block says
    /// so (`Shared::ready_latency`).
    ///
    /// # Errors
    /// The block can't be created, the configuration is out of range, or the process
    /// can't start.
    fn start(&self, request: &HostRequest) -> Result<Box<dyn PluginHost>, PlatformError>;
}

/// One running host process. Dropping it ends the process.
pub trait PluginHost: Send {
    /// The shared block, for the mixer's CLAP filter.
    fn block(&self) -> HostBlock;

    /// Whether the process has exited (a crash, a failure, or a stop).
    fn exited(&mut self) -> bool;

    /// The next control message the host sent, without waiting.
    fn try_recv(&mut self) -> Option<FromHost>;

    /// Sends a control message. False when the host is gone.
    fn send(&mut self, message: &ToHost) -> bool;

    /// Ends the process now.
    fn kill(&mut self);
}

/// In-process stand-ins for plugin hosts, for engine tests (feature `fake`). Each
/// "host" is a thread over a block on the heap that acts like `jstream-plugin-host`
/// running a unity-gain effect: it publishes a latency, beats the heartbeat, copies each
/// input block to the output under the same number, and answers the control messages.
/// The test makes it crash or hang through [`FakeHostControl`].
#[cfg(any(test, feature = "fake"))]
pub mod fake {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use jstream_plugin_proto::control::{FromHost, ParamDesc, PluginDesc, ToHost};
    use jstream_plugin_proto::{CHANNELS, STATE_STOPPED, Shared, first_useful};

    use super::{HostBlock, HostRequest, PluginHost, PluginHosts};
    use crate::PlatformError;

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The latency every fake plugin reports, frames.
    pub const FAKE_LATENCY: u32 = 16;

    /// The one parameter every fake plugin has.
    pub fn fake_param() -> ParamDesc {
        ParamDesc {
            id: 1,
            name: "Mix".into(),
            module: String::new(),
            min: 0.0,
            max: 1.0,
            default: 1.0,
            flags: 0,
        }
    }

    /// One fake host, as the test sees it.
    #[derive(Debug)]
    pub struct FakeHostControl {
        /// What the engine asked for.
        pub request: HostRequest,
        crashed: AtomicBool,
        hung: AtomicBool,
        /// Still loading: it read `Start` but isn't ready (or beating) until released.
        loading: AtomicBool,
        exited: AtomicBool,
        /// The state the host answers `SaveState` with.
        state: Mutex<Vec<u8>>,
        /// Messages the host received, in order.
        received: Mutex<Vec<ToHost>>,
    }

    impl FakeHostControl {
        /// The process dies: no more heartbeat, no more blocks, and it reports exited.
        pub fn crash(&self) {
            self.crashed.store(true, Ordering::Release);
        }

        /// The process hangs: no more heartbeat or blocks, but it hasn't exited.
        pub fn hang(&self) {
            self.hung.store(true, Ordering::Release);
        }

        /// Ends a slow load ([`FakePluginHosts::start_loading`]): the host gets ready.
        pub fn finish_loading(&self) {
            self.loading.store(false, Ordering::Release);
        }

        /// Whether the process has exited.
        pub fn has_exited(&self) -> bool {
            self.exited.load(Ordering::Acquire)
        }

        /// Sets the state the host saves.
        pub fn set_state(&self, state: Vec<u8>) {
            *lock(&self.state) = state;
        }

        /// The control messages the host received.
        pub fn received(&self) -> Vec<ToHost> {
            lock(&self.received).clone()
        }
    }

    /// A plugin file a scan finds, and what it offers.
    type ScannedFile = (PathBuf, Vec<PluginDesc>);

    /// Fake [`PluginHosts`]: scans return what the test registered, and every start
    /// runs a fake host thread.
    #[derive(Clone, Debug, Default)]
    pub struct FakePluginHosts {
        files: Arc<Mutex<Vec<ScannedFile>>>,
        hosts: Arc<Mutex<Vec<Arc<FakeHostControl>>>>,
        refuse_starts: Arc<AtomicBool>,
        start_hung: Arc<AtomicBool>,
        start_loading: Arc<AtomicBool>,
    }

    impl FakePluginHosts {
        /// No plugin files, no hosts.
        pub fn new() -> Self {
            Self::default()
        }

        /// Makes `scan(file)` find `plugins`.
        pub fn add_file(&self, file: impl Into<PathBuf>, plugins: Vec<PluginDesc>) {
            lock(&self.files).push((file.into(), plugins));
        }

        /// Every host started so far, oldest first.
        pub fn hosts(&self) -> Vec<Arc<FakeHostControl>> {
            lock(&self.hosts).clone()
        }

        /// Makes every later `start` fail, as when the host exe is missing.
        pub fn refuse_starts(&self, refuse: bool) {
            self.refuse_starts.store(refuse, Ordering::Release);
        }

        /// Makes every later host hang from the start, before it reads `Start`.
        pub fn start_hung(&self, hung: bool) {
            self.start_hung.store(hung, Ordering::Release);
        }

        /// Makes every later host load slowly, as a real plugin can: it reads `Start`
        /// but neither gets ready nor beats until [`FakeHostControl::finish_loading`].
        pub fn start_loading(&self, loading: bool) {
            self.start_loading.store(loading, Ordering::Release);
        }
    }

    impl PluginHosts for FakePluginHosts {
        fn search_paths(&self) -> Vec<PathBuf> {
            Vec::new()
        }

        fn scan(&self, file: &Path) -> Result<Vec<PluginDesc>, PlatformError> {
            lock(&self.files)
                .iter()
                .find(|(f, _)| f == file)
                .map(|(_, plugins)| plugins.clone())
                .ok_or_else(|| PlatformError::NotFound {
                    kind: "plugin file",
                    id: file.display().to_string(),
                })
        }

        fn start(&self, request: &HostRequest) -> Result<Box<dyn PluginHost>, PlatformError> {
            if self.refuse_starts.load(Ordering::Acquire) {
                return Err(PlatformError::Unsupported {
                    what: "starting a plugin host (the fake refuses)".into(),
                });
            }
            let shared: Arc<Shared> = Shared::new_boxed().into();
            shared
                .init(request.config)
                .map_err(|e| PlatformError::Unsupported {
                    what: format!("a plugin block configuration: {e:?}"),
                })?;
            let control = Arc::new(FakeHostControl {
                request: request.clone(),
                crashed: AtomicBool::new(false),
                hung: AtomicBool::new(self.start_hung.load(Ordering::Acquire)),
                loading: AtomicBool::new(self.start_loading.load(Ordering::Acquire)),
                exited: AtomicBool::new(false),
                state: Mutex::new(request.state.clone().unwrap_or_default()),
                received: Mutex::new(Vec::new()),
            });
            lock(&self.hosts).push(Arc::clone(&control));
            let inbox = Arc::new(Mutex::new(VecDeque::new()));
            let outbox = Arc::new(Mutex::new(VecDeque::new()));
            let thread = {
                let shared = Arc::clone(&shared);
                let control = Arc::clone(&control);
                let inbox = Arc::clone(&inbox);
                let outbox = Arc::clone(&outbox);
                std::thread::spawn(move || run(&shared, &control, &inbox, &outbox))
            };
            let mut host = FakeHost {
                shared,
                control,
                inbox,
                outbox,
                thread: Some(thread),
            };
            host.send(&ToHost::Start {
                state: request.state.clone(),
            });
            Ok(Box::new(host))
        }
    }

    struct FakeHost {
        shared: Arc<Shared>,
        control: Arc<FakeHostControl>,
        /// Engine → host.
        inbox: Arc<Mutex<VecDeque<ToHost>>>,
        /// Host → engine.
        outbox: Arc<Mutex<VecDeque<FromHost>>>,
        thread: Option<JoinHandle<()>>,
    }

    impl PluginHost for FakeHost {
        fn block(&self) -> HostBlock {
            Arc::clone(&self.shared) as HostBlock
        }

        fn exited(&mut self) -> bool {
            self.control.has_exited()
        }

        fn try_recv(&mut self) -> Option<FromHost> {
            lock(&self.outbox).pop_front()
        }

        fn send(&mut self, message: &ToHost) -> bool {
            if self.control.has_exited() {
                return false;
            }
            lock(&self.inbox).push_back(message.clone());
            true
        }

        fn kill(&mut self) {
            self.control.crash();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    impl Drop for FakeHost {
        fn drop(&mut self) {
            self.kill();
        }
    }

    /// The fake host's thread.
    fn run(
        shared: &Shared,
        control: &FakeHostControl,
        inbox: &Mutex<VecDeque<ToHost>>,
        outbox: &Mutex<VecDeque<FromHost>>,
    ) {
        let Some(config) = shared.config() else {
            control.exited.store(true, Ordering::Release);
            return;
        };
        let samples = config.block_frames as usize * CHANNELS;
        let mut buffer = vec![0.0; samples];
        let mut next = 0u64;
        let mut asked = false;
        let mut started = false;
        let s = &shared.status;
        loop {
            if control.crashed.load(Ordering::Acquire) {
                break;
            }
            if control.hung.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            while let Some(message) = lock(inbox).pop_front() {
                lock(&control.received).push(message.clone());
                match message {
                    ToHost::Start { .. } => asked = true,
                    ToHost::SaveState => {
                        let state = lock(&control.state).clone();
                        lock(outbox).push_back(FromHost::State(state));
                    }
                    ToHost::Stop => {
                        s.host_state.store(STATE_STOPPED, Ordering::Release);
                        control.exited.store(true, Ordering::Release);
                        return;
                    }
                }
            }
            if s.stop.load(Ordering::Acquire) != 0 {
                s.host_state.store(STATE_STOPPED, Ordering::Release);
                break;
            }
            if asked && !started && !control.loading.load(Ordering::Acquire) {
                started = true;
                shared.publish_ready(FAKE_LATENCY);
                lock(outbox).push_back(FromHost::Params(vec![fake_param()]));
            }
            if started {
                // Like the real host, it beats only once its real-time loop runs.
                s.heartbeat.fetch_add(1, Ordering::Release);
                let head = shared.input.head();
                next = next.max(first_useful(head, config.depth));
                while next < head {
                    if shared.input.read(next, &mut buffer) {
                        shared.output.write(next, &buffer);
                        s.processed.fetch_add(1, Ordering::Release);
                    }
                    next += 1;
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        control.exited.store(true, Ordering::Release);
    }

    #[cfg(test)]
    mod tests {
        use std::time::Instant;

        use jstream_plugin_proto::Config;

        use super::*;

        const CONFIG: Config = Config {
            sample_rate: 48_000,
            block_frames: 64,
            depth: 2,
        };

        fn request(state: Option<Vec<u8>>) -> HostRequest {
            HostRequest {
                file: "a.clap".into(),
                plugin_id: "org.example.gain".into(),
                config: CONFIG,
                state,
            }
        }

        fn wait(what: &str, mut done: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done() {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        fn recv(host: &mut dyn PluginHost) -> FromHost {
            let mut got = None;
            wait("a message", || {
                got = host.try_recv();
                got.is_some()
            });
            got.unwrap()
        }

        #[test]
        fn scans_find_only_registered_files() {
            let hosts = FakePluginHosts::new();
            let plugin = PluginDesc {
                id: "org.example.gain".into(),
                name: "Gain".into(),
                vendor: String::new(),
                version: String::new(),
                description: String::new(),
                params: vec![fake_param()],
            };
            hosts.add_file("a.clap", vec![plugin.clone()]);
            assert_eq!(hosts.scan(Path::new("a.clap")).unwrap(), vec![plugin]);
            assert!(matches!(
                hosts.scan(Path::new("b.clap")),
                Err(PlatformError::NotFound {
                    kind: "plugin file",
                    ..
                })
            ));
            assert!(hosts.search_paths().is_empty());
        }

        #[test]
        fn a_host_starts_copies_blocks_saves_state_and_stops() {
            let hosts = FakePluginHosts::new();
            let mut host = hosts.start(&request(Some(vec![1, 2]))).unwrap();
            let block = host.block();
            let shared: &Shared = (*block).as_ref();
            wait("ready", || shared.ready_latency() == Some(FAKE_LATENCY));
            assert_eq!(recv(&mut *host), FromHost::Params(vec![fake_param()]));

            let samples = CONFIG.block_frames as usize * CHANNELS;
            let input: Vec<f32> = (0..samples).map(|i| i as f32).collect();
            assert!(shared.input.write(0, &input));
            let mut output = vec![0.0; samples];
            wait("the block", || shared.output.read(0, &mut output));
            assert_eq!(output, input);

            assert!(host.send(&ToHost::SaveState));
            assert_eq!(recv(&mut *host), FromHost::State(vec![1, 2]));
            let control = &hosts.hosts()[0];
            control.set_state(vec![3]);
            assert!(host.send(&ToHost::SaveState));
            assert_eq!(recv(&mut *host), FromHost::State(vec![3]));

            assert!(host.send(&ToHost::Stop));
            wait("the exit", || host.exited());
            assert!(!host.send(&ToHost::SaveState));
            assert_eq!(
                shared.status.host_state.load(Ordering::Acquire),
                STATE_STOPPED
            );
            assert_eq!(
                control.received(),
                vec![
                    ToHost::Start {
                        state: Some(vec![1, 2])
                    },
                    ToHost::SaveState,
                    ToHost::SaveState,
                    ToHost::Stop
                ]
            );
            assert_eq!(control.request, request(Some(vec![1, 2])));
        }

        #[test]
        fn a_crash_exits_and_a_hang_stops_the_heartbeat_without_exiting() {
            let hosts = FakePluginHosts::new();
            let mut crashing = hosts.start(&request(None)).unwrap();
            let mut hanging = hosts.start(&request(None)).unwrap();
            let [crash, hang] = <[_; 2]>::try_from(hosts.hosts()).unwrap();
            crash.crash();
            wait("the crash", || crashing.exited());

            hang.hang();
            let block = hanging.block();
            let heartbeat = || {
                let shared: &Shared = (*block).as_ref();
                shared.status.heartbeat.load(Ordering::Acquire)
            };
            std::thread::sleep(Duration::from_millis(10));
            let before = heartbeat();
            std::thread::sleep(Duration::from_millis(20));
            assert_eq!(heartbeat(), before);
            assert!(!hanging.exited());
            hanging.kill();
            assert!(hanging.exited());
        }

        #[test]
        fn the_stop_word_ends_a_host_and_refused_starts_fail() {
            let hosts = FakePluginHosts::new();
            let mut host = hosts.start(&request(None)).unwrap();
            let block = host.block();
            let shared: &Shared = (*block).as_ref();
            shared.status.stop.store(1, Ordering::Release);
            wait("the exit", || host.exited());

            hosts.refuse_starts(true);
            assert!(hosts.start(&request(None)).is_err());
            let bad = HostRequest {
                config: Config { depth: 0, ..CONFIG },
                ..request(None)
            };
            hosts.refuse_starts(false);
            assert!(hosts.start(&bad).is_err());
            assert_eq!(hosts.hosts().len(), 1);
        }
    }
}
