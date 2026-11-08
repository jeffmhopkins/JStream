//! The outputs panel (#1206): a header with the pipeline's health from `stats.tick` and
//! Start all (or Stop all), then one compact row for the program output and for each
//! output, with its state, Start or Stop, its FX button, which shows that chain in the
//! Filters panel (spec-panels.md 9, ADR-0080), and a ⋯ menu (Settings…, Duplicate,
//! Delete…). A running output's row has a second line with its bitrate, drops and
//! uptime from `stats.tick`'s `outputs`. An output's settings (its server URL or path,
//! stream key, video mix, encoder profile and audio bus) are in its own dialog, which
//! applies nothing until OK or Apply; adding an output opens the same dialog. The
//! encoder profiles and video mixes are edited in the Encoders & mixes dialog
//! ([`encoders`](super::encoders)). It also holds the logic behind the header's Go Live
//! (`go_live_targets`, `live_outputs`, `start_or_stop_all`).
//!
//! The stream key is write-only (R-SEC-001). It is read from its password field only
//! when OK or Apply sends it, wrapped in a [`SecretString`] at once, sent with
//! `output.set_key`, and the field is emptied. It never goes into a signal, the page's
//! storage, the console or an error message, and the engine never sends it back: the
//! panel only learns `key_set` from `output.status`.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/RULES.md#R-SEC-001

use crate::app::SessionContext;
use crate::dialog::Modal;
use crate::dock::PanelId;
use crate::icons::Icon;
use crate::menu::{Entry, Menus};
use crate::panels::encoders::{LibraryPick, Profiles};
use crate::panels::filters::FilterDialogs;
use crate::panels::stats::{mbps, uptime};
use crate::session::Session;
use crate::workspace::DockState;
use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, Batch, BusId, BusInfo, Call, CallResult, DropCounts, Edit, EncoderProfileId,
    EncoderProfileInfo, Event, FilterChain, FilterChains, FilterOwner, OutputId, OutputInfo,
    OutputKind, OutputState, OutputStatsInfo, Query, QueryResult, RecordContainer, Rev, RtmpTarget,
    SceneId, SceneRole, SecretString, VideoMixId, VideoMixInfo,
};
use jstream_control::{Control, ControlError};
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;

/// One output as the panel shows it: its collection state and its runtime state.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputRow {
    /// What it is, where it sends and its encoder profile.
    pub info: OutputInfo,
    /// Whether it runs.
    pub state: OutputState,
    /// Whether the engine's credential store holds a stream key for it. The key itself
    /// never reaches the UI.
    pub key_set: bool,
    /// Whether the engine says publishing sends the stream key unencrypted
    /// (`output.status`'s `cleartext`, from 0.37).
    pub cleartext: bool,
}

/// What `output.status` reports that the panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// Whether it runs.
    pub state: OutputState,
    /// Whether a stream key is set.
    pub key_set: bool,
    /// Whether publishing sends the stream key unencrypted.
    pub cleartext: bool,
}

/// The outputs, encoder profiles and video mixes at one revision, with each output's
/// status.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// The revision `state.get` read.
    pub rev: Rev,
    /// The outputs, ordered by ID.
    pub outputs: Vec<OutputRow>,
    /// The encoder profiles an output can choose, ordered by ID.
    pub encoders: Vec<EncoderProfileInfo>,
    /// The audio buses an output can carry, ordered by ID, the master among them.
    pub buses: Vec<BusInfo>,
    /// The video mixes an output can show, ordered by ID.
    pub mixes: Vec<VideoMixInfo>,
    /// The scenes a mix can show, by ID and name, in display order: groups left out.
    pub scenes: Vec<(SceneId, String)>,
}

/// Reads the outputs, encoder profiles, video mixes, buses and scenes (`state.get`),
/// then each output's status (`output.status`).
pub async fn load(control: &dyn Control) -> Result<Snapshot, String> {
    let (rev, outputs, encoders, buses, mixes, scenes) =
        match query(control, Query::StateGet).await? {
            QueryResult::StateGet {
                rev,
                outputs,
                encoders,
                buses,
                video_mixes,
                scenes,
                ..
            } => (rev, outputs, encoders, buses, video_mixes, scenes),
            other => return Err(unexpected("state.get", other.name())),
        };
    let mut rows = Vec::with_capacity(outputs.len());
    for info in outputs {
        let Status {
            state,
            key_set,
            cleartext,
        } = status(control, &info.output_id).await?;
        rows.push(OutputRow {
            info,
            state,
            key_set,
            cleartext,
        });
    }
    Ok(Snapshot {
        rev,
        outputs: rows,
        encoders,
        buses,
        mixes,
        scenes: scenes
            .into_iter()
            .filter(|scene| scene.role != SceneRole::Group)
            .map(|scene| (scene.scene_id, scene.name))
            .collect(),
    })
}

/// One output's runtime state, whether its stream key is set, and whether publishing
/// sends that key unencrypted (`output.status`).
pub async fn status(control: &dyn Control, output_id: &OutputId) -> Result<Status, String> {
    let query_status = Query::OutputStatus {
        output_id: output_id.clone(),
    };
    match query(control, query_status).await? {
        QueryResult::OutputStatus {
            state,
            key_set,
            cleartext,
            ..
        } => Ok(Status {
            state,
            key_set,
            cleartext,
        }),
        other => Err(unexpected("output.status", other.name())),
    }
}

/// Sends one edit as a batch of one.
pub async fn edit(control: &dyn Control, edit: Edit) -> Result<(), String> {
    control
        .call(Call::Batch(Batch::single(edit)))
        .await
        .map(drop)
        .map_err(|error| describe(&error))
}

/// Sends `edits` as one batch, one undo step. Nothing to send sends nothing.
pub async fn send_edits(control: &dyn Control, edits: Vec<Edit>) -> Result<(), String> {
    if edits.is_empty() {
        return Ok(());
    }
    let batch = Batch {
        edits,
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    };
    control
        .call(Call::Batch(batch))
        .await
        .map(drop)
        .map_err(|error| describe(&error))
}

/// Sends one action. Its progress comes back as `output.state_changed` events.
pub async fn action(control: &dyn Control, action: Action) -> Result<(), String> {
    control
        .call(Call::Action { action })
        .await
        .map(drop)
        .map_err(|error| describe(&error))
}

/// Starts or stops each of `outputs`, one after another. One output's failure doesn't
/// keep the others from being tried: every output is attempted, and the error names
/// each one that failed, with its reason.
pub async fn start_or_stop_all(
    control: &dyn Control,
    outputs: Vec<OutputId>,
    start: bool,
) -> Result<(), String> {
    let verb = if start { "start" } else { "stop" };
    let mut failures = Vec::new();
    for output_id in outputs {
        let name = output_id.to_string();
        let call = if start {
            Action::OutputStart { output_id }
        } else {
            Action::OutputStop { output_id }
        };
        if let Err(reason) = action(control, call).await {
            failures.push(format!("couldn't {verb} {name}: {reason}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// Sends one query and returns its result.
async fn query(control: &dyn Control, query: Query) -> Result<QueryResult, String> {
    match control.call(Call::Query { query }).await {
        Ok(response) => match response.result {
            CallResult::Query { result } => Ok(result),
            _ => Err("the engine answered a query with something else".into()),
        },
        Err(error) => Err(describe(&error)),
    }
}

/// A failed call, for the panel. The engine's messages never carry a stream key
/// (R-SEC-001), and the panel adds none.
fn describe(error: &ControlError) -> String {
    error.to_string()
}

/// A query answered with another query's result.
fn unexpected(asked: &str, got: &str) -> String {
    format!("the engine answered `{asked}` with `{got}`")
}

/// The kind of output the create form makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewKind {
    /// A stream to an RTMP(S) server.
    Rtmp,
    /// A recording to a file on the engine's machine.
    Record,
    /// The virtual camera: the program as a webcam other apps on the engine's machine
    /// see.
    VirtualCam,
}

impl NewKind {
    /// Whether the form needs a target (a server URL or a path) for this kind.
    pub fn needs_target(self) -> bool {
        !matches!(self, NewKind::VirtualCam)
    }
}

/// The `output.create` edit for a new output. `target` is the RTMP server URL or the
/// recording's path, as the form took it; the engine validates it. A virtual camera
/// takes none, so `target` is ignored.
pub fn create_edit(output_id: OutputId, kind: NewKind, target: &str) -> Edit {
    let target = target.trim().to_owned();
    let kind = match kind {
        NewKind::Rtmp => OutputKind::Rtmp {
            target: RtmpTarget::Generic { url: target },
        },
        NewKind::Record => OutputKind::Record {
            path: target,
            container: RecordContainer::Mp4,
        },
        NewKind::VirtualCam => OutputKind::VirtualCam,
    };
    Edit::OutputCreate { output_id, kind }
}

/// The first `output-N` ID no existing output has.
pub fn next_output_id<'a>(existing: impl IntoIterator<Item = &'a OutputId> + Clone) -> OutputId {
    (1_u32..)
        .map(|n| OutputId::new(format!("output-{n}")))
        .find(|id| !existing.clone().into_iter().any(|taken| taken == id))
        .unwrap_or_else(|| OutputId::new("output"))
}

/// The stream key the user typed, ready to send: surrounding whitespace (a pasted
/// newline) trimmed. `None` when nothing is left. The typed string is consumed, so
/// the caller keeps no copy of it.
pub fn take_key(typed: String) -> Option<SecretString> {
    let trimmed = typed.trim();
    if trimmed.is_empty() {
        None
    } else if trimmed.len() == typed.len() {
        Some(SecretString::new(typed))
    } else {
        Some(SecretString::new(trimmed))
    }
}

/// The edit that has `output_id` carry the bus a select's `value` names: one audio track
/// (`output.set_audio_buses`, protocol 0.59), which a stopped output takes.
pub fn bus_edit(output_id: OutputId, value: &str) -> Edit {
    Edit::OutputSetAudioBuses {
        output_id,
        audio_buses: vec![BusId::new(value)],
    }
}

/// Whether an output runs: every state but `stopped` and `failed`.
pub fn is_running(state: OutputState) -> bool {
    !matches!(state, OutputState::Stopped | OutputState::Failed)
}

/// What the page knows about the engine's virtual camera. Only the desktop app says
/// (its `vcam`, see `host.rs`); in a browser it is [`Vcam::Unknown`], and starting the
/// output is how to find out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vcam {
    /// Registered for this install: the camera can be started.
    Registered,
    /// Not registered: Windows doesn't list the camera until it is.
    NotRegistered,
    /// Registered, but not for a current copy of this install's DLL: an update changed
    /// it, or an older JStream registered its own folder. The old registration keeps
    /// working until the user opts in to register again.
    Outdated,
    /// The app was started with `--no-vcam`, so its engine has no virtual camera.
    Off,
    /// The page doesn't know: it isn't running in the desktop app.
    Unknown,
}

/// The optional step that registers the camera. It asks for administrator rights once.
pub const REGISTER_HINT: &str = "Registering the virtual camera is optional: to use it, run `jstream-daemon --register-vcam` from the JStream folder (it asks for administrator rights once), then restart JStream.";

/// The optional step that updates an outdated registration. Until then the camera keeps
/// using the copy registered before.
pub const UPDATE_HINT: &str = "Optional: run `jstream-daemon --register-vcam` from the JStream folder to register this version's camera (it asks for administrator rights), then restart JStream. Until then the camera registered before is used.";

impl Vcam {
    /// The desktop app's `vcam` value; anything else, or none, is [`Vcam::Unknown`].
    pub fn from_host(value: Option<&str>) -> Vcam {
        match value {
            Some("registered") => Vcam::Registered,
            Some("not_registered") => Vcam::NotRegistered,
            Some("outdated") => Vcam::Outdated,
            Some("off") => Vcam::Off,
            _ => Vcam::Unknown,
        }
    }

    /// The status line a virtual camera output shows.
    pub fn status(self) -> &'static str {
        match self {
            Vcam::Registered => "Virtual camera: registered",
            Vcam::NotRegistered => "Virtual camera: not registered",
            Vcam::Outdated => "Virtual camera: registered, but not for this version",
            Vcam::Off => "Virtual camera: off in this app",
            Vcam::Unknown => "Virtual camera: registration not known here",
        }
    }

    /// What to do about it, if anything.
    pub fn hint(self) -> Option<&'static str> {
        match self {
            Vcam::Registered => None,
            Vcam::NotRegistered => Some(REGISTER_HINT),
            Vcam::Outdated => Some(UPDATE_HINT),
            Vcam::Off => Some("Start JStream without `--no-vcam` to use it."),
            Vcam::Unknown => Some(
                "The engine checks the camera when it starts: an unregistered one is refused with how to register it.",
            ),
        }
    }
}

