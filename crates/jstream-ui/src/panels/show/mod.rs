//! The Show panel: a show's timeline in the bottom dock, with two views of the same show.
//! **Edit** (`edit.rs`) is the timeline: tracks, ruler, play head, bookmarks, loops and the
//! zoom bar, with the editing tools. **Run** (`run.rs`) is for show time: a big Continue,
//! Now and Next, the bookmarks as buttons, clocks, what is up now and the tracks folded
//! into one strip. The bar along the top picks the show and the view.
//!
//! Both views read one [`ShowPanel`] from context: the shows, the runner's state and the
//! part of the show in view. What the panel draws is [`model`]; how time sits on screen is
//! [`time`]. Every button, key and click that drives the runner goes through
//! [`ShowPanel::send`], one [`Command`] each.
//!
//! Docs: crates/jstream-ui/README.md#design

pub mod drop;
pub mod edit;
pub mod edits;
pub mod inspect_edits;
pub mod inspector;
pub mod model;
pub mod prompter;
pub mod run;
pub mod time;
pub mod wire;

use crate::app::SessionContext;
use crate::store::{Collection, Studio};
use jstream_control::protocol::{Edit, NextMonitor as WireMonitor, ShowInfo};
use leptos::prelude::*;
use model::{RunPhase, RunView, ShowView};
use time::Viewport;

/// Which of the panel's two views shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The timeline, with the editing tools.
    #[default]
    Edit,
    /// The show-time view: Continue, Now and Next, the bookmark buttons, the clocks.
    Run,
}

/// What the panel asks the runner to do: one for each button, key and click that drives
/// it. Each is one call to the engine.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Start the show, by ID, from its start.
    Play(String),
    /// Stop the running show.
    Stop,
    /// Hold the running show's play head where it is (`show.pause`); Play or Continue
    /// plays on.
    Pause,
    /// Continue: play on from a pause, or to the next pausing bookmark (`show.continue`).
    Continue,
    /// Back to the previous pausing bookmark.
    Back,
    /// Jump to a bookmark, by ID, at once.
    GoTo(String),
    /// Arm a spot, in show time, for the next Continue (a click on the Run view's strip
    /// while the show runs). `None` disarms.
    Arm(Option<u32>),
    /// Move the running show's play head to a spot, in show time, at once (a click or a
    /// drag on the Edit view's ruler).
    Seek(u32),
    /// Lock or unlock editing of the show, by ID.
    Lock(String, bool),
    /// Run on preview, leaving program alone, or not.
    Rehearse(bool),
    /// What the Next monitor shows, for the show, by ID.
    NextMonitor(String, model::NextMonitor),
    /// Add an overlay track to the show, by ID, above its other tracks.
    AddOverlayTrack(String),
}

/// The panel's shared state, in context for its views.
#[derive(Clone, Copy)]
pub struct ShowPanel {
    /// Every show in the collection, in the picker's order.
    pub shows: RwSignal<Vec<ShowView>>,
    /// The show the panel has open, by ID.
    pub open: RwSignal<Option<String>>,
    /// Where the runner is.
    pub run: RwSignal<RunView>,
    /// Which view shows.
    pub mode: RwSignal<Mode>,
    /// The part of the open show in view in the Edit view.
    pub viewport: RwSignal<Viewport>,
    /// The time now (`performance.now()`), ticking about 30 times a second while the
    /// panel is open, for the play head and the clocks.
    pub clock: RwSignal<f64>,
    /// The problem bar's message, where a command that failed says so.
    pub problem: RwSignal<Option<String>>,
    /// The shows whose editing is locked, by ID: the panel's own state, never saved.
    pub locked: RwSignal<std::collections::HashSet<String>>,
    /// The shared state and the open session, where the panel runs in the app.
    engine: Option<(Studio, SessionContext)>,
    /// The key bound to Continue, as its key cap shows it ("Space"), if any.
    pub continue_key: RwSignal<Option<String>>,
    /// The element selected in the Edit view (a clip, marker, cue, bookmark or loop),
    /// by its ID within the open show: what the Inspector's Show page edits.
    pub selected: RwSignal<Option<String>>,
    /// The open show as a drag in the Edit view would leave it, drawn in its place while
    /// the drag lasts; sent as one `show.set` when the drag ends.
    pub preview: RwSignal<Option<ShowView>>,
    /// A scene dragged from the Scenes panel, while it is: where the pointer is, and
    /// whether it was let go.
    pub carry: RwSignal<Option<Carry>>,
    /// Whether the show picker is a name field, renaming the open show.
    pub renaming: RwSignal<bool>,
    /// Where Play starts the open show, while none runs: set by a click or a drag on the
    /// ruler, `None` for its start.
    pub cursor: RwSignal<Option<u32>>,
    /// Whether the Run view's bookmark notes are unfolded: the panel's own state, never
    /// saved.
    pub notes_open: RwSignal<bool>,
    /// The notes' teleprompter settings, kept by the browser.
    pub prompter: RwSignal<prompter::Prompter>,
}

