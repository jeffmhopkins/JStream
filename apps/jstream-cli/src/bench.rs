//! `bench`: a reproducible load run, judged against pass or fail gates (#1220).
//!
//! It builds a standard scenario on the engine it is connected to, made only of built-in
//! sources that need no hardware, so every run is the same. It records the program to a
//! file in `--out`, switches between the scenario's scenes with a transition on a fixed
//! beat, and reads `stats.tick` and `profile.get` while it does. Then it removes what it
//! made, writes `report.json`, `summary.md` and one frame sheet per switch to `--out`,
//! and fails ([`exit::GATES_FAILED`](crate::exit::GATES_FAILED)) if any gate fails.
//!
//! The gates are the alpha.5 bar by default ([`Gates::default`]): no dropped frame or
//! packet at any stage, no skipped render tick, the recording at the program's rate and
//! the program's frames inside their budget. `--gates <file>` reads other limits from a
//! JSON file; a field it leaves out keeps its default.

use crate::block::block_on;
use crate::exit::CliError;
use jstream_control::protocol::{
    Action, ActionResult, Batch, BlendMode, Call, CallResult, DropCounts, Edit, Event, FrameSheet,
    FrameStrip, ItemGeometry, ItemId, OutputId, OutputKind, OutputStatsInfo, ProfileInfo, Query,
    QueryResult, RecordContainer, SceneId, Settings, Size, SourceId, SourceKind,
    TransitionPresetId, Vec2, ViewCounts,
};
use jstream_control::{Control, EventStream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::task::{Context, Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

/// The run's length when `--seconds` names none.
pub const DEFAULT_SECONDS: u32 = 20;
/// The beat of the scene switches when `--switch-every` names none, in milliseconds.
pub const DEFAULT_SWITCH_MS: u32 = 4000;
/// The transition each switch takes when `--preset` names none.
pub const DEFAULT_PRESET: &str = "fade";
/// The shortest run: the stats come once a second, and a gate needs a few of them.
pub const MIN_SECONDS: u32 = 5;
/// The longest run: `profile.get` covers at most a minute.
pub const MAX_SECONDS: u32 = 60;
/// How long the recording may take to send its first frame before the run gives up.
const START_TIMEOUT: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 20 });
/// How long to wait for one more `stats.tick` after the run, so the last second counts.
const LAST_TICK_TIMEOUT: Duration = Duration::from_secs(3);
/// The canvas size the scenarios lay their items out for, when `state.get` has none.
const FALLBACK_CANVAS: (u32, u32) = (1920, 1080);

/// A standard scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    /// Two scenes, each a solid colour and a running timer: the floor every machine
    /// should hold.
    Basic,
    /// Two scenes of layered, moving work: an animated backdrop, shapes blended over it
    /// with transparency, a nested scene, a 3D-tilted item and a running timer.
    Layered,
}

impl Scenario {
    /// Every scenario, in the order `--scenario all` runs them.
    pub const ALL: [Scenario; 2] = [Scenario::Basic, Scenario::Layered];

    /// Its name on the command line and in the report.
    pub fn name(self) -> &'static str {
        match self {
            Scenario::Basic => "basic",
            Scenario::Layered => "layered",
        }
    }

    /// The scenario a name selects, `all` for every one.
    pub fn parse(name: &str) -> Option<Vec<Scenario>> {
        match name {
            "all" => Some(Scenario::ALL.to_vec()),
            _ => Scenario::ALL
                .into_iter()
                .find(|scenario| scenario.name() == name)
                .map(|scenario| vec![scenario]),
        }
    }
}

/// What `bench` was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BenchOptions {
    /// The scenarios to run, one after another.
    pub scenarios: Vec<Scenario>,
    /// How long each one runs, from its recording's first frame.
    pub seconds: u32,
    /// The beat of the scene switches, in milliseconds.
    pub switch_ms: u32,
    /// The transition preset each switch takes.
    pub preset: String,
    /// The directory the recordings, frame sheets and reports go to. Created if missing.
    pub out: String,
    /// A JSON file of gate limits to use instead of the defaults.
    pub gates: Option<String>,
}

/// The limits a run must stay within. Every count is over the measured window, from the
/// recording's first frame to the end of the run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Gates {
    /// Capture frames a mailbox replaced before the renderer took them.
    pub capture_frames: u64,
    /// Audio frames dropped before the mixer.
    pub audio_frames: u64,
    /// Render ticks skipped to catch up with the clock.
    pub render_ticks: u64,
    /// Video frames dropped before an encoder took them.
    pub encode_frames: u64,
    /// Audio blocks dropped before an encoder took them.
    pub encode_blocks: u64,
    /// Encoded packets dropped on the way to an output.
    pub output_packets: u64,
    /// The recording's own dropped video frames.
    pub recording_dropped_frames: u64,
    /// The recording's other dropped packets.
    pub recording_dropped_packets: u64,
    /// The recording's frame rate over the window, as a share of the program's.
    pub min_fps_ratio: f64,
    /// The share of the program's frames, in percent, that took longer than their
    /// budget.
    pub max_over_budget_pct: f64,
    /// Scene switches whose frame sheet couldn't be read.
    pub failed_switches: u64,
    /// Video frames any encoded stream (an output's or a preview's) lost between its
    /// encoder's input and where it sends them, from `profile.get`'s `streams`.
    pub stream_dropped_frames: u64,
    /// Video frames any encoded stream's encoder took more than a frame period late.
    pub stream_late_frames: u64,
    /// Mixer blocks whose mix took longer than the audio they hold.
    pub mixer_late_blocks: u64,
    /// Frames the mixer skipped to catch up with the clock.
    pub mixer_skipped_frames: u64,
    /// Frames the audio inputs had no audio for when the mixer read them.
    pub mixer_underrun_frames: u64,
    /// Frames the audio inputs dropped before the mixer read them.
    pub mixer_dropped_frames: u64,
    /// Times a client's view showed its waiting text where a picture belongs, from
    /// `profile.get`'s `views` (ADR-0132). A scene switch that flashes "Starting…"
    /// counts here.
    pub view_placeholders: u64,
    /// Times a client's view showed why it had no picture instead of one.
    pub view_blanks: u64,
    /// Frames a client's view left undecoded to catch up.
    pub view_skipped: u64,
    /// Frames or decoders a client's view had refused.
    pub view_decode_errors: u64,
}

