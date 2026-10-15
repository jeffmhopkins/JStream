//! The outputs panel: create, edit and delete outputs, set or clear their stream keys,
//! start and stop them, go live, and the pipeline's health from `stats.tick`.
//!
//! The stream key is write-only (R-SEC-001). It is read from its password field only
//! when the user saves it, wrapped in a [`SecretString`] at once, sent with
//! `output.set_key`, and the field is emptied. It never goes into a signal, the page's
//! storage, the console or an error message, and the engine never sends it back: the
//! panel only learns `key_set` from `output.status`.
//!
//! Docs: crates/jstream-ui/README.md#design, docs/RULES.md#R-SEC-001

use crate::app::SessionContext;
use crate::session::Session;
use futures_util::StreamExt;
use jstream_control::protocol::{
    Action, Batch, Call, CallResult, DropCounts, Edit, EncoderProfileId, EncoderProfileInfo, Event,
    OutputId, OutputInfo, OutputKind, OutputState, Query, QueryResult, RecordContainer, Rev,
    RtmpTarget, SecretString,
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
}

/// The outputs and encoder profiles at one revision, with each output's status.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// The revision `state.get` read.
    pub rev: Rev,
    /// The outputs, ordered by ID.
    pub outputs: Vec<OutputRow>,
    /// The encoder profiles an output can choose, ordered by ID.
    pub encoders: Vec<EncoderProfileInfo>,
}

/// Reads the outputs and encoder profiles (`state.get`), then each output's status
/// (`output.status`).
pub async fn load(control: &dyn Control) -> Result<Snapshot, String> {
    let (rev, outputs, encoders) = match query(control, Query::StateGet).await? {
        QueryResult::StateGet {
            rev,
            outputs,
            encoders,
            ..
        } => (rev, outputs, encoders),
        other => return Err(unexpected("state.get", other.name())),
    };
    let mut rows = Vec::with_capacity(outputs.len());
    for info in outputs {
        let (state, key_set) = status(control, &info.output_id).await?;
        rows.push(OutputRow {
            info,
            state,
            key_set,
        });
    }
    Ok(Snapshot {
        rev,
        outputs: rows,
        encoders,
    })
}

/// One output's runtime state and whether its stream key is set (`output.status`).
pub async fn status(
    control: &dyn Control,
    output_id: &OutputId,
) -> Result<(OutputState, bool), String> {
    let query_status = Query::OutputStatus {
        output_id: output_id.clone(),
    };
    match query(control, query_status).await? {
        QueryResult::OutputStatus { state, key_set, .. } => Ok((state, key_set)),
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
}

/// The `output.create` edit for a new output. `target` is the RTMP server URL or the
/// recording's path, as the form took it; the engine validates it.
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

/// Whether an output runs: every state but `stopped` and `failed`.
pub fn is_running(state: OutputState) -> bool {
    !matches!(state, OutputState::Stopped | OutputState::Failed)
}

/// Why an output can't be started now, or `None` when it can.
pub fn start_blocker(row: &OutputRow) -> Option<&'static str> {
    if is_running(row.state) {
        return Some("It is already running.");
    }
    match &row.info.kind {
        OutputKind::Rtmp { .. } if !row.key_set => Some("Set a stream key first."),
        OutputKind::Rtmp { .. } | OutputKind::Record { .. } => None,
        _ => Some("This engine's output kind is newer than this page."),
    }
}