/// A scene dragged from the Scenes panel, or a source's audio from the Sources panel or
/// a mixer strip, towards the Show panel's tracks. The panel it comes from sets it as
/// the pointer moves and marks it dropped when the button comes up; the Edit view
/// places it where it was let go and clears it.
#[derive(Clone, Debug, PartialEq)]
pub struct Carry {
    /// The scene's ID; empty for a source's audio.
    pub scene: String,
    /// The source whose audio it is, for a source's audio.
    pub source: Option<String>,
    /// Its name, for the pausing bookmark at a scene's end.
    pub name: String,
    /// Where the pointer is, in the page's CSS pixels.
    pub x: f64,
    /// Where the pointer is, in CSS pixels from the viewport's top.
    pub y: f64,
    /// Whether the button came up: place it.
    pub dropped: bool,
}

impl ShowPanel {
    /// Carries source `source`'s audio (called `name`) towards the tracks, with the
    /// pointer at `event`; `dropped` when the button came up there.
    pub fn carry_source(
        &self,
        source: &str,
        name: &str,
        event: &leptos::ev::PointerEvent,
        dropped: bool,
    ) {
        self.carry.set(Some(Carry {
            scene: String::new(),
            source: Some(source.to_owned()),
            name: name.to_owned(),
            x: f64::from(event.client_x()),
            y: f64::from(event.client_y()),
            dropped,
        }));
    }
}

/// Where the browser keeps the show last opened.
const OPEN_KEY: &str = "jstream.show.open";

/// The show last opened in this browser, if any.
fn remembered_show() -> Option<String> {
    window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|s| s.get_item(OPEN_KEY).ok().flatten())
}

impl ShowPanel {
    /// A panel with no shows, reporting problems to `problem`.
    pub fn new(problem: RwSignal<Option<String>>) -> ShowPanel {
        ShowPanel {
            problem,
            locked: RwSignal::new(std::collections::HashSet::new()),
            engine: None,
            shows: RwSignal::new(Vec::new()),
            open: RwSignal::new(None),
            run: RwSignal::new(RunView::default()),
            mode: RwSignal::new(Mode::Edit),
            viewport: RwSignal::new(Viewport::fit(0)),
            clock: RwSignal::new(0.0),
            continue_key: RwSignal::new(None),
            selected: RwSignal::new(None),
            preview: RwSignal::new(None),
            carry: RwSignal::new(None),
            renaming: RwSignal::new(false),
            notes_open: RwSignal::new(true),
            cursor: RwSignal::new(None),
            prompter: RwSignal::new(prompter::Prompter::default()),
        }
    }

    /// The open show: the one named, else the running one, else the first; drawn as a
    /// drag would leave it while one lasts.
    pub fn show(&self) -> Option<ShowView> {
        let open = self.open.get();
        let running = self.run.with(|r| r.show.clone());
        let show = self.shows.with(|shows| {
            open.or(running)
                .and_then(|id| shows.iter().find(|s| s.id == id).cloned())
                .or_else(|| shows.first().cloned())
        })?;
        let preview = self
            .preview
            .with(|p| p.as_ref().filter(|p| p.id == show.id).cloned());
        Some(preview.unwrap_or(show))
    }

    /// Whether the open show is the one running.
    pub fn is_running(&self) -> bool {
        let id = self.show().map(|s| s.id);
        self.run.with(|r| r.show.is_some() && r.show == id)
    }

    /// The play head now: the runner's while it runs the open show, else where Play will
    /// start the open show.
    pub fn head(&self) -> u32 {
        if self.is_running() {
            let now = self.clock.get();
            self.run.with(|r| r.head_at(now))
        } else {
            self.cursor.get().unwrap_or(0)
        }
    }

