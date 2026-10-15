//! One run of one plugin for one filter: the host's main thread. It reads the block's
//! configuration and the engine's `Start`, creates and activates the plugin, publishes
//! its latency, and runs the real-time loop on its own thread. Meanwhile it answers the
//! engine's control messages, runs the plugin's main-thread callbacks, passes on its
//! state changes, and re-activates it when it asks for a restart. It ends on the
//! engine's `Stop`, its `stop` word, or the end of its control stream.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use jstream_plugin_clap::{ClapError, Library, ParamInfo, Plugin, Processor};
use jstream_plugin_proto::control::{
    FromHost, PARAM_BYPASS, PARAM_ENUM, PARAM_HIDDEN, PARAM_READ_ONLY, PARAM_STEPPED, ParamDesc,
    ToHost, read_frame,
};
use jstream_plugin_proto::{
    Config, ERROR_ACTIVATE, ERROR_BAD_CONFIG, ERROR_LOAD, ERROR_NOT_AN_EFFECT, MAX_PLUGIN_LATENCY,
    STATE_FAILED, STATE_STOPPED, Shared,
};

use crate::rt;

/// How often the main thread looks at the plugin's requests and the block's `stop` word
/// while no control message arrives.
const POLL: Duration = Duration::from_millis(10);

/// A run that ended badly. Why is on the block (`host_error`) and was sent to the engine
/// as an `Error` message.
#[derive(Debug)]
pub(crate) struct Failure;

/// Writes control messages to the engine.
pub(crate) struct Outbox<W: Write>(pub(crate) W);

impl<W: Write> Outbox<W> {
    /// Sends `message`. A write error means the engine is gone; the run then ends on
    /// its `stop` word or the end of its stream, so it is ignored here.
    pub(crate) fn send(&mut self, message: &FromHost) {
        let _ = self
            .0
            .write_all(&message.encode())
            .and_then(|()| self.0.flush());
    }
}

/// The plugin's parameter, as the control channel carries it.
pub(crate) fn param_desc(p: &ParamInfo) -> ParamDesc {
    let flags = [
        (p.stepped, PARAM_STEPPED),
        (p.is_enum, PARAM_ENUM),
        (p.hidden, PARAM_HIDDEN),
        (p.read_only, PARAM_READ_ONLY),
        (p.bypass, PARAM_BYPASS),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .fold(0, |flags, (_, bit)| flags | bit);
    ParamDesc {
        id: p.id,
        name: p.name.clone(),
        module: p.module.clone(),
        min: p.min,
        max: p.max,
        default: p.default,
        flags,
    }
}

/// Marks the block failed with `error` and tells the engine why.
fn fail<W: Write>(shared: &Shared, out: &mut Outbox<W>, error: u32, why: String) -> Failure {
    shared.status.host_error.store(error, Ordering::Relaxed);
    shared
        .status
        .host_state
        .store(STATE_FAILED, Ordering::Release);
    out.send(&FromHost::Error(why));
    Failure
}

/// Reads the engine's control messages on their own thread, so the main thread never
/// blocks on them. The end of the stream, or a frame that isn't a message, reads as
/// `Stop`: the engine is gone or broken.
fn spawn_reader(mut input: impl Read + Send + 'static) -> Receiver<ToHost> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        loop {
            let message = match read_frame(&mut input) {
                Ok(Some(payload)) => ToHost::decode(&payload).unwrap_or(ToHost::Stop),
                Ok(None) | Err(_) => ToHost::Stop,
            };
            let stop = message == ToHost::Stop;
            if tx.send(message).is_err() || stop {
                return;
            }
        }
    });
    rx
}

/// How a stretch of real-time running ended.
enum Outcome {
    Stop,
    Restart,
}