impl Default for Gates {
    /// The alpha.5 bar: nothing dropped or late anywhere, the recording at the
    /// program's rate (within 2%, for the stats' one-second granularity), every program
    /// frame in budget, and every client view showing pictures without a placeholder.
    fn default() -> Self {
        Gates {
            capture_frames: 0,
            audio_frames: 0,
            render_ticks: 0,
            encode_frames: 0,
            encode_blocks: 0,
            output_packets: 0,
            recording_dropped_frames: 0,
            recording_dropped_packets: 0,
            min_fps_ratio: 0.98,
            max_over_budget_pct: 0.0,
            failed_switches: 0,
            stream_dropped_frames: 0,
            stream_late_frames: 0,
            mixer_late_blocks: 0,
            mixer_skipped_frames: 0,
            mixer_underrun_frames: 0,
            mixer_dropped_frames: 0,
            view_placeholders: 0,
            view_blanks: 0,
            view_skipped: 0,
            view_decode_errors: 0,
        }
    }
}

impl Gates {
    /// The gates in `path`, or the defaults without one.
    ///
    /// # Errors
    /// [`CliError::Usage`] when the file can't be read or isn't a gates object.
    pub fn load(path: Option<&str>) -> Result<Gates, CliError> {
        let Some(path) = path else {
            return Ok(Gates::default());
        };
        let text = std::fs::read_to_string(path)
            .map_err(|error| CliError::Usage(format!("could not read `{path}`: {error}")))?;
        serde_json::from_str(&text)
            .map_err(|error| CliError::Usage(format!("`{path}` is not a gates file: {error}")))
    }
}

/// One gate's verdict.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Check {
    /// What it measures.
    pub name: &'static str,
    /// What the run measured.
    pub value: f64,
    /// The limit.
    pub limit: f64,
    /// Whether the limit is a floor (`>=`) rather than a ceiling (`<=`).
    pub at_least: bool,
    /// Whether the run passed it.
    pub pass: bool,
}

impl Check {
    fn at_most(name: &'static str, value: f64, limit: f64) -> Check {
        Check {
            name,
            value,
            limit,
            at_least: false,
            pass: value <= limit,
        }
    }

    fn at_least(name: &'static str, value: f64, limit: f64) -> Check {
        Check {
            name,
            value,
            limit,
            at_least: true,
            pass: value >= limit,
        }
    }
}

/// One scene switch of a run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Switch {
    /// The scene switched to.
    pub scene: String,
    /// Milliseconds into the run.
    pub at_ms: u64,
    /// The frame sheet's file in the output directory, when it was read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
    /// Why there is no sheet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What the recording did over the window.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Recording {
    /// Video frames handed to the muxer.
    pub frames: u64,
    /// The window's length on the engine's clock, in milliseconds.
    pub ms: u64,
    /// `frames` over `ms`, a second.
    pub fps: f64,
    /// Dropped video frames.
    pub dropped_frames: u64,
    /// Other dropped packets.
    pub dropped_packets: u64,
    /// The mean bit rate, in kilobits a second.
    pub kbps: f64,
}

/// One scenario's run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Run {
    /// The scenario.
    pub scenario: Scenario,
    /// The `stats.tick` events the window covers.
    pub ticks: u64,
    /// The program's frame rate, from `profile.get`'s frame budget.
    pub program_fps: f64,
    /// The drops at each stage over the window.
    pub drops: DropCounts,
    /// The recording over the window.
    pub recording: Recording,
    /// The switches, in order.
    pub switches: Vec<Switch>,
    /// `profile.get` over the window, read before the recording stopped.
    pub profile: ProfileInfo,
    /// Every gate's verdict.
    pub checks: Vec<Check>,
    /// Whether every gate passed.
    pub pass: bool,
}

/// The whole bench: every scenario's run.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    /// The CLI that ran it.
    pub cli: &'static str,
    /// The limits it was judged by.
    pub gates: Gates,
    /// Each scenario's run.
    pub runs: Vec<Run>,
    /// Whether every run passed.
    pub pass: bool,
}

/// Runs the bench on `control` and writes its summary to `out`. Returns whether every
/// gate passed.
///
/// # Errors
/// A [`CliError`] when the engine refuses the scenario or the output directory can't be
/// written. What the run made is removed again either way.
pub fn run(
    control: &dyn Control,
    options: &BenchOptions,
    out: &mut dyn Write,
) -> Result<bool, CliError> {
    let gates = Gates::load(options.gates.as_deref())?;
    let dir = PathBuf::from(&options.out);
    std::fs::create_dir_all(&dir)?;
    // Absolute for the engine, another process, but not `canonicalize`d: on Windows
    // that gives a `\\?\` path, which the engine refuses as a recording path.
    let dir = std::path::absolute(&dir)?;
    let canvas = canvas(control)?;
    let program = program_scene(control)?;
    let mut runs = Vec::new();
    for &scenario in &options.scenarios {
        writeln!(out, "bench: {} for {}s", scenario.name(), options.seconds)?;
        let plan = Plan::new(scenario, canvas);
        let measured = plan
            .create(control)
            .and_then(|()| measure(control, &plan, options, &dir));
        // Removed whatever happened, and the program put back where it was.
        plan.remove(control, program.as_ref());
        let measured = measured?;
        runs.push(judge(scenario, measured, &gates));
    }
    let pass = runs.iter().all(|run| run.pass);
    let report = Report {
        cli: env!("CARGO_PKG_VERSION"),
        gates,
        runs,
        pass,
    };
    let summary = summary(&report);
    std::fs::write(
        dir.join("report.json"),
        serde_json::to_string_pretty(&report).map_err(std::io::Error::other)?,
    )?;
    std::fs::write(dir.join("summary.md"), &summary)?;
    out.write_all(summary.as_bytes())?;
    writeln!(out, "report: {}", dir.join("report.json").display())?;
    Ok(pass)
}

