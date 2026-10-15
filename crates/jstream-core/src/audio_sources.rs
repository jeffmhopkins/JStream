//! Audio sources: one mixer input for each audio-only source in the collection
//! (`audio_input`, `desktop_audio` and `app_audio`, from `jstream-sources`) while the
//! program stage runs, each sent to the master bus at unity.
//!
//! The engine records what it wants under the collection lock ([`AudioSources::sync`]).
//! The program stage's [`AudioInputs`] opens those inputs when it starts, and its
//! `jstream-audio-sources` thread keeps them in line from then on: it adds an input for
//! a new source, removes a deleted one's (the mixer frees its ring on the collector
//! thread), and reopens one whose settings changed. Opening a device never runs under
//! the collection lock.
//!
//! A source whose device or application can't be recorded is unavailable rather than an
//! error: the program runs without it, and the thread tries again on every tick. The
//! same tick reopens an input whose stream failed (a device unplugged, or a followed
//! default replaced by one in another format), and finds an application again when the
//! process it recorded has exited (it restarted, under a new process ID).
//!
//! A source that makes audio of its own (a `browser` page) gets an input too, fed by
//! the running source itself ([`jstream_sources::SourceAudio`]) from when it is built
//! until it is deleted or built again.
//!
//! While the collection has no audio source, the program records the default
//! microphone and desktop audio, as it did before audio sources existed, so a
//! collection made earlier is not silent. A page's audio doesn't count as an audio
//! source here: it plays alongside the defaults.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use jstream_audio::{InputControl, MixerControl};
use jstream_platform::{AudioInputTarget, AudioIo, AudioProcess, AudioStream, DeviceId};
use jstream_protocol::{SourceId, SourceKind};
use jstream_sources::{AudioTarget, SourceAudio};

use crate::drops::{AudioInputDrops, DropLedger, DropSourceId, lock_ledger};
use crate::lock;

/// How often the thread retries unavailable sources, reopens failed streams and checks
/// that recorded applications still run.
const TICK: Duration = Duration::from_millis(500);

/// The audio sources the collection holds, for the program stage to record. Lives in
/// the engine state, under the collection lock.
#[derive(Default)]
pub(crate) struct AudioSources {
    wanted: Arc<Wanted>,
}

/// What the collection wants recorded, shared with the program stage's thread.
#[derive(Default)]
pub(crate) struct Wanted {
    state: Mutex<WantedState>,
    /// Wakes the thread when the collection changes, or when the program stops.
    changed: Condvar,
}

#[derive(Default)]
struct WantedState {
    targets: BTreeMap<SourceId, Recorded>,
    /// How each source fares while the program stage runs.
    status: BTreeMap<SourceId, AudioStatus>,
}

/// Whether the program stage records an audio source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AudioStatus {
    /// It has a mixer input, fed by its device or application.
    Recording,
    /// It can't be recorded now, for this reason; the program runs without it.
    Unavailable(String),
}

/// What the program records for one source.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Recorded {
    /// An audio-only source's device or application.
    Device(AudioTarget),
    /// A running source's own audio.
    Own(OwnAudio),
}

/// A running source's own audio, compared by identity: a source built again has new
/// audio, and its input is opened again.
#[derive(Clone)]
struct OwnAudio(Arc<dyn SourceAudio>);

impl PartialEq for OwnAudio {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for OwnAudio {}

impl fmt::Debug for OwnAudio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OwnAudio").field(&self.0.format()).finish()
    }
}