/// Runs plugin `id` from `library` over `shared` until the engine stops it.
/// `register_rt` runs on the real-time thread first, and its guard lives as long as
/// that thread's loop (MMCSS on Windows).
///
/// # Errors
/// A block the engine didn't initialise for this version, no `Start`, or a plugin that
/// fails to load, isn't a stereo audio effect or won't activate. The block is marked
/// failed and the engine told why.
pub(crate) fn run<G>(
    shared: &Shared,
    library: &Arc<Library>,
    id: &str,
    input: impl Read + Send + 'static,
    output: impl Write,
    register_rt: impl Fn() -> G + Sync,
) -> Result<(), Failure> {
    let mut out = Outbox(output);
    let Some(config) = shared.config() else {
        return Err(fail(
            shared,
            &mut out,
            ERROR_BAD_CONFIG,
            "the shared block isn't initialised for this host's version".into(),
        ));
    };
    let inbox = spawn_reader(input);
    let state = match inbox.recv() {
        Ok(ToHost::Start { state }) => state,
        _ => {
            return Err(fail(
                shared,
                &mut out,
                ERROR_BAD_CONFIG,
                "the engine sent no Start".into(),
            ));
        }
    };
    let mut plugin = match Plugin::new(library, id) {
        Ok(plugin) => plugin,
        Err(e @ ClapError::NotAnEffect) => {
            return Err(fail(shared, &mut out, ERROR_NOT_AN_EFFECT, e.to_string()));
        }
        Err(e) => return Err(fail(shared, &mut out, ERROR_LOAD, e.to_string())),
    };
    if let Some(state) = state
        && let Err(e) = plugin.load_state(&state)
    {
        // The plugin runs with its defaults; the engine keeps the state it sent.
        out.send(&FromHost::Error(format!("restoring the saved state: {e}")));
    }
    let mut processor = activate(shared, &mut out, &mut plugin, config)?;
    out.send(&FromHost::Params(
        plugin.params().iter().map(param_desc).collect(),
    ));

    loop {
        let stop_rt = AtomicBool::new(false);
        let outcome = std::thread::scope(|scope| {
            let rt = scope.spawn(|| {
                let _guard = register_rt();
                rt::run(shared, config, processor, &stop_rt);
            });
            let outcome = main_loop(shared, &inbox, &mut out, &mut plugin);
            stop_rt.store(true, Ordering::Release);
            match rt.join() {
                Ok(()) => outcome,
                Err(_) => Outcome::Stop,
            }
        });
        // The processor was dropped on the real-time thread.
        let _ = plugin.deactivate();
        match outcome {
            Outcome::Stop => break,
            Outcome::Restart => {
                processor = activate(shared, &mut out, &mut plugin, config)?;
                out.send(&FromHost::Params(
                    plugin.params().iter().map(param_desc).collect(),
                ));
            }
        }
    }
    shared
        .status
        .host_state
        .store(STATE_STOPPED, Ordering::Release);
    Ok(())
}

/// Activates the plugin for the block's rate and block size, checks its latency, and
/// publishes it with the host ready.
fn activate<W: Write>(
    shared: &Shared,
    out: &mut Outbox<W>,
    plugin: &mut Plugin,
    config: Config,
) -> Result<Processor, Failure> {
    let processor = match plugin.activate(config.sample_rate, config.block_frames) {
        Ok(processor) => processor,
        Err(e @ (ClapError::NotStereo | ClapError::Ports)) => {
            return Err(fail(shared, out, ERROR_NOT_AN_EFFECT, e.to_string()));
        }
        Err(e) => return Err(fail(shared, out, ERROR_ACTIVATE, e.to_string())),
    };
    let latency = plugin.latency().unwrap_or(0);
    if latency > MAX_PLUGIN_LATENCY {
        drop(processor);
        let _ = plugin.deactivate();
        return Err(fail(
            shared,
            out,
            ERROR_ACTIVATE,
            format!("the plugin's latency of {latency} frames is over {MAX_PLUGIN_LATENCY}"),
        ));
    }
    shared.publish_ready(latency);
    Ok(processor)
}