/// The canvas size from `state.get`.
fn canvas(control: &dyn Control) -> Result<(u32, u32), CliError> {
    match query(control, Query::StateGet)? {
        QueryResult::StateGet { canvas, .. } => Ok((canvas.width, canvas.height)),
        _ => Ok(FALLBACK_CANVAS),
    }
}

/// The scene on program before the bench, to put back after it.
fn program_scene(control: &dyn Control) -> Result<Option<SceneId>, CliError> {
    match query(control, Query::StudioGet)? {
        QueryResult::StudioGet { program_scene, .. } => Ok(program_scene),
        _ => Ok(None),
    }
}

fn query(control: &dyn Control, query: Query) -> Result<QueryResult, CliError> {
    match block_on(control.call(Call::Query { query }))?.result {
        CallResult::Query { result } => Ok(result),
        other => Err(CliError::Bench(format!(
            "the engine answered a query with {other:?}"
        ))),
    }
}

fn action(control: &dyn Control, action: Action) -> Result<Option<ActionResult>, CliError> {
    match block_on(control.call(Call::Action { action }))?.result {
        CallResult::Action { result } => Ok(result),
        _ => Ok(None),
    }
}

fn batch(control: &dyn Control, edits: Vec<Edit>) -> Result<(), CliError> {
    block_on(control.call(Call::Batch(Batch {
        edits,
        expected_rev: None,
        idempotency_key: None,
        ends_gesture: None,
    })))?;
    Ok(())
}

/// What a scenario puts on the engine: its scenes (the ones it switches between first,
/// then the nested ones), its sources and its recording, all under IDs of its own.
struct Plan {
    scenario: Scenario,
    prefix: String,
    edits: Vec<Edit>,
    /// The scenes it switches between.
    scenes: Vec<SceneId>,
    /// Every scene it creates, nested ones last.
    created_scenes: Vec<SceneId>,
    sources: Vec<SourceId>,
    /// The sources among `sources` that show a nested scene.
    scene_sources: Vec<SourceId>,
    output: OutputId,
}

impl Plan {
    fn new(scenario: Scenario, (width, height): (u32, u32)) -> Plan {
        let prefix = format!("bench_{}_{}", scenario.name(), std::process::id());
        let mut plan = Plan {
            scenario,
            output: OutputId::new(format!("{prefix}_rec")),
            prefix,
            edits: Vec::new(),
            scenes: Vec::new(),
            created_scenes: Vec::new(),
            sources: Vec::new(),
            scene_sources: Vec::new(),
        };
        let (w, h) = (width as f32, height as f32);
        let full = (w * 0.5, h * 0.5, w, h, 0.0);
        match scenario {
            Scenario::Basic => {
                for (n, colour) in [(1, "#203a5cff"), (2, "#5c2034ff")] {
                    let scene = plan.scene(&format!("a{n}"), true);
                    let bg = plan.source(&format!("bg{n}"), "color", json!({"color": colour}));
                    plan.item(&scene, &bg, full, None, None);
                    let timer = plan.source(&format!("timer{n}"), "timer", json!({}));
                    plan.item(
                        &scene,
                        &timer,
                        (w * 0.5, h * 0.5, w * 0.4, h * 0.15, 0.0),
                        None,
                        None,
                    );
                }
            }
            Scenario::Layered => {
                // A nested scene both top scenes place: a colour and a shape.
                let inner = plan.scene("inner", false);
                let inner_bg = plan.source("inner_bg", "color", json!({"color": "#10203080"}));
                plan.item(&inner, &inner_bg, full, None, None);
                let ring = plan.source("ring", "shape", json!({"color": "#f0c040ff"}));
                plan.item(
                    &inner,
                    &ring,
                    (w * 0.5, h * 0.5, h * 0.6, h * 0.6, 0.0),
                    None,
                    None,
                );
                let nested = plan.scene_source("nested", &inner);
                for (n, colour, blend) in [
                    (1, "#2a4a6a", BlendMode::Screen),
                    (2, "#6a2a4a", BlendMode::Multiply),
                ] {
                    let scene = plan.scene(&format!("a{n}"), true);
                    let backdrop = plan.source(
                        &format!("backdrop{n}"),
                        "backdrop",
                        json!({"color": colour}),
                    );
                    plan.item(&scene, &backdrop, full, None, None);
                    for (k, (x, colour)) in
                        [(0.25, "#ff6040c0"), (0.5, "#40ff80c0"), (0.75, "#4080ffc0")]
                            .into_iter()
                            .enumerate()
                    {
                        let shape = plan.source(
                            &format!("shape{n}_{k}"),
                            "shape",
                            json!({"color": colour}),
                        );
                        plan.item(
                            &scene,
                            &shape,
                            (w * x, h * 0.35, h * 0.4, h * 0.4, 15.0 * k as f32),
                            Some(0.8),
                            Some(blend),
                        );
                    }
                    let tilted = plan.item(
                        &scene,
                        &nested,
                        (w * 0.7, h * 0.7, w * 0.45, h * 0.45, 0.0),
                        Some(0.9),
                        None,
                    );
                    plan.tilt(&tilted, 25.0);
                    let timer = plan.source(&format!("timer{n}"), "timer", json!({}));
                    plan.item(
                        &scene,
                        &timer,
                        (w * 0.3, h * 0.8, w * 0.35, h * 0.12, 0.0),
                        None,
                        None,
                    );
                }
            }
        }
        plan
    }

    fn id(&self, name: &str) -> String {
        format!("{}_{name}", self.prefix)
    }

    fn scene(&mut self, name: &str, top: bool) -> SceneId {
        let scene = SceneId::new(self.id(name));
        self.edits.push(Edit::SceneCreate {
            scene_id: scene.clone(),
            name: format!("Bench {} {name}", self.scenario.name()),
            role: None,
        });
        if top {
            self.scenes.push(scene.clone());
            self.created_scenes.insert(0, scene.clone());
        } else {
            self.created_scenes.push(scene.clone());
        }
        scene
    }

    fn source(&mut self, name: &str, kind: &str, settings: Value) -> SourceId {
        let source = SourceId::new(self.id(name));
        let settings = match settings {
            Value::Object(map) => Settings(map.into_iter().collect()),
            _ => Settings::new(),
        };
        self.edits.push(Edit::SourceCreate {
            source_id: source.clone(),
            name: format!("Bench {name}"),
            kind: SourceKind::Builtin { kind: kind.into() },
            settings,
            duplicated_from: None,
        });
        self.sources.push(source.clone());
        source
    }

