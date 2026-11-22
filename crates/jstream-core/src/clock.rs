//! The engine's media clock, which a client can hold and step one frame at a time
//! (ADR-0172): `clock.hold`, `clock.step` and `clock.release`.
//!
//! Every animation the engine draws reads its time from one clock: transitions and
//! Moves, motion modifiers, timelines, the sky's warp, animated sources and what
//! `video.snapshot` and `video.frames` draw. Running, that clock is the platform's (or
//! the engine's own before the host injects one). Held, it stands still at the moment it
//! was held and moves only when stepped, so a machine too slow to composite in real time
//! can still render every frame of a move exactly, a snapshot per step.
//!
//! Released, it never runs backward: it stays at the held time until the platform's
//! clock passes it, then runs with it again. So a render stepped faster than real time
//! only waits for real time to catch up, and one stepped slower jumps forward to now.

use std::sync::{Arc, Mutex, PoisonError};

use jstream_media::{MediaTime, Rounding, Timebase};
use jstream_platform::MediaClock;

/// Why [`HeldClock::step`] moved nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepError {
    /// The clock runs: it is stepped only while held.
    NotHeld,
    /// The step would take the held time past what a media time can hold.
    Overflow,
}

/// The engine's clock: the platform's, held and stepped by hand when a client asks.
pub(crate) struct HeldClock {
    state: Mutex<State>,
}

struct State {
    /// The clock it runs on while not held.
    inner: Arc<dyn MediaClock>,
    /// The time it stands at while held.
    held: Option<MediaTime>,
    /// The time it never falls below again: the held time at the last release.
    floor: Option<MediaTime>,
}

impl HeldClock {
    /// Runs on `inner` until held.
    pub(crate) fn new(inner: Arc<dyn MediaClock>) -> Self {
        Self {
            state: Mutex::new(State {
                inner,
                held: None,
                floor: None,
            }),
        }
    }

    /// Runs on `inner` from now on, released: times on the old clock mean nothing on
    /// the new one.
    pub(crate) fn set_inner(&self, inner: Arc<dyn MediaClock>) {
        let mut state = self.lock();
        state.inner = inner;
        state.held = None;
        state.floor = None;
    }

    /// How far it is ahead of the platform's clock: `None` while it runs with it, and
    /// the held time's lead (zero or more) while held, or the floor's while it waits
    /// after a release for real time to catch up (ADR-0172 decision 4). An output
    /// follows real time, so it starts only while this is `None`.
    pub(crate) fn lead(&self) -> Option<std::time::Duration> {
        let state = self.lock();
        let ahead = match (state.held, state.floor) {
            (Some(held), _) => held,
            (None, Some(floor)) => floor,
            (None, None) => return None,
        };
        let real = state.inner.now();
        if state.held.is_none() && ahead <= real {
            return None;
        }
        let lead = ahead
            .checked_sub(real)
            .ok()
            .and_then(|lead| lead.rescale(Timebase::NANOS, Rounding::Up).ok())
            .map_or(0, |lead| u64::try_from(lead.ticks()).unwrap_or(0));
        Some(std::time::Duration::from_nanos(lead))
    }

    /// Holds it at its time now. Holding it again keeps the time it is held at.
    pub(crate) fn hold(&self) -> MediaTime {
        let mut state = self.lock();
        let now = state.now();
        *state.held.get_or_insert(now)
    }

    /// Moves the held time on by `ms` milliseconds and returns it.
    ///
    /// # Errors
    /// [`StepError::NotHeld`] when it runs, and [`StepError::Overflow`] when the held
    /// time can't move that far.
    pub(crate) fn step(&self, ms: u32) -> Result<MediaTime, StepError> {
        let mut state = self.lock();
        let held = state.held.ok_or(StepError::NotHeld)?;
        let next = MediaTime::new(i64::from(ms), Timebase::MILLIS)
            .rescale(held.timebase(), Rounding::Nearest)
            .ok()
            .and_then(|by| held.checked_add(by).ok())
            .ok_or(StepError::Overflow)?;
        state.held = Some(next);
        Ok(next)
    }

    /// Runs it again from the held time, never below it. Returns whether it was held.
    pub(crate) fn release(&self) -> bool {
        let mut state = self.lock();
        let Some(held) = state.held.take() else {
            return false;
        };
        state.floor = Some(held);
        true
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn now(&self) -> MediaTime {
        if let Some(held) = self.held {
            return held;
        }
        let now = self.inner.now();
        match self.floor {
            Some(floor) if floor > now => floor,
            _ => now,
        }
    }
}

impl MediaClock for HeldClock {
    fn timebase(&self) -> Timebase {
        self.lock().inner.timebase()
    }

