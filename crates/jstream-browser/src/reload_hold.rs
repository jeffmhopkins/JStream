//! A page's reload hold in the helper (#1151): from a reload until the reloaded document
//! paints for real, the helper sends the engine none of the page's paints.
//!
//! Chromium's first paint after a new document starts loading (`OnLoadStart`) can be its
//! blank surface, opaque white over the whole page, `background: transparent` or not.
//! On Linux about one reload in seven painted that, either before the main frame's load
//! ended (`OnLoadEnd`) or just after it, and it was always the first paint since the
//! load start. So the hold lasts until both have happened: the load ended or failed,
//! and the first paint since the load start was skipped. [`DEADLINE`] after the load
//! start counts as the load's end, so a page whose load never ends (a hung subresource)
//! still shows, from its second paint: its first is still skipped, as it may be the
//! blank even then (a render-blocking stylesheet can delay it past the deadline). A load aborted by another doesn't end it: the load that
//! replaced it does. Only the latest reload's latest load start arms the deadline, so an
//! earlier one's never cuts a later reload's hold short.
//!
//! A hold that ends may have skipped the page's last paint: a static page can paint once
//! and then nothing more on its own. So once a hold ends, the helper asks CEF for a paint
//! [`REPAINT_AFTER`] later unless one went through by then. Not at once: asked for right
//! as the load ends, Chromium paints the blank, white, before the renderer's first frame.
//!
//! Docs: crates/jstream-browser/README.md#purpose, docs/adr/0021-browser-source.md

use std::time::Duration;

/// How long after the reloaded document starts loading the hold stops waiting for the
/// load to end. It still waits for the first paint since the load start.
pub(crate) const DEADLINE: Duration = Duration::from_millis(500);

/// How long after a hold ends the helper asks for a paint, if none went through by then.
pub(crate) const REPAINT_AFTER: Duration = Duration::from_millis(100);

/// What ending a hold leaves to do: [`ReloadHold::needs_repaint`] with it
/// [`REPAINT_AFTER`] later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Released {
    paints: u64,
    /// The hold's last reload or load start: a later one's hold is not this one's.
    armed: u64,
}

/// What becomes of one paint of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Paint {
    /// It goes to the engine.
    Send,
    /// It is skipped: the page is held.
    Skip,
    /// It is skipped, and the hold ends with it.
    SkipLast(Released),
}

/// Whether a page's paints are held after a reload.
#[derive(Debug, Default)]
pub(crate) struct ReloadHold {
    held: bool,
    /// A load start came since the reload: paints before it are the old document's.
    started: bool,
    /// The main frame's load ended or failed since the last load start.
    finished: bool,
    /// The first paint since the last load start was skipped.
    skipped: bool,
    /// Counts reloads and load starts: a deadline releases the hold only if nothing came
    /// after the start that armed it.
    armed: u64,
    /// Paints that went to the engine.
    paints: u64,
}

impl ReloadHold {
    /// The page is reloading: its paints are held from now on.
    pub(crate) fn reload(&mut self) {
        self.held = true;
        self.started = false;
        self.finished = false;
        self.skipped = false;
        self.armed = self.armed.wrapping_add(1);
    }

    /// The main frame started loading a document. While held, returns the token of the
    /// deadline this start arms, to be passed to [`Self::deadline`] [`DEADLINE`] later.
    pub(crate) fn load_started(&mut self) -> Option<u64> {
        if !self.held {
            return None;
        }
        self.started = true;
        self.finished = false;
        self.skipped = false;
        self.armed = self.armed.wrapping_add(1);
        Some(self.armed)
    }

    /// The deadline armed with `token` passed: if no reload or load start came since, it
    /// counts as the load's end. Returns the release if that ended the hold: it does once
    /// the first paint since the load start was skipped.
    pub(crate) fn deadline(&mut self, token: u64) -> Option<Released> {
        if self.held && token == self.armed {
            self.load_finished()
        } else {
            None
        }
    }