    fn scene_source(&mut self, name: &str, scene: &SceneId) -> SourceId {
        let source = SourceId::new(self.id(name));
        self.edits.push(Edit::SourceCreate {
            source_id: source.clone(),
            name: format!("Bench {name}"),
            kind: SourceKind::Scene {
                scene_id: scene.clone(),
            },
            settings: Settings::new(),
            duplicated_from: None,
        });
        self.sources.push(source.clone());
        self.scene_sources.push(source.clone());
        source
    }

    /// Places `source` in `scene` centred at `(x, y)`, `width` by `height`, turned by
    /// `rotation` degrees.
    fn item(
        &mut self,
        scene: &SceneId,
        source: &SourceId,
        (x, y, width, height, rotation): (f32, f32, f32, f32, f32),
        opacity: Option<f32>,
        blend: Option<BlendMode>,
    ) -> ItemId {
        let n = self
            .edits
            .iter()
            .filter(|edit| matches!(edit, Edit::ItemAdd { .. }))
            .count();
        let item = ItemId::new(self.id(&format!("item{n}")));
        let geometry = geometry(x, y, width, height, rotation);
        self.edits.push(Edit::ItemAdd {
            scene_id: scene.clone(),
            item_id: item.clone(),
            source_id: source.clone(),
            transform: None,
            geometry,
            opacity: opacity.and_then(jstream_control::protocol::Finite32::new),
            blend,
        });
        item
    }

    /// Tilts `item`'s placement back by `degrees`, in 3D.
    fn tilt(&mut self, item: &ItemId, degrees: f32) {
        for edit in self.edits.iter_mut().rev() {
            if let Edit::ItemAdd {
                item_id,
                geometry: Some(geometry),
                ..
            } = edit
                && item_id == item
            {
                if let Some(tilt) = jstream_control::protocol::Finite32::new(degrees) {
                    geometry.tilt_deg = tilt;
                }
                return;
            }
        }
    }

    /// Creates the scenario and its recording to `path`.
    fn create(&self, control: &dyn Control) -> Result<(), CliError> {
        batch(control, self.edits.clone())
    }

    /// Removes everything the scenario made, ignoring what is already gone, after
    /// putting `program` back on program.
    fn remove(&self, control: &dyn Control, program: Option<&SceneId>) {
        let _ = action(
            control,
            Action::OutputStop {
                output_id: self.output.clone(),
            },
        );
        if let Some(program) = program {
            let _ = action(
                control,
                Action::SceneSwitch {
                    scene_id: program.clone(),
                    preset_id: Some(TransitionPresetId::new("cut")),
                },
            );
        }
        let mut edits = vec![Edit::OutputDelete {
            output_id: self.output.clone(),
        }];
        // What shows a thing goes before it: the top scenes, the sources that show the
        // nested scenes, the nested scenes, then the other sources.
        let (top, nested) = self.created_scenes.split_at(self.scenes.len());
        let (showing_scenes, others): (Vec<_>, Vec<_>) = self
            .sources
            .iter()
            .partition(|source| self.scene_sources.contains(source));
        let scenes = |scenes: &[SceneId]| {
            scenes
                .iter()
                .map(|scene| Edit::SceneDelete {
                    scene_id: scene.clone(),
                })
                .collect::<Vec<_>>()
        };
        let sources = |sources: Vec<&SourceId>| {
            sources
                .into_iter()
                .map(|source| Edit::SourceDelete {
                    source_id: source.clone(),
                })
                .collect::<Vec<_>>()
        };
        edits.extend(scenes(top));
        edits.extend(sources(showing_scenes));
        edits.extend(scenes(nested));
        edits.extend(sources(others));
        // One batch when it all goes; else one edit at a time, so a part already gone
        // doesn't keep the rest.
        if batch(control, edits.clone()).is_err() {
            for edit in edits {
                let _ = batch(control, vec![edit]);
            }
        }
    }
}

/// A placement centred at `(x, y)`, `width` by `height`, turned by `rotation` degrees.
fn geometry(x: f32, y: f32, width: f32, height: f32, rotation: f32) -> Option<ItemGeometry> {
    let mut geometry =
        ItemGeometry::try_new(Vec2::new(x, y)?, Size::try_new(width, height).ok()?).ok()?;
    geometry.rotation_deg = jstream_control::protocol::Finite32::new(rotation)?;
    Some(geometry)
}

/// What one scenario's run measured, before it is judged.
struct Measured {
    first: Tick,
    last: Tick,
    ticks: u64,
    switches: Vec<Switch>,
    profile: ProfileInfo,
}

/// What one `stats.tick` said: the drop totals and the recording's stats.
#[derive(Clone, Debug, Default, PartialEq)]
struct Tick {
    drops: DropCounts,
    recording: Option<OutputStatsInfo>,
}

impl Tick {
    fn read(event: &Event, output: &OutputId) -> Option<Tick> {
        match event {
            Event::StatsTick { drops, outputs, .. } => Some(Tick {
                drops: *drops,
                recording: outputs
                    .iter()
                    .find(|stats| &stats.output_id == output)
                    .cloned(),
            }),
            _ => None,
        }
    }

    fn recording_frames(&self) -> u64 {
        self.recording.as_ref().map_or(0, |stats| stats.frames)
    }
}

/// Every event that has arrived, without waiting for more.
fn drain(events: &mut EventStream) -> Vec<Event> {
    let mut cx = Context::from_waker(Waker::noop());
    let mut arrived = Vec::new();
    while let Poll::Ready(Some(event)) = events.as_mut().poll_next(&mut cx) {
        arrived.push(event);
    }
    arrived
}