    /// Moves the play head to `at_ms`: a seek while the open show runs, else where Play
    /// will start it. Another show running doesn't move.
    pub fn move_head(&self, at_ms: u32) {
        if untrack(|| self.is_running()) {
            self.send(Command::Seek(at_ms));
        } else {
            self.cursor.set(Some(at_ms));
        }
    }

    /// A panel that follows the studio's shows and running show, and sends its commands
    /// through `session`.
    pub fn connected(studio: Studio, session: SessionContext) -> ShowPanel {
        let panel = ShowPanel {
            engine: Some((studio, session)),
            prompter: RwSignal::new(prompter::Prompter::restore()),
            open: RwSignal::new(remembered_show()),
            ..ShowPanel::new(studio.problem)
        };
        // Where Play starts belongs to the show it was set on.
        Effect::new(move |seen: Option<Option<String>>| {
            let open = panel.open.get();
            if seen.is_some_and(|seen| seen != open) {
                panel.cursor.set(None);
            }
            open
        });
        // The show last opened in this browser opens again next time.
        Effect::new(move |_| {
            if let (Some(id), Ok(Some(storage))) = (panel.open.get(), window().local_storage()) {
                let _ = storage.set_item(OPEN_KEY, &id);
            }
        });
        Effect::new(move |_| {
            let locked = panel.locked.get();
            let shows = studio.collection.with(|c| {
                c.as_ref().map_or_else(Vec::new, |c| {
                    c.shows
                        .iter()
                        .map(|s| {
                            let mut view = wire::view_of(s, c);
                            view.locked = locked.contains(&view.id);
                            view
                        })
                        .collect()
                })
            });
            panel.shows.set(shows);
        });
        let continue_keys = Memo::new(move |_| {
            studio.collection.with(|c| {
                c.as_ref().map_or_else(Vec::new, |c| {
                    wire::keys_for(c, &jstream_control::protocol::Action::ShowContinue)
                })
            })
        });
        Effect::new(move |_| {
            let keys = continue_keys.get();
            panel.continue_key.set(keys.first().cloned());
            let mut run = studio.show.with(|s| wire::run_of(s.as_ref()));
            run.continue_keys = keys;
            // Off script names the scene put up by hand, as the Scenes panel does.
            if let Some(id) = &run.off_script {
                let name = studio.collection.with(|c| {
                    c.as_ref()
                        .and_then(|c| c.scene(&jstream_control::protocol::SceneId::new(id.clone())))
                        .map(|s| s.name.clone())
                });
                if let Some(name) = name {
                    run.off_script = Some(name);
                }
            }
            panel.run.set(run);
        });
        panel
    }

    /// Asks the runner to do `command`: the `show.*` action it runs, or for Lock the
    /// panel's own switch, or for the Next monitor a `show.set` of the open show.
    pub fn send(&self, command: Command) {
        match &command {
            Command::Lock(id, on) => {
                let (id, on) = (id.clone(), *on);
                self.locked.update(|l| {
                    if on {
                        l.insert(id);
                    } else {
                        l.remove(&id);
                    }
                });
                return;
            }
            Command::AddOverlayTrack(id) => {
                self.set_show(id, |s| {
                    edits::add_overlay_track(s);
                });
                return;
            }
            Command::NextMonitor(id, monitor) => {
                self.set_show(id, |s| {
                    s.next_monitor = match monitor {
                        model::NextMonitor::Live => WireMonitor::Live,
                        model::NextMonitor::Still => WireMonitor::Still,
                        model::NextMonitor::Off => WireMonitor::Off,
                    };
                });
                return;
            }
            _ => {}
        }
        let Some((studio, session)) = self.engine else {
            return;
        };
        let open = self.show().map(|s| s.id);
        if let (Some(action), Some(session)) =
            (wire::action_of(&command, open.as_deref()), session.get())
        {
            studio.act(&session, action);
        }
        // Play starts from the start, then goes where the ruler put the play head.
        if let Command::Play(_) = command
            && let Some(at_ms) = self.cursor.get_untracked().filter(|at| *at > 0)
            && let Some(session) = session.get()
        {
            studio.act(
                &session,
                jstream_control::protocol::Action::ShowSeek { at_ms },
            );
        }
        if matches!(command, Command::Play(_) | Command::Stop) {
            self.cursor.set(None);
        }
    }

    /// Changes show `id` with `f` and sends it as one `show.set`, so one undo step. A
    /// change that changes nothing sends nothing.
    pub fn set_show(&self, id: &str, f: impl FnOnce(&mut ShowInfo)) {
        let Some(mut show) = self.info(id) else {
            return;
        };
        f(&mut show);
        self.commit(show);
    }