/// Why an output can't be started now, or `None` when it can. `vcam` matters only to
/// a virtual camera output: one the app says is unregistered or off can't start.
pub fn start_blocker(row: &OutputRow, vcam: Vcam) -> Option<&'static str> {
    if is_running(row.state) {
        return Some("It is already running.");
    }
    match &row.info.kind {
        OutputKind::Rtmp { .. } if !row.key_set => Some("Set a stream key first."),
        OutputKind::Rtmp { .. } | OutputKind::Record { .. } => None,
        OutputKind::VirtualCam => match vcam {
            Vcam::NotRegistered => Some("Register the virtual camera first."),
            Vcam::Off => Some("The virtual camera is off in this app."),
            Vcam::Registered | Vcam::Outdated | Vcam::Unknown => None,
        },
        _ => Some("This engine's output kind is newer than this page."),
    }
}

/// What a start refused with "no virtual camera" means: the engine was built or started
/// without a camera, which registering can't change.
pub const NO_CAMERA_HINT: &str = "This engine has no virtual camera: it runs on Windows 11 only, and the desktop app has none when started with `--no-vcam`.";

/// A failed virtual camera start, for the panel. The engine's two reasons that need
/// explaining get a hint: an unregistered camera (the platform's "is not registered",
/// which names only the flag) gets the full registration command, and an engine with
/// no camera at all ("no virtual camera") gets why. Other reasons, and other kinds,
/// are shown as they are.
pub fn explain_start_failure(kind: &OutputKind, reason: String) -> String {
    if !matches!(kind, OutputKind::VirtualCam) {
        reason
    } else if reason.contains("is not registered") {
        format!("{reason}. {REGISTER_HINT}")
    } else if reason.contains("no virtual camera") {
        format!("{reason}. {NO_CAMERA_HINT}")
    } else {
        reason
    }
}

/// The outputs "Go live" starts: every stream (RTMP) output that is stopped or failed
/// and has its stream key set. Recordings start on their own buttons.
pub fn go_live_targets<'a>(rows: impl IntoIterator<Item = &'a OutputRow>) -> Vec<OutputId> {
    rows.into_iter()
        .filter(|row| matches!(row.info.kind, OutputKind::Rtmp { .. }))
        .filter(|row| start_blocker(row, Vcam::Unknown).is_none())
        .map(|row| row.info.output_id.clone())
        .collect()
}

/// The stream (RTMP) outputs that run: what "End stream" stops.
pub fn live_outputs<'a>(rows: impl IntoIterator<Item = &'a OutputRow>) -> Vec<OutputId> {
    rows.into_iter()
        .filter(|row| matches!(row.info.kind, OutputKind::Rtmp { .. }))
        .filter(|row| is_running(row.state))
        .map(|row| row.info.output_id.clone())
        .collect()
}

/// What the panel's Start all button does now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AllOutputs {
    /// Starts these: every output that is stopped or failed and can start.
    Start(Vec<OutputId>),
    /// Stops these: every output that runs, when none is left to start.
    Stop(Vec<OutputId>),
    /// Nothing runs, and nothing can start.
    Nothing,
}

/// What Start all does over `rows`: start each output that can start (any kind, unlike
/// the header's Go Live, which starts streams), or, once none can, stop each that runs.
pub fn all_outputs<'a>(rows: impl IntoIterator<Item = &'a OutputRow>, vcam: Vcam) -> AllOutputs {
    let rows: Vec<&OutputRow> = rows.into_iter().collect();
    let startable: Vec<OutputId> = rows
        .iter()
        .filter(|row| start_blocker(row, vcam).is_none())
        .map(|row| row.info.output_id.clone())
        .collect();
    if !startable.is_empty() {
        return AllOutputs::Start(startable);
    }
    let running: Vec<OutputId> = rows
        .iter()
        .filter(|row| is_running(row.state))
        .map(|row| row.info.output_id.clone())
        .collect();
    if running.is_empty() {
        AllOutputs::Nothing
    } else {
        AllOutputs::Stop(running)
    }
}

/// A running output's second line: its bitrate, what it lost this run (frames and
/// packets) and its uptime, as "2.45 Mbps · 0 dropped · 00:12:34".
pub fn live_line(stats: &OutputStatsInfo) -> String {
    let dropped = stats.dropped_frames.saturating_add(stats.dropped_packets);
    format!(
        "{} · {dropped} dropped · {}",
        mbps(stats.kbps),
        uptime(Some(stats.uptime_ms))
    )
}

/// The batch that makes `output_id` a copy of `info`: the same kind and target, encoder,
/// mix and bus, then each of its filters with its settings, in order. The stream key
/// isn't copied, as the page never has it (R-SEC-001). `None` for a kind this page
/// doesn't edit.
pub fn duplicate_edits(output_id: &OutputId, info: &OutputInfo) -> Option<Vec<Edit>> {
    let draft = OutputDraft::of(info)?;
    let mut edits = draft_edits(output_id, None, &draft);
    let owner = FilterOwner::Output {
        output_id: output_id.clone(),
    };
    for (chain, filters) in [
        (FilterChain::Video, &info.filters.video),
        (FilterChain::Audio, &info.filters.audio),
    ] {
        for filter in filters {
            edits.push(Edit::FilterAdd {
                owner: owner.clone(),
                chain,
                filter_id: filter.filter_id.clone(),
                kind: filter.kind.clone(),
                settings: filter.settings.clone(),
                enabled: (!filter.enabled).then_some(false),
                index: None,
            });
        }
    }
    Some(edits)
}

/// What an output is and where it sends, in a line.
pub fn describe_kind(kind: &OutputKind) -> String {
    match kind {
        OutputKind::Rtmp {
            target: RtmpTarget::Generic { url },
        } => format!("Stream to {url}"),
        OutputKind::Rtmp { .. } => "Stream (a target this page doesn't know)".into(),
        OutputKind::Record { path, .. } => format!("Record to {path}"),
        OutputKind::VirtualCam => "Virtual camera: the program as a webcam".into(),
        _ => "An output kind this page doesn't know".into(),
    }
}

/// The icon of an output of this kind (WS-04): the broadcast arcs for a stream, film
/// for a recording, the webcam for the virtual camera.
pub fn kind_icon(kind: &OutputKind) -> &'static str {
    match kind {
        OutputKind::Record { .. } => "film",
        OutputKind::VirtualCam => "webcam",
        _ => "out",
    }
}

/// The warning an output's row and settings show when the engine says its server is plain
/// `rtmp://` to another machine (`output.status`'s `cleartext`): the stream key would
/// cross the network unencrypted (#353). The rule is the engine's
/// (`OutputKind::sends_key_in_cleartext`), so `rtmps://` and a loopback relay get none.
pub const CLEARTEXT_WARNING: &str = "This server is plain rtmp://, so the stream key is sent unencrypted. Use rtmps:// if the service offers it.";

/// [`CLEARTEXT_WARNING`] for an output that needs it, else `None`.
pub fn cleartext_warning(row: &OutputRow) -> Option<&'static str> {
    row.cleartext.then_some(CLEARTEXT_WARNING)
}

/// An output state, for its badge.
pub fn state_label(state: OutputState) -> &'static str {
    match state {
        OutputState::Stopped => "Stopped",
        OutputState::Starting => "Starting",
        OutputState::Active => "Live",
        OutputState::Reconnecting => "Reconnecting",
        OutputState::Stopping => "Stopping",
        OutputState::Failed => "Failed",
        _ => "Unknown",
    }
}

/// An FX button's label (spec-panels.md U1): "FX" and how many filters the chains hold.
pub fn fx_label(chains: &FilterChains) -> String {
    format!("FX {}", chains.video.len() + chains.audio.len())
}

/// How many `stats.tick` intervals [`Health`] sums over: about five seconds.
pub const HEALTH_WINDOW: usize = 5;

/// The pipeline's health, from `stats.tick`'s running drop totals: what was dropped
/// over the last [`HEALTH_WINDOW`] ticks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    /// The last tick seen: its `seq` and totals.
    last: Option<(u64, DropCounts)>,
    /// The drops of each recent interval, newest last, with the ticks missed in it.
    recent: VecDeque<(DropCounts, u64)>,
}

/// What [`Health::reading`] reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HealthReading {
    /// Whether any tick has arrived yet.
    pub sampled: bool,
    /// Drops over the window.
    pub drops: DropCounts,
    /// Ticks the window is missing (a gap in `seq`): the drops still count, over a
    /// longer time.
    pub missed_ticks: u64,
}

impl Health {
    /// Takes one `stats.tick`. A `seq` that doesn't increase, or a total that shrank,
    /// means the engine restarted: the totals start over and nothing is counted for
    /// that tick.
    pub fn observe(&mut self, seq: u64, drops: DropCounts) {
        if let Some((last_seq, last)) = self.last
            && seq > last_seq
            && let Some(delta) = difference(drops, last)
        {
            self.recent.push_back((delta, seq - last_seq - 1));
            while self.recent.len() > HEALTH_WINDOW {
                self.recent.pop_front();
            }
        } else if self.last.is_some() {
            self.recent.clear();
        }
        self.last = Some((seq, drops));
    }

    /// The drops over the window.
    pub fn reading(&self) -> HealthReading {
        let (drops, missed_ticks) = self.recent.iter().fold(
            (DropCounts::default(), 0_u64),
            |(sum, missed), &(delta, gap)| (sum.saturating_add(delta), missed + gap),
        );
        HealthReading {
            sampled: self.last.is_some(),
            drops,
            missed_ticks,
        }
    }
}

/// `now - before`, field by field, or `None` when a total went down.
fn difference(now: DropCounts, before: DropCounts) -> Option<DropCounts> {
    Some(DropCounts {
        capture_frames: now.capture_frames.checked_sub(before.capture_frames)?,
        audio_frames: now.audio_frames.checked_sub(before.audio_frames)?,
        render_ticks: now.render_ticks.checked_sub(before.render_ticks)?,
        encode_frames: now.encode_frames.checked_sub(before.encode_frames)?,
        encode_blocks: now.encode_blocks.checked_sub(before.encode_blocks)?,
        output_packets: now.output_packets.checked_sub(before.output_packets)?,
    })
}

impl HealthReading {
    /// The reading in a line: the drops by stage, or that nothing was dropped.
    pub fn summary(&self) -> String {
        if !self.sampled {
            return "No stats yet.".into();
        }
        let d = self.drops;
        let parts: Vec<String> = [
            (d.output_packets, "output packets"),
            (d.encode_frames, "encoder frames"),
            (d.encode_blocks, "encoder audio blocks"),
            (d.render_ticks, "render ticks"),
            (d.capture_frames, "capture frames"),
            (d.audio_frames, "audio frames"),
        ]
        .into_iter()
        .filter(|&(count, _)| count > 0)
        .map(|(count, what)| format!("{count} {what}"))
        .collect();
        if parts.is_empty() {
            "Healthy: nothing dropped in the last few seconds.".into()
        } else {
            format!("Dropped {} in the last few seconds.", parts.join(", "))
        }
    }

    /// Whether anything was dropped over the window.
    pub fn dropping(&self) -> bool {
        self.drops != DropCounts::default()
    }
}

impl HealthReading {
    /// The reading in a word or two, for the panel's header: "Healthy", "Dropping (N)"
    /// with everything dropped over the window, or "No stats yet". [`summary`] is the
    /// detail, on hover.
    ///
    /// [`summary`]: HealthReading::summary
    pub fn short(&self) -> String {
        if !self.sampled {
            return "No stats yet".into();
        }
        let d = self.drops;
        let total = [
            d.output_packets,
            d.encode_frames,
            d.encode_blocks,
            d.render_ticks,
            d.capture_frames,
            d.audio_frames,
        ]
        .into_iter()
        .fold(0_u64, u64::saturating_add);
        if total == 0 {
            "Healthy".into()
        } else {
            format!("Dropping ({total})")
        }
    }
}

/// The kind an existing output is, as the settings dialog edits it, or `None` for a
/// kind (or an RTMP target) this page doesn't know.
pub fn new_kind(kind: &OutputKind) -> Option<NewKind> {
    match kind {
        OutputKind::Rtmp {
            target: RtmpTarget::Generic { .. },
        } => Some(NewKind::Rtmp),
        OutputKind::Record { .. } => Some(NewKind::Record),
        OutputKind::VirtualCam => Some(NewKind::VirtualCam),
        _ => None,
    }
}

/// An output's target as its field shows it: the server URL, the recording's path, or
/// nothing.
pub fn target_of(kind: &OutputKind) -> String {
    match kind {
        OutputKind::Rtmp {
            target: RtmpTarget::Generic { url },
        } => url.clone(),
        OutputKind::Record { path, .. } => path.clone(),
        _ => String::new(),
    }
}

/// Where an output sends, short enough for its row: the server's host, the
/// recording's file name, or "Webcam" for the virtual camera. The full URL or path is
/// in the row's tooltip and its settings.
pub fn target_summary(kind: &OutputKind) -> String {
    match kind {
        OutputKind::Rtmp {
            target: RtmpTarget::Generic { url },
        } => {
            let rest = url.split_once("://").map_or(url.as_str(), |(_, rest)| rest);
            let host = rest.split('/').next().unwrap_or_default();
            if host.is_empty() {
                url.clone()
            } else {
                host.to_owned()
            }
        }
        OutputKind::Rtmp { .. } => "Stream".into(),
        OutputKind::Record { path, .. } => path
            .rsplit(['/', '\\'])
            .find(|part| !part.is_empty())
            .unwrap_or(path)
            .to_owned(),
        OutputKind::VirtualCam => "Webcam".into(),
        _ => "Unknown kind".into(),
    }
}