/// Starts the recording, waits for its first frame, then switches on the beat for the
/// run's length, reading the stats as they come.
fn measure(
    control: &dyn Control,
    plan: &Plan,
    options: &BenchOptions,
    dir: &Path,
) -> Result<Measured, CliError> {
    let path = dir.join(format!("bench-{}.mp4", plan.scenario.name()));
    batch(
        control,
        vec![Edit::OutputCreate {
            output_id: plan.output.clone(),
            kind: OutputKind::Record {
                path: path.display().to_string(),
                container: RecordContainer::Mp4,
            },
        }],
    )?;
    let first_scene = plan.scenes.first().cloned();
    if let Some(scene) = &first_scene {
        action(
            control,
            Action::SceneSwitch {
                scene_id: scene.clone(),
                preset_id: Some(TransitionPresetId::new("cut")),
            },
        )?;
    }
    let mut events = control.events();
    action(
        control,
        Action::OutputStart {
            output_id: plan.output.clone(),
        },
    )?;
    // The window opens on the first tick with a frame recorded.
    let started = Instant::now();
    let first = loop {
        if let Some(tick) = drain(&mut events)
            .iter()
            .filter_map(|event| Tick::read(event, &plan.output))
            .find(|tick| tick.recording_frames() > 0)
        {
            break tick;
        }
        if started.elapsed() > START_TIMEOUT {
            return Err(CliError::Bench(format!(
                "the recording sent no frame within {}s",
                START_TIMEOUT.as_secs()
            )));
        }
        thread::sleep(Duration::from_millis(50));
    };
    let window = Instant::now();
    let length = Duration::from_secs(options.seconds.into());
    let beat = Duration::from_millis(options.switch_ms.max(500).into());
    let mut last = first.clone();
    let mut ticks = 0u64;
    let mut switches = Vec::new();
    let mut next_switch = window + beat;
    let mut target = 1usize;
    let take = |events: &mut EventStream, last: &mut Tick, ticks: &mut u64| {
        for tick in drain(events)
            .iter()
            .filter_map(|event| Tick::read(event, &plan.output))
        {
            // A tick that doesn't list the recording keeps its last stats.
            let recording = tick.recording.or_else(|| last.recording.take());
            *last = Tick {
                drops: tick.drops,
                recording,
            };
            *ticks += 1;
        }
    };
    while window.elapsed() < length {
        take(&mut events, &mut last, &mut ticks);
        if Instant::now() >= next_switch && plan.scenes.len() > 1 {
            let scene = plan.scenes[target % plan.scenes.len()].clone();
            target += 1;
            switches.push(switch(
                control,
                &scene,
                options,
                dir,
                window,
                switches.len(),
            ));
            next_switch += beat;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // One more tick, so the run's last second is in the window.
    let waited = Instant::now();
    let before = ticks;
    while ticks == before && waited.elapsed() < LAST_TICK_TIMEOUT {
        take(&mut events, &mut last, &mut ticks);
        thread::sleep(Duration::from_millis(50));
    }
    let seconds = options.seconds.clamp(1, MAX_SECONDS);
    let profile = match query(
        control,
        Query::ProfileGet {
            seconds: Some(seconds),
            top: None,
        },
    )? {
        QueryResult::ProfileGet { profile } => profile,
        _ => ProfileInfo::default(),
    };
    Ok(Measured {
        first,
        last,
        ticks,
        switches,
        profile,
    })
}

/// Switches to `scene` with the run's transition, keeping its frame sheet as
/// `switch-<n>.png` in `dir`.
fn switch(
    control: &dyn Control,
    scene: &SceneId,
    options: &BenchOptions,
    dir: &Path,
    window: Instant,
    n: usize,
) -> Switch {
    let at_ms = u64::try_from(window.elapsed().as_millis()).unwrap_or(u64::MAX);
    let watched = action(
        control,
        Action::SceneSwitchAndWatch {
            scene_id: scene.clone(),
            preset_id: Some(TransitionPresetId::new(options.preset.clone())),
            strip: FrameStrip::new(6, 150),
        },
    );
    let saved = watched.and_then(|result| match result {
        Some(ActionResult::SceneSwitchAndWatch { sheet }) => save_sheet(&sheet, dir, n),
        _ => Err(CliError::Bench("no frame sheet came back".into())),
    });
    match saved {
        Ok(file) => Switch {
            scene: scene.to_string(),
            at_ms,
            sheet: Some(file),
            error: None,
        },
        Err(error) => Switch {
            scene: scene.to_string(),
            at_ms,
            sheet: None,
            error: Some(error.to_string()),
        },
    }
}

fn save_sheet(sheet: &FrameSheet, dir: &Path, n: usize) -> Result<String, CliError> {
    let png = decode_base64(&sheet.png)
        .ok_or_else(|| CliError::Bench("the frame sheet's PNG is not base64".into()))?;
    let file = format!("switch-{n}.png");
    std::fs::write(dir.join(&file), png)?;
    Ok(file)
}

/// Standard base64 with padding, as frame sheets carry their PNG.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 {
            return None;
        }
        let mut word = 0u32;
        for &c in &chunk[..4 - pad] {
            word = word << 6 | value(c)?;
        }
        word <<= 6 * pad as u32;
        let decoded = [(word >> 16) as u8, (word >> 8) as u8, word as u8];
        out.extend_from_slice(&decoded[..3 - pad]);
    }
    Some(out)
}

