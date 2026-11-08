//! What the clients' views did with their preview pictures (`preview.report`, protocol
//! 0.103, ADR-0132): the latest running totals each session reported per view, read
//! into each profile sample like the streams' totals, and a window's growth.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jstream_protocol::{
    ClientId, MAX_PROFILE_SECONDS, MAX_SESSION_VIEWS, ProtocolError, ViewCounts, ViewProfile,
};

use crate::lock;

use super::finite;

/// A view, by the session that reported it and its label.
pub(crate) type ViewKey = (ClientId, String);

/// One view's latest report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ViewReading {
    /// The session's client name.
    client: String,
    /// Which run of the view: a report whose counts fall below the last starts the
    /// next, whose counts start from zero.
    run: u64,
    counts: ViewCounts,
    /// When it was last reported.
    at: Instant,
}

/// A view's entry, and when its session ended, if it has.
#[derive(Clone, Debug)]
struct ViewEntry {
    reading: ViewReading,
    ended: Option<Instant>,
}

/// The views every session has reported. Its lock is its own; the control side takes it
/// on a report and on a sample, never with another lock held.
#[derive(Default)]
pub(crate) struct ViewMeters {
    views: Mutex<BTreeMap<ViewKey, ViewEntry>>,
}

impl ViewMeters {
    /// Records `counts` for the view `view` of the session `client_id`, whose client is
    /// named `client`.
    ///
    /// # Errors
    /// `invalid_argument` on `view` when the session already reports
    /// [`MAX_SESSION_VIEWS`] others.
    pub(crate) fn report(
        &self,
        client_id: &ClientId,
        client: &str,
        view: &str,
        counts: ViewCounts,
    ) -> Result<(), ProtocolError> {
        let now = Instant::now();
        let mut views = lock(&self.views);
        let key = (client_id.clone(), view.to_owned());
        match views.get_mut(&key) {
            Some(entry) => {
                let reading = &mut entry.reading;
                if !counts.follows(&reading.counts) {
                    reading.run = reading.run.saturating_add(1);
                }
                reading.counts = counts;
                reading.at = now;
                client.clone_into(&mut reading.client);
                entry.ended = None;
            }
            None => {
                let open = views
                    .iter()
                    .filter(|((session, _), entry)| session == client_id && entry.ended.is_none())
                    .count();
                if open >= MAX_SESSION_VIEWS {
                    return Err(ProtocolError::invalid_argument(
                        "view",
                        format!("a session reports at most {MAX_SESSION_VIEWS} views"),
                    ));
                }
                views.insert(
                    key,
                    ViewEntry {
                        reading: ViewReading {
                            client: client.to_owned(),
                            run: 0,
                            counts,
                            at: now,
                        },
                        ended: None,
                    },
                );
            }
        }
        Ok(())
    }

    /// Marks the session's views ended: they stay in the windows that reach back to
    /// their last report, and are forgotten after that.
    pub(crate) fn end_session(&self, client_id: &ClientId) {
        let now = Instant::now();
        for ((session, _), entry) in lock(&self.views).iter_mut() {
            if session == client_id {
                entry.ended.get_or_insert(now);
            }
        }
    }

    /// Every view's latest reading, forgetting the ended views no window reaches.
    pub(crate) fn read(&self) -> BTreeMap<ViewKey, ViewReading> {
        let keep_for = Duration::from_secs(u64::from(MAX_PROFILE_SECONDS) + 1);
        let now = Instant::now();
        let mut views = lock(&self.views);
        views.retain(|_, entry| {
            entry
                .ended
                .is_none_or(|_| now.saturating_duration_since(entry.reading.at) <= keep_for)
        });
        views
            .iter()
            .map(|(key, entry)| (key.clone(), entry.reading.clone()))
            .collect()
    }
}