impl AudioSources {
    /// Brings the wanted set in line with the collection's audio-only sources and the
    /// running sources' own audio (`own`, from `LiveSources::audio`), and wakes the
    /// program stage's thread when it changed.
    pub(crate) fn sync(
        &mut self,
        state: &jstream_graph::State,
        own: BTreeMap<SourceId, Arc<dyn SourceAudio>>,
    ) {
        let mut targets: BTreeMap<SourceId, Recorded> = state
            .sources()
            .filter_map(|source| {
                let SourceKind::Builtin { kind } = source.kind() else {
                    return None;
                };
                let target = AudioTarget::of(kind, source.settings())?;
                Some((source.id().clone(), Recorded::Device(target)))
            })
            .collect();
        for (id, audio) in own {
            if state.source(&id).is_some() {
                targets.entry(id).or_insert(Recorded::Own(OwnAudio(audio)));
            }
        }
        let mut wanted = lock(&self.wanted.state);
        if wanted.targets != targets {
            // A deleted source has no status from now on, even before the thread closes
            // its input.
            wanted.status.retain(|id, _| targets.contains_key(id));
            wanted.targets = targets;
            self.wanted.changed.notify_all();
        }
    }

    /// The wanted set, for the program stage.
    pub(crate) fn wanted(&self) -> Arc<Wanted> {
        Arc::clone(&self.wanted)
    }

    /// How the program stage fares with `source_id`, while it runs.
    #[cfg(test)]
    pub(crate) fn status(&self, source_id: &SourceId) -> Option<AudioStatus> {
        lock(&self.wanted.state).status.get(source_id).cloned()
    }
}

/// One input the program records: an audio source, or while there is none, the default
/// microphone or desktop audio.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Source(SourceId),
    Microphone,
    Desktop,
}

/// What one input records, resolved as far as it can be without the OS.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Want {
    Target(AudioInputTarget),
    /// The running process with this executable name.
    App(String),
    /// A running source's own audio.
    Own(OwnAudio),
}

impl Want {
    fn of(recorded: &Recorded) -> Self {
        let target = match recorded {
            Recorded::Device(target) => target,
            Recorded::Own(audio) => return Self::Own(audio.clone()),
        };
        match target {
            AudioTarget::Input { device: None } => Self::Target(AudioInputTarget::DefaultInput),
            AudioTarget::Input {
                device: Some(device),
            } => Self::Target(AudioInputTarget::Input(DeviceId::new(device.clone()))),
            AudioTarget::Desktop => Self::Target(AudioInputTarget::DefaultOutputLoopback),
            AudioTarget::App { exe } => Self::App(exe.clone()),
        }
    }
}

/// The inputs to record: one per audio source, or the defaults while there is none,
/// and one per running source's own audio.
fn wanted_inputs(targets: &BTreeMap<SourceId, Recorded>) -> BTreeMap<Key, Want> {
    let mut wanted: BTreeMap<Key, Want> = targets
        .iter()
        .map(|(id, recorded)| (Key::Source(id.clone()), Want::of(recorded)))
        .collect();
    if !targets.values().any(|r| matches!(r, Recorded::Device(_))) {
        wanted.extend([
            (
                Key::Microphone,
                Want::Target(AudioInputTarget::DefaultInput),
            ),
            (
                Key::Desktop,
                Want::Target(AudioInputTarget::DefaultOutputLoopback),
            ),
        ]);
    }
    wanted
}

