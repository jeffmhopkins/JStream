//! The cap on how many commands a plugin sends to `Control` a second (ADR-0069).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The span the cap counts over.
pub(crate) const WINDOW: Duration = Duration::from_secs(1);

/// A sliding one-second window over the commands a plugin sent to `Control`: a call is
/// admitted when its commands (every edit of a batch, or its one action, query or history
/// operation) fit, with the ones admitted in the [`WINDOW`] before it, within `cap`. A
/// call the window refuses is not counted, so a plugin that keeps calling past the cap
/// gets through again once its earlier commands are a second old. A call carrying more
/// commands than `cap` never fits.
#[derive(Debug)]
pub(crate) struct CommandRate {
    cap: usize,
    /// When each command admitted in the last [`WINDOW`] was, oldest first; at most `cap`.
    admitted: VecDeque<Instant>,
}

impl CommandRate {
    pub(crate) fn new(cap: u32) -> Self {
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        CommandRate {
            cap,
            admitted: VecDeque::with_capacity(cap.min(1024)),
        }
    }

    /// The most commands admitted in any [`WINDOW`].
    pub(crate) fn cap(&self) -> usize {
        self.cap
    }

    /// Whether a call of `commands` commands at `now` is within the cap, counting them if
    /// it is.
    pub(crate) fn admit(&mut self, now: Instant, commands: usize) -> bool {
        while let Some(&oldest) = self.admitted.front() {
            if now.saturating_duration_since(oldest) >= WINDOW {
                self.admitted.pop_front();
            } else {
                break;
            }
        }
        if commands > self.cap - self.admitted.len() {
            return false;
        }
        self.admitted.extend(std::iter::repeat_n(now, commands));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{CommandRate, WINDOW};
    use std::time::{Duration, Instant};

    #[test]
    fn the_cap_holds_over_any_second_and_frees_as_calls_age() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut rate = CommandRate::new(3);
        assert!(rate.admit(at(0), 1));
        assert!(rate.admit(at(100), 1));
        assert!(rate.admit(at(200), 1));
        assert!(
            !rate.admit(at(300), 1),
            "a fourth command within the second"
        );
        assert!(
            !rate.admit(at(999), 1),
            "a refused call doesn't count, but none aged"
        );
        assert!(rate.admit(at(1_000), 1), "the first call is a second old");
        assert!(!rate.admit(at(1_050), 1));
        assert!(rate.admit(at(1_100), 1));
        // A long gap frees the whole window.
        let later = at(1_100) + WINDOW * 5;
        for _ in 0..3 {
            assert!(rate.admit(later, 1));
        }
        assert!(!rate.admit(later, 1));
    }

    #[test]
    fn every_command_of_a_batch_counts() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut rate = CommandRate::new(5);
        assert!(rate.admit(at(0), 3), "a batch of three");
        assert!(
            !rate.admit(at(10), 3),
            "three more don't fit in the two left"
        );
        assert!(rate.admit(at(20), 2), "two do");
        assert!(!rate.admit(at(30), 1));
        assert!(rate.admit(at(1_000), 3), "the first batch is a second old");
        assert!(!rate.admit(at(5_000), 6), "a batch past the cap never fits");
        assert!(rate.admit(at(5_000), 5), "a batch of the cap does");
    }

    #[test]
    fn a_cap_of_zero_admits_nothing() {
        let mut rate = CommandRate::new(0);
        assert!(!rate.admit(Instant::now(), 1));
    }
}