/// The name of the encoder profile an output uses, or "Engine default".
pub fn encoder_name(encoders: &[EncoderProfileInfo], chosen: Option<&EncoderProfileId>) -> String {
    chosen.map_or_else(
        || "Engine default".to_owned(),
        |id| {
            encoders
                .iter()
                .find(|e| &e.profile_id == id)
                .map_or_else(|| id.to_string(), |e| e.name.clone())
        },
    )
}

/// What the output settings dialog edits. Nothing in it reaches the engine until OK or
/// Apply, which send only what changed ([`draft_edits`]). The stream key isn't here: it
/// is read from its password field only when it is sent (R-SEC-001).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDraft {
    /// What it is. Only a new output picks it.
    pub kind: NewKind,
    /// The server URL or the recording's path, as typed.
    pub target: String,
    /// The encoder profile, or the engine's default.
    pub encoder: Option<EncoderProfileId>,
    /// The video mix, or the program at the canvas size.
    pub mix: Option<VideoMixId>,
    /// The audio bus it carries.
    pub bus: BusId,
}

impl OutputDraft {
    /// A new output of `kind`: no target yet, the engine's encoder, the program and the
    /// master bus.
    pub fn new(kind: NewKind) -> OutputDraft {
        OutputDraft {
            kind,
            target: String::new(),
            encoder: None,
            mix: None,
            bus: BusId::master(),
        }
    }

    /// The output `info` as it is, or `None` for a kind this page doesn't edit.
    pub fn of(info: &OutputInfo) -> Option<OutputDraft> {
        Some(OutputDraft {
            kind: new_kind(&info.kind)?,
            target: target_of(&info.kind),
            encoder: info.encoder_profile.clone(),
            mix: info.video_mix.clone(),
            bus: info
                .audio_buses
                .first()
                .cloned()
                .unwrap_or_else(BusId::master),
        })
    }

    /// Whether OK can send it: a stream or a recording needs its target.
    pub fn ready(&self) -> bool {
        !self.kind.needs_target() || !self.target.trim().is_empty()
    }

    /// This draft after the output changed from `before` to `now` while its dialog was
    /// open (another client's edit): each field still as the dialog had it from `before`
    /// takes the new value, and a field the person changed keeps theirs.
    #[must_use]
    pub fn reseed(&self, before: &OutputDraft, now: &OutputDraft) -> OutputDraft {
        fn pick<T: Clone + PartialEq>(mine: &T, before: &T, now: &T) -> T {
            if mine == before {
                now.clone()
            } else {
                mine.clone()
            }
        }
        OutputDraft {
            kind: pick(&self.kind, &before.kind, &now.kind),
            target: pick(&self.target, &before.target, &now.target),
            encoder: pick(&self.encoder, &before.encoder, &now.encoder),
            mix: pick(&self.mix, &before.mix, &now.mix),
            bus: pick(&self.bus, &before.bus, &now.bus),
        }
    }
}

/// The edits that make `before` (or, with `None`, a new output) what `draft` says, in
/// one batch: `output.create` first for a new one, then each field that differs. A
/// virtual camera carries no target. Nothing changed is an empty list.
pub fn draft_edits(
    output_id: &OutputId,
    before: Option<&OutputInfo>,
    draft: &OutputDraft,
) -> Vec<Edit> {
    let mut edits = Vec::new();
    let (encoder, mix, bus) = match before {
        None => {
            edits.push(create_edit(output_id.clone(), draft.kind, &draft.target));
            (None, None, Some(BusId::master()))
        }
        Some(info) => {
            let target = draft.target.trim();
            if draft.kind.needs_target() && target != target_of(&info.kind) {
                edits.push(match draft.kind {
                    NewKind::Record => Edit::OutputSetPath {
                        output_id: output_id.clone(),
                        path: target.to_owned(),
                    },
                    NewKind::Rtmp | NewKind::VirtualCam => Edit::OutputSetTarget {
                        output_id: output_id.clone(),
                        target: RtmpTarget::Generic {
                            url: target.to_owned(),
                        },
                    },
                });
            }
            (
                info.encoder_profile.clone(),
                info.video_mix.clone(),
                info.audio_buses.first().cloned(),
            )
        }
    };
    // A virtual camera has no encoder, mix or audio: its dialog hides them, so a value
    // left from another kind is not sent.
    if draft.kind == NewKind::VirtualCam {
        return edits;
    }
    if draft.encoder != encoder {
        edits.push(Edit::OutputSetEncoder {
            output_id: output_id.clone(),
            profile_id: draft.encoder.clone(),
        });
    }
    if draft.mix != mix {
        edits.push(Edit::OutputSetVideoMix {
            output_id: output_id.clone(),
            mix_id: draft.mix.clone(),
        });
    }
    if bus.as_ref() != Some(&draft.bus) {
        edits.push(bus_edit(output_id.clone(), draft.bus.as_str()));
    }
    edits
}

/// Why [`send_settings`] failed, and the output it created before that, if it did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendFailure {
    /// The new output the create batch made before a later step failed. A retry must
    /// pass it back, so it sets the key on that output instead of creating another.
    pub created: Option<OutputId>,
    /// What failed.
    pub reason: String,
}

/// Sends what the settings dialog's OK or Apply asks for: the batch of what changed,
/// then the stream key step. `existing` is the output being edited as it is now. With
/// none, a `created` output (made by an earlier try whose key step failed) gets only
/// the key step, since its fields went with its create; otherwise a new output is
/// created under the first ID free now, read from the engine, as the panel's list may
/// not have caught up with one added a moment ago. Returns the output's ID.
pub async fn send_settings(
    control: &dyn Control,
    existing: Option<&OutputInfo>,
    created: Option<OutputId>,
    draft: &OutputDraft,
    key: KeyChange,
    typed: Option<SecretString>,
) -> Result<OutputId, SendFailure> {
    let fail = |created: Option<OutputId>| move |reason: String| SendFailure { created, reason };
    let (output_id, made) = match (existing, created) {
        (Some(info), _) => {
            let output_id = info.output_id.clone();
            send_edits(control, draft_edits(&output_id, Some(info), draft))
                .await
                .map_err(fail(None))?;
            (output_id, None)
        }
        (None, Some(output_id)) => (output_id.clone(), Some(output_id)),
        (None, None) => {
            let taken = load(control).await.map_err(fail(None))?.outputs;
            let output_id = next_output_id(taken.iter().map(|row| &row.info.output_id));
            send_edits(control, draft_edits(&output_id, None, draft))
                .await
                .map_err(fail(None))?;
            (output_id.clone(), Some(output_id))
        }
    };
    let keyed = match (key, typed) {
        (KeyChange::Replace, Some(key)) => Some(Action::OutputSetKey {
            output_id: output_id.clone(),
            key,
        }),
        (KeyChange::Clear, _) => Some(Action::OutputClearKey {
            output_id: output_id.clone(),
        }),
        _ => None,
    };
    if let Some(keyed) = keyed {
        action(control, keyed).await.map_err(fail(made))?;
    }
    Ok(output_id)
}

/// What OK or Apply does to the stream key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyChange {
    /// Keeps the key the engine holds, or none.
    Keep,
    /// Sends the key typed in the field.
    Replace,
    /// Removes the key.
    Clear,
}

/// The dialog the panel has open over an output.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Editing {
    /// The settings of a new output.
    New,
    /// The settings of this output.
    Output(OutputId),
}

/// The panel's state, shared by its parts. Every field is `Copy`.
#[derive(Clone, Copy)]
struct Panel {
    /// The open session.
    session: SessionContext,
    /// The outputs, by ID.
    rows: RwSignal<BTreeMap<OutputId, OutputRow>>,
    /// The encoder profiles an output can choose.
    encoders: RwSignal<Vec<EncoderProfileInfo>>,
    /// The audio buses an output can carry.
    buses: RwSignal<Vec<BusInfo>>,
    /// The video mixes an output can show.
    mixes: RwSignal<Vec<VideoMixInfo>>,
    /// The scenes a mix can show, by ID and name.
    scenes: RwSignal<Vec<(SceneId, String)>>,
    /// The last failed call, shown until the next one succeeds or is dismissed.
    problem: RwSignal<Option<String>>,
    /// Bumped by each reload, so an older reload that finishes late is dropped.
    loads: StoredValue<u64>,
    /// Bumped by each `output.state_changed`, so a reload that read the outputs before
    /// one arrived knows its states may be older than the rows'.
    state_events: StoredValue<u64>,
    /// What the host says about the virtual camera.
    vcam: Vcam,
    /// Bumped after a stream key changed, for the header ([`Studio::keys_changed`]).
    ///
    /// [`Studio::keys_changed`]: crate::store::Studio::keys_changed
    keys_changed: Option<RwSignal<u64>>,
    /// The page's shared state, for the master stage's chains.
    studio: Option<crate::store::Studio>,
    /// What an FX button picks the Filters panel's chains through, and the dock that
    /// shows that panel.
    fx: Option<(FilterDialogs, Option<DockState>)>,
    /// The page's one pop-up menu, for a row's ⋯ button and right-click.
    menus: Option<Menus>,
    /// The output settings dialog, when it is open.
    settings: RwSignal<Option<Editing>>,
    /// The output whose delete waits for a confirmation.
    deleting: RwSignal<Option<OutputId>>,
    /// Whether the Encoders & mixes dialog is open.
    libraries: RwSignal<bool>,
    /// What the Encoders & mixes dialog's editor has open.
    pick: RwSignal<Option<LibraryPick>>,
    /// Each running output's latest stats, from the last `stats.tick`.
    stats: RwSignal<BTreeMap<OutputId, OutputStatsInfo>>,
}

impl Panel {
    /// Shows `owner`'s chains in the Filters panel, from an FX button.
    fn show_chains(self, owner: FilterOwner) {
        if let Some((dialogs, dock)) = self.fx {
            dialogs.pick(owner);
            if let Some(dock) = dock {
                dock.0.update(|d| d.show(PanelId::Filters));
            }
        }
    }

    /// Runs `work` on the open session and shows its error, if it fails. Does nothing
    /// without a session.
    fn run<F, Fut>(self, work: F)
    where
        F: FnOnce(Session) -> Fut + 'static,
        Fut: Future<Output = Result<(), String>> + 'static,
    {
        let Some(session) = self.session.get() else {
            return;
        };
        spawn_local(async move {
            let outcome = work(session).await;
            self.problem.try_set(outcome.err());
        });
    }

    /// Re-reads every output, encoder profile and video mix.
    fn reload(self) {
        let Some(session) = self.session.get() else {
            return;
        };
        spawn_local(async move { self.refresh(&session).await });
    }

    /// Re-reads every output, encoder profile and video mix, and returns once the panel
    /// has them, or a later reload has taken over.
    async fn refresh(self, session: &Session) {
        let Some(mine) = self.loads.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        {
            let heard = self.state_events.try_get_value();
            let loaded = load(&*session.control).await;
            if self.loads.try_get_value() != Some(mine) {
                return;
            }
            // A state change heard while this read was out may be newer than what it
            // read (an output started just after its status was read): the rows take
            // what was read, and a fresh read, sent after the change, settles them.
            if self.state_events.try_get_value() != heard {
                self.reload();
            }
            match loaded {
                Ok(snapshot) => {
                    self.rows.try_set(
                        snapshot
                            .outputs
                            .into_iter()
                            .map(|row| (row.info.output_id.clone(), row))
                            .collect(),
                    );
                    self.encoders.try_set(snapshot.encoders);
                    self.buses.try_set(snapshot.buses);
                    self.mixes.try_set(snapshot.mixes);
                    self.scenes.try_set(snapshot.scenes);
                }
                Err(problem) => {
                    self.problem.try_set(Some(problem));
                }
            }
        }
    }

    /// Opens the Encoders & mixes dialog, its editor on `pick` (or on nothing).
    fn open_library(self, pick: Option<LibraryPick>) {
        self.pick.set(pick);
        self.libraries.set(true);
    }

    /// Adds a copy of `output_id` under the first ID free now, read from the engine, in
    /// one batch ([`duplicate_edits`]), then opens the copy's settings, where its stream
    /// key or path can be set.
    fn duplicate(self, output_id: OutputId) {
        let Some(session) = self.session.get() else {
            return;
        };
        spawn_local(async move {
            let made = async {
                let taken = load(&*session.control).await?.outputs;
                let info = taken
                    .iter()
                    .find(|row| row.info.output_id == output_id)
                    .map(|row| row.info.clone())
                    .ok_or_else(|| format!("{output_id} isn't there any more."))?;
                let copy = next_output_id(taken.iter().map(|row| &row.info.output_id));
                let edits = duplicate_edits(&copy, &info)
                    .ok_or("This output's kind is newer than this page, so it can't be copied.")?;
                send_edits(&*session.control, edits).await?;
                Ok::<_, String>(copy)
            }
            .await;
            match made {
                Ok(copy) => {
                    self.problem.try_set(None);
                    self.refresh(&session).await;
                    self.settings.try_set(Some(Editing::Output(copy)));
                }
                Err(why) => {
                    self.problem.try_set(Some(why));
                }
            }
        });
    }

