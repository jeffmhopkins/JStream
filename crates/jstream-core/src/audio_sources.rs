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
//! Each source's input runs its audio filter chain (ADR-0026): the thread keeps the
//! input's filters in line with the chain on the same passes, updating a filter in place
//! when only its settings changed. A filter the mixer refuses is bypassed, and new
//! settings it refuses are tried again on the next pass; both are logged.
//!
//! A source that makes audio of its own (a `browser` page) gets an input too, fed by
//! the running source itself ([`jstream_sources::SourceAudio`]) from when it is built
//! until it is deleted or built again, and not while its audio is turned off.
//!
//! While the collection has no audio source, the program records the default
//! microphone and desktop audio, as it did before audio sources existed, so a
//! collection made earlier is not silent. A page's audio doesn't count as an audio
//! source here: it plays alongside the defaults.
//!
//! Each source's open input (an audio source's, a page's or a media file's) is also
//! listed in [`Routed`], where the engine's audio routing (`crate::audio`) finds it to
//! set its fader, mute and sends. A source's input opens muted, so it is heard only once
//! the routing has set it up.
//!
//! A live source with sound of its own that plays on the program's clock (a media file)
//! is one more input, opened and routed like an audio source's: the engine records it
//! here once the source is built ([`AudioSources::sync_live`]), and the thread opens its
//! stream on a mixer input as it opens a device, unless the source's audio is off. A
//! rebuilt source is a new input. Like a page's audio, its sound counts as no audio
//! source for the rule above: a video doesn't stop the microphone being recorded.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use jstream_audio::{InputControl, MixerControl};
use jstream_platform::{
    AudioInputTarget, AudioIo, AudioProcess, AudioStream, DeviceId, MediaClock,
};
use jstream_protocol::{SourceId, SourceKind};
use jstream_sources::{AudioTarget, SourceAudio};

use crate::drops::{AudioInputDrops, DropLedger, DropSourceId, lock_ledger};
use crate::live::LiveAudio;
use crate::filters::{Chain, Chained, chain_of};
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
    /// The audio sources' open inputs, for the routing.
    routed: Routed,
}

/// The mixer input of each audio source that records, kept by the program stage's
/// thread for the engine's audio routing. Its lock is taken last: nothing else is
/// locked while it is held but what was locked before it.
#[derive(Default)]
pub(crate) struct Routed {
    inputs: Mutex<BTreeMap<SourceId, InputControl>>,
    /// Set when an input opened or closed, until the routing has caught up.
    changed: AtomicBool,
}

impl Routed {
    fn insert(&self, id: SourceId, control: InputControl) {
        lock(&self.inputs).insert(id, control);
        self.changed.store(true, Ordering::Release);
    }

    fn remove(&self, id: &SourceId) {
        lock(&self.inputs).remove(id);
        self.changed.store(true, Ordering::Release);
    }
}

/// Each source the program records for: an audio-only source of the collection, or a
/// running source with audio of its own.
type Targets = BTreeMap<SourceId, Target>;

/// What the program records for one source, and whether its audio is on (ADR-0028: off,
/// the engine doesn't open its stream).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    recorded: Recorded,
    on: bool,
}

#[derive(Default)]
struct WantedState {
    targets: Targets,
    /// The live sources' own sound.
    live: BTreeMap<SourceId, LiveAudio>,
    /// The sources whose audio is off, of any kind: a live source's sound isn't opened
    /// while it is.
    off: BTreeSet<SourceId>,
    /// Each source's mixer input's packets, as the last pass saw them, for tests.
    #[cfg(test)]
    packets: BTreeMap<SourceId, u64>,
    /// Each source's audio filter chain, as the mixer runs it. Sources without filters
    /// have no entry.
    chains: BTreeMap<SourceId, Vec<Chained>>,
    /// How each source fares while the program stage runs.
    status: BTreeMap<SourceId, AudioStatus>,
}