    /// Show `id` as the engine has it, if the panel runs in the app.
    pub fn info(&self, id: &str) -> Option<ShowInfo> {
        self.collection(|c| c.shows.iter().find(|s| s.show_id.as_str() == id).cloned())
            .flatten()
    }

    /// `f` of the collection, untracked, where the panel runs in the app.
    pub fn collection<R>(&self, f: impl FnOnce(&Collection) -> R) -> Option<R> {
        let (studio, _) = self.engine?;
        studio.collection.with_untracked(|c| c.as_ref().map(f))
    }

    /// `show` as the panel draws it, with its lock.
    pub fn view_of(&self, show: &ShowInfo) -> Option<ShowView> {
        let locked = self
            .locked
            .with_untracked(|l| l.contains(show.show_id.as_str()));
        self.collection(|c| {
            let mut view = wire::view_of(show, c);
            view.locked = locked;
            view
        })
    }

    /// Sends `show` as one `show.set`, unless it is the show the engine has.
    pub fn commit(&self, show: ShowInfo) {
        if self.info(show.show_id.as_str()).as_ref() == Some(&show) {
            return;
        }
        // The engine runs the same check and would refuse the whole edit; say why here.
        if let Some((field, why)) = show.problem() {
            self.refuse(format!(
                "That change would break the show ({field}: {why})."
            ));
            return;
        }
        self.apply(vec![Edit::ShowSet { show }]);
    }

    /// Sends `edits` as one batch, so one undo step.
    pub fn apply(&self, edits: Vec<Edit>) {
        let Some((studio, session)) = self.engine else {
            return;
        };
        if let Some(session) = session.get() {
            studio.apply(&session, edits);
        }
    }

    /// A new ID for something in the collection, with this prefix.
    pub fn fresh_id(&self, prefix: &str) -> Option<String> {
        let (studio, session) = self.engine?;
        let session = session.get()?;
        Some(studio.fresh_id(&session, prefix))
    }

    /// Shows `text` in the problem bar.
    pub fn refuse(&self, text: impl Into<String>) {
        self.problem.set(Some(text.into()));
    }