    /// Re-reads one output's status: after its stream key changed, which no event
    /// reports.
    async fn refresh_status(self, session: &Session, output_id: OutputId) -> Result<(), String> {
        let Status {
            state,
            key_set,
            cleartext,
        } = status(&*session.control, &output_id).await?;
        self.rows.try_update(|rows| {
            if let Some(row) = rows.get_mut(&output_id) {
                row.state = state;
                row.key_set = key_set;
                row.cleartext = cleartext;
            }
        });
        if let Some(keys_changed) = self.keys_changed {
            keys_changed.try_update(|n| *n += 1);
        }
        Ok(())
    }

    /// Starts an output. A refused virtual camera start says how to fix it.
    fn start(self, output_id: OutputId) {
        let kind = self
            .rows
            .with_untracked(|rows| rows.get(&output_id).map(|row| row.info.kind.clone()));
        self.run(move |session| async move {
            action(&*session.control, Action::OutputStart { output_id })
                .await
                .map_err(|reason| match &kind {
                    Some(kind) => explain_start_failure(kind, reason),
                    None => reason,
                })
        });
    }

    /// Stops an output.
    fn stop(self, output_id: OutputId) {
        self.run(move |session| async move {
            action(&*session.control, Action::OutputStop { output_id }).await
        });
    }

    /// Opens the pop-up menu of `output_id`'s row at the pointer: Settings…, Duplicate,
    /// then Delete…, which asks first and is off while the output runs. Without the
    /// page's menu, its settings open instead.
    fn open_menu(self, event: &leptos::ev::MouseEvent, output_id: OutputId) {
        let Some(menus) = self.menus else {
            self.settings.set(Some(Editing::Output(output_id)));
            return;
        };
        let running = self.rows.with_untracked(|rows| {
            rows.get(&output_id)
                .is_some_and(|row| is_running(row.state))
        });
        let open = output_id.clone();
        let copy = output_id.clone();
        menus.open(
            event,
            vec![
                Entry::item("Settings…", move || {
                    self.settings.set(Some(Editing::Output(open.clone())));
                }),
                Entry::item("Duplicate", move || self.duplicate(copy.clone())),
                Entry::Sep,
                Entry::item("Delete…", move || {
                    self.deleting.set(Some(output_id.clone()))
                })
                .disabled_if(running),
            ],
        );
    }
}

/// The outputs panel: a header with the pipeline's health, then a compact row for the
/// program and for each output, and the buttons that open the add-output and Encoders &
/// mixes dialogs. An output's settings are in its own dialog (#1206). Go Live is in the
/// header. Reads the open session from [`SessionContext`] context.
#[component]
pub fn Outputs() -> impl IntoView {
    let Some(session) = use_context::<SessionContext>() else {
        return view! { <p class="placeholder">"Not connected."</p> }.into_any();
    };
    let panel = Panel {
        session,
        rows: RwSignal::new(BTreeMap::new()),
        encoders: RwSignal::new(Vec::new()),
        buses: RwSignal::new(Vec::new()),
        mixes: RwSignal::new(Vec::new()),
        scenes: RwSignal::new(Vec::new()),
        problem: RwSignal::new(None),
        loads: StoredValue::new(0),
        state_events: StoredValue::new(0),
        vcam: Vcam::from_host(crate::host::desktop_vcam().as_deref()),
        keys_changed: use_context::<crate::store::Studio>().map(|studio| studio.keys_changed),
        studio: use_context(),
        fx: use_context::<FilterDialogs>().map(|dialogs| (dialogs, use_context())),
        menus: use_context(),
        settings: RwSignal::new(None),
        deleting: RwSignal::new(None),
        libraries: RwSignal::new(false),
        pick: RwSignal::new(None),
        stats: RwSignal::new(BTreeMap::new()),
    };
    let health = RwSignal::new(Health::default());

    // Follow the engine: re-read on a collection change (or lost events), take state
    // changes and stats as they come. The loop holds only the event stream, which ends
    // when the session closes.
    if let Some(open) = session.get() {
        let mut events = open.control.events();
        drop(open);
        panel.reload();
        spawn_local(async move {
            while let Some(event) = events.next().await {
                match event {
                    Event::StateChanged { .. } | Event::EventsDropped { .. } => panel.reload(),
                    Event::OutputStateChanged { output_id, state } => {
                        panel.state_events.try_update_value(|n| *n += 1);
                        let known = panel
                            .rows
                            .try_update(|rows| match rows.get_mut(&output_id) {
                                Some(row) => {
                                    row.state = state;
                                    true
                                }
                                None => false,
                            });
                        if known == Some(false) {
                            panel.reload();
                        }
                    }
                    Event::StatsTick {
                        seq,
                        drops,
                        outputs,
                        ..
                    } => {
                        health.try_update(|health| health.observe(seq, drops));
                        panel.stats.try_set(
                            outputs
                                .into_iter()
                                .map(|stats| (stats.output_id.clone(), stats))
                                .collect(),
                        );
                    }
                    _ => {}
                }
            }
        });
    }

    let ids = move || {
        panel
            .rows
            .with(|rows| rows.keys().cloned().collect::<Vec<_>>())
    };
    let reading = move || health.with(Health::reading);
    let infos = Signal::derive(move || {
        panel.rows.with(|rows| {
            rows.values()
                .map(|row| row.info.clone())
                .collect::<Vec<_>>()
        })
    });
    let send = Callback::new(move |change: Edit| {
        panel.run(move |session| async move { edit(&*session.control, change).await });
    });
    let close_libraries = move || panel.libraries.set(false);
    let all = Memo::new(move |_| {
        panel
            .rows
            .with(|rows| all_outputs(rows.values(), panel.vcam))
    });
    let start_or_stop_all_now = move || {
        let (outputs, start) = match all.get_untracked() {
            AllOutputs::Start(outputs) => (outputs, true),
            AllOutputs::Stop(outputs) => (outputs, false),
            AllOutputs::Nothing => return,
        };
        panel.run(move |session| async move {
            start_or_stop_all(&*session.control, outputs, start).await
        });
    };

    view! {
        <section class="outputs">
            <div class="outputs-head">
                <h2>"Outputs"</h2>
                <span
                    class="health"
                    class:dropping=move || reading().dropping()
                    class:idle=move || !reading().sampled
                    role="status"
                    title=move || reading().summary()
                >
                    <span class="health-dot" aria-hidden="true"></span>
                    {move || reading().short()}
                </span>
                <button
                    type="button"
                    class="all-outputs"
                    class:stop=move || matches!(all.get(), AllOutputs::Stop(_))
                    disabled=move || all.get() == AllOutputs::Nothing
                    title=move || match all.get() {
                        AllOutputs::Start(_) => "Start every output that can start",
                        AllOutputs::Stop(_) => "Stop every running output",
                        AllOutputs::Nothing => "No output can start yet",
                    }
                    on:click=move |_| start_or_stop_all_now()
                >
                    {move || if matches!(all.get(), AllOutputs::Stop(_)) { "■ Stop all" } else { "▶ Start all" }}
                </button>
            </div>
            {move || {
                panel
                    .problem
                    .get()
                    .map(|problem| {
                        view! {
                            <p class="problem" role="alert">
                                {problem}
                                " "
                                <button class="link" on:click=move |_| panel.problem.set(None)>
                                    "Dismiss"
                                </button>
                            </p>
                        }
                    })
            }}
            <ul class="output-rows" aria-label="Outputs">
                <ProgramRow panel=panel />
                <For each=ids key=|id| id.clone() let:output_id>
                    <OutputLine panel=panel output_id=output_id />
                </For>
            </ul>
            <div class="outputs-foot">
                <button type="button" on:click=move |_| panel.settings.set(Some(Editing::New))>
                    "+ Add output"
                </button>
                <button type="button" class="secondary" on:click=move |_| panel.open_library(None)>
                    "Encoders & mixes…"
                </button>
            </div>
            {move || {
                panel
                    .settings
                    .get()
                    .map(|editing| view! { <OutputDialog panel=panel editing=editing /> })
            }}
            {move || {
                panel
                    .deleting
                    .get()
                    .map(|output_id| view! { <DeleteDialog panel=panel output_id=output_id /> })
            }}
            <Show when=move || panel.libraries.get()>
                <Modal label="Encoders & mixes" class="libraries-dialog" cancel=close_libraries>
                    <div class="modal-header">
                        <div class="modal-titles">
                            <h2 class="modal-title">"Encoders & mixes"</h2>
                            <span class="modal-sub">"What an output can encode with and show"</span>
                        </div>
                        <button
                            type="button"
                            class="modal-close"
                            aria-label="Close"
                            on:click=move |_| close_libraries()
                        >
                            "×"
                        </button>
                    </div>
                    <div class="libraries-body">
                        <Profiles
                            encoders=panel.encoders
                            mixes=panel.mixes
                            outputs=infos
                            scenes=panel.scenes.into()
                            send=send
                            pick=panel.pick
                        />
                        {move || {
                            panel.problem.get().map(|problem| view! { <p class="problem" role="alert">{problem}</p> })
                        }}
                    </div>
                    <div class="modal-footer">
                        <button
                            type="button"
                            class="primary"
                            data-primary
                            on:click=move |_| close_libraries()
                        >
                            "Close"
                        </button>
                    </div>
                </Modal>
            </Show>
        </section>
    }
    .into_any()
}

/// The program's row, first: "master" and the FX button of the master chain
/// (spec-panels.md U1). What the program filters do is its tooltip.
#[component]
fn ProgramRow(panel: Panel) -> impl IntoView {
    let master = move || {
        panel.studio.map_or_else(
            || fx_label(&FilterChains::default()),
            |studio| {
                studio
                    .collection
                    .with(|c| fx_label(&c.as_ref().map(|c| c.master.clone()).unwrap_or_default()))
            },
        )
    };
    view! {
        <li class="output-row program" title=PROGRAM_NOTE>
            <span class="output-row-icon"><Icon name="layers" size=15 /></span>
            <span class="output-row-main">
                <span class="output-row-name">"Program output"</span>
            </span>
            <span class="output-row-status">"master"</span>
            <button
                type="button"
                class="fx-button on"
                title="Filters on everything leaving JStream"
                aria-label="Program output filters"
                disabled=panel.fx.is_none()
                on:click=move |_| panel.show_chains(FilterOwner::Master)
            >
                {master}
            </button>
        </li>
    }
}

/// What the program's filters do: its row's tooltip (spec-panels.md U2).
pub const PROGRAM_NOTE: &str =
    "Program filters apply to every output; each output can add its own after them.";

/// One output's row: its kind, name and where it sends, its state, Start or Stop, its
/// FX button and its ⋯ menu. A double-click opens its settings. A live row is marked,
/// and a stopped one that can't start says why under its name.
#[component]
fn OutputLine(panel: Panel, output_id: OutputId) -> impl IntoView {
    let row = {
        let output_id = output_id.clone();
        Memo::new(move |_| panel.rows.with(|rows| rows.get(&output_id).cloned()))
    };
    let id = StoredValue::new(output_id.clone());
    let state = move || row.with(|row| row.as_ref().map(|row| row.state));
    let running = move || state().is_some_and(is_running);
    let blocker =
        move || row.with(|row| row.as_ref().and_then(|row| start_blocker(row, panel.vcam)));
    let summary = move || {
        row.with(|row| {
            row.as_ref().map(|row| {
                let target = target_summary(&row.info.kind);
                if matches!(row.info.kind, OutputKind::VirtualCam) {
                    target
                } else {
                    let encoder = panel
                        .encoders
                        .with(|e| encoder_name(e, row.info.encoder_profile.as_ref()));
                    format!("{target} · {encoder}")
                }
            })
        })
    };
    let full = move || row.with(|row| row.as_ref().map(|row| describe_kind(&row.info.kind)));
    let warning = move || row.with(|row| row.as_ref().and_then(cleartext_warning));
    let label = move || row.with(|r| r.as_ref().map(|r| fx_label(&r.info.filters)));
    let filtered = move || row.with(|r| r.as_ref().is_some_and(|r| !r.info.filters.is_empty()));
    let live = {
        let output_id = output_id.clone();
        move || {
            if !running() {
                return None;
            }
            panel
                .stats
                .with(|stats| stats.get(&output_id).map(live_line))
        }
    };
    let name = output_id.to_string();
    let owner = FilterOwner::Output {
        output_id: output_id.clone(),
    };

    view! {
        <li
            class="output-row"
            class:live=move || state() == Some(OutputState::Active)
            class:busy=move || running() && state() != Some(OutputState::Active)
            class:failed=move || state() == Some(OutputState::Failed)
            data-output=name.clone()
            on:dblclick=move |event| {
                if !from_button(&event) {
                    panel.settings.set(Some(Editing::Output(id.get_value())));
                }
            }
            on:contextmenu=move |event| panel.open_menu(&event, id.get_value())
        >
            <span class="output-row-icon">
                {move || {
                    row.with(|row| row.as_ref().map(|row| kind_icon(&row.info.kind)))
                        .map(|icon| view! { <Icon name=icon size=15 /> })
                }}
            </span>
            <span class="output-row-main" title=full>
                <span class="output-row-name">{name.clone()}</span>
                <span class="output-row-target">
                    {summary}
                    {move || {
                        warning()
                            .map(|warning| {
                                view! {
                                    <span
                                        class="output-row-warning"
                                        role="img"
                                        aria-label="Stream key sent unencrypted"
                                        title=warning
                                    >
                                        "⚠"
                                    </span>
                                }
                            })
                    }}
                </span>
                {move || {
                    blocker()
                        .filter(|_| !running())
                        .map(|why| view! { <span class="output-row-note">{why}</span> })
                }}
                {move || live().map(|line| view! { <span class="output-row-stats">{line}</span> })}
            </span>
            <span
                class="badge"
                class:live=move || state() == Some(OutputState::Active)
                class:busy=move || running() && state() != Some(OutputState::Active)
                class:failed=move || state() == Some(OutputState::Failed)
            >
                {move || state().map_or("Gone", state_label)}
            </span>
            <Show
                when=running
                fallback=move || {
                    view! {
                        <button
                            type="button"
                            class="row-action start"
                            aria-label="Start"
                            disabled=move || blocker().is_some()
                            title=move || blocker().unwrap_or("Start")
                            on:click=move |_| panel.start(id.get_value())
                        >
                            "▶"
                        </button>
                    }
                }
            >
                <button
                    type="button"
                    class="row-action stop"
                    aria-label="Stop"
                    title="Stop"
                    on:click=move |_| panel.stop(id.get_value())
                >
                    "■"
                </button>
            </Show>
            <button
                type="button"
                class="fx-button"
                class:on=filtered
                title=format!("Filters for {name} only")
                aria-label=format!("{name} filters")
                disabled=panel.fx.is_none()
                on:click=move |_| panel.show_chains(owner.clone())
            >
                {label}
            </button>
            <button
                type="button"
                class="row-action more"
                aria-label=format!("More for {name}")
                title="More"
                on:click=move |event| panel.open_menu(&event, id.get_value())
            >
                "⋯"
            </button>
        </li>
    }
}