/// Whether the program stage records an audio source or mixes a source's sound.
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
        let mut targets: Targets = state
            .sources()
            .filter_map(|source| {
                let SourceKind::Builtin { kind } = source.kind() else {
                    return None;
                };
                let target = AudioTarget::of(kind, source.settings())?;
                let on = source.audio().is_none_or(|audio| audio.enabled);
                let recorded = Recorded::Device(target);
                Some((source.id().clone(), Target { recorded, on }))
            })
            .collect();
        for (id, audio) in own {
            if let Some(source) = state.source(&id) {
                let on = source.audio().is_none_or(|audio| audio.enabled);
                let recorded = Recorded::Own(OwnAudio(audio));
                targets.entry(id).or_insert(Target { recorded, on });
            }
        }
        let off: BTreeSet<SourceId> = state
            .sources()
            .filter(|source| source.audio().is_some_and(|audio| !audio.enabled))
            .map(|source| source.id().clone())
            .collect();
        let chains: BTreeMap<SourceId, Vec<Chained>> = state
            .sources()
            .map(|source| (source.id().clone(), chain_of(source)))
            .filter(|(_, chain)| !chain.is_empty())
            .collect();
        let mut wanted = lock(&self.wanted.state);
        if wanted.chains != chains {
            wanted.chains = chains;
            self.wanted.changed.notify_all();
        }
        if wanted.targets != targets || wanted.off != off {
            wanted.targets = targets;
            wanted.off = off;
            changed(&mut wanted, &self.wanted.changed);
        }
    }

    /// Brings the live sources' sound in line with `live`, and wakes the program
    /// stage's thread when it changed.
    pub(crate) fn sync_live(&mut self, live: BTreeMap<SourceId, LiveAudio>) {
        let mut wanted = lock(&self.wanted.state);
        if wanted.live != live {
            wanted.live = live;
            changed(&mut wanted, &self.wanted.changed);
        }
    }

    /// The wanted set, for the program stage.
    pub(crate) fn wanted(&self) -> Arc<Wanted> {
        Arc::clone(&self.wanted)
    }

    /// The audio sources' open mixer inputs.
    pub(crate) fn inputs(&self) -> MutexGuard<'_, BTreeMap<SourceId, InputControl>> {
        lock(&self.wanted.routed.inputs)
    }

    /// Whether an input opened or closed since the last call.
    pub(crate) fn take_inputs_changed(&self) -> bool {
        self.wanted.routed.changed.swap(false, Ordering::AcqRel)
    }

    /// Lists `input` as `source_id`'s, as the program stage does when it opens one.
    #[cfg(test)]
    pub(crate) fn insert_input(&self, source_id: SourceId, input: InputControl) {
        self.wanted.routed.insert(source_id, input);
    }

    /// The packets `source_id`'s mixer input has received, as the program stage's last
    /// pass saw them.
    #[cfg(test)]
    pub(crate) fn packets(&self, source_id: &SourceId) -> Option<u64> {
        lock(&self.wanted.state).packets.get(source_id).copied()
    }

    /// How the program stage fares with `source_id`, while it runs.
    #[cfg(test)]
    pub(crate) fn status(&self, source_id: &SourceId) -> Option<AudioStatus> {
        lock(&self.wanted.state).status.get(source_id).cloned()
    }
}

