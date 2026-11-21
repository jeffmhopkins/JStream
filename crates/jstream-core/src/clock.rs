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

    /// Whether it is held.
    pub(crate) fn is_held(&self) -> bool {
        self.lock().held.is_some()
    }

    /// Holds it at its time now. Holding it again keeps the time it is held at.
    pub(crate) fn hold(&self) -> MediaTime {
        let mut state = self.lock();
        let now = state.now();
        *state.held.get_or_insert(now)
    }

    /// Moves the held time on by `ms` milliseconds and returns it, or `None` when it
    /// isn't held.
    pub(crate) fn step(&self, ms: u32) -> Option<MediaTime> {
        let mut state = self.lock();
        let held = state.held?;
        let by = MediaTime::new(i64::from(ms), Timebase::MILLIS)
            .rescale(held.timebase(), Rounding::Nearest)
            .ok()?;
        let next = held.checked_add(by).ok()?;
        state.held = Some(next);
        Some(next)
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
        assert!(!clock.is_held());
        assert_eq!(clock.step(33), None, "a clock that runs isn't stepped");
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
        assert_eq!(clock.step(40).map(MediaTime::ticks), Some(1_040));
        assert_eq!(clock.step(40).map(MediaTime::ticks), Some(1_080));
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
            clock.step(100);
        }
        // Stepped to 11 s while real time reached 2 s: it waits at 11 s for real time.
        hand.set(2_000);
        assert!(clock.release());
        assert_eq!(ms(&clock), 11_000);
        hand.set(10_000);
        assert_eq!(ms(&clock), 11_000);
        hand.set(12_000);
        assert_eq!(
            ms(&clock),
            12_000,
            "then it runs with the platform's clock again"
        );
        // Stepped less than real time passed: it jumps forward to now.
        clock.hold();
        clock.step(10);
        hand.set(20_000);
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
        assert!(!clock.is_held());
        assert_eq!(ms(&clock), 7);
    }
}