/// Whether a double-click landed on one of the row's buttons, which keep their own
/// clicks.
fn from_button(event: &leptos::ev::MouseEvent) -> bool {
    use leptos::web_sys::wasm_bindgen::JsCast;
    event
        .target()
        .and_then(|t| t.dyn_into::<leptos::web_sys::Element>().ok())
        .and_then(|e| e.closest("button").ok().flatten())
        .is_some()
}

/// The settings dialog of one output, or of a new one (OBS's Settings › Output): its
/// target and stream key, then the video it shows, its encoder and the bus it carries.
/// Nothing applies until OK or Apply, which send what changed in one batch, then the
/// key. Fields a running output can't change are off, with a note. Delete, bottom left,
/// asks first.
#[component]
fn OutputDialog(panel: Panel, editing: Editing) -> impl IntoView {
    // A new output's ID is picked when it is sent; until then, the first one free.
    let output_id = match &editing {
        Editing::Output(id) => id.clone(),
        Editing::New => panel
            .rows
            .with_untracked(|rows| next_output_id(rows.keys())),
    };
    let is_new = editing == Editing::New;
    let id = StoredValue::new(output_id.clone());
    let row = {
        let output_id = output_id.clone();
        Memo::new(move |_| {
            if is_new {
                None
            } else {
                panel.rows.with(|rows| rows.get(&output_id).cloned())
            }
        })
    };
    let start = row
        .with_untracked(|row| row.as_ref().and_then(|row| OutputDraft::of(&row.info)))
        .unwrap_or_else(|| OutputDraft::new(NewKind::Rtmp));
    // What the fields started from: when another client changes the output while the
    // dialog is open, the fields still as they started follow it (#1206).
    let seeded = StoredValue::new(start.clone());
    let draft = RwSignal::new(start);
    Effect::new(move |_| {
        let Some(now) = row.with(|row| row.as_ref().and_then(|row| OutputDraft::of(&row.info)))
        else {
            return;
        };
        let before = seeded.get_value();
        if now != before {
            draft.update(|d| *d = d.reseed(&before, &now));
            seeded.set_value(now);
        }
    });
    let key = RwSignal::new(KeyChange::Keep);
    let key_field = NodeRef::<html::Input>::new();
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    // The new output a first Add created when its key step then failed: a retry sets the
    // key on it rather than creating another.
    let created = RwSignal::new(None::<OutputId>);

    let kind = move || draft.with(|d| d.kind);
    let running = move || row.with(|row| row.as_ref().is_some_and(|row| is_running(row.state)));
    let key_set = move || row.with(|row| row.as_ref().is_some_and(|row| row.key_set));
    let gone = move || !is_new && row.with(Option::is_none);
    let close = move || panel.settings.set(None);

    // OK and Apply: the batch of what changed, then the stream key. The key goes from
    // its field straight into a `SecretString`, and the field is emptied before the
    // call is sent (R-SEC-001).
    let apply = move |then_close: bool| {
        if busy.get_untracked() {
            return;
        }
        let created_now = created.get_untracked();
        let before = row.get_untracked().map(|row| row.info).or_else(|| {
            let id = created_now.as_ref()?;
            panel
                .rows
                .with_untracked(|rows| rows.get(id).map(|row| row.info.clone()))
        });
        let wanted = draft.get_untracked();
        // An empty key field keeps the key (or none), whatever was typed and erased.
        let typed = match key.get_untracked() {
            KeyChange::Replace => key_field.get_untracked().and_then(|field| {
                let typed = field.value();
                field.set_value("");
                take_key(typed)
            }),
            KeyChange::Keep | KeyChange::Clear => None,
        };
        let change = match (key.get_untracked(), &typed) {
            (KeyChange::Replace, None) => KeyChange::Keep,
            (change, _) => change,
        };
        let Some(session) = panel.session.get() else {
            return;
        };
        busy.set(true);
        spawn_local(async move {
            let sent = send_settings(
                &*session.control,
                before.as_ref(),
                created_now,
                &wanted,
                change,
                typed,
            )
            .await;
            let outcome = match sent {
                Ok(output_id) => {
                    // From here on a retry edits this output: if the status read below
                    // fails, Add again must not create a second one.
                    if is_new {
                        created.try_set(Some(output_id.clone()));
                    }
                    if change == KeyChange::Keep {
                        Ok(())
                    } else {
                        panel.refresh_status(&session, output_id).await
                    }
                }
                Err(failure) => {
                    if failure.created.is_some() {
                        created.try_set(failure.created);
                    }
                    Err(failure.reason)
                }
            };
            busy.try_set(false);
            match outcome {
                Ok(()) => {
                    problem.try_set(None);
                    key.try_set(KeyChange::Keep);
                    if then_close {
                        panel.settings.try_set(None);
                    }
                }
                Err(why) => {
                    problem.try_set(Some(why));
                }
            }
        });
    };
    let can_send = move || !busy.get() && !gone() && draft.with(OutputDraft::ready);

    let title = if is_new {
        "Add output".to_owned()
    } else {
        format!("{output_id} settings")
    };
    let sub = move || {
        if let Some(made) = created.get() {
            format!("{made} was added; Add again retries only what failed")
        } else if is_new {
            format!(
                "It will be {}",
                panel.rows.with(|rows| next_output_id(rows.keys()))
            )
        } else {
            row.with(|row| {
                row.as_ref().map_or_else(
                    || "This output was deleted.".to_owned(),
                    |row| {
                        format!(
                            "{} · {}",
                            describe_kind(&row.info.kind),
                            state_label(row.state)
                        )
                    },
                )
            })
        }
    };
    let set_target = move |event: leptos::ev::Event| {
        let typed = event_target_value(&event);
        draft.update(|d| d.target = typed);
    };

    view! {
        <Modal label="Output settings" class="output-dialog" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">{title}</h2>
                    <span class="modal-sub">{sub}</span>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>
                    "×"
                </button>
            </div>
            <div class="output-dialog-body">
                <fieldset class="output-section">
                    <legend>"General"</legend>
                    <Show when=move || is_new && created.with(Option::is_none)>
                        <label class="output-field">
                            <span class="label">"Kind"</span>
                            <select
                                aria-label="Kind"
                                on:change=move |event| {
                                    let picked = match event_target_value(&event).as_str() {
                                        "record" => NewKind::Record,
                                        "virtual_cam" => NewKind::VirtualCam,
                                        _ => NewKind::Rtmp,
                                    };
                                    draft.update(|d| d.kind = picked);
                                    // A key typed for a stream doesn't follow it to
                                    // another kind.
                                    if let Some(field) = key_field.get_untracked() {
                                        field.set_value("");
                                    }
                                    key.set(KeyChange::Keep);
                                }
                            >
                                <option value="rtmp" prop:selected=move || kind() == NewKind::Rtmp>
                                    "Stream (RTMP)"
                                </option>
                                <option value="record" prop:selected=move || kind() == NewKind::Record>
                                    "Recording (MP4)"
                                </option>
                                <option
                                    value="virtual_cam"
                                    prop:selected=move || kind() == NewKind::VirtualCam
                                >
                                    "Virtual camera"
                                </option>
                            </select>
                        </label>
                    </Show>
                    <Show when=move || kind().needs_target()>
                        <label class="output-field">
                            <span class="label">
                                {move || match kind() {
                                    NewKind::Record => "File path on the engine's machine",
                                    NewKind::Rtmp | NewKind::VirtualCam => "Server URL (without the stream key)",
                                }}
                            </span>
                            <span class="path-row">
                                <input
                                    type="text"
                                    aria-label=move || {
                                        if kind() == NewKind::Record { "File path" } else { "Server URL" }
                                    }
                                    prop:value=move || draft.with(|d| d.target.clone())
                                    on:input=set_target
                                    disabled=running
                                    placeholder=move || match kind() {
                                        NewKind::Record => "/home/me/Videos/show.mp4",
                                        NewKind::Rtmp | NewKind::VirtualCam => "rtmps://live.example.com/app",
                                    }
                                />
                                <Show when=move || kind() == NewKind::Record && !running()>
                                    {crate::browse::browse_button(
                                        crate::browse::BrowseKind::Save,
                                        vec!["mp4".into()],
                                        "Save the recording".into(),
                                        || true,
                                        move || crate::browse::file_name(&draft.with(|d| d.target.clone())),
                                        move || crate::browse::start_folder(
                                            jstream_control::protocol::AssetRoot::Absolute,
                                            &draft.with(|d| d.target.clone()),
                                            None,
                                        ),
                                        move |picked| draft.update(|d| d.target = picked),
                                    )}
                                </Show>
                            </span>
                        </label>
                        <Show when=running>
                            <p class="output-note">"Stop the output to change where it sends."</p>
                        </Show>
                        {move || {
                            row.with(|row| row.as_ref().and_then(cleartext_warning))
                                .map(|warning| view! { <p class="output-note warning" role="note">{warning}</p> })
                        }}
                    </Show>
                    <Show when=move || kind() == NewKind::Rtmp>
                        <div class="output-field stream-key">
                            <span class="label">"Stream key"</span>
                            <Show
                                when=move || key.get() == KeyChange::Replace || (!key_set() && key.get() != KeyChange::Clear)
                                fallback=move || {
                                    view! {
                                        <span class="key-row">
                                            <span class="key-mask" aria-label="Stream key">
                                                {move || if key.get() == KeyChange::Clear { "Removed on OK" } else { "••••••••" }}
                                            </span>
                                            <Show
                                                when=move || key.get() == KeyChange::Clear
                                                fallback=move || {
                                                    view! {
                                                        <button type="button" on:click=move |_| key.set(KeyChange::Replace)>
                                                            "Change…"
                                                        </button>
                                                        <button type="button" class="secondary" on:click=move |_| key.set(KeyChange::Clear)>
                                                            "Remove"
                                                        </button>
                                                    }
                                                }
                                            >
                                                <button type="button" class="secondary" on:click=move |_| key.set(KeyChange::Keep)>
                                                    "Keep it"
                                                </button>
                                            </Show>
                                        </span>
                                    }
                                }
                            >
                                <span class="key-row">
                                    <input
                                        type="password"
                                        name="stream-key-entry"
                                        node_ref=key_field
                                        autocomplete="off"
                                        spellcheck="false"
                                        aria-label="Stream key"
                                        placeholder=move || {
                                            if key_set() { "Paste the new key" } else { "Paste the stream key" }
                                        }
                                        on:input=move |_| {
                                            if key.get_untracked() == KeyChange::Keep {
                                                key.set(KeyChange::Replace);
                                            }
                                        }
                                    />
                                    <Show when=key_set>
                                        <button type="button" class="secondary" on:click=move |_| {
                                            if let Some(field) = key_field.get_untracked() {
                                                field.set_value("");
                                            }
                                            key.set(KeyChange::Keep);
                                        }>
                                            "Keep current"
                                        </button>
                                    </Show>
                                </span>
                            </Show>
                            <span class="output-note">
                                "Write-only: JStream keeps it in the engine's credential store and never shows it again."
                            </span>
                        </div>
                    </Show>
                    <Show when=move || kind() == NewKind::VirtualCam>
                        <p class="vcam-status" class:unregistered=move || panel.vcam.hint().is_some()>
                            {panel.vcam.status()}
                        </p>
                        {panel.vcam.hint().map(|hint| view! { <p class="output-note">{hint}</p> })}
                        <Show when=move || is_new>
                            <p class="output-note">
                                "Other apps on the engine's machine see the program as a webcam, JStream Virtual Camera."
                            </p>
                        </Show>
                    </Show>
                </fieldset>
                <Show when=move || kind() != NewKind::VirtualCam>
                    <fieldset class="output-section">
                        <legend>"Video"</legend>
                        <label class="output-field" title="An output shows the program at the canvas size unless it picks a mix.">
                            <span class="label">"Shows"</span>
                            <span class="encoder-row">
                            <select
                                aria-label="Video"
                                on:change=move |event| {
                                    let chosen = event_target_value(&event);
                                    draft.update(|d| d.mix = (!chosen.is_empty()).then(|| VideoMixId::new(chosen)));
                                }
                            >
                                <option value="" prop:selected=move || draft.with(|d| d.mix.is_none())>
                                    "Program (canvas size)"
                                </option>
                                <For
                                    each=move || panel.mixes.get()
                                    key=|mix| (mix.mix_id.clone(), mix.name.clone())
                                    let:mix
                                >
                                    {
                                        let mix_id = mix.mix_id.clone();
                                        view! {
                                            <option
                                                value=mix.mix_id.to_string()
                                                prop:selected=move || draft.with(|d| d.mix.as_ref() == Some(&mix_id))
                                            >
                                                {mix.name}
                                            </option>
                                        }
                                    }
                                </For>
                            </select>
                            <button
                                type="button"
                                class="secondary"
                                disabled=move || draft.with(|d| d.mix.is_none())
                                title=move || {
                                    if draft.with(|d| d.mix.is_none()) {
                                        "The program at the canvas size has nothing to edit"
                                    } else {
                                        "Edit this mix in Encoders & mixes"
                                    }
                                }
                                on:click=move |_| {
                                    let mix = draft.with_untracked(|d| d.mix.clone());
                                    panel.open_library(Some(LibraryPick::Mix(mix)));
                                }
                            >
                                "Edit mix…"
                            </button>
                            </span>
                        </label>
                        <Show when=running>
                            <p class="output-note">"A new mix or encoder applies when the output starts again."</p>
                        </Show>
                    </fieldset>
                    <fieldset class="output-section">
                        <legend>"Encoder"</legend>
                        <span class="encoder-row">
                            <select
                                aria-label="Encoder"
                                on:change=move |event| {
                                    let chosen = event_target_value(&event);
                                    draft.update(|d| d.encoder = (!chosen.is_empty()).then(|| EncoderProfileId::new(chosen)));
                                }
                            >
                                <option value="" prop:selected=move || draft.with(|d| d.encoder.is_none())>
                                    "Engine default"
                                </option>
                                <For
                                    each=move || panel.encoders.get()
                                    key=|encoder| (encoder.profile_id.clone(), encoder.name.clone())
                                    let:encoder
                                >
                                    {
                                        let profile_id = encoder.profile_id.clone();
                                        view! {
                                            <option
                                                value=encoder.profile_id.to_string()
                                                prop:selected=move || {
                                                    draft.with(|d| d.encoder.as_ref() == Some(&profile_id))
                                                }
                                            >
                                                {encoder.name}
                                            </option>
                                        }
                                    }
                                </For>
                            </select>
                            <button
                                type="button"
                                class="secondary"
                                disabled=move || draft.with(|d| d.encoder.is_none())
                                title=move || {
                                    if draft.with(|d| d.encoder.is_none()) {
                                        "The engine's default can't be edited: New… makes a profile"
                                    } else {
                                        "Edit this encoder in Encoders & mixes"
                                    }
                                }
                                on:click=move |_| {
                                    let encoder = draft.with_untracked(|d| d.encoder.clone());
                                    panel.open_library(Some(LibraryPick::Encoder(encoder)));
                                }
                            >
                                "Edit encoder…"
                            </button>
                            <button
                                type="button"
                                class="secondary"
                                title="Make an encoder profile in Encoders & mixes"
                                on:click=move |_| panel.open_library(Some(LibraryPick::Encoder(None)))
                            >
                                "New…"
                            </button>
                        </span>
                    </fieldset>
                    <fieldset class="output-section">
                        <legend>"Audio"</legend>
                        <label class="output-field">
                            <span class="label">"Bus"</span>
                            <select
                                aria-label="Audio"
                                disabled=running
                                title=move || {
                                    if running() { "Stop the output to change its bus" } else { "The bus it carries" }
                                }
                                on:change=move |event| {
                                    let chosen = event_target_value(&event);
                                    draft.update(|d| d.bus = BusId::new(chosen));
                                }
                            >
                                <For
                                    each=move || panel.buses.get()
                                    key=|bus| (bus.bus_id.clone(), bus.name.clone())
                                    let:bus
                                >
                                    {
                                        let bus_id = bus.bus_id.clone();
                                        view! {
                                            <option
                                                value=bus.bus_id.to_string()
                                                prop:selected=move || draft.with(|d| d.bus == bus_id)
                                            >
                                                {bus.name}
                                            </option>
                                        }
                                    }
                                </For>
                            </select>
                        </label>
                    </fieldset>
                </Show>
                {move || problem.get().map(|why| view! { <p class="problem" role="alert">{why}</p> })}
            </div>
            <div class="modal-footer">
                <Show when=move || !is_new>
                    <button
                        type="button"
                        class="danger secondary output-delete"
                        disabled=move || running() || gone()
                        title=move || if running() { "Stop it before deleting it" } else { "Delete this output" }
                        on:click=move |_| {
                            panel.settings.set(None);
                            panel.deleting.set(Some(id.get_value()));
                        }
                    >
                        "Delete…"
                    </button>
                </Show>
                <button type="button" class="secondary" on:click=move |_| close()>
                    "Cancel"
                </button>
                <Show when=move || !is_new>
                    <button type="button" class="secondary" disabled=move || !can_send() on:click=move |_| apply(false)>
                        "Apply"
                    </button>
                </Show>
                <button
                    type="button"
                    class="primary"
                    data-primary
                    disabled=move || !can_send()
                    on:click=move |_| apply(true)
                >
                    {if is_new { "Add" } else { "OK" }}
                </button>
            </div>
        </Modal>
    }
}