/// The run's verdict against `gates`.
fn judge(scenario: Scenario, measured: Measured, gates: &Gates) -> Run {
    let Measured {
        first,
        last,
        ticks,
        switches,
        profile,
    } = measured;
    let drops = delta(&first.drops, &last.drops);
    let recording = recording(first.recording.as_ref(), last.recording.as_ref());
    let budget = f64::from(profile.frame_budget_ms.get());
    let program_fps = if budget > 0.0 { 1000.0 / budget } else { 0.0 };
    let (frames, over) = profile
        .drawers
        .iter()
        .filter(|drawer| drawer.drawer == jstream_control::protocol::Drawer::Program)
        .fold((0u64, 0u64), |(frames, over), drawer| {
            (frames + drawer.frames, over + drawer.over_budget)
        });
    let over_pct = if frames > 0 {
        over as f64 * 100.0 / frames as f64
    } else {
        0.0
    };
    let fps_ratio = if program_fps > 0.0 {
        recording.fps / program_fps
    } else {
        0.0
    };
    let failed = switches.iter().filter(|s| s.sheet.is_none()).count() as u64;
    let streams = profile
        .streams
        .iter()
        .fold((0u64, 0u64), |(dropped, late), stream| {
            (
                dropped.saturating_add(stream.dropped),
                late.saturating_add(stream.late),
            )
        });
    let mixer = profile.mixer.unwrap_or_default();
    let views = profile
        .views
        .iter()
        .fold(ViewCounts::default(), |sum, view| ViewCounts {
            received: sum.received.saturating_add(view.counts.received),
            shown: sum.shown.saturating_add(view.counts.shown),
            skipped: sum.skipped.saturating_add(view.counts.skipped),
            decode_errors: sum.decode_errors.saturating_add(view.counts.decode_errors),
            placeholders: sum.placeholders.saturating_add(view.counts.placeholders),
            blanks: sum.blanks.saturating_add(view.counts.blanks),
            reopens: sum.reopens.saturating_add(view.counts.reopens),
        });
    let count = |n: u64| n as f64;
    let checks = vec![
        Check::at_most(
            "capture_frames",
            count(drops.capture_frames),
            count(gates.capture_frames),
        ),
        Check::at_most(
            "audio_frames",
            count(drops.audio_frames),
            count(gates.audio_frames),
        ),
        Check::at_most(
            "render_ticks",
            count(drops.render_ticks),
            count(gates.render_ticks),
        ),
        Check::at_most(
            "encode_frames",
            count(drops.encode_frames),
            count(gates.encode_frames),
        ),
        Check::at_most(
            "encode_blocks",
            count(drops.encode_blocks),
            count(gates.encode_blocks),
        ),
        Check::at_most(
            "output_packets",
            count(drops.output_packets),
            count(gates.output_packets),
        ),
        Check::at_most(
            "recording_dropped_frames",
            count(recording.dropped_frames),
            count(gates.recording_dropped_frames),
        ),
        Check::at_most(
            "recording_dropped_packets",
            count(recording.dropped_packets),
            count(gates.recording_dropped_packets),
        ),
        Check::at_least("fps_ratio", fps_ratio, gates.min_fps_ratio),
        Check::at_most("over_budget_pct", over_pct, gates.max_over_budget_pct),
        Check::at_most(
            "failed_switches",
            count(failed),
            count(gates.failed_switches),
        ),
        Check::at_most(
            "stream_dropped_frames",
            count(streams.0),
            count(gates.stream_dropped_frames),
        ),
        Check::at_most(
            "stream_late_frames",
            count(streams.1),
            count(gates.stream_late_frames),
        ),
        Check::at_most(
            "mixer_late_blocks",
            count(mixer.late_blocks),
            count(gates.mixer_late_blocks),
        ),
        Check::at_most(
            "mixer_skipped_frames",
            count(mixer.skipped_frames),
            count(gates.mixer_skipped_frames),
        ),
        Check::at_most(
            "mixer_underrun_frames",
            count(mixer.underrun_frames),
            count(gates.mixer_underrun_frames),
        ),
        Check::at_most(
            "mixer_dropped_frames",
            count(mixer.dropped_frames),
            count(gates.mixer_dropped_frames),
        ),
        // A client that reports no view (a CLI, an MCP agent) passes these vacuously;
        // the summary says so.
        Check::at_most(
            "view_placeholders",
            count(views.placeholders),
            count(gates.view_placeholders),
        ),
        Check::at_most("view_blanks", count(views.blanks), count(gates.view_blanks)),
        Check::at_most(
            "view_skipped",
            count(views.skipped),
            count(gates.view_skipped),
        ),
        Check::at_most(
            "view_decode_errors",
            count(views.decode_errors),
            count(gates.view_decode_errors),
        ),
    ];
    let pass = checks.iter().all(|check| check.pass);
    Run {
        scenario,
        ticks,
        program_fps,
        drops,
        recording,
        switches,
        profile,
        checks,
        pass,
    }
}

/// `last` less `first`, field by field.
fn delta(first: &DropCounts, last: &DropCounts) -> DropCounts {
    DropCounts {
        capture_frames: last.capture_frames.saturating_sub(first.capture_frames),
        audio_frames: last.audio_frames.saturating_sub(first.audio_frames),
        render_ticks: last.render_ticks.saturating_sub(first.render_ticks),
        encode_frames: last.encode_frames.saturating_sub(first.encode_frames),
        encode_blocks: last.encode_blocks.saturating_sub(first.encode_blocks),
        output_packets: last.output_packets.saturating_sub(first.output_packets),
    }
}

/// The recording over the window, from its first and last stats.
fn recording(first: Option<&OutputStatsInfo>, last: Option<&OutputStatsInfo>) -> Recording {
    let (Some(first), Some(last)) = (first, last) else {
        return Recording::default();
    };
    let frames = last.frames.saturating_sub(first.frames);
    let ms = last.uptime_ms.saturating_sub(first.uptime_ms);
    let bytes = last.bytes.saturating_sub(first.bytes);
    let (fps, kbps) = if ms > 0 {
        (
            frames as f64 * 1000.0 / ms as f64,
            bytes as f64 * 8.0 / ms as f64,
        )
    } else {
        (0.0, 0.0)
    };
    Recording {
        frames,
        ms,
        fps,
        // Run totals: the run started with the recording, so they are the window's.
        dropped_frames: last.dropped_frames,
        dropped_packets: last.dropped_packets,
        kbps,
    }
}