    /// The names of every show.
    fn names(&self) -> Vec<String> {
        self.collection(|c| c.shows.iter().map(|s| s.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Makes a new, empty show, "Show N", and opens it, with its name ready to type.
    pub fn new_show(&self) {
        let Some(id) = self.fresh_id("show") else {
            return;
        };
        let names = self.names();
        let taken: Vec<&str> = names.iter().map(String::as_str).collect();
        let show = edits::new_show(&id, &edits::new_show_name(&taken));
        self.apply(vec![Edit::ShowSet { show }]);
        self.open.set(Some(id));
        self.mode.set(Mode::Edit);
        self.renaming.set(true);
    }

    /// Copies show `id` as a new show, "… copy", and opens it.
    pub fn duplicate_show(&self, id: &str) {
        let (Some(show), Some(new_id)) = (self.info(id), self.fresh_id("show")) else {
            return;
        };
        let names = self.names();
        let taken: Vec<&str> = names.iter().map(String::as_str).collect();
        let copy = edits::duplicate(&show, &new_id, &edits::copy_name(&show.name, &taken));
        self.apply(vec![Edit::ShowSet { show: copy }]);
        self.open.set(Some(new_id));
    }

    /// Deletes show `id` (`show.delete`, undoable); the picker falls back to another.
    pub fn delete_show(&self, id: &str) {
        if self.info(id).is_none() {
            return;
        }
        self.apply(vec![Edit::ShowDelete { show_id: id.into() }]);
        self.open.set(None);
        self.selected.set(None);
    }

    /// Renames show `id`; an empty name is no change.
    pub fn rename_show(&self, id: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.set_show(id, |s| name.clone_into(&mut s.name));
    }
}

/// The status at the bar's right end: "Paused on Main · 2:14", "Playing", "Stopped".
pub fn status(show: &ShowView, run: &RunView, now: f64) -> String {
    match &run.phase {
        RunPhase::Stopped => "Stopped".to_owned(),
        RunPhase::Playing => match &run.looping {
            Some((id, pass)) => {
                let name = show
                    .loops
                    .iter()
                    .find(|l| &l.id == id)
                    .map_or("loop", |l| l.name.as_str());
                format!("Looping {name} · pass {pass}")
            }
            None => "Playing".to_owned(),
        },
        RunPhase::Transition => "In transition".to_owned(),
        RunPhase::Paused { .. } if run.off_script.is_some() => format!(
            "Off script: {} is on program",
            run.off_script.as_deref().unwrap_or_default()
        ),
        RunPhase::Paused { bookmark, since } => {
            let waited = (now - since).max(0.0) as u32;
            match show.bookmarks.iter().find(|b| &b.id == bookmark) {
                Some(b) => format!("Paused at {} · {}", b.name, time::clock(waited)),
                // `show.pause` holds the play head anywhere.
                None => format!(
                    "Paused at {} · {}",
                    time::clock(run.at_ms),
                    time::clock(waited)
                ),
            }
        }
    }
}

/// The Show panel.
#[component]
pub fn Show() -> impl IntoView {
    let panel = use_context::<ShowPanel>().unwrap_or_else(|| {
        match (use_context::<Studio>(), use_context::<SessionContext>()) {
            (Some(studio), Some(session)) => ShowPanel::connected(studio, session),
            (studio, _) => {
                ShowPanel::new(studio.map_or_else(|| RwSignal::new(None), |s| s.problem))
            }
        }
    });
    provide_context(panel);
    if let Ok(handle) = set_interval_with_handle(
        move || {
            panel.clock.try_set(crate::store::now_ms());
        },
        std::time::Duration::from_millis(33),
    ) {
        on_cleanup(move || handle.clear());
    }
    let mode = panel.mode;
    let has_show = Memo::new(move |_| panel.shows.with(|s| !s.is_empty()));
    view! {
        <div class="show-panel" class:run=move || mode.get() == Mode::Run>
            {move || match has_show.get() {
                false => view! {
                    <div class="empty-state">
                        <crate::icons::Icon name="flag" size=18 />
                        <p class="empty-title">"No show yet"</p>
                        <p class="empty-note">
                            "A show puts scenes on program in order, with bookmarks that wait for Continue."
                        </p>
                        <button type="button" class="primary"
                            on:click=move |_| panel.new_show()>"+ New show"</button>
                    </div>
                }.into_any(),
                true => match mode.get() {
                    Mode::Edit => view! { <edit::EditView /> }.into_any(),
                    Mode::Run => view! { <run::RunView /> }.into_any(),
                },
            }}
        </div>
    }
}

/// The Edit / Run switch, at the left of either view's bar beside the show picker, and
/// the picker's menu: New show…, Duplicate, Rename, Delete. Renaming turns the picker
/// into a name field: Enter or leaving it keeps the name, Escape doesn't.
#[component]
pub fn ModeSwitch() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let menus = use_context::<crate::menu::Menus>();
    let pick = move |m: Mode| move |_| panel.mode.set(m);
    // The show named open, even before the engine's answer brings a new one in.
    let open_id = move || {
        panel
            .open
            .get_untracked()
            .or_else(|| panel.show().map(|s| s.id))
            .unwrap_or_default()
    };
    let show_menu = move |e: leptos::ev::MouseEvent| {
        use crate::menu::Entry;
        let Some(menus) = menus else { return };
        let id = open_id();
        let (dup, del) = (id.clone(), id);
        menus.open(
            &e,
            vec![
                Entry::item("New show…", move || panel.new_show()),
                Entry::item("Duplicate", move || panel.duplicate_show(&dup)),
                Entry::item("Rename", move || panel.renaming.set(true)),
                Entry::Sep,
                Entry::item("Delete", move || panel.delete_show(&del)).danger(),
            ],
        );
    };
    let field = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if panel.renaming.get() {
            request_animation_frame(move || {
                if let Some(el) = field.get_untracked() {
                    let _ = el.focus();
                    el.select();
                }
            });
        }
    });
    let finish = move |keep: bool| {
        if !panel.renaming.get_untracked() {
            return;
        }
        panel.renaming.set(false);
        if keep && let Some(el) = field.get_untracked() {
            panel.rename_show(&open_id(), &el.value());
        }
    };
    let picker = move || {
        if panel.renaming.get() {
            let name = move || panel.show().map(|s| s.name).unwrap_or_default();
            return view! {
                <input class="show-picker show-name" aria-label="Show name" node_ref=field
                    prop:value=name
                    on:keydown=move |e| match e.key().as_str() {
                        "Enter" => finish(true),
                        "Escape" => finish(false),
                        _ => {}
                    }
                    on:blur=move |_| finish(true) />
            }
            .into_any();
        }
        view! {
            <select class="show-picker" aria-label="Show"
                on:change=move |ev| {
                    panel.open.set(Some(event_target_value(&ev)));
                    panel.selected.set(None);
                }>
                {move || {
                    let open = panel.show().map(|s| s.id);
                    panel.shows.get().into_iter().map(|s| {
                        let selected = open.as_deref() == Some(s.id.as_str());
                        view! { <option value=s.id.clone() selected=selected>{s.name.clone()}</option> }
                    }).collect_view()
                }}
            </select>
        }
        .into_any()
    };
    view! {
        <div class="show-picker-group">
            {picker}
            <button type="button" class="show-btn show-picker-menu" title="Shows: new, duplicate, rename, delete"
                aria-label="Show menu" on:click=show_menu>"⋯"</button>
            <div class="segmented" role="group" aria-label="View">
                <button type="button"
                    aria-pressed=move || (panel.mode.get() == Mode::Edit).to_string()
                    on:click=pick(Mode::Edit)>"Edit"</button>
                <button type="button"
                    aria-pressed=move || (panel.mode.get() == Mode::Run).to_string()
                    on:click=pick(Mode::Run)>"Run"</button>
            </div>
        </div>
    }
}