/// After a change to the wanted set: a deleted source, or one whose audio is off, has no
/// status from now on, even before the thread closes its input; and the thread wakes.
fn changed(wanted: &mut WantedState, wake: &Condvar) {
    let wants = Wants::of(wanted);
    wanted.status.retain(|id, _| wants.names(id));
    wake.notify_all();
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
    /// A live source's own sound.
    Live(LiveAudio),
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

/// What the program stage's thread works from: a copy of the wanted set.
#[derive(Clone)]
struct Wants {
    targets: Targets,
    /// The live sources' sound, but for the sources whose audio is off.
    live: BTreeMap<SourceId, LiveAudio>,
    /// Each source's audio filter chain.
    chains: BTreeMap<SourceId, Vec<Chained>>,
}

impl Wants {
    fn of(state: &WantedState) -> Self {
        Self {
            targets: state.targets.clone(),
            live: state
                .live
                .iter()
                .filter(|(id, _)| !state.off.contains(*id))
                .map(|(id, live)| (id.clone(), live.clone()))
                .collect(),
            chains: state.chains.clone(),
        }
    }

    /// Whether `id` is a source this opens an input for.
    fn names(&self, id: &SourceId) -> bool {
        self.targets.get(id).is_some_and(|target| target.on) || self.live.contains_key(id)
    }
}

/// The inputs to record: one per audio source, per running source's own audio and per
/// live source's sound whose audio is on, and the defaults while the collection has no
/// audio-only source at all (one whose audio is off still counts, so turning the last
/// one off doesn't bring the defaults back; a page's or a file's sound doesn't count).
fn wanted_inputs(wants: &Wants) -> BTreeMap<Key, Want> {
    let targets = &wants.targets;
    let mut wanted: BTreeMap<Key, Want> = targets
        .iter()
        .filter(|(_, target)| target.on)
        .map(|(id, target)| (Key::Source(id.clone()), Want::of(&target.recorded)))
        .chain(
            wants
                .live
                .iter()
                .map(|(id, live)| (Key::Source(id.clone()), Want::Live(live.clone()))),
        )
        .collect();
    if !targets
        .values()
        .any(|target| matches!(target.recorded, Recorded::Device(_)))
    {
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
    /// Where each audio source's input is listed for the routing.
    wanted: Arc<Wanted>,
    audio: Arc<dyn AudioIo>,
    /// The media clock a live source's sound is stamped on.
    clock: Arc<dyn MediaClock>,
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
    /// The source's filters on this input.
    chain: Chain,
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
        clock: Arc<dyn MediaClock>,
        wanted: Arc<Wanted>,
        ledger: Arc<Mutex<DropLedger>>,
    ) -> Result<Self, String> {
        let open = Arc::new(Mutex::new(Open {
            mixer,
            wanted: Arc::clone(&wanted),
            audio,
            clock,
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
        let wants = Wants::of(&lock(&inputs.wanted.state));
        let pass = lock(&inputs.open).reconcile(&wants);
        pass.finish(&inputs.wanted, &wants);
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
        for (key, input) in std::mem::take(&mut open.inputs) {
            if let Some(live) = input.live {
                open.close(&key, live);
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
        let wants = {
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
            Wants::of(&state)
        };
        let pass = lock(open).reconcile(&wants);
        pass.finish(wanted, &wants);
    }
}

/// What one reconcile pass did: each source's status, and what to log once the inputs'
/// lock is released (nothing is logged under a lock).
struct Pass {
    status: BTreeMap<SourceId, AudioStatus>,
    notes: Vec<Note>,
    /// Each source's mixer input's packets so far, for tests.
    #[cfg(test)]
    packets: BTreeMap<SourceId, u64>,
}

/// A change in one input worth a log line.
enum Note {
    Reopening(Key, &'static str),
    Available(Key),
    Unavailable(Key, String),
    /// A filter, or a filter's new settings, the mixer refused.
    Refused(Key, String),
}

impl Pass {
    /// Logs the pass's notes and records the statuses. Called with no lock held.
    fn finish(self, wanted: &Wanted, wants: &Wants) {
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
                Note::Refused(key, reason) => {
                    tracing::warn!(input = ?key, reason, "audio filter not applied");
                }
            }
        }
        let mut state = lock(&wanted.state);
        #[cfg(test)]
        {
            state.packets = self.packets;
        }
        let now = Wants::of(&state);
        state.status = self
            .status
            .into_iter()
            .filter(|(id, _)| wants.names(id) && now.names(id))
            .collect();
    }
}

impl Open {
    /// Brings the inputs in line with `targets` and returns each source's status and
    /// what to log:
    /// closes the inputs no longer wanted or wanted differently, reopens failed ones and
    /// those whose application exited, opens every input that isn't recording, and
    /// brings each source input's filters in line with its chain.
    fn reconcile(&mut self, wants: &Wants) -> Pass {
        let mut notes = Vec::new();
        self.removing.retain(|control| !control.remove());
        let wanted = wanted_inputs(wants);
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
                self.close(&key, live);
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
                    self.close(&key, live);
                } else {
                    input.live = Some(live);
                }
            }
            if input.live.is_none() {
                match self.open(&key, &input.want, processes.as_ref()) {
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
            if let (Key::Source(id), Some(live)) = (&key, input.live.as_mut()) {
                let chain = wants.chains.get(id).map_or(&[][..], Vec::as_slice);
                for reason in live.chain.apply(&live.control, chain) {
                    notes.push(Note::Refused(key.clone(), reason));
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
        #[cfg(test)]
        let packets = self
            .inputs
            .iter()
            .filter_map(|(key, input)| {
                let (Key::Source(id), Some(live)) = (key, &input.live) else {
                    return None;
                };
                Some((id.clone(), live.control.stats().packets))
            })
            .collect();
        Pass {
            status,
            notes,
            #[cfg(test)]
            packets,
        }
    }

    /// Opens one input: resolves its target, adds a mixer input in the format the
    /// device delivers (sent to master at unity, as every new input is), and starts the
    /// stream that feeds it. An audio source's input opens muted and is listed for the
    /// routing, which sets it up as the collection says; the default microphone and
    /// desktop inputs stay as they open.
    fn open(
        &mut self,
        key: &Key,
        want: &Want,
        processes: Option<&Result<Vec<AudioProcess>, jstream_platform::PlatformError>>,
    ) -> Result<Live, String> {
        let (target, pid) = match want {
            Want::Live(live) => {
                let Key::Source(id) = key else {
                    return Err("a live source's sound without its source".into());
                };
                return self.open_live(id, live);
            }
            Want::Own(audio) => return self.open_own(key, audio),
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
        if matches!(key, Key::Source(_)) {
            control.set_muted(true);
        }
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
        if let Key::Source(id) = key {
            self.wanted.routed.insert(id.clone(), control.clone());
        }
        Ok(Live {
            stream,
            control,
            drops,
            pid,
            chain: Chain::default(),
        })
    }

    /// Opens a running source's own audio: a mixer input in the format it makes, fed by
    /// the source, opened muted and listed for the routing as an audio source's is.
    fn open_own(&mut self, key: &Key, audio: &OwnAudio) -> Result<Live, String> {
        let format = audio.0.format();
        let (producer, control) = self
            .mixer
            .add_input(format)
            .map_err(|err| format!("mixer input: {err}"))?;
        control.set_muted(true);
        let stream = audio.0.open(Box::new(producer));
        let drops = lock_ledger(&self.ledger).add(AudioInputDrops::new(control.clone()));
        if let Key::Source(id) = key {
            self.wanted.routed.insert(id.clone(), control.clone());
        }
        Ok(Live {
            stream,
            control,
            drops,
            pid: None,
            chain: Chain::default(),
        })
    }

    /// Opens a live source's sound: a mixer input in its format, fed by the source's
    /// own stream, opened muted and listed for the routing as an audio source's is.
    fn open_live(&mut self, id: &SourceId, live: &LiveAudio) -> Result<Live, String> {
        let (producer, control) = self
            .mixer
            .add_input(live.audio.format())
            .map_err(|err| format!("mixer input: {err}"))?;
        control.set_muted(true);
        let stream = match live
            .audio
            .open(Box::new(producer), Arc::clone(&self.clock), live.start)
        {
            Ok(stream) => stream,
            Err(err) => {
                self.remove(control);
                return Err(err);
            }
        };
        let drops = lock_ledger(&self.ledger).add(AudioInputDrops::new(control.clone()));
        self.wanted.routed.insert(id.clone(), control.clone());
        Ok(Live {
            stream,
            control,
            drops,
            pid: None,
            chain: Chain::default(),
        })
    }

    /// Stops a recording input and removes it from the mixer and the routing, latching
    /// its drops.
    fn close(&mut self, key: &Key, live: Live) {
        if let Key::Source(id) = key {
            self.wanted.routed.remove(id);
        }
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