/// The Markdown pass or fail summary.
pub fn summary(report: &Report) -> String {
    let mut text = String::new();
    let verdict = if report.pass { "PASS" } else { "FAIL" };
    let _ = writeln!(text, "# jstream bench: {verdict}\n");
    for run in &report.runs {
        let _ = writeln!(
            text,
            "## {} ({})\n\nRecording {:.1} fps of the program's {:.1}, {:.0} kbit/s over {:.1}s and {} stats ticks; {} switches.\n",
            run.scenario.name(),
            if run.pass { "pass" } else { "fail" },
            run.recording.fps,
            run.program_fps,
            run.recording.kbps,
            run.recording.ms as f64 / 1000.0,
            run.ticks,
            run.switches.len(),
        );
        if run.profile.views.is_empty() {
            let _ = writeln!(
                text,
                "No client reported a view, so the view gates pass without a picture shown.\n"
            );
        } else {
            for view in &run.profile.views {
                let c = &view.counts;
                let _ = writeln!(
                    text,
                    "View `{}` of `{}`: {} frames received, {} shown, {} skipped, {} placeholders, {} reopens.",
                    view.view,
                    view.client,
                    c.received,
                    c.shown,
                    c.skipped,
                    c.placeholders,
                    c.reopens,
                );
            }
            text.push('\n');
        }
        let _ = writeln!(text, "| Gate | Measured | Limit | |\n|---|---|---|---|");
        for check in &run.checks {
            let _ = writeln!(
                text,
                "| {} | {} | {} {} | {} |",
                check.name,
                number(check.value),
                if check.at_least { ">=" } else { "<=" },
                number(check.limit),
                if check.pass { "pass" } else { "**fail**" },
            );
        }
        for (n, switch) in run.switches.iter().enumerate() {
            if let Some(error) = &switch.error {
                let _ = writeln!(
                    text,
                    "\nSwitch {n} to `{}`: no frame sheet ({error}).",
                    switch.scene
                );
            }
        }
        text.push('\n');
    }
    text
}

