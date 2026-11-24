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
pub mod model;
pub mod run;
pub mod time;

use crate::store::Studio;
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
}

impl ShowPanel {
    /// A panel with no shows, reporting problems to `problem`.
    pub fn new(problem: RwSignal<Option<String>>) -> ShowPanel {
        ShowPanel {
            problem,
            shows: RwSignal::new(Vec::new()),
            open: RwSignal::new(None),
            run: RwSignal::new(RunView::default()),
            mode: RwSignal::new(Mode::Edit),
            viewport: RwSignal::new(Viewport::fit(0)),
            clock: RwSignal::new(0.0),
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

    /// Asks the runner to do `command`.
    ///
    /// The engine's `show.*` calls arrive with the show protocol (ST0); until then a
    /// command only says so in the problem bar.
    pub fn send(&self, command: Command) {
        let _ = command;
        self.problem
            .set(Some("This build's engine can't run shows yet.".into()));
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
        let problem = use_context::<Studio>().map_or_else(|| RwSignal::new(None), |s| s.problem);
        ShowPanel::new(problem)
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
    view! {
        <div class="show-panel" class:run=move || mode.get() == Mode::Run>
            {move || match panel.show() {
                None => view! {
                    <div class="show-empty">
                        <p class="show-empty-title">"No show yet"</p>
                        <p class="show-empty-hint">
                            "A show puts scenes on program in order, with bookmarks that wait for Continue."
                        </p>
                    </div>
                }.into_any(),
                Some(_) => match mode.get() {
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
