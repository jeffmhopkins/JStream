//! The cap on how many calls a plugin sends to `Control` a second (ADR-0069).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The span the cap counts over.
pub(crate) const WINDOW: Duration = Duration::from_secs(1);

/// A sliding one-second window over the calls a plugin sent to `Control`: a call is
/// admitted while fewer than `cap` were admitted in the [`WINDOW`] before it. A call the
/// window refuses is not counted, so a plugin that keeps calling past the cap gets
/// through again once its earlier calls are a second old.
#[derive(Debug)]
pub(crate) struct CallRate {
    cap: usize,
    /// When each call admitted in the last [`WINDOW`] was, oldest first; at most `cap`.
    admitted: VecDeque<Instant>,
}

impl CallRate {
    pub(crate) fn new(cap: u32) -> Self {
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        CallRate {
            cap,
            admitted: VecDeque::with_capacity(cap.min(1024)),
        }
    }

    /// The most calls admitted in any [`WINDOW`].
    pub(crate) fn cap(&self) -> usize {
        self.cap
    }

    /// Whether a call at `now` is within the cap, counting it if it is.
    pub(crate) fn admit(&mut self, now: Instant) -> bool {
        while let Some(&oldest) = self.admitted.front() {
            if now.saturating_duration_since(oldest) >= WINDOW {
                self.admitted.pop_front();
            } else {
                break;
            }
        }
        if self.admitted.len() >= self.cap {
            return false;
        }
        self.admitted.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{CallRate, WINDOW};
    use std::time::{Duration, Instant};

    #[test]
    fn the_cap_holds_over_any_second_and_frees_as_calls_age() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut rate = CallRate::new(3);
        assert!(rate.admit(at(0)));
        assert!(rate.admit(at(100)));
        assert!(rate.admit(at(200)));
        assert!(!rate.admit(at(300)), "a fourth call within the second");
        assert!(
            !rate.admit(at(999)),
            "a refused call doesn't count, but none aged"
        );
        assert!(rate.admit(at(1_000)), "the first call is a second old");
        assert!(!rate.admit(at(1_050)));
        assert!(rate.admit(at(1_100)));
        // A long gap frees the whole window.
        let later = at(1_100) + WINDOW * 5;
        for _ in 0..3 {
            assert!(rate.admit(later));
        }
        assert!(!rate.admit(later));
    }

    #[test]
    fn a_cap_of_zero_admits_nothing() {
        let mut rate = CallRate::new(0);
        assert!(!rate.admit(Instant::now()));
    }
}