fn number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        format!("{value:.3}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jstream_control::protocol::{DrawerProfile, Finite32, OutputHealth};

    fn stats(frames: u64, uptime_ms: u64, bytes: u64, dropped: u64) -> OutputStatsInfo {
        OutputStatsInfo {
            output_id: OutputId::new("rec"),
            uptime_ms,
            bytes,
            frames,
            kbps: 0,
            dropped_frames: dropped,
            dropped_packets: 0,
            health: OutputHealth::Good,
        }
    }

    fn profile(budget_ms: f32, frames: u64, over: u64) -> ProfileInfo {
        let drawer: DrawerProfile = serde_json::from_value(json!({
            "drawer": "program",
            "frames": frames,
            "over_budget": over,
        }))
        .expect("a drawer");
        ProfileInfo {
            frame_budget_ms: Finite32::new(budget_ms).expect("finite"),
            drawers: vec![drawer],
            ..ProfileInfo::default()
        }
    }

    fn measured(first: Tick, last: Tick, profile: ProfileInfo) -> Measured {
        Measured {
            first,
            last,
            ticks: 10,
            switches: vec![Switch {
                scene: "a".into(),
                at_ms: 4000,
                sheet: Some("switch-0.png".into()),
                error: None,
            }],
            profile,
        }
    }

    #[test]
    fn a_clean_run_at_the_programs_rate_passes() {
        let first = Tick {
            drops: DropCounts {
                capture_frames: 4,
                ..DropCounts::default()
            },
            recording: Some(stats(10, 1_000, 1_000, 0)),
        };
        let last = Tick {
            drops: DropCounts {
                capture_frames: 4,
                ..DropCounts::default()
            },
            recording: Some(stats(10 + 600, 11_000, 1_000 + 10_000_000, 0)),
        };
        let run = judge(
            Scenario::Basic,
            measured(first, last, profile(1000.0 / 60.0, 600, 0)),
            &Gates::default(),
        );
        assert!(run.pass, "{:?}", run.checks);
        assert!((run.recording.fps - 60.0).abs() < 1e-9);
        assert!((run.recording.kbps - 8000.0).abs() < 1e-9);
        // Drops before the window don't count.
        assert_eq!(run.drops.capture_frames, 0);
    }

    #[test]
    fn a_drop_a_slow_recording_or_a_frame_over_budget_fails() {
        let first = Tick {
            drops: DropCounts::default(),
            recording: Some(stats(0, 0, 0, 0)),
        };
        let last = Tick {
            drops: DropCounts {
                render_ticks: 2,
                ..DropCounts::default()
            },
            recording: Some(stats(500, 10_000, 1, 3)),
        };
        let run = judge(
            Scenario::Layered,
            measured(first, last, profile(1000.0 / 60.0, 600, 6)),
            &Gates::default(),
        );
        let failed: Vec<_> = run
            .checks
            .iter()
            .filter(|check| !check.pass)
            .map(|check| check.name)
            .collect();
        assert_eq!(
            failed,
            [
                "render_ticks",
                "recording_dropped_frames",
                "fps_ratio",
                "over_budget_pct"
            ]
        );
        assert!(!run.pass);
        let text = summary(&Report {
            cli: "test",
            gates: Gates::default(),
            runs: vec![run],
            pass: false,
        });
        assert!(text.starts_with("# jstream bench: FAIL"), "{text}");
        assert!(
            text.contains("| render_ticks | 2 | <= 0 | **fail** |"),
            "{text}"
        );
    }

    /// ADR-0132: a stream's late frame, a late mixer block and a view's placeholder fail
    /// the run like a drop does, and the summary lists the views.
    #[test]
    fn a_late_stream_a_late_block_or_a_placeholder_fails() {
        let tick = Tick {
            drops: DropCounts::default(),
            recording: Some(stats(0, 0, 0, 0)),
        };
        let last = Tick {
            drops: DropCounts::default(),
            recording: Some(stats(600, 10_000, 1, 0)),
        };
        let mut info = profile(1000.0 / 60.0, 600, 0);
        info.streams = vec![
            serde_json::from_value(json!({
                "stream": {"type": "preview", "channel": 1},
                "late": 2,
            }))
            .expect("a stream"),
        ];
        info.mixer = Some(jstream_control::protocol::MixerProfile {
            late_blocks: 1,
            ..Default::default()
        });
        info.views = vec![
            serde_json::from_value(json!({
                "client": "jstream-ui",
                "view": "canvas",
                "counts": {"received": 600, "shown": 600, "placeholders": 1, "reopens": 2},
            }))
            .expect("a view"),
        ];
        let run = judge(
            Scenario::Basic,
            measured(tick, last, info),
            &Gates::default(),
        );
        let failed: Vec<_> = run
            .checks
            .iter()
            .filter(|check| !check.pass)
            .map(|check| check.name)
            .collect();
        assert_eq!(
            failed,
            [
                "stream_late_frames",
                "mixer_late_blocks",
                "view_placeholders"
            ]
        );
        let text = summary(&Report {
            cli: "test",
            gates: Gates::default(),
            runs: vec![run],
            pass: false,
        });
        assert!(
            text.contains("View `canvas` of `jstream-ui`: 600 frames received"),
            "{text}"
        );
    }

    #[test]
    fn a_gates_file_overrides_only_what_it_names() {
        let gates: Gates =
            serde_json::from_str(r#"{"min_fps_ratio": 0.9, "render_ticks": 3}"#).expect("gates");
        assert_eq!(gates.render_ticks, 3);
        assert!((gates.min_fps_ratio - 0.9).abs() < 1e-12);
        assert_eq!(gates.encode_frames, 0);
        assert!(serde_json::from_str::<Gates>(r#"{"renderticks": 3}"#).is_err());
    }

    /// Emits a `stats.tick` every 100 ms for `output`, its frames at 60 a second, until
    /// `stop`.
    fn ticking(
        engine: &jstream_control::MockEngine,
        output: OutputId,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> thread::JoinHandle<()> {
        let engine = engine.clone();
        thread::spawn(move || {
            let mut n = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                n += 1;
                let mut recording = stats(n * 6, n * 100, n * 1000, 0);
                recording.output_id = output.clone();
                engine.emit(Event::StatsTick {
                    seq: n,
                    drops: DropCounts::default(),
                    load: None,
                    chains: None,
                    outputs: vec![recording],
                });
                thread::sleep(Duration::from_millis(100));
            }
        })
    }

    fn leftovers(control: &dyn Control) -> Vec<String> {
        let state = serde_json::to_value(query(control, Query::StateGet).expect("state"))
            .expect("json")
            .to_string();
        state
            .match_indices("bench_")
            .map(|(at, _)| state[at..].chars().take(40).collect())
            .collect()
    }

    fn options(dir: &Path) -> BenchOptions {
        BenchOptions {
            scenarios: Scenario::ALL.to_vec(),
            seconds: 1,
            switch_ms: 500,
            preset: DEFAULT_PRESET.into(),
            out: dir.display().to_string(),
            gates: None,
        }
    }

    #[test]
    fn a_mock_run_builds_records_switches_reports_and_cleans_up() {
        let engine = jstream_control::MockEngine::new();
        let control = engine.connect();
        block_on(control.hello(jstream_control::protocol::Hello {
            protocol: jstream_control::protocol::PROTOCOL_VERSION,
            client_name: "bench-test".into(),
        }))
        .expect("hello");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tickers: Vec<_> = Scenario::ALL
            .into_iter()
            .map(|scenario| {
                ticking(
                    &engine,
                    Plan::new(scenario, FALLBACK_CANVAS).output,
                    stop.clone(),
                )
            })
            .collect();
        let dir = std::env::temp_dir().join(format!("jstream-bench-test-{}", std::process::id()));
        let mut out = Vec::new();
        let pass = run(&control, &options(&dir), &mut out);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for ticker in tickers {
            ticker.join().expect("ticker");
        }
        let out = String::from_utf8(out).expect("utf-8");
        // The mock has no profile (so no frame budget) and no frame sheets.
        assert!(!pass.expect("the bench ran"), "{out}");
        assert!(out.contains("## basic (fail)"), "{out}");
        assert!(out.contains("## layered (fail)"), "{out}");
        assert!(out.contains("| encode_frames | 0 | <= 0 | pass |"), "{out}");
        assert!(out.contains("| failed_switches |"), "{out}");
        let report: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("report.json")).expect("report.json"),
        )
        .expect("a JSON report");
        assert_eq!(report["runs"].as_array().map(Vec::len), Some(2));
        assert!(
            report["runs"][0]["recording"]["frames"].as_u64() > Some(0),
            "{report}"
        );
        assert!(dir.join("summary.md").exists());
        assert_eq!(leftovers(&control), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_recording_that_never_starts_fails_the_run_and_still_cleans_up() {
        let engine = jstream_control::MockEngine::new();
        let control = engine.connect();
        block_on(control.hello(jstream_control::protocol::Hello {
            protocol: jstream_control::protocol::PROTOCOL_VERSION,
            client_name: "bench-test".into(),
        }))
        .expect("hello");
        let dir = std::env::temp_dir().join(format!("jstream-bench-idle-{}", std::process::id()));
        let mut options = options(&dir);
        options.scenarios = vec![Scenario::Layered];
        let error = run(&control, &options, &mut Vec::new()).expect_err("no stats ticks");
        assert!(error.to_string().contains("no frame"), "{error}");
        assert_eq!(error.exit_code(), crate::exit::BENCH_FAILED);
        assert_eq!(leftovers(&control), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base64_decodes_with_and_without_padding() {
        assert_eq!(decode_base64("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(decode_base64("aGk=").as_deref(), Some(&b"hi"[..]));
        assert_eq!(decode_base64("YWJj").as_deref(), Some(&b"abc"[..]));
        assert_eq!(decode_base64(""), Some(Vec::new()));
        assert_eq!(decode_base64("abc"), None);
        assert_eq!(decode_base64("a*c="), None);
    }

    #[test]
    fn each_scenario_names_its_scenes_sources_and_items_under_its_own_prefix() {
        for scenario in Scenario::ALL {
            let plan = Plan::new(scenario, (1920, 1080));
            assert_eq!(
                plan.scenes.len(),
                2,
                "{scenario:?} switches between two scenes"
            );
            for edit in &plan.edits {
                let id = match edit {
                    Edit::SceneCreate { scene_id, .. } => scene_id.to_string(),
                    Edit::SourceCreate { source_id, .. } => source_id.to_string(),
                    Edit::ItemAdd {
                        item_id, geometry, ..
                    } => {
                        assert!(geometry.is_some(), "every item is placed");
                        item_id.to_string()
                    }
                    other => panic!("unexpected {other:?}"),
                };
                assert!(id.starts_with(&plan.prefix), "{id}");
            }
        }
        assert_eq!(Scenario::parse("all"), Some(Scenario::ALL.to_vec()));
        assert_eq!(Scenario::parse("layered"), Some(vec![Scenario::Layered]));
        assert_eq!(Scenario::parse("webcam"), None);
    }
}