/// The outputs "Go live" starts: every stream (RTMP) output that is stopped or failed
/// and has its stream key set. Recordings start on their own buttons.
pub fn go_live_targets<'a>(rows: impl IntoIterator<Item = &'a OutputRow>) -> Vec<OutputId> {
    rows.into_iter()
        .filter(|row| matches!(row.info.kind, OutputKind::Rtmp { .. }))
        .filter(|row| start_blocker(row).is_none())
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

/// What an output is and where it sends, in a line.
pub fn describe_kind(kind: &OutputKind) -> String {
    match kind {
        OutputKind::Rtmp {
            target: RtmpTarget::Generic { url },
        } => format!("Stream to {url}"),
        OutputKind::Rtmp { .. } => "Stream (a target this page doesn't know)".into(),
        OutputKind::Record { path, .. } => format!("Record to {path}"),
        _ => "An output kind this page doesn't know".into(),
    }
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
            return "No stats yet: the engine sends them while its pipeline runs.".into();
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

/// The panel's state, shared by its parts. Every field is `Copy`.
#[derive(Clone, Copy)]
struct Panel {
    /// The open session.
    session: SessionContext,
    /// The outputs, by ID.
    rows: RwSignal<BTreeMap<OutputId, OutputRow>>,
    /// The encoder profiles an output can choose.
    encoders: RwSignal<Vec<EncoderProfileInfo>>,
    /// The last failed call, shown until the next one succeeds or is dismissed.
    problem: RwSignal<Option<String>>,
    /// Bumped by each reload, so an older reload that finishes late is dropped.
    loads: StoredValue<u64>,
}

impl Panel {
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

    /// Re-reads every output and encoder profile.
    fn reload(self) {
        let Some(session) = self.session.get() else {
            return;
        };
        let Some(mine) = self.loads.try_update_value(|n| {
            *n += 1;
            *n
        }) else {
            return;
        };
        spawn_local(async move {
            let loaded = load(&*session.control).await;
            if self.loads.try_get_value() != Some(mine) {
                return;
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
                }
                Err(problem) => {
                    self.problem.try_set(Some(problem));
                }
            }
        });
    }

    /// Re-reads one output's status: after its stream key changed, which no event
    /// reports.
    fn refresh_status(self, output_id: OutputId) {
        self.run(move |session| async move {
            let (state, key_set) = status(&*session.control, &output_id).await?;
            self.rows.try_update(|rows| {
                if let Some(row) = rows.get_mut(&output_id) {
                    row.state = state;
                    row.key_set = key_set;
                }
            });
            Ok(())
        });
    }

    /// Starts or stops each of `outputs`, attempting every one and reporting each
    /// failure ([`start_or_stop_all`]).
    fn start_or_stop(self, outputs: Vec<OutputId>, start: bool) {
        self.run(move |session| async move {
            start_or_stop_all(&*session.control, outputs, start).await
        });
    }
}

/// The outputs panel, with go-live and the pipeline's health. Reads the open session
/// from [`SessionContext`] context.
#[component]
pub fn Outputs() -> impl IntoView {
    let Some(session) = use_context::<SessionContext>() else {
        return view! { <p class="placeholder">"Not connected."</p> }.into_any();
    };
    let panel = Panel {
        session,
        rows: RwSignal::new(BTreeMap::new()),
        encoders: RwSignal::new(Vec::new()),
        problem: RwSignal::new(None),
        loads: StoredValue::new(0),
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
                    Event::StatsTick { seq, drops } => {
                        health.try_update(|health| health.observe(seq, drops));
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
    let targets = move || panel.rows.with(|rows| go_live_targets(rows.values()));
    let live = move || panel.rows.with(|rows| live_outputs(rows.values()));
    let reading = move || health.with(Health::reading);

    view! {
        <section class="outputs">
            <h2>"Outputs"</h2>
            <div class="go-live">
                <button
                    class="primary"
                    disabled=move || targets().is_empty()
                    title="Starts every stopped stream output that has its key set"
                    on:click=move |_| panel.start_or_stop(targets(), true)
                >
                    "Go live"
                </button>
                <button
                    disabled=move || live().is_empty()
                    on:click=move |_| panel.start_or_stop(live(), false)
                >
                    "End stream"
                </button>
            </div>
            <p class="health" class:dropping=move || reading().dropping() role="status">
                {move || reading().summary()}
            </p>
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
            <ul class="output-list">
                <For each=ids key=|id| id.clone() let:output_id>
                    <OutputCard panel=panel output_id=output_id />
                </For>
            </ul>
            <NewOutputForm panel=panel />
        </section>
    }
    .into_any()
}

/// One output: its state, target, encoder, stream key and buttons.
#[component]
fn OutputCard(panel: Panel, output_id: OutputId) -> impl IntoView {
    let row = {
        let output_id = output_id.clone();
        Memo::new(move |_| panel.rows.with(|rows| rows.get(&output_id).cloned()))
    };
    let editing = RwSignal::new(false);
    let target = RwSignal::new(String::new());
    let key_field = NodeRef::<html::Input>::new();
    let id = StoredValue::new(output_id.clone());

    let running = move || row.with(|row| row.as_ref().is_some_and(|row| is_running(row.state)));
    let blocker = move || row.with(|row| row.as_ref().and_then(start_blocker));
    let is_rtmp = move || {
        row.with(|row| {
            row.as_ref()
                .is_some_and(|row| matches!(row.info.kind, OutputKind::Rtmp { .. }))
        })
    };
    let key_set = move || row.with(|row| row.as_ref().is_some_and(|row| row.key_set));

    let start = move |_| {
        let output_id = id.get_value();
        panel.run(move |session| async move {
            action(&*session.control, Action::OutputStart { output_id }).await
        });
    };
    let stop = move |_| {
        let output_id = id.get_value();
        panel.run(move |session| async move {
            action(&*session.control, Action::OutputStop { output_id }).await
        });
    };
    let delete = move |_| {
        let output_id = id.get_value();
        panel.run(move |session| async move {
            edit(&*session.control, Edit::OutputDelete { output_id }).await
        });
    };
    let begin_edit = move |_| {
        let url = row.with(|row| match row.as_ref().map(|row| &row.info.kind) {
            Some(OutputKind::Rtmp {
                target: RtmpTarget::Generic { url },
            }) => url.clone(),
            _ => String::new(),
        });
        target.set(url);
        editing.set(true);
    };
    let save_target = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let output_id = id.get_value();
        let url = target.get_untracked().trim().to_owned();
        panel.run(move |session| async move {
            let set = Edit::OutputSetTarget {
                output_id,
                target: RtmpTarget::Generic { url },
            };
            edit(&*session.control, set).await?;
            editing.try_set(false);
            Ok(())
        });
    };
    // The key goes from the field straight into a `SecretString`, and the field is
    // emptied before the call is sent. A button click or Enter saves it, not a form
    // submit, so the browser has no form submission to offer saving as a password.
    let save_key = move || {
        let Some(field) = key_field.get_untracked() else {
            return;
        };
        let typed = field.value();
        field.set_value("");
        let Some(key) = take_key(typed) else {
            panel
                .problem
                .set(Some("Type or paste a stream key first.".into()));
            return;
        };
        let output_id = id.get_value();
        panel.run(move |session| async move {
            let set = Action::OutputSetKey {
                output_id: output_id.clone(),
                key,
            };
            action(&*session.control, set).await?;
            panel.refresh_status(output_id);
            Ok(())
        });
    };
    let clear_key = move |_| {
        let output_id = id.get_value();
        panel.run(move |session| async move {
            let clear = Action::OutputClearKey {
                output_id: output_id.clone(),
            };
            action(&*session.control, clear).await?;
            panel.refresh_status(output_id);
            Ok(())
        });
    };
    let choose_encoder = move |event: leptos::ev::Event| {
        let chosen = event_target_value(&event);
        let profile_id = (!chosen.is_empty()).then(|| EncoderProfileId::new(chosen));
        let output_id = id.get_value();
        panel.run(move |session| async move {
            let set = Edit::OutputSetEncoder {
                output_id,
                profile_id,
            };
            edit(&*session.control, set).await
        });
    };
    let chosen_encoder = move || {
        row.with(|row| {
            row.as_ref()
                .and_then(|row| row.info.encoder_profile.clone())
        })
    };

    view! {
        <li class="output">
            <div class="output-head">
                <strong>{output_id.to_string()}</strong>
                <span
                    class="badge"
                    class:live=move || {
                        row.with(|row| row.as_ref().map(|row| row.state))
                            == Some(OutputState::Active)
                    }
                    class:failed=move || {
                        row.with(|row| row.as_ref().map(|row| row.state))
                            == Some(OutputState::Failed)
                    }
                >
                    {move || row.with(|row| row.as_ref().map_or("Gone", |row| state_label(row.state)))}
                </span>
            </div>
            <p class="target">
                {move || row.with(|row| row.as_ref().map(|row| describe_kind(&row.info.kind)))}
            </p>
            <Show when=move || editing.get()>
                <form class="inline" on:submit=save_target>
                    <input type="text" aria-label="Server URL" bind:value=target />
                    <button type="submit">"Save"</button>
                    <button type="button" on:click=move |_| editing.set(false)>
                        "Cancel"
                    </button>
                </form>
            </Show>
            <label class="encoder">
                "Encoder"
                <select on:change=choose_encoder>
                    <option value="" prop:selected=move || chosen_encoder().is_none()>
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
                                        chosen_encoder().as_ref() == Some(&profile_id)
                                    }
                                >
                                    {encoder.name}
                                </option>
                            }
                        }
                    </For>
                </select>
            </label>
            <Show when=is_rtmp>
                <div class="key">
                    <span class="key-status">
                        {move || if key_set() { "Stream key: set" } else { "Stream key: not set" }}
                    </span>
                    <input
                        type="password"
                        name="stream-key-entry"
                        node_ref=key_field
                        autocomplete="off"
                        spellcheck="false"
                        aria-label="Stream key"
                        placeholder=move || {
                            if key_set() { "Type a new key to replace it" } else { "Paste the stream key" }
                        }
                        on:keydown=move |event| {
                            if event.key() == "Enter" {
                                event.prevent_default();
                                save_key();
                            }
                        }
                    />
                    <button type="button" class="primary" on:click=move |_| save_key()>
                        "Save key"
                    </button>
                    <button type="button" disabled=move || !key_set() on:click=clear_key>
                        "Clear"
                    </button>
                </div>
            </Show>
            <div class="buttons">
                <Show
                    when=running
                    fallback=move || {
                        view! {
                            <button
                                class="primary"
                                disabled=move || blocker().is_some()
                                title=move || blocker().unwrap_or_default()
                                on:click=start
                            >
                                "Start"
                            </button>
                        }
                    }
                >
                    <button on:click=stop>"Stop"</button>
                </Show>
                <Show when=is_rtmp>
                    <button disabled=running on:click=begin_edit>
                        "Edit"
                    </button>
                </Show>
                <button disabled=running title="Stop it before deleting it" on:click=delete>
                    "Delete"
                </button>
            </div>
            {move || blocker().filter(|_| !running()).map(|why| view! { <p class="hint">{why}</p> })}
        </li>
    }
}