    fn now(&self) -> MediaTime {
        self.lock().now()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    use jstream_media::{MediaTime, Timebase};
    use jstream_platform::MediaClock;

    use super::HeldClock;

    /// A platform clock moved by hand, in milliseconds.
    #[derive(Default)]
    struct Hand(AtomicI64);

    impl Hand {
        fn set(&self, ms: i64) {
            self.0.store(ms, Ordering::SeqCst);
        }
    }

    impl MediaClock for Hand {
        fn timebase(&self) -> Timebase {
            Timebase::MILLIS
        }

        fn now(&self) -> MediaTime {
            MediaTime::new(self.0.load(Ordering::SeqCst), Timebase::MILLIS)
        }
    }

    fn ms(clock: &HeldClock) -> i64 {
        clock.now().ticks()
    }

    #[test]
    fn running_it_reads_the_platform_clock() {
        let hand = Arc::new(Hand::default());
        let clock = HeldClock::new(Arc::clone(&hand) as Arc<dyn MediaClock>);
        hand.set(1_500);
        assert_eq!(ms(&clock), 1_500);
        assert_eq!(
            clock.step(33),
            Err(super::StepError::NotHeld),
            "a clock that runs isn't stepped"
        );
        assert_eq!(clock.lead(), None);
        assert!(!clock.release(), "nor released");
    }

    #[test]
    fn held_it_stands_still_and_moves_only_by_its_steps() {
        let hand = Arc::new(Hand::default());
        let clock = HeldClock::new(Arc::clone(&hand) as Arc<dyn MediaClock>);
        hand.set(1_000);
        assert_eq!(clock.hold().ticks(), 1_000);
        hand.set(9_000);
        assert_eq!(ms(&clock), 1_000, "real time passing doesn't move it");
        assert_eq!(clock.step(40).map(MediaTime::ticks), Ok(1_040));
        assert_eq!(clock.step(40).map(MediaTime::ticks), Ok(1_080));
        assert_eq!(ms(&clock), 1_080);
        assert_eq!(
            clock.hold().ticks(),
            1_080,
            "holding again keeps the held time"
        );
    }

    #[test]
    fn released_it_never_runs_backward() {
        let hand = Arc::new(Hand::default());
        let clock = HeldClock::new(Arc::clone(&hand) as Arc<dyn MediaClock>);
        hand.set(1_000);
        clock.hold();
        for _ in 0..100 {
            clock.step(100).expect("held");
        }
        // Stepped to 11 s while real time reached 2 s: it waits at 11 s for real time.
        hand.set(2_000);
        assert_eq!(clock.lead(), Some(std::time::Duration::from_secs(9)));
        assert!(clock.release());
        assert_eq!(ms(&clock), 11_000);
        hand.set(10_000);
        assert_eq!(ms(&clock), 11_000);
        assert_eq!(
            clock.lead(),
            Some(std::time::Duration::from_secs(1)),
            "released, it is still ahead until real time catches up"
        );
        hand.set(12_000);
        assert_eq!(clock.lead(), None, "caught up");
        assert_eq!(
            ms(&clock),
            12_000,
            "then it runs with the platform's clock again"
        );
        // Stepped less than real time passed: it jumps forward to now.
        clock.hold();
        clock.step(10).expect("held");
        hand.set(20_000);
        assert_eq!(
            clock.lead(),
            Some(std::time::Duration::ZERO),
            "held behind real time, it still doesn't follow it"
        );
        clock.release();
        assert_eq!(ms(&clock), 20_000);
    }

    #[test]
    fn steps_land_in_the_platform_clocks_time_base() {
        let hand = Arc::new(Hand::default());
        let clock = HeldClock::new(Arc::clone(&hand) as Arc<dyn MediaClock>);
        clock.hold();
        let stepped = clock.step(1_000).expect("held");
        assert_eq!(stepped.timebase(), Timebase::MILLIS);
        assert_eq!(stepped.ticks(), 1_000);
    }

    #[test]
    fn a_new_platform_clock_releases_it() {
        let hand = Arc::new(Hand::default());
        let clock = HeldClock::new(Arc::clone(&hand) as Arc<dyn MediaClock>);
        hand.set(5_000);
        clock.hold();
        let other = Arc::new(Hand::default());
        other.set(7);
        clock.set_inner(Arc::clone(&other) as Arc<dyn MediaClock>);
        assert_eq!(ms(&clock), 7);
    }
}
