//! The Agent activity panel (AG-01, ADR-0083): what agents connected to the engine did,
//! newest first, as the engine's activity log (`activity.list`) says. Each row is the
//! client's name and how long ago, in mono, over the engine's sentence for the call.
//!
//! Calls from JStream's own UIs (this one, and paired browsers) are left out: they are
//! the user's own. The panel asks for new entries when it opens, after each event that
//! most calls send, and every 5 seconds (`POLL`), which catches failed calls and quiet
//! actions.
//!
//! Its Clear button empties the list shown and nothing else: the engine's activity log,
//! undo history and audit log are untouched, and the panel goes on showing only what
//! agents do after it. The mark lives for the workspace (`ActivityCleared`), so switching
//! tabs or moving the panel keeps the list cleared; reconnecting shows the log again.
//!
//! Docs: docs/adr/0083-activity-log-over-the-protocol.md, docs/UI-FEATURES.md#live-panels

use crate::app::SessionContext;
use crate::session::CLIENT_NAME;
use crate::store::query;
use futures_util::StreamExt;
use jstream_protocol::{ActivityEntry, Event, MAX_ACTIVITY_ENTRIES, Query, QueryResult};
use leptos::prelude::*;
use leptos::task::spawn_local;
use std::time::Duration;

/// How often the panel asks for new entries while it is shown.
const POLL: Duration = Duration::from_secs(5);

/// The prefix a paired browser's calls carry in the log (ADR-0044).
const DEVICE_PREFIX: &str = "device:";

/// Whether `client_name` is an agent's, not one of JStream's own UIs.
pub fn is_agent(client_name: &str) -> bool {
    client_name != CLIENT_NAME && !client_name.starts_with(DEVICE_PREFIX)
}