/// Whether the Next monitor shows beside the canvas, and how: while the open show runs
/// in the Run view, its Next monitor isn't Off, and studio mode is off. `Some(still)`
/// when it shows.
pub fn next_monitor_shown(studio: Studio) -> Memo<Option<bool>> {
    let panel = use_context::<ShowPanel>();
    Memo::new(move |_| {
        let panel = panel?;
        if studio.studio_mode.get() || panel.mode.get() != Mode::Run || !panel.is_running() {
            return None;
        }
        match panel.show()?.next_monitor {
            model::NextMonitor::Off => None,
            model::NextMonitor::Still => Some(true),
            model::NextMonitor::Live => Some(false),
        }
    })
}

/// The Next monitor beside the canvas: what Continue will put on program, under a label
/// naming it, as one picture (Still) or a live one (Live).
#[component]
pub fn NextPane() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let studio = expect_context::<Studio>();
    let shown = next_monitor_shown(studio);
    let target = Memo::new(move |_| {
        let show = panel.show()?;
        let run = panel.run.get();
        let head = panel.head();
        Some((
            run::next_picture(&show, &run, head),
            run::next(&show, &run, head).title,
        ))
    });
    let scene = Signal::derive(move || {
        target
            .get()
            .and_then(|(scene, _)| scene)
            .map(jstream_control::protocol::SceneId::new)
    });
    let label = move || {
        let title = target.get().map(|(_, title)| title).unwrap_or_default();
        let still = shown.get() == Some(true);
        format!(
            "Next on Continue · {title}{}",
            if still {
                " · still picture, not rendered live"
            } else {
                ""
            }
        )
    };
    view! {
        <div class="stage-pane show-next-pane">
            <div class="stage-label show-next-label">{label}</div>
            <section class="region monitor show-next" aria-label="Next on Continue">
                {move || {
                    let still = shown.get() == Some(true);
                    view! {
                        <crate::canvas::monitor::Monitor scene=scene still=still label="show-next" />
                    }
                }}
            </section>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::model::demo;
    use super::*;

    #[test]
    fn status_names_the_pause_and_the_time_waited() {
        let show = demo::friday();
        let run = RunView {
            show: Some(show.id.clone()),
            phase: RunPhase::Paused {
                bookmark: "b3".to_owned(),
                since: 1000.0,
            },
            ..RunView::default()
        };
        assert_eq!(status(&show, &run, 135_000.0), "Paused at Close-up · 2:14");
    }

    #[test]
    fn status_says_when_a_hand_switch_took_the_show_off_script() {
        let show = demo::friday();
        let run = RunView {
            phase: RunPhase::Paused {
                bookmark: String::new(),
                since: 0.0,
            },
            off_script: Some("Interview".to_owned()),
            ..RunView::default()
        };
        assert_eq!(
            status(&show, &run, 0.0),
            "Off script: Interview is on program"
        );
    }

    #[test]
    fn status_names_the_loop_and_its_pass() {
        let show = demo::friday();
        let run = RunView {
            phase: RunPhase::Playing,
            looping: Some(("l1".to_owned(), 3)),
            ..RunView::default()
        };
        assert_eq!(status(&show, &run, 0.0), "Looping Waiting room · pass 3");
    }
}
