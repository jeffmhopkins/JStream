//! What the page's views of preview streams did with their pictures, reported to the
//! engine (`preview.report`, protocol 0.103, ADR-0132) so `profile.get` and the bench
//! see what a person saw: the canvas and each projector picture keep a [`ViewTally`],
//! and [`report_every_second`] sends its running totals while they change.
//!
//! Docs: crates/jstream-ui/README.md#views, docs/adr/0132-bench-gates-views-and-quality-probe.md

use jstream_control::Control;
use jstream_control::protocol::{Action, Call, MAX_VIEW_LABEL_CHARS, ViewCounts};
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Duration;

/// How often a changed tally is reported.
pub const REPORT_EVERY: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
struct Tally {
    counts: ViewCounts,
    /// A stream has been opened for the view, so the next is a reopen.
    opened: bool,
    /// The counts last reported.
    reported: Option<ViewCounts>,
}

/// One view's running totals, shared by what draws it and what reports it.
#[derive(Clone, Debug, Default)]
pub struct ViewTally(Rc<RefCell<Tally>>);

impl ViewTally {
    fn add(&self, count: impl FnOnce(&mut ViewCounts)) {
        let mut tally = self.0.borrow_mut();
        count(&mut tally.counts);
    }

    /// A preview frame arrived for the view.
    pub fn received(&self) {
        self.add(|c| c.received = c.received.saturating_add(1));
    }

    /// A picture was drawn.
    pub fn shown(&self) {
        self.add(|c| c.shown = c.shown.saturating_add(1));
    }

    /// A frame was left undecoded, to catch up.
    pub fn skipped(&self) {
        self.add(|c| c.skipped = c.skipped.saturating_add(1));
    }

    /// The browser refused a frame or a decoder.
    pub fn decode_error(&self) {
        self.add(|c| c.decode_errors = c.decode_errors.saturating_add(1));
    }

    /// The view showed its waiting text where a picture belongs.
    pub fn placeholder(&self) {
        self.add(|c| c.placeholders = c.placeholders.saturating_add(1));
    }

    /// The view's stream ended or failed, and it showed why.
    pub fn blank(&self) {
        self.add(|c| c.blanks = c.blanks.saturating_add(1));
    }

    /// A stream was opened for the view: every one after the first is a reopen.
    pub fn opened(&self) {
        let mut tally = self.0.borrow_mut();
        if tally.opened {
            tally.counts.reopens = tally.counts.reopens.saturating_add(1);
        }
        tally.opened = true;
    }

    /// The totals so far.
    pub fn counts(&self) -> ViewCounts {
        self.0.borrow().counts
    }

    /// The report to send now for the view `view`: its totals, unless they are what
    /// was last reported. Taking it counts it as reported.
    pub fn due(&self, view: &str) -> Option<Action> {
        let mut tally = self.0.borrow_mut();
        if tally.reported == Some(tally.counts) {
            return None;
        }
        tally.reported = Some(tally.counts);
        Some(Action::PreviewReport {
            view: view.to_owned(),
            counts: tally.counts,
        })
    }
}

/// A view's label: `kind`, then `:` and `name` when there is one, without control
/// characters, cut to [`MAX_VIEW_LABEL_CHARS`].
pub fn label(kind: &str, name: Option<&str>) -> String {
    let full = match name {
        Some(name) => format!("{kind}:{name}"),
        None => kind.to_owned(),
    };
    full.chars()
        .filter(|c| !c.is_control())
        .take(MAX_VIEW_LABEL_CHARS)
        .collect()
}

/// Sends `tally`'s report for `view` on `control` every [`REPORT_EVERY`] while it
/// changes, until the returned handle is cleared or the session is gone. A refused
/// report (a session past its views' limit) is dropped: the view only goes unreported.
pub fn report_every_second(
    control: Weak<dyn Control>,
    view: String,
    tally: ViewTally,
) -> Option<leptos::prelude::IntervalHandle> {
    let send = move || {
        let Some(control) = control.upgrade() else {
            return;
        };
        if let Some(action) = tally.due(&view) {
            leptos::task::spawn_local(async move {
                let _reported = control.call(Call::Action { action }).await;
            });
        }
    };
    leptos::prelude::set_interval_with_handle(send, REPORT_EVERY).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tally_reports_only_what_changed_and_counts_reopens_after_the_first() {
        let tally = ViewTally::default();
        tally.opened();
        tally.placeholder();
        tally.received();
        tally.shown();
        let Some(Action::PreviewReport { view, counts }) = tally.due("canvas") else {
            panic!("a report is due");
        };
        assert_eq!(view, "canvas");
        assert_eq!(
            counts,
            ViewCounts {
                received: 1,
                shown: 1,
                placeholders: 1,
                ..ViewCounts::default()
            }
        );
        assert_eq!(tally.due("canvas"), None, "nothing changed");
        tally.opened();
        tally.skipped();
        tally.decode_error();
        tally.blank();
        let Some(Action::PreviewReport { counts, .. }) = tally.due("canvas") else {
            panic!("a report is due");
        };
        assert_eq!(
            (
                counts.reopens,
                counts.skipped,
                counts.decode_errors,
                counts.blanks
            ),
            (1, 1, 1, 1)
        );
    }

    #[test]
    fn a_label_is_cut_and_keeps_no_control_characters() {
        assert_eq!(label("canvas", None), "canvas");
        assert_eq!(label("projector", Some("a\nb")), "projector:ab");
        let long = label("projector", Some(&"é".repeat(100)));
        assert_eq!(long.chars().count(), MAX_VIEW_LABEL_CHARS);
        assert!(jstream_control::protocol::check_view_label(&long).is_ok());
    }
}