/// How long before `now_ms` the entry at `time_ms` was, as the prototype says it
/// ("2 min ago"). Both are the engine's clock.
pub fn ago(now_ms: u64, time_ms: u64) -> String {
    let secs = now_ms.saturating_sub(time_ms) / 1000;
    match secs {
        0..=44 => "just now".to_owned(),
        45..=3599 => format!("{} min ago", (secs + 30) / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// The row's meta line: who, when, and the error code of a failed call.
pub fn meta(entry: &ActivityEntry, now_ms: u64) -> String {
    let mut line = format!("{} · {}", entry.client_name, ago(now_ms, entry.time_ms));
    if let Some(error) = &entry.error {
        line.push_str(" · failed: ");
        line.push_str(error);
    }
    line
}

/// Where the panel's Clear button last left the log: the newest entry's `seq` then
/// (0 before any clear). The panel shows only entries after it. Provided by the
/// workspace so the mark outlives one mounting of the panel.
#[derive(Clone, Copy)]
pub struct ActivityCleared(pub RwSignal<u64>);

impl ActivityCleared {
    /// Nothing cleared yet.
    pub fn new() -> Self {
        ActivityCleared(RwSignal::new(0))
    }
}

impl Default for ActivityCleared {
    fn default() -> Self {
        Self::new()
    }
}

/// `newer` (newest first, as `activity.list` answers) in front of `kept`, without
/// repeats or anything at or before `cleared`, keeping at most [`MAX_ACTIVITY_ENTRIES`].
pub fn merge(
    newer: Vec<ActivityEntry>,
    kept: &[ActivityEntry],
    cleared: u64,
) -> Vec<ActivityEntry> {
    let newest_kept = kept.first().map_or(cleared, |entry| entry.seq.max(cleared));
    let mut merged: Vec<ActivityEntry> = newer
        .into_iter()
        .filter(|entry| entry.seq > newest_kept)
        .collect();
    merged.extend(kept.iter().cloned());
    merged.truncate(MAX_ACTIVITY_ENTRIES as usize);
    merged
}

/// The panel's state, shared by its event loop, its timer and its view.
#[derive(Clone, Copy)]
struct Log {
    session: SessionContext,
    /// The `seq` the Clear button last left the log at.
    cleared: RwSignal<u64>,
    /// Every entry read since the last clear, newest first, agents' or not.
    entries: RwSignal<Vec<ActivityEntry>>,
    /// The engine's clock at the last answer.
    now_ms: RwSignal<u64>,
    problem: RwSignal<Option<String>>,
    /// Whether a read is in flight, and whether another is due after it.
    reading: StoredValue<(bool, bool)>,
}

impl Log {
    /// Reads the entries newer than those kept. Reads requested while one is in flight
    /// coalesce into one more after it.
    fn read(self) {
        let Some((busy, _)) = self.reading.try_get_value() else {
            return;
        };
        if busy {
            self.reading.set_value((true, true));
            return;
        }
        self.reading.set_value((true, false));
        spawn_local(async move {
            loop {
                let Some(session) = self.session.get() else {
                    break;
                };
                let cleared = self.cleared.try_get_untracked().unwrap_or(0);
                let after_seq = self
                    .entries
                    .try_with_untracked(|kept| kept.first().map(|entry| entry.seq))
                    .flatten()
                    .or((cleared > 0).then_some(cleared));
                let asked = Query::ActivityList {
                    after_seq,
                    limit: None,
                };
                match query(&session, asked).await {
                    Ok(QueryResult::ActivityList { now_ms, entries }) => {
                        self.problem.try_set(None);
                        self.now_ms.try_set(now_ms);
                        if !entries.is_empty() {
                            // A clear while this read was out moves the mark: use it now.
                            let cleared = self.cleared.try_get_untracked().unwrap_or(cleared);
                            self.entries
                                .try_update(|kept| *kept = merge(entries, kept, cleared));
                        }
                    }
                    Ok(other) => {
                        self.problem
                            .try_set(Some(format!("Unexpected answer: {}", other.name())));
                    }
                    Err(error) => {
                        self.problem.try_set(Some(error.to_string()));
                    }
                }
                let Some((_, again)) = self.reading.try_get_value() else {
                    break;
                };
                self.reading.set_value((again, false));
                if !again {
                    break;
                }
            }
        });
    }
}

/// The Agent activity panel. Reads the open session from [`SessionContext`] context.
#[component]
pub fn AgentActivity() -> impl IntoView {
    let Some(session) = use_context::<SessionContext>() else {
        return view! { <p class="placeholder">"Not connected."</p> }.into_any();
    };
    let cleared = use_context::<ActivityCleared>().unwrap_or_default().0;
    let log = Log {
        session,
        cleared,
        entries: RwSignal::new(Vec::new()),
        now_ms: RwSignal::new(0),
        problem: RwSignal::new(None),
        reading: StoredValue::new((false, false)),
    };
    let clear = move |_| {
        let newest = log
            .entries
            .with_untracked(|kept| kept.first().map_or(0, |entry| entry.seq));
        log.cleared
            .update(|cleared| *cleared = (*cleared).max(newest));
        log.entries.set(Vec::new());
    };

    if let Some(open) = session.get() {
        let mut events = open.control.events();
        drop(open);
        log.read();
        spawn_local(async move {
            while let Some(event) = events.next().await {
                match event {
                    Event::StateChanged { .. }
                    | Event::StudioChanged { .. }
                    | Event::OutputStateChanged { .. }
                    | Event::LiveEnded { .. }
                    | Event::EventsDropped { .. } => log.read(),
                    _ => {}
                }
            }
        });
        if let Ok(timer) = set_interval_with_handle(move || log.read(), POLL) {
            on_cleanup(move || timer.clear());
        }
    }

    let shown = move || {
        log.entries.with(|entries| {
            entries
                .iter()
                .filter(|entry| is_agent(&entry.client_name))
                .cloned()
                .collect::<Vec<_>>()
        })
    };

    view! {
        <section class="agent-activity">
            <h2>"Agent activity"</h2>
            {move || {
                log.problem
                    .get()
                    .map(|problem| view! { <p class="problem" role="alert">{problem}</p> })
            }}
            {move || {
                let rows = shown();
                if rows.is_empty() {
                    return view! {
                        <p class="placeholder">
                            "Nothing yet. What agents connected over MCP change shows here, newest first."
                        </p>
                    }
                        .into_any();
                }
                let now = log.now_ms.get();
                view! {
                    <div class="activity-bar">
                        <button
                            type="button"
                            class="link"
                            title="Empty this list. The engine's activity log and undo history are kept."
                            on:click=clear
                        >
                            "Clear"
                        </button>
                    </div>
                    <ol class="activity-log">
                        {rows
                            .into_iter()
                            .map(|entry| {
                                let failed = entry.error.is_some();
                                view! {
                                    <li class:failed=failed>
                                        <span class="activity-meta">{meta(&entry, now)}</span>
                                        <span class="activity-summary">{entry.summary.clone()}</span>
                                    </li>
                                }
                            })
                            .collect_view()}
                    </ol>
                }
                    .into_any()
            }}
        </section>
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use super::{ago, is_agent, merge, meta};
    use jstream_protocol::{ActivityEntry, ClientId, MAX_ACTIVITY_ENTRIES};

    fn entry(seq: u64, client_name: &str) -> ActivityEntry {
        ActivityEntry {
            seq,
            time_ms: 1_000_000,
            client_id: ClientId::new("client_3"),
            client_name: client_name.into(),
            call: "scene.switch".into(),
            summary: "Switched to “Desktop + Cam”".into(),
            error: None,
        }
    }

    #[test]
    fn only_agents_are_shown() {
        assert!(is_agent("claude-desktop"));
        assert!(is_agent("jstream-mcp"));
        assert!(!is_agent("jstream-ui"));
        assert!(!is_agent("device:Tablet"));
    }

    #[test]
    fn times_read_like_the_prototype() {
        let at = 10_000_000;
        assert_eq!(ago(at, at), "just now");
        assert_eq!(ago(at, at + 5_000), "just now");
        assert_eq!(ago(at + 44_000, at), "just now");
        assert_eq!(ago(at + 45_000, at), "1 min ago");
        assert_eq!(ago(at + 120_000, at), "2 min ago");
        assert_eq!(ago(at + 4 * 60_000 + 20_000, at), "4 min ago");
        assert_eq!(ago(at + 2 * 3_600_000, at), "2 h ago");
        assert_eq!(ago(at + 3 * 86_400_000, at), "3 d ago");
    }

    #[test]
    fn the_meta_line_names_the_client_and_a_failure() {
        let mut row = entry(1, "claude-desktop");
        assert_eq!(meta(&row, 1_120_000), "claude-desktop · 2 min ago");
        row.error = Some("rev_conflict".into());
        assert_eq!(
            meta(&row, 1_120_000),
            "claude-desktop · 2 min ago · failed: rev_conflict"
        );
    }

    #[test]
    fn newer_entries_go_in_front_once_and_the_list_stays_bounded() {
        let kept = vec![entry(2, "a"), entry(1, "a")];
        let merged = merge(vec![entry(4, "a"), entry(3, "a"), entry(2, "a")], &kept, 0);
        let seqs: Vec<u64> = merged.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [4, 3, 2, 1]);

        let many: Vec<ActivityEntry> = (1..=150).rev().map(|n| entry(n, "a")).collect();
        let bounded = merge(many, &[], 0);
        assert_eq!(bounded.len(), MAX_ACTIVITY_ENTRIES as usize);
        assert_eq!(bounded[0].seq, 150);
    }

    #[test]
    fn a_cleared_list_takes_only_entries_after_the_clear() {
        let newer = vec![entry(7, "a"), entry(6, "a"), entry(5, "a"), entry(4, "a")];
        let merged = merge(newer, &[], 5);
        let seqs: Vec<u64> = merged.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [7, 6]);
    }
}
