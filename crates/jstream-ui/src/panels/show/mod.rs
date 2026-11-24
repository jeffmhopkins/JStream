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
pub mod run;
pub mod time;
pub mod wire;

use crate::app::SessionContext;
use crate::store::Studio;
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
    /// Continue: play on from a pause, or to the next pausing bookmark (`show.continue`).
    Continue,
    /// Back to the previous pausing bookmark.
    Back,
    /// Jump to a bookmark, by ID, at once.
    GoTo(String),
    /// Arm a spot, in show time, for the next Continue (a ruler or strip click while the
    /// show runs). `None` disarms.
    Arm(Option<u32>),
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
        }
    }

    /// The open show: the one named, else the running one, else the first.
    pub fn show(&self) -> Option<ShowView> {
        let open = self.open.get();
        let running = self.run.with(|r| r.show.clone());
        self.shows.with(|shows| {
            open.or(running)
                .and_then(|id| shows.iter().find(|s| s.id == id).cloned())
                .or_else(|| shows.first().cloned())
        })
    }

    /// Whether the open show is the one running.
    pub fn is_running(&self) -> bool {
        let id = self.show().map(|s| s.id);
        self.run.with(|r| r.show.is_some() && r.show == id)
    }

    /// The play head now.
    pub fn head(&self) -> u32 {
        let now = self.clock.get();
        self.run.with(|r| r.head_at(now))
    }

    /// A panel that follows the studio's shows and running show, and sends its commands
    /// through `session`.
    pub fn connected(studio: Studio, session: SessionContext) -> ShowPanel {
        let panel = ShowPanel {
            engine: Some((studio, session)),
            ..ShowPanel::new(studio.problem)
        };
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
    }

    /// Changes show `id` with `f` and sends it as one `show.set`, so one undo step. A
    /// change that changes nothing sends nothing.
    pub fn set_show(&self, id: &str, f: impl FnOnce(&mut ShowInfo)) {
        let Some((studio, session)) = self.engine else {
            return;
        };
        let Some(before) = studio.collection.with_untracked(|c| {
            c.as_ref()
                .and_then(|c| c.shows.iter().find(|s| s.show_id.as_str() == id).cloned())
        }) else {
            return;
        };
        let mut show = before.clone();
        f(&mut show);
        if show == before {
            return;
        }
        if let Some(session) = session.get() {
            studio.apply(&session, vec![Edit::ShowSet { show }]);
        }
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
        RunPhase::Paused { bookmark, since } => {
            let name = show
                .bookmarks
                .iter()
                .find(|b| &b.id == bookmark)
                .map_or("a bookmark", |b| b.name.as_str());
            let waited = (now - since).max(0.0) as u32;
            format!("Paused at {name} · {}", time::clock(waited))
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
                    <div class="show-empty">
                        <p class="show-empty-title">"No show yet"</p>
                        <p class="show-empty-hint">
                            "A show puts scenes on program in order, with bookmarks that wait for Continue."
                        </p>
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

/// The Edit / Run switch, at the left of either view's bar beside the show picker.
#[component]
pub fn ModeSwitch() -> impl IntoView {
    let panel = expect_context::<ShowPanel>();
    let pick = move |m: Mode| move |_| panel.mode.set(m);
    view! {
        <div class="show-picker-group">
            <select class="show-picker" aria-label="Show"
                on:change=move |ev| panel.open.set(Some(event_target_value(&ev)))>
                {move || {
                    let open = panel.show().map(|s| s.id);
                    panel.shows.get().into_iter().map(|s| {
                        let selected = open.as_deref() == Some(s.id.as_str());
                        view! { <option value=s.id.clone() selected=selected>{s.name.clone()}</option> }
                    }).collect_view()
                }}
            </select>
            <div class="show-mode" role="group" aria-label="View">
                <button type="button" class:on=move || panel.mode.get() == Mode::Edit
                    aria-pressed=move || (panel.mode.get() == Mode::Edit).to_string()
                    on:click=pick(Mode::Edit)>"Edit"</button>
                <button type="button" class:on=move || panel.mode.get() == Mode::Run
                    aria-pressed=move || (panel.mode.get() == Mode::Run).to_string()
                    on:click=pick(Mode::Run)>"Run"</button>
            </div>
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