/// The form that adds an output: a stream to an RTMP(S) server, or a recording.
#[component]
fn NewOutputForm(panel: Panel) -> impl IntoView {
    let kind = RwSignal::new(NewKind::Rtmp);
    let target = RwSignal::new(String::new());
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        let output_id = panel
            .rows
            .with_untracked(|rows| next_output_id(rows.keys()));
        let create = create_edit(output_id, kind.get_untracked(), &target.get_untracked());
        panel.run(move |session| async move {
            edit(&*session.control, create).await?;
            target.try_set(String::new());
            Ok(())
        });
    };
    view! {
        <form class="new-output" on:submit=submit>
            <h3>"Add an output"</h3>
            <label>
                "Kind"
                <select on:change=move |event| {
                    kind.set(
                        if event_target_value(&event) == "record" {
                            NewKind::Record
                        } else {
                            NewKind::Rtmp
                        },
                    );
                }>
                    <option value="rtmp" prop:selected=move || kind.get() == NewKind::Rtmp>
                        "Stream (RTMP)"
                    </option>
                    <option value="record" prop:selected=move || kind.get() == NewKind::Record>
                        "Recording (MP4)"
                    </option>
                </select>
            </label>
            <label>
                {move || match kind.get() {
                    NewKind::Rtmp => "Server URL (without the stream key)",
                    NewKind::Record => "File path on the engine's machine",
                }}
                <input
                    type="text"
                    bind:value=target
                    placeholder=move || match kind.get() {
                        NewKind::Rtmp => "rtmps://live.example.com/app",
                        NewKind::Record => "/home/me/Videos/show.mp4",
                    }
                />
            </label>
            <button type="submit" disabled=move || target.with(|t| t.trim().is_empty())>
                "Add"
            </button>
        </form>
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Health, NewKind, OutputRow, create_edit, describe_kind, go_live_targets, live_outputs,
        load, next_output_id, start_blocker, start_or_stop_all, status, take_key,
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
                video_mix: None,
            },
            state,
            key_set,
        }
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
        assert_eq!(start_blocker(row), Some("Set a stream key first."));
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
        let (_, key_set) = now(status(&control, &id)).expect("status");
        assert!(key_set);

        let loaded = now(load(&control)).expect("load");
        assert_eq!(go_live_targets(&loaded.outputs), vec![id.clone()]);
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
}