/// The program stage's audio inputs and the thread that keeps them in line with the
/// collection. Dropping it stops the thread and closes every input: each stream stops
/// before its input control goes, so no ring is freed on a real-time thread
/// (docs/RULES.md#R-RT-003).
pub(crate) struct AudioInputs {
    open: Arc<Mutex<Open>>,
    wanted: Arc<Wanted>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// The open inputs. Only the program stage's thread and its owner take this lock.
struct Open {
    mixer: MixerControl,
    audio: Arc<dyn AudioIo>,
    ledger: Arc<Mutex<DropLedger>>,
    inputs: BTreeMap<Key, Input>,
    /// Inputs whose removal the mixer's command queue could not take yet.
    removing: Vec<InputControl>,
}

/// One wanted input, and its stream while it records.
struct Input {
    want: Want,
    live: Option<Live>,
    /// Why it isn't recording, once that has been reported.
    unavailable: Option<String>,
}

/// A recording input.
struct Live {
    /// Declared first, so the stream (and the producer it calls) stops before the
    /// control below lets go of the ring.
    stream: Box<dyn AudioStream>,
    control: InputControl,
    drops: DropSourceId,
    /// The process an application input records.
    pid: Option<u32>,
}

impl AudioInputs {
    /// Opens the inputs the collection wants on `mixer`, then starts the thread that
    /// follows the collection. An input that can't be opened is unavailable, not an
    /// error.
    ///
    /// # Errors
    /// The thread could not be started.
    pub(crate) fn start(
        mixer: MixerControl,
        audio: Arc<dyn AudioIo>,
        wanted: Arc<Wanted>,
        ledger: Arc<Mutex<DropLedger>>,
    ) -> Result<Self, String> {
        let open = Arc::new(Mutex::new(Open {
            mixer,
            audio,
            ledger,
            inputs: BTreeMap::new(),
            removing: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let mut inputs = Self {
            open,
            wanted,
            stop,
            thread: None,
        };
        let targets = lock(&inputs.wanted.state).targets.clone();
        let pass = lock(&inputs.open).reconcile(&targets);
        pass.finish(&inputs.wanted, &targets);
        let thread = thread::Builder::new()
            .name("jstream-audio-sources".into())
            .spawn({
                let open = Arc::clone(&inputs.open);
                let wanted = Arc::clone(&inputs.wanted);
                let stop = Arc::clone(&inputs.stop);
                move || run(&open, &wanted, &stop)
            })
            .map_err(|err| format!("starting the jstream-audio-sources thread: {err}"))?;
        inputs.thread = Some(thread);
        Ok(inputs)
    }

    /// Stops the thread and closes every input, latching their drop counters.
    pub(crate) fn close(&mut self) {
        self.stop.store(true, Ordering::Release);
        {
            // Under the wanted lock, so the thread is either waiting (and is woken) or
            // will see the flag before it waits again.
            let _wanted = lock(&self.wanted.state);
            self.wanted.changed.notify_all();
        }
        if let Some(thread) = self.thread.take()
            && let Err(err) = thread.join()
        {
            tracing::error!(panic = ?err, "the jstream-audio-sources thread panicked");
        }
        // Cleared first, so no status says recording once a stream has stopped.
        lock(&self.wanted.state).status.clear();
        let mut open = lock(&self.open);
        for (_, input) in std::mem::take(&mut open.inputs) {
            if let Some(live) = input.live {
                open.close(live);
            }
        }
        open.removing.clear();
    }
}

impl Drop for AudioInputs {
    fn drop(&mut self) {
        self.close();
    }
}

/// The thread: reconciles whenever the collection changes, and on every tick.
fn run(open: &Mutex<Open>, wanted: &Wanted, stop: &AtomicBool) {
    loop {
        let targets = {
            let state = lock(&wanted.state);
            if stop.load(Ordering::Acquire) {
                return;
            }
            let state = wanted
                .changed
                .wait_timeout(state, TICK)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
            if stop.load(Ordering::Acquire) {
                return;
            }
            state.targets.clone()
        };
        let pass = lock(open).reconcile(&targets);
        pass.finish(wanted, &targets);
    }
}

/// What one reconcile pass did: each source's status, and what to log once the inputs'
/// lock is released (nothing is logged under a lock).
struct Pass {
    status: BTreeMap<SourceId, AudioStatus>,
    notes: Vec<Note>,
}

/// A change in one input worth a log line.
enum Note {
    Reopening(Key, &'static str),
    Available(Key),
    Unavailable(Key, String),
}

impl Pass {
    /// Logs the pass's notes and records the statuses. Called with no lock held.
    fn finish(self, wanted: &Wanted, targets: &BTreeMap<SourceId, Recorded>) {
        for note in &self.notes {
            match note {
                Note::Reopening(key, reason) => {
                    tracing::info!(input = ?key, reason, "audio input stopped; reopening it");
                }
                Note::Available(key) => {
                    tracing::info!(input = ?key, "audio input is available again");
                }
                Note::Unavailable(key, reason) => {
                    tracing::warn!(input = ?key, reason, "audio input is unavailable");
                }
            }
        }
        let mut state = lock(&wanted.state);
        state.status = self
            .status
            .into_iter()
            .filter(|(id, _)| targets.contains_key(id) && state.targets.contains_key(id))
            .collect();
    }
}

impl Open {
    /// Brings the inputs in line with `targets` and returns each source's status and
    /// what to log:
    /// closes the inputs no longer wanted or wanted differently, reopens failed ones and
    /// those whose application exited, and opens every input that isn't recording.
    fn reconcile(&mut self, targets: &BTreeMap<SourceId, Recorded>) -> Pass {
        let mut notes = Vec::new();
        self.removing.retain(|control| !control.remove());
        let wanted = wanted_inputs(targets);
        let stale: Vec<Key> = self
            .inputs
            .iter()
            .filter(|(key, input)| wanted.get(key) != Some(&input.want))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            if let Some(Input {
                live: Some(live), ..
            }) = self.inputs.remove(&key)
            {
                self.close(live);
            }
        }
        // Listed once per pass, and only when an application is wanted.
        let processes = wanted
            .values()
            .any(|want| matches!(want, Want::App(_)))
            .then(|| self.audio.processes());
        for (key, want) in wanted {
            let mut input = self.inputs.remove(&key).unwrap_or(Input {
                want,
                live: None,
                unavailable: None,
            });
            if let Some(live) = input.live.take() {
                if let Some(reason) = ended(&live, &input.want, processes.as_ref()) {
                    notes.push(Note::Reopening(key.clone(), reason));
                    self.close(live);
                } else {
                    input.live = Some(live);
                }
            }
            if input.live.is_none() {
                match self.open(&input.want, processes.as_ref()) {
                    Ok(live) => {
                        if input.unavailable.take().is_some() {
                            notes.push(Note::Available(key.clone()));
                        }
                        input.live = Some(live);
                    }
                    Err(reason) => {
                        if input.unavailable.as_ref() != Some(&reason) {
                            notes.push(Note::Unavailable(key.clone(), reason.clone()));
                        }
                        input.unavailable = Some(reason);
                    }
                }
            }
            self.inputs.insert(key, input);
        }
        let status = self
            .inputs
            .iter()
            .filter_map(|(key, input)| {
                let Key::Source(id) = key else {
                    return None;
                };
                let status = match (&input.live, &input.unavailable) {
                    (Some(_), _) => AudioStatus::Recording,
                    (None, reason) => AudioStatus::Unavailable(reason.clone().unwrap_or_default()),
                };
                Some((id.clone(), status))
            })
            .collect();
        Pass { status, notes }
    }

    /// Opens one input: resolves its target, adds a mixer input in the format the
    /// device delivers (sent to master at unity, as every new input is), and starts the
    /// stream that feeds it.
    fn open(
        &mut self,
        want: &Want,
        processes: Option<&Result<Vec<AudioProcess>, jstream_platform::PlatformError>>,
    ) -> Result<Live, String> {
        let (target, pid) = match want {
            Want::Own(audio) => return self.open_own(audio),
            Want::Target(target) => (target.clone(), None),
            Want::App(exe) => {
                let processes = match processes {
                    Some(Ok(processes)) => processes,
                    Some(Err(err)) => return Err(format!("listing processes: {err}")),
                    None => return Err("processes were not listed".into()),
                };
                let pid = find_app(processes, exe)?;
                (AudioInputTarget::ProcessLoopback { pid }, Some(pid))
            }
        };
        let format = self
            .audio
            .input_format(&target)
            .map_err(|err| err.to_string())?;
        let (producer, control) = self
            .mixer
            .add_input(format)
            .map_err(|err| format!("mixer input: {err}"))?;
        let stream = match self.audio.open_input(&target, Box::new(producer)) {
            Ok(stream) if stream.format() == format => stream,
            Ok(stream) => {
                let opened = stream.format();
                drop(stream);
                self.remove(control);
                return Err(format!(
                    "the device changed its format while opening ({opened:?}, not {format:?})"
                ));
            }
            Err(err) => {
                self.remove(control);
                return Err(err.to_string());
            }
        };
        let drops = lock_ledger(&self.ledger).add(AudioInputDrops::new(control.clone()));
        Ok(Live {
            stream,
            control,
            drops,
            pid,
        })
    }

    /// Opens a running source's own audio: a mixer input in the format it makes, fed by
    /// the source.
    fn open_own(&mut self, audio: &OwnAudio) -> Result<Live, String> {
        let format = audio.0.format();
        let (producer, control) = self
            .mixer
            .add_input(format)
            .map_err(|err| format!("mixer input: {err}"))?;
        let stream = audio.0.open(Box::new(producer));
        let drops = lock_ledger(&self.ledger).add(AudioInputDrops::new(control.clone()));
        Ok(Live {
            stream,
            control,
            drops,
            pid: None,
        })
    }

    /// Stops a recording input and removes it from the mixer, latching its drops.
    fn close(&mut self, live: Live) {
        let Live {
            stream,
            control,
            drops,
            ..
        } = live;
        drop(stream);
        lock_ledger(&self.ledger).remove(drops);
        self.remove(control);
    }

    /// Removes an input from the mixer, or retries on a later pass when its command
    /// queue is full.
    fn remove(&mut self, control: InputControl) {
        if !control.remove() {
            self.removing.push(control);
        }
    }
}

/// Why a recording input has to be reopened, if it does: its stream failed, or the
/// application it records exited.
fn ended(
    live: &Live,
    want: &Want,
    processes: Option<&Result<Vec<AudioProcess>, jstream_platform::PlatformError>>,
) -> Option<&'static str> {
    if live.control.stats().failed {
        return Some("the stream failed");
    }
    if let (Want::App(exe), Some(pid), Some(Ok(processes))) = (want, live.pid, processes)
        && !processes
            .iter()
            .any(|p| p.pid == pid && p.exe.eq_ignore_ascii_case(exe))
    {
        return Some("the application exited");
    }
    None
}

/// The process that records application `exe`: of the processes with that executable
/// name, one whose parent is not one of them (so a browser's root, not a tab), lowest
/// ID first.
fn find_app(processes: &[AudioProcess], exe: &str) -> Result<u32, String> {
    if exe.is_empty() {
        return Err("no application is named".into());
    }
    let matching: Vec<&AudioProcess> = processes
        .iter()
        .filter(|p| p.exe.eq_ignore_ascii_case(exe))
        .collect();
    let root = matching
        .iter()
        .filter(|p| {
            p.parent_pid
                .is_none_or(|parent| !matching.iter().any(|m| m.pid == parent))
        })
        .map(|p| p.pid)
        .min();
    root.or_else(|| matching.iter().map(|p| p.pid).min())
        .ok_or_else(|| format!("{exe} is not running"))
}

#[cfg(test)]
mod tests {
    use super::{AudioProcess, find_app};

    fn process(pid: u32, parent_pid: Option<u32>, exe: &str) -> AudioProcess {
        AudioProcess {
            pid,
            parent_pid,
            exe: exe.into(),
        }
    }

    #[test]
    fn an_application_is_its_root_process() {
        let processes = [
            process(4, None, "System"),
            process(30, Some(20), "chrome.exe"),
            process(20, Some(10), "chrome.exe"),
            process(10, Some(4), "explorer.exe"),
            process(25, Some(20), "chrome.exe"),
        ];
        assert_eq!(find_app(&processes, "Chrome.EXE"), Ok(20));
        assert_eq!(find_app(&processes, "explorer.exe"), Ok(10));
        assert_eq!(
            find_app(&processes, "game.exe"),
            Err("game.exe is not running".into())
        );
        assert_eq!(
            find_app(&processes, ""),
            Err("no application is named".into())
        );
    }

    #[test]
    fn a_tree_without_a_root_falls_back_to_the_lowest_id() {
        // Each names the other as parent: no root, so the lowest ID.
        let processes = [
            process(7, Some(5), "game.exe"),
            process(5, Some(7), "game.exe"),
        ];
        assert_eq!(find_app(&processes, "game.exe"), Ok(5));
    }
}