/// The views reported in the window from `since` to `now`, `span` long: each view's
/// growth since `before` (from zero for a view or run `before` doesn't have), in label
/// order per client.
pub(crate) fn view_profiles(
    now: &BTreeMap<ViewKey, ViewReading>,
    before: Option<&BTreeMap<ViewKey, ViewReading>>,
    since: Instant,
    span: Duration,
) -> Vec<ViewProfile> {
    let seconds = span.as_secs_f32();
    let rate = |count: u64| {
        if seconds > 0.0 {
            finite(count as f32 / seconds)
        } else {
            finite(0.0)
        }
    };
    let mut out: Vec<ViewProfile> = now
        .iter()
        .filter(|(_, reading)| reading.at > since)
        .map(|(key, reading)| {
            let earlier = before
                .and_then(|before| before.get(key))
                .filter(|earlier| earlier.run == reading.run)
                .map(|earlier| earlier.counts)
                .unwrap_or_default();
            let counts = reading.counts.since(&earlier);
            ViewProfile {
                client: reading.client.clone(),
                view: key.1.clone(),
                received_fps: rate(counts.received),
                shown_fps: rate(counts.shown),
                counts,
            }
        })
        .collect();
    out.sort_by(|a, b| (&a.client, &a.view).cmp(&(&b.client, &b.view)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(received: u64, placeholders: u64) -> ViewCounts {
        ViewCounts {
            received,
            shown: received,
            placeholders,
            ..ViewCounts::default()
        }
    }

    #[test]
    fn a_window_is_the_growth_since_its_base_and_a_restart_counts_from_zero() {
        let meters = ViewMeters::default();
        let a = ClientId::new("client_1");
        let start = Instant::now();
        meters
            .report(&a, "ui", "canvas", counts(30, 1))
            .expect("reported");
        meters
            .report(&a, "ui", "projector", counts(10, 1))
            .expect("reported");
        let base = meters.read();
        meters
            .report(&a, "ui", "canvas", counts(90, 2))
            .expect("reported");
        // A reloaded page: its counts start again.
        meters
            .report(&a, "ui", "projector", counts(4, 1))
            .expect("reported");
        let now = meters.read();
        let views = view_profiles(&now, Some(&base), start, Duration::from_secs(2));
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].view, "canvas");
        assert_eq!(views[0].counts, counts(60, 1));
        assert_eq!(views[0].received_fps.get(), 30.0);
        assert_eq!(views[1].view, "projector");
        assert_eq!(views[1].counts, counts(4, 1));
    }

    #[test]
    fn a_view_not_reported_in_the_window_is_left_out() {
        let meters = ViewMeters::default();
        let a = ClientId::new("client_1");
        meters
            .report(&a, "ui", "canvas", counts(30, 1))
            .expect("reported");
        let after = Instant::now() + Duration::from_millis(1);
        let views = view_profiles(&meters.read(), None, after, Duration::from_secs(1));
        assert!(views.is_empty());
    }

    #[test]
    fn a_session_reports_at_most_sixteen_views() {
        let meters = ViewMeters::default();
        let a = ClientId::new("client_1");
        for i in 0..MAX_SESSION_VIEWS {
            meters
                .report(&a, "ui", &format!("v{i}"), ViewCounts::default())
                .expect("within the limit");
        }
        assert!(
            meters
                .report(&a, "ui", "one more", ViewCounts::default())
                .is_err()
        );
        // A known view still reports, and another session has its own limit.
        meters
            .report(&a, "ui", "v0", counts(1, 0))
            .expect("a known view");
        let b = ClientId::new("client_2");
        meters
            .report(&b, "ui", "canvas", ViewCounts::default())
            .expect("its own");
    }

    #[test]
    fn an_ended_session_stays_while_a_window_reaches_it() {
        let meters = ViewMeters::default();
        let a = ClientId::new("client_1");
        let start = Instant::now();
        meters
            .report(&a, "ui", "canvas", counts(5, 1))
            .expect("reported");
        meters.end_session(&a);
        let views = view_profiles(&meters.read(), None, start, Duration::from_secs(1));
        assert_eq!(views.len(), 1);
    }
}