    /// The main frame's load ended or failed. Returns the release if that ended the hold:
    /// it does once the first paint since the load start was skipped.
    pub(crate) fn load_finished(&mut self) -> Option<Released> {
        if !self.held {
            return None;
        }
        self.finished = true;
        if self.skipped { self.release() } else { None }
    }

    /// The page painted: whether the paint goes to the engine.
    pub(crate) fn paint(&mut self) -> Paint {
        if !self.held {
            self.paints = self.paints.wrapping_add(1);
            return Paint::Send;
        }
        if self.started {
            self.skipped = true;
        }
        if self.finished && self.skipped {
            self.release().map_or(Paint::Skip, Paint::SkipLast)
        } else {
            Paint::Skip
        }
    }

    /// [`REPAINT_AFTER`] after `released`: whether to ask CEF for a paint, because none
    /// went through since and no new hold began.
    pub(crate) fn needs_repaint(&self, released: Released) -> bool {
        !self.held && self.armed == released.armed && self.paints == released.paints
    }

    fn release(&mut self) -> Option<Released> {
        std::mem::take(&mut self.held).then_some(Released {
            paints: self.paints,
            armed: self.armed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_that_never_reloaded_paints() {
        let mut hold = ReloadHold::default();
        // Its first load arms nothing and releases nothing.
        assert_eq!(hold.load_started(), None);
        assert_eq!(hold.paint(), Paint::Send);
        assert_eq!(hold.load_finished(), None);
        assert_eq!(hold.paint(), Paint::Send);
    }

    #[test]
    fn the_deadline_is_long_past_the_white_paint_and_short_of_a_noticeable_gap() {
        // Chromium's white paint came within milliseconds of the load start (#1151).
        assert!(DEADLINE >= Duration::from_millis(100));
        assert!(DEADLINE <= Duration::from_secs(1));
        assert!(REPAINT_AFTER < DEADLINE);
    }

    #[test]
    fn a_white_paint_before_the_load_end_is_skipped() {
        let mut hold = ReloadHold::default();
        hold.reload();
        assert_eq!(hold.paint(), Paint::Skip, "the old document's");
        hold.load_started().expect("held");
        assert_eq!(
            hold.paint(),
            Paint::Skip,
            "the new one's first, maybe white"
        );
        assert!(
            hold.load_finished().is_some(),
            "the load end releases the hold"
        );
        assert_eq!(hold.paint(), Paint::Send);
    }

    #[test]
    fn a_white_paint_after_the_load_end_is_skipped() {
        let mut hold = ReloadHold::default();
        hold.reload();
        hold.load_started().expect("held");
        assert_eq!(
            hold.load_finished(),
            None,
            "no paint of the new document yet"
        );
        assert!(
            matches!(hold.paint(), Paint::SkipLast(_)),
            "its first, maybe white, ends the hold"
        );
        assert_eq!(hold.paint(), Paint::Send);
    }

    #[test]
    fn the_old_document_s_paints_are_not_the_new_one_s_first() {
        let mut hold = ReloadHold::default();
        hold.reload();
        assert_eq!(hold.paint(), Paint::Skip);
        assert_eq!(hold.paint(), Paint::Skip);
        hold.load_started().expect("held");
        assert_eq!(hold.load_finished(), None);
    }

    #[test]
    fn a_load_that_never_ends_is_released_by_its_deadline() {
        let mut hold = ReloadHold::default();
        hold.reload();
        let token = hold.load_started().expect("held");
        assert_eq!(hold.paint(), Paint::Skip);
        assert!(hold.deadline(token).is_some());
        assert_eq!(hold.paint(), Paint::Send);
        assert_eq!(
            hold.load_finished(),
            None,
            "a late load end releases nothing more"
        );
    }

    #[test]
    fn a_deadline_before_any_paint_still_skips_the_first() {
        let mut hold = ReloadHold::default();
        hold.reload();
        let token = hold.load_started().expect("held");
        // A render-blocking stylesheet keeps the page from painting past the deadline.
        assert_eq!(hold.deadline(token), None, "still held");
        assert!(
            matches!(hold.paint(), Paint::SkipLast(_)),
            "its first paint, maybe the blank, is skipped and ends the hold"
        );
        assert_eq!(hold.paint(), Paint::Send);
    }

    #[test]
    fn a_reload_during_a_reload_holds_until_a_load_finishes() {
        let mut hold = ReloadHold::default();
        hold.reload();
        hold.load_started().expect("held");
        assert_eq!(hold.paint(), Paint::Skip);
        // Reloaded again: the first load is aborted, and its paint no longer counts.
        hold.reload();
        assert_eq!(hold.load_finished(), None);
        hold.load_started().expect("held");
        assert_eq!(hold.paint(), Paint::Skip);
        assert!(hold.load_finished().is_some());
        assert_eq!(hold.paint(), Paint::Send);
    }

    #[test]
    fn only_the_latest_load_start_s_deadline_releases() {
        let mut hold = ReloadHold::default();
        hold.reload();
        let first = hold.load_started().expect("held");
        // A second reload, and its load start, before the first deadline.
        hold.reload();
        assert_eq!(hold.deadline(first), None, "a reload since disarms it");
        let second = hold.load_started().expect("held");
        assert_eq!(hold.deadline(first), None);
        assert_eq!(hold.paint(), Paint::Skip);
        assert!(hold.deadline(second).is_some());
        assert_eq!(hold.paint(), Paint::Send);
    }

    #[test]
    fn a_load_start_since_disarms_a_deadline() {
        let mut hold = ReloadHold::default();
        hold.reload();
        let aborted = hold.load_started().expect("held");
        // The load was aborted and replaced (a redirect) before its deadline.
        let replaced = hold.load_started().expect("held");
        assert_eq!(hold.paint(), Paint::Skip);
        assert_eq!(hold.deadline(aborted), None);
        assert!(hold.deadline(replaced).is_some());
    }

    #[test]
    fn a_released_page_that_paints_nothing_more_is_repainted() {
        let mut hold = ReloadHold::default();
        assert_eq!(hold.paint(), Paint::Send);
        hold.reload();
        hold.load_started().expect("held");
        assert_eq!(hold.load_finished(), None);
        // A static page's one paint ends the hold, skipped: nothing else comes.
        let Paint::SkipLast(released) = hold.paint() else {
            panic!("the hold ends");
        };
        assert!(hold.needs_repaint(released));
    }

    #[test]
    fn a_released_page_that_painted_since_is_not_repainted() {
        let mut hold = ReloadHold::default();
        hold.reload();
        hold.load_started().expect("held");
        assert_eq!(hold.paint(), Paint::Skip);
        let released = hold.load_finished().expect("released");
        assert_eq!(hold.paint(), Paint::Send);
        assert!(!hold.needs_repaint(released));
    }

    #[test]
    fn an_earlier_hold_s_repaint_is_not_a_later_one_s() {
        let mut hold = ReloadHold::default();
        hold.reload();
        hold.load_started().expect("held");
        assert_eq!(hold.load_finished(), None);
        let Paint::SkipLast(first) = hold.paint() else {
            panic!("the hold ends");
        };
        // Reloaded again before the first repaint is due, and released again.
        hold.reload();
        hold.load_started().expect("held");
        assert_eq!(hold.paint(), Paint::Skip);
        let second = hold.load_finished().expect("released");
        assert!(
            !hold.needs_repaint(first),
            "it may fire right at the second load end"
        );
        assert!(hold.needs_repaint(second));
    }

    #[test]
    fn a_page_reloading_again_is_not_repainted() {
        let mut hold = ReloadHold::default();
        hold.reload();
        hold.load_started().expect("held");
        assert_eq!(hold.paint(), Paint::Skip);
        let released = hold.load_finished().expect("released");
        hold.reload();
        assert!(
            !hold.needs_repaint(released),
            "it would paint the next document's blank"
        );
    }
}