/// The main thread's work while the real-time thread runs.
fn main_loop<W: Write>(
    shared: &Shared,
    inbox: &Receiver<ToHost>,
    out: &mut Outbox<W>,
    plugin: &mut Plugin,
) -> Outcome {
    loop {
        match inbox.recv_timeout(POLL) {
            Ok(ToHost::SaveState) => {
                let state = match plugin.save_state() {
                    Ok(state) => state,
                    Err(ClapError::NoState) => Vec::new(),
                    Err(e) => {
                        out.send(&FromHost::Error(format!("saving the state: {e}")));
                        Vec::new()
                    }
                };
                out.send(&FromHost::State(state));
            }
            // A second Start changes nothing.
            Ok(ToHost::Start { .. }) | Err(RecvTimeoutError::Timeout) => {}
            Ok(ToHost::Stop) | Err(RecvTimeoutError::Disconnected) => return Outcome::Stop,
        }
        if shared.status.stop.load(Ordering::Acquire) != 0 {
            return Outcome::Stop;
        }
        plugin.run_main_thread_callback();
        if plugin.take_state_dirty() {
            out.send(&FromHost::StateDirty);
        }
        if plugin.take_restart_request() {
            return Outcome::Restart;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_plugin_clap::test_plugin;
    use jstream_plugin_proto::control::MAX_FRAME;
    use jstream_plugin_proto::{CHANNELS, DEFAULT_DEPTH, ERROR_NONE, ParamEvent};
    use std::sync::Mutex;
    use std::time::Instant;

    const CONFIG: Config = Config {
        sample_rate: 48_000,
        block_frames: 64,
        depth: DEFAULT_DEPTH,
    };

    /// A pipe: what the test writes, the host reads.
    struct Pipe(Receiver<Vec<u8>>, Vec<u8>);

    impl Read for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            while self.1.is_empty() {
                match self.0.recv() {
                    Ok(bytes) => self.1 = bytes,
                    Err(_) => return Ok(0),
                }
            }
            let n = buf.len().min(self.1.len());
            buf[..n].copy_from_slice(&self.1[..n]);
            self.1.drain(..n);
            Ok(n)
        }
    }

    /// What the host wrote, shared with the test.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn messages(&self) -> Vec<FromHost> {
            let bytes = self.0.lock().unwrap().clone();
            let mut stream = bytes.as_slice();
            let mut messages = Vec::new();
            while let Some(payload) = read_frame(&mut stream).unwrap() {
                assert!(payload.len() <= MAX_FRAME);
                messages.push(FromHost::decode(&payload).unwrap());
            }
            messages
        }
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ok() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// A host running on its own thread, and the engine's ends of it.
    struct Running {
        shared: Arc<Shared>,
        /// The engine's end of the control pipe.
        tx: std::sync::mpsc::Sender<Vec<u8>>,
        /// What the host wrote.
        sink: Sink,
        host: std::thread::JoinHandle<Result<(), Failure>>,
    }

    /// Starts a host for `id`, sending `start` first when given.
    fn start(id: &'static str, start: Option<ToHost>) -> Running {
        let shared: Arc<Shared> = Shared::new_boxed().into();
        shared.init(CONFIG).unwrap();
        let (tx, rx) = channel();
        if let Some(start) = start {
            tx.send(start.encode()).unwrap();
        }
        let sink = Sink::default();
        let host = {
            let shared = Arc::clone(&shared);
            let sink = sink.clone();
            std::thread::spawn(move || {
                let library = test_plugin::library().unwrap();
                run(&shared, &library, id, Pipe(rx, Vec::new()), sink, || ())
            })
        };
        Running {
            shared,
            tx,
            sink,
            host,
        }
    }

    #[test]
    fn runs_the_plugin_over_the_block_until_stopped() {
        let Running {
            shared,
            tx,
            sink,
            host,
        } = start(
            test_plugin::GAIN_ID,
            Some(ToHost::Start {
                state: Some(0.5f64.to_le_bytes().to_vec()),
            }),
        );
        wait_for("ready", || shared.ready_latency().is_some());
        assert_eq!(shared.ready_latency(), Some(test_plugin::LATENCY));
        // The mixer's side: blocks in, processed blocks back under the same number.
        let samples = CONFIG.block_frames as usize * CHANNELS;
        let input = vec![1.0f32; samples];
        shared.input.write(0, &input);
        shared.params_in.push(ParamEvent {
            id: test_plugin::GAIN_PARAM,
            value: 0.25,
        });
        shared.input.write(1, &input);
        let mut out = vec![0.0; samples];
        wait_for("block 1", || shared.output.read(1, &mut out));
        // The restored gain (0.5) or the change (0.25), whichever block it was.
        assert!(out.iter().all(|s| *s == 0.5 || *s == 0.25));
        wait_for("the echo", || {
            shared.params_out.pop()
                == Some(ParamEvent {
                    id: test_plugin::ECHO_PARAM,
                    value: 0.25,
                })
        });
        // Saved state reflects the change.
        tx.send(ToHost::SaveState.encode()).unwrap();
        wait_for("the state", || {
            sink.messages()
                .contains(&FromHost::State(0.25f64.to_le_bytes().to_vec()))
        });
        // A reset request is acted on.
        shared.status.reset_seq.store(3, Ordering::Release);
        wait_for("the reset", || {
            shared.status.host_reset_seq.load(Ordering::Acquire) == 3
        });
        tx.send(ToHost::Stop.encode()).unwrap();
        host.join().unwrap().unwrap();
        assert_eq!(
            shared.status.host_state.load(Ordering::Acquire),
            STATE_STOPPED
        );
        let messages = sink.messages();
        let FromHost::Params(params) = &messages[0] else {
            panic!("expected the parameters first, got {messages:?}");
        };
        assert_eq!(params.len(), 1);
        assert_eq!((params[0].id, params[0].name.as_str()), (7, "Gain"));
        assert!(shared.status.heartbeat.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn the_end_of_the_control_stream_or_the_stop_word_ends_the_run() {
        let Running {
            shared, tx, host, ..
        } = start(test_plugin::GAIN_ID, Some(ToHost::Start { state: None }));
        wait_for("ready", || shared.ready_latency().is_some());
        drop(tx);
        host.join().unwrap().unwrap();

        let Running {
            shared,
            tx: _tx,
            host,
            ..
        } = start(test_plugin::GAIN_ID, Some(ToHost::Start { state: None }));
        wait_for("ready", || shared.ready_latency().is_some());
        shared.status.stop.store(1, Ordering::Release);
        host.join().unwrap().unwrap();
        assert_eq!(
            shared.status.host_state.load(Ordering::Acquire),
            STATE_STOPPED
        );
    }

    #[test]
    fn failures_are_marked_on_the_block_and_reported() {
        let cases = [
            (test_plugin::SYNTH_ID, ERROR_NOT_AN_EFFECT),
            (test_plugin::MONO_ID, ERROR_NOT_AN_EFFECT),
            (test_plugin::WIDE_ID, ERROR_NOT_AN_EFFECT),
            ("org.example.none", ERROR_LOAD),
        ];
        for (id, error) in cases {
            let Running {
                shared,
                tx: _,
                sink,
                host,
            } = start(id, Some(ToHost::Start { state: None }));
            assert!(host.join().unwrap().is_err());
            assert_eq!(
                shared.status.host_state.load(Ordering::Acquire),
                STATE_FAILED,
                "{id}"
            );
            assert_eq!(shared.status.host_error.load(Ordering::Relaxed), error);
            assert!(matches!(sink.messages().last(), Some(FromHost::Error(_))));
        }
        // No Start (the stream ended): refused before loading anything.
        let Running {
            shared,
            tx,
            sink: _,
            host,
        } = start(test_plugin::GAIN_ID, None);
        drop(tx);
        assert!(host.join().unwrap().is_err());
        assert_eq!(
            shared.status.host_error.load(Ordering::Relaxed),
            ERROR_BAD_CONFIG
        );
        // A block nobody initialised.
        let shared = Shared::new_boxed();
        let (_tx, rx) = channel::<Vec<u8>>();
        let library = test_plugin::library().unwrap();
        let sink = Sink::default();
        let run = run(
            &shared,
            &library,
            test_plugin::GAIN_ID,
            Pipe(rx, Vec::new()),
            sink,
            || (),
        );
        assert!(run.is_err());
        assert_eq!(
            shared.status.host_state.load(Ordering::Relaxed),
            STATE_FAILED
        );
        assert_ne!(shared.status.host_error.load(Ordering::Relaxed), ERROR_NONE);
    }

    #[test]
    fn a_bad_saved_state_is_reported_and_the_plugin_runs_with_defaults() {
        let Running {
            shared,
            tx,
            sink,
            host,
        } = start(
            test_plugin::GAIN_ID,
            Some(ToHost::Start {
                state: Some(vec![1, 2, 3]),
            }),
        );
        wait_for("ready", || shared.ready_latency().is_some());
        assert!(
            sink.messages()
                .iter()
                .any(|m| matches!(m, FromHost::Error(e) if e.contains("state")))
        );
        tx.send(ToHost::Stop.encode()).unwrap();
        host.join().unwrap().unwrap();
    }
}