/// Asks before an output is deleted.
#[component]
fn DeleteDialog(panel: Panel, output_id: OutputId) -> impl IntoView {
    let close = move || panel.deleting.set(None);
    let id = StoredValue::new(output_id.clone());
    let delete = move || {
        let output_id = id.get_value();
        panel.deleting.set(None);
        panel.run(move |session| async move {
            edit(&*session.control, Edit::OutputDelete { output_id }).await
        });
    };
    view! {
        <Modal label="Delete output" class="confirm-dialog" cancel=close>
            <div class="modal-header">
                <div class="modal-titles">
                    <h2 class="modal-title">{format!("Delete {output_id}?")}</h2>
                </div>
                <button type="button" class="modal-close" aria-label="Close" on:click=move |_| close()>
                    "×"
                </button>
            </div>
            <p class="confirm-body">"The output and its settings are removed."</p>
            <div class="modal-footer">
                <button type="button" class="secondary" on:click=move |_| close()>
                    "Cancel"
                </button>
                <button type="button" class="danger" on:click=move |_| delete()>
                    "Delete"
                </button>
            </div>
        </Modal>
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CLEARTEXT_WARNING, Health, NO_CAMERA_HINT, NewKind, OutputRow, REGISTER_HINT, UPDATE_HINT,
        Vcam, cleartext_warning, create_edit, describe_kind, explain_start_failure,
        go_live_targets, live_outputs, load, next_output_id, start_blocker, start_or_stop_all,
        status, take_key,
    };
    use jstream_control::protocol::{
        Action, DropCounts, Edit, OutputId, OutputInfo, OutputKind, OutputState, RtmpTarget,
    };
    use jstream_control::{Control, MockEngine};
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that is ready at once, as the mock's are.
    fn now<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the mock's futures are ready at once"),
        }
    }

    fn rtmp(id: &str, state: OutputState, key_set: bool) -> OutputRow {
        OutputRow {
            info: OutputInfo {
                output_id: OutputId::new(id),
                kind: OutputKind::Rtmp {
                    target: RtmpTarget::Generic {
                        url: "rtmp://live.example.com/app".into(),
                    },
                },
                encoder_profile: None,
                filters: Default::default(),
                video_mix: None,
                audio_buses: vec![jstream_protocol::BusId::master()],
            },
            state,
            key_set,
            cleartext: true,
        }
    }

    /// Gives the mock engine an encoder profile, `hq`, and returns its ID.
    fn with_encoder(control: &impl Control) -> jstream_control::protocol::EncoderProfileId {
        let hq = jstream_control::protocol::EncoderProfileId::new("hq");
        let create = Edit::EncoderCreate {
            profile_id: hq.clone(),
            name: "High quality".into(),
            profile: jstream_control::protocol::EncoderProfile::DEFAULT,
        };
        now(super::edit(control, create)).expect("an encoder");
        hq
    }

    fn drops(output_packets: u64) -> DropCounts {
        DropCounts {
            output_packets,
            ..DropCounts::default()
        }
    }

    /// Creating, keying, starting and loading an output through the calls the panel
    /// makes, against the mock engine.
    #[test]
    fn the_panel_s_calls_drive_an_output() {
        let engine = MockEngine::new();
        let control = engine.connect();
        let hello = jstream_control::protocol::Hello {
            protocol: jstream_control::protocol::PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        let id = next_output_id(std::iter::empty());
        assert_eq!(id.as_str(), "output-1");
        let create = create_edit(id.clone(), NewKind::Rtmp, " rtmp://live.example.com/app ");
        now(super::edit(&control, create)).expect("create");

        let loaded = now(load(&control)).expect("load");
        assert_eq!(loaded.outputs.len(), 1);
        let row = &loaded.outputs[0];
        assert_eq!(row.state, OutputState::Stopped);
        assert!(!row.key_set);
        assert_eq!(
            start_blocker(row, Vcam::Unknown),
            Some("Set a stream key first.")
        );
        assert!(go_live_targets(&loaded.outputs).is_empty());
        assert_eq!(
            describe_kind(&row.info.kind),
            "Stream to rtmp://live.example.com/app"
        );

        let key = take_key("abcd-1234\n".into()).expect("a key");
        let set = Action::OutputSetKey {
            output_id: id.clone(),
            key,
        };
        now(super::action(&control, set)).expect("set key");
        assert_eq!(
            engine.stream_key(&id).map(|key| key.expose().to_owned()),
            Some("abcd-1234".into())
        );
        let status = now(status(&control, &id)).expect("status");
        assert!(status.key_set);
        assert!(status.cleartext, "plain rtmp:// to another machine");

        let loaded = now(load(&control)).expect("load");
        assert_eq!(go_live_targets(&loaded.outputs), vec![id.clone()]);

        // The Audio select lists the buses and moves the output onto one.
        let vod = jstream_protocol::BusId::new("vod");
        let bus = Edit::BusCreate {
            bus_id: vod.clone(),
            name: "VOD".into(),
        };
        now(super::edit(&control, bus)).expect("a bus");
        now(super::edit(&control, super::bus_edit(id.clone(), "vod"))).expect("onto the bus");
        let loaded = now(load(&control)).expect("load");
        let buses: Vec<_> = loaded.buses.iter().map(|b| b.bus_id.clone()).collect();
        assert_eq!(buses, vec![jstream_protocol::BusId::master(), vod.clone()]);
        assert_eq!(loaded.outputs[0].info.audio_buses, vec![vod]);
        now(super::action(
            &control,
            Action::OutputStart {
                output_id: id.clone(),
            },
        ))
        .expect("start");
        let loaded = now(load(&control)).expect("load");
        assert_eq!(loaded.outputs[0].state, OutputState::Active);
        assert_eq!(live_outputs(&loaded.outputs), vec![id.clone()]);

        // A running output can't be deleted: the engine refuses and the panel says so.
        let refused = now(super::edit(&control, Edit::OutputDelete { output_id: id }));
        assert!(refused.is_err());
    }

    /// A refused key never comes back in the error the panel shows (R-SEC-001).
    #[test]
    fn a_refused_key_is_not_echoed() {
        let engine = MockEngine::new();
        let control = engine.connect();
        let hello = jstream_control::protocol::Hello {
            protocol: jstream_control::protocol::PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        let id = OutputId::new("o");
        now(super::edit(
            &control,
            create_edit(id.clone(), NewKind::Rtmp, "rtmp://h/app"),
        ))
        .expect("create");
        let secret = "hunter2 with-a-space";
        let key = take_key(secret.into()).expect("a key");
        let set = Action::OutputSetKey { output_id: id, key };
        let problem = now(super::action(&control, set)).expect_err("refused");
        assert!(!problem.contains("hunter2"), "{problem}");
    }

    #[test]
    fn keys_are_trimmed_and_empty_ones_refused() {
        assert!(take_key(String::new()).is_none());
        assert!(take_key("  \n".into()).is_none());
        let key = take_key(" k ".into()).expect("a key");
        assert_eq!(key.expose(), "k");
        assert_eq!(format!("{key:?}"), "\"***\"");
    }

    #[test]
    fn new_ids_skip_taken_ones() {
        let taken = [OutputId::new("output-1"), OutputId::new("output-3")];
        assert_eq!(next_output_id(&taken).as_str(), "output-2");
    }

    /// Go live tries every output: one that fails doesn't keep the rest from starting,
    /// and each failure is reported (#257).
    #[test]
    fn go_live_attempts_every_output_and_reports_each_failure() {
        let engine = MockEngine::new();
        let control = engine.connect();
        let hello = jstream_control::protocol::Hello {
            protocol: jstream_control::protocol::PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        let real = OutputId::new("real");
        now(super::edit(
            &control,
            create_edit(real.clone(), NewKind::Rtmp, "rtmp://h/app"),
        ))
        .expect("create");
        let targets = vec![
            OutputId::new("ghost-1"),
            real.clone(),
            OutputId::new("ghost-2"),
        ];

        let problem = now(start_or_stop_all(&control, targets, true)).expect_err("two fail");
        assert!(problem.contains("couldn't start ghost-1"), "{problem}");
        assert!(problem.contains("couldn't start ghost-2"), "{problem}");
        assert!(!problem.contains("couldn't start real"), "{problem}");
        let loaded = now(load(&control)).expect("load");
        assert_eq!(loaded.outputs[0].state, OutputState::Active);

        now(start_or_stop_all(&control, vec![real], false)).expect("stop");
        let loaded = now(load(&control)).expect("load");
        assert_eq!(loaded.outputs[0].state, OutputState::Stopped);
    }

    /// Secret fields (the stream key, the control token) sit in no `<form>` and opt out
    /// of autocomplete, so the browser has no form submission to offer saving as a
    /// password (#257).
    #[test]
    fn secret_fields_are_outside_forms_and_opt_out_of_autocomplete() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let password = concat!("type=", "\"password\"");
        let off = concat!("autocomplete=", "\"off\"");
        let mut seen = 0;
        for file in ["app.rs", "panels/outputs.rs"] {
            let text = std::fs::read_to_string(src.join(file)).expect("read source");
            assert!(!text.contains(concat!("new-", "password")), "{file}");
            let lines: Vec<&str> = text.lines().collect();
            let mut in_form = false;
            for (n, line) in lines.iter().enumerate() {
                if line.contains(concat!("<", "form")) {
                    in_form = true;
                }
                if line.contains(concat!("</", "form>")) {
                    in_form = false;
                }
                if line.contains(password) {
                    seen += 1;
                    assert!(!in_form, "{file}:{}: password field in a form", n + 1);
                    let input = lines[n..].iter().take_while(|line| !line.contains("/>"));
                    assert!(
                        input.clone().any(|line| line.contains(off)),
                        "{file}:{}: no autocomplete=off",
                        n + 1
                    );
                }
            }
        }
        assert_eq!(seen, 2, "the stream key and the control token");
    }

    /// A virtual camera output is added, started and stopped through the panel's calls,
    /// and neither "Go live" nor "End stream" touches it.
    #[test]
    fn the_panel_s_calls_drive_a_virtual_camera() {
        let engine = MockEngine::new();
        let control = engine.connect();
        let hello = jstream_control::protocol::Hello {
            protocol: jstream_control::protocol::PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        assert!(!NewKind::VirtualCam.needs_target());
        assert!(NewKind::Rtmp.needs_target() && NewKind::Record.needs_target());
        let id = OutputId::new("output-1");
        let create = create_edit(id.clone(), NewKind::VirtualCam, "ignored");
        assert_eq!(
            create,
            Edit::OutputCreate {
                output_id: id.clone(),
                kind: OutputKind::VirtualCam
            }
        );
        now(super::edit(&control, create)).expect("create");

        let loaded = now(load(&control)).expect("load");
        let row = &loaded.outputs[0];
        assert_eq!(row.info.kind, OutputKind::VirtualCam);
        assert_eq!(
            describe_kind(&row.info.kind),
            "Virtual camera: the program as a webcam"
        );
        assert_eq!(start_blocker(row, Vcam::Registered), None);
        assert_eq!(start_blocker(row, Vcam::Unknown), None);
        assert!(go_live_targets(&loaded.outputs).is_empty());

        now(super::action(
            &control,
            Action::OutputStart {
                output_id: id.clone(),
            },
        ))
        .expect("start");
        let loaded = now(load(&control)).expect("load");
        assert_eq!(loaded.outputs[0].state, OutputState::Active);
        assert!(live_outputs(&loaded.outputs).is_empty(), "not a stream");
        now(super::action(
            &control,
            Action::OutputStop { output_id: id },
        ))
        .expect("stop");
        let loaded = now(load(&control)).expect("load");
        assert_eq!(loaded.outputs[0].state, OutputState::Stopped);
    }

    /// The desktop app's word on the camera decides whether it can start and what the
    /// card says; in a browser the page doesn't know and lets the engine decide.
    #[test]
    fn the_host_s_vcam_status_gates_the_start() {
        let row = OutputRow {
            info: OutputInfo {
                output_id: OutputId::new("cam"),
                kind: OutputKind::VirtualCam,
                encoder_profile: None,
                filters: Default::default(),
                video_mix: None,
                audio_buses: vec![jstream_protocol::BusId::master()],
            },
            state: OutputState::Stopped,
            key_set: false,
            cleartext: false,
        };
        let parse = |value| Vcam::from_host(value);
        assert_eq!(parse(Some("registered")), Vcam::Registered);
        assert_eq!(parse(Some("not_registered")), Vcam::NotRegistered);
        assert_eq!(parse(Some("outdated")), Vcam::Outdated);
        assert_eq!(parse(Some("off")), Vcam::Off);
        assert_eq!(parse(Some("later")), Vcam::Unknown);
        assert_eq!(parse(None), Vcam::Unknown);

        assert_eq!(
            start_blocker(&row, Vcam::NotRegistered),
            Some("Register the virtual camera first.")
        );
        assert_eq!(
            start_blocker(&row, Vcam::Off),
            Some("The virtual camera is off in this app.")
        );
        assert_eq!(
            start_blocker(&row, Vcam::Outdated),
            None,
            "the engine decides"
        );
        assert_eq!(Vcam::Registered.hint(), None);
        assert_eq!(Vcam::NotRegistered.hint(), Some(REGISTER_HINT));
        assert_eq!(Vcam::Outdated.hint(), Some(UPDATE_HINT));
        for hint in [REGISTER_HINT, UPDATE_HINT] {
            assert!(hint.contains("jstream-daemon --register-vcam"), "{hint}");
            assert!(
                hint.contains("ptional"),
                "the step is named optional: {hint}"
            );
        }
        assert!(
            Vcam::Unknown
                .hint()
                .is_some_and(|hint| !hint.contains("--register-vcam")),
            "a browser can't tell whether registering would help"
        );
        assert!(
            Vcam::Off
                .hint()
                .is_some_and(|hint| hint.contains("--no-vcam"))
        );
        // A stream doesn't care about the camera.
        let stream = rtmp("a", OutputState::Stopped, true);
        assert_eq!(start_blocker(&stream, Vcam::Off), None);
    }

    /// The engine's reasons, as `jstream-core` and `WindowsVcam::start` word them: an
    /// unregistered camera gets the registration command; an engine with no camera gets
    /// why, and no registration advice, which couldn't help (#316 B1).
    #[test]
    fn a_refused_camera_start_gets_the_hint_that_fits() {
        let unregistered = "action_failed: starting the virtual camera: the virtual camera: its \
             media source {0B5A3E7C} is not registered (registering it is optional: run \
             `jstream-daemon --register-vcam`)"
            .to_owned();
        let explained = explain_start_failure(&OutputKind::VirtualCam, unregistered.clone());
        assert!(explained.starts_with(&unregistered), "{explained}");
        assert!(explained.contains(REGISTER_HINT), "{explained}");

        let reason = "action_failed: this engine has no virtual camera".to_owned();
        let explained = explain_start_failure(&OutputKind::VirtualCam, reason.clone());
        assert!(explained.starts_with(&reason), "{explained}");
        assert!(explained.contains(NO_CAMERA_HINT), "{explained}");
        assert!(!explained.contains("register-vcam"), "{explained}");
        let busy = "the virtual camera is in use by another output".to_owned();
        assert_eq!(
            explain_start_failure(&OutputKind::VirtualCam, busy.clone()),
            busy
        );
        let stream = OutputKind::Rtmp {
            target: RtmpTarget::Generic {
                url: "rtmp://h/app".into(),
            },
        };
        assert_eq!(explain_start_failure(&stream, reason.clone()), reason);
    }

    /// The card warns when the engine reports `cleartext`, whatever the URL says: the
    /// rule is the engine's (#353, #562). Through the mock, plain `rtmp://` to another
    /// machine is flagged and `rtmps://` and a loopback relay aren't.
    #[test]
    fn the_engine_s_cleartext_flag_decides_the_warning() {
        let mut row = rtmp("a", OutputState::Stopped, true);
        assert_eq!(cleartext_warning(&row), Some(CLEARTEXT_WARNING));
        row.cleartext = false;
        assert_eq!(cleartext_warning(&row), None);

        let engine = MockEngine::new();
        let control = engine.connect();
        let hello = jstream_control::protocol::Hello {
            protocol: jstream_control::protocol::PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        for (id, url, warns) in [
            ("remote", "rtmp://live.example.com/app", true),
            ("tls", "rtmps://live.example.com/app", false),
            ("relay", "rtmp://127.0.0.1:1936/live", false),
        ] {
            let create = create_edit(OutputId::new(id), NewKind::Rtmp, url);
            now(super::edit(&control, create)).expect("create");
            let status = now(status(&control, &OutputId::new(id))).expect("status");
            assert_eq!(status.cleartext, warns, "{url}");
        }
    }

    #[test]
    fn go_live_picks_ready_stopped_streams() {
        let rows = [
            rtmp("a", OutputState::Stopped, true),
            rtmp("b", OutputState::Failed, true),
            rtmp("c", OutputState::Stopped, false),
            rtmp("d", OutputState::Active, true),
        ];
        let ids = |list: Vec<OutputId>| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert_eq!(ids(go_live_targets(&rows)), ["a", "b"]);
        assert_eq!(ids(live_outputs(&rows)), ["d"]);
    }

    #[test]
    fn an_fx_button_counts_both_chains() {
        use super::fx_label;
        use jstream_control::protocol::{FilterChains, FilterId, FilterInfo, Settings};
        let filter = |id: &str| FilterInfo {
            filter_id: FilterId::new(id),
            kind: "gain".into(),
            settings: Settings::new(),
            enabled: true,
        };
        assert_eq!(fx_label(&FilterChains::default()), "FX 0");
        let chains = FilterChains {
            video: vec![filter("v")],
            audio: vec![filter("a"), filter("b")],
        };
        assert_eq!(fx_label(&chains), "FX 3");
    }

    #[test]
    fn health_sums_the_last_ticks() {
        let mut health = Health::default();
        assert!(!health.reading().sampled);
        health.observe(1, drops(10));
        let first = health.reading();
        assert!(first.sampled);
        assert!(!first.dropping(), "the first tick is only a baseline");
        health.observe(2, drops(13));
        health.observe(4, drops(15));
        let reading = health.reading();
        assert_eq!(reading.drops.output_packets, 5);
        assert_eq!(reading.missed_ticks, 1);
        assert!(reading.summary().contains("5 output packets"));

        // Old intervals fall out of the window.
        for seq in 5..=9 {
            health.observe(seq, drops(15));
        }
        assert!(!health.reading().dropping());
        assert!(health.reading().summary().starts_with("Healthy"));
    }

    #[test]
    fn an_engine_restart_starts_health_over() {
        let mut health = Health::default();
        health.observe(5, drops(10));
        health.observe(6, drops(12));
        assert!(health.reading().dropping());
        health.observe(1, drops(0));
        assert!(!health.reading().dropping());
        health.observe(2, drops(1));
        assert_eq!(health.reading().drops.output_packets, 1);
    }

    /// The settings dialog sends only what changed: a new output is created with its
    /// encoder, mix and bus in one batch, and an edit of a stopped one sends one edit
    /// per changed field (#1206).
    #[test]
    fn the_settings_dialog_sends_what_changed_in_one_batch() {
        use super::{OutputDraft, draft_edits, send_edits};
        use jstream_control::protocol::{BusId, VideoMixId};
        let engine = MockEngine::new();
        let control = engine.connect();
        let hello = jstream_control::protocol::Hello {
            protocol: jstream_control::protocol::PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        let vod = BusId::new("vod");
        now(super::edit(
            &control,
            Edit::BusCreate {
                bus_id: vod.clone(),
                name: "VOD".into(),
            },
        ))
        .expect("a bus");
        let hq = with_encoder(&control);

        let id = OutputId::new("output-1");
        let mut draft = OutputDraft::new(NewKind::Record);
        assert!(!draft.ready(), "a recording needs its path");
        draft.target = " /tmp/show.mp4 ".into();
        assert!(draft.ready());
        assert_eq!(
            draft_edits(&id, None, &draft),
            vec![create_edit(id.clone(), NewKind::Record, "/tmp/show.mp4")],
            "the defaults need no edits of their own"
        );
        draft.encoder = Some(hq.clone());
        draft.bus = vod.clone();
        let edits = draft_edits(&id, None, &draft);
        assert_eq!(edits.len(), 3, "{edits:?}");
        let before = now(load(&control)).expect("load").rev;
        now(send_edits(&control, edits)).expect("create");
        let loaded = now(load(&control)).expect("load");
        assert_eq!(loaded.rev.0, before.0 + 1, "one batch, one undo step");
        let info = &loaded.outputs[0].info;
        assert_eq!(info.encoder_profile, Some(hq));
        assert_eq!(info.audio_buses, vec![vod]);
        assert_eq!(
            OutputDraft::of(info),
            Some(OutputDraft {
                target: "/tmp/show.mp4".into(),
                ..draft.clone()
            })
        );

        // Unchanged, nothing is sent; a new path and the program again are two edits.
        let mut again = OutputDraft::of(info).expect("a recording");
        assert!(draft_edits(&id, Some(info), &again).is_empty());
        now(send_edits(&control, Vec::new())).expect("nothing to send");
        again.target = "/tmp/other.mp4".into();
        again.encoder = None;
        assert_eq!(
            draft_edits(&id, Some(info), &again),
            vec![
                Edit::OutputSetPath {
                    output_id: id.clone(),
                    path: "/tmp/other.mp4".into(),
                },
                Edit::OutputSetEncoder {
                    output_id: id.clone(),
                    profile_id: None,
                },
            ]
        );
        now(send_edits(&control, draft_edits(&id, Some(info), &again))).expect("edit");
        let info = now(load(&control)).expect("load").outputs[0].info.clone();
        assert_eq!(super::target_of(&info.kind), "/tmp/other.mp4");
        assert_eq!(info.encoder_profile, None);

        // A stream's server URL is `output.set_target`, and a mix `output.set_video_mix`.
        let stream = rtmp("s", OutputState::Stopped, true).info;
        let mut edited = OutputDraft::of(&stream).expect("a stream");
        edited.target = "rtmps://live.example.com/app".into();
        edited.mix = Some(VideoMixId::new("vertical"));
        assert_eq!(
            draft_edits(&stream.output_id, Some(&stream), &edited),
            vec![
                Edit::OutputSetTarget {
                    output_id: stream.output_id.clone(),
                    target: RtmpTarget::Generic {
                        url: "rtmps://live.example.com/app".into(),
                    },
                },
                Edit::OutputSetVideoMix {
                    output_id: stream.output_id.clone(),
                    mix_id: Some(VideoMixId::new("vertical")),
                },
            ]
        );
    }

    /// A row says where an output sends in a word: the server's host, the file's name,
    /// or the webcam, and which encoder it uses.
    #[test]
    fn a_row_summarises_the_target_and_encoder() {
        use super::{encoder_name, target_summary};
        use jstream_control::protocol::{EncoderProfileId, RecordContainer};
        let url = |url: &str| OutputKind::Rtmp {
            target: RtmpTarget::Generic { url: url.into() },
        };
        assert_eq!(
            target_summary(&url("rtmps://live.example.com/app")),
            "live.example.com"
        );
        assert_eq!(
            target_summary(&url("rtmp://127.0.0.1:1935/live")),
            "127.0.0.1:1935"
        );
        assert_eq!(target_summary(&url("odd")), "odd");
        let record = |path: &str| OutputKind::Record {
            path: path.into(),
            container: RecordContainer::Mp4,
        };
        assert_eq!(
            target_summary(&record("/home/me/Videos/show.mp4")),
            "show.mp4"
        );
        assert_eq!(target_summary(&record("C:\\Videos\\show.mp4")), "show.mp4");
        assert_eq!(target_summary(&OutputKind::VirtualCam), "Webcam");

        let engine = MockEngine::new();
        let control = engine.connect();
        let hello = jstream_control::protocol::Hello {
            protocol: jstream_control::protocol::PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        let hq = with_encoder(&control);
        let encoders = now(load(&control)).expect("load").encoders;
        assert_eq!(encoder_name(&encoders, None), "Engine default");
        assert_eq!(encoder_name(&encoders, Some(&hq)), encoders[0].name);
        let gone = EncoderProfileId::new("gone");
        assert_eq!(encoder_name(&encoders, Some(&gone)), "gone");
    }

    /// The header's health is a word or two, with the detail on hover.
    #[test]
    fn health_has_a_short_form_for_the_header() {
        let mut health = Health::default();
        assert_eq!(health.reading().short(), "No stats yet");
        health.observe(1, drops(10));
        assert_eq!(health.reading().short(), "Healthy");
        health.observe(
            2,
            DropCounts {
                output_packets: 13,
                render_ticks: 2,
                ..DropCounts::default()
            },
        );
        assert_eq!(health.reading().short(), "Dropping (5)");
        assert!(health.reading().summary().contains("3 output packets"));
    }

    /// An Add whose key step fails leaves the output it created, and the retry sets the
    /// key on that output instead of creating a second one (#1279 B1). A virtual camera
    /// sends no encoder, mix or bus left from another kind.
    #[test]
    fn a_failed_key_after_a_create_is_retried_on_the_same_output() {
        use super::{KeyChange, OutputDraft, SendFailure, draft_edits, send_settings};
        use jstream_control::protocol::BusId;
        let engine = MockEngine::new();
        let control = engine.connect();
        let hello = jstream_control::protocol::Hello {
            protocol: jstream_control::protocol::PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        let mut draft = OutputDraft::new(NewKind::Rtmp);
        draft.target = "rtmps://live.example.com/app".into();
        let bad = take_key("has a space".into());
        let failed = now(send_settings(
            &control,
            None,
            None,
            &draft,
            KeyChange::Replace,
            bad,
        ))
        .expect_err("the engine refuses the key");
        let id = OutputId::new("output-1");
        assert_eq!(failed.created, Some(id.clone()));
        let SendFailure { reason, .. } = failed;
        assert!(!reason.contains("has a space"), "{reason}");

        let good = take_key("abcd-1234".into());
        let sent = now(send_settings(
            &control,
            None,
            Some(id.clone()),
            &draft,
            KeyChange::Replace,
            good,
        ))
        .expect("the key alone");
        assert_eq!(sent, id);
        let loaded = now(load(&control)).expect("load");
        assert_eq!(loaded.outputs.len(), 1, "no second output");
        assert!(loaded.outputs[0].key_set);

        let mut cam = OutputDraft::new(NewKind::VirtualCam);
        cam.bus = BusId::new("vod");
        let cam_id = OutputId::new("cam");
        assert_eq!(
            draft_edits(&cam_id, None, &cam),
            vec![create_edit(cam_id.clone(), NewKind::VirtualCam, "")]
        );
    }

    /// Duplicate copies the kind, target, encoder, mix, bus and filters in one batch,
    /// and never a stream key; the mock engine takes the batch.
    #[test]
    fn a_duplicate_copies_settings_and_filters_but_not_the_key() {
        use super::{OutputDraft, duplicate_edits};
        use jstream_control::protocol::{
            FilterChain, FilterId, FilterOwner, Hello, PROTOCOL_VERSION,
        };
        let engine = MockEngine::new();
        let control = engine.connect();
        let hello = Hello {
            protocol: PROTOCOL_VERSION,
            client_name: "test".into(),
        };
        now(control.hello(hello)).expect("hello");
        let hq = with_encoder(&control);
        let original = OutputId::new("output-1");
        now(super::send_edits(
            &control,
            vec![
                create_edit(
                    original.clone(),
                    NewKind::Rtmp,
                    "rtmps://live.example.com/app",
                ),
                Edit::OutputSetEncoder {
                    output_id: original.clone(),
                    profile_id: Some(hq.clone()),
                },
                Edit::FilterAdd {
                    owner: FilterOwner::Output {
                        output_id: original.clone(),
                    },
                    chain: FilterChain::Video,
                    filter_id: FilterId::new("vignette-1"),
                    kind: "vignette".into(),
                    settings: Default::default(),
                    enabled: Some(false),
                    index: None,
                },
            ],
        ))
        .expect("the original");
        now(control.call(jstream_control::protocol::Call::Action {
            action: Action::OutputSetKey {
                output_id: original.clone(),
                key: take_key("abcd-1234".into()).expect("a key"),
            },
        }))
        .expect("its key");
        let info = now(load(&control)).expect("load").outputs[0].info.clone();

        let copy = OutputId::new("output-2");
        let edits = duplicate_edits(&copy, &info).expect("a kind this page edits");
        assert_eq!(
            edits.first(),
            Some(&create_edit(
                copy.clone(),
                NewKind::Rtmp,
                "rtmps://live.example.com/app"
            ))
        );
        assert!(
            edits.iter().all(
                |e| !matches!(e, Edit::OutputCreate { output_id, .. } if output_id == &original)
            )
        );
        now(super::send_edits(&control, edits)).expect("one batch");

        let loaded = now(load(&control)).expect("load");
        let made = loaded
            .outputs
            .iter()
            .find(|row| row.info.output_id == copy)
            .expect("the copy");
        assert_eq!(OutputDraft::of(&made.info), OutputDraft::of(&info));
        assert_eq!(made.info.filters.video.len(), 1);
        assert_eq!(made.info.filters.video[0].kind, "vignette");
        assert!(
            !made.info.filters.video[0].enabled,
            "a disabled filter stays off"
        );
        assert!(!made.key_set, "the stream key isn't copied");
    }

    /// Start all starts what can start, of any kind; once nothing can, it stops what
    /// runs.
    #[test]
    fn start_all_starts_what_can_start_then_stops_what_runs() {
        use super::{AllOutputs, all_outputs};
        let mut record = rtmp("output-3", OutputState::Stopped, false);
        record.info.kind = OutputKind::Record {
            path: "/tmp/a.mp4".into(),
            container: jstream_control::protocol::RecordContainer::Mp4,
        };
        let rows = [
            rtmp("output-1", OutputState::Stopped, true),
            rtmp("output-2", OutputState::Stopped, false),
            record,
            rtmp("output-4", OutputState::Active, true),
        ];
        assert_eq!(
            all_outputs(&rows, Vcam::Unknown),
            AllOutputs::Start(vec![OutputId::new("output-1"), OutputId::new("output-3")])
        );
        let running = [
            rtmp("output-1", OutputState::Active, true),
            rtmp("output-2", OutputState::Stopped, false),
            rtmp("output-4", OutputState::Reconnecting, true),
        ];
        assert_eq!(
            all_outputs(&running, Vcam::Unknown),
            AllOutputs::Stop(vec![OutputId::new("output-1"), OutputId::new("output-4")])
        );
        let idle = [rtmp("output-2", OutputState::Stopped, false)];
        assert_eq!(all_outputs(&idle, Vcam::Unknown), AllOutputs::Nothing);
        assert_eq!(all_outputs(&[], Vcam::Unknown), AllOutputs::Nothing);
    }

    #[test]
    fn a_live_row_shows_its_bitrate_drops_and_uptime() {
        let stats: jstream_control::protocol::OutputStatsInfo =
            serde_json::from_value(serde_json::json!({
                "output_id": "output-1",
                "uptime_ms": 754_000,
                "kbps": 2450,
                "dropped_frames": 2,
                "dropped_packets": 1,
            }))
            .expect("stats");
        assert_eq!(super::live_line(&stats), "2.45 Mbps · 3 dropped · 00:12:34");
    }

    /// Another client's change reaches the fields the person hasn't touched, and leaves
    /// the ones they changed.
    #[test]
    fn a_reseed_keeps_what_was_changed_and_follows_the_rest() {
        use super::OutputDraft;
        use jstream_control::protocol::{BusId, EncoderProfileId, VideoMixId};
        let before = OutputDraft {
            kind: NewKind::Rtmp,
            target: "rtmps://a.example.com/app".into(),
            encoder: None,
            mix: None,
            bus: BusId::master(),
        };
        let mut mine = before.clone();
        mine.target = "rtmps://mine.example.com/app".into();
        let now = OutputDraft {
            target: "rtmps://theirs.example.com/app".into(),
            encoder: Some(EncoderProfileId::new("hq")),
            mix: Some(VideoMixId::new("mix_1")),
            ..before.clone()
        };
        let reseeded = mine.reseed(&before, &now);
        assert_eq!(reseeded.target, "rtmps://mine.example.com/app");
        assert_eq!(reseeded.encoder, now.encoder);
        assert_eq!(reseeded.mix, now.mix);
        assert_eq!(before.reseed(&before, &now), now, "untouched, it follows");
    }
}
