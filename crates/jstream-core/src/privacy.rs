//! The privacy screen (ADR-0027 §4): runtime state that covers every output.
//!
//! Like the program scene it is not collection state: `privacy.set` changes it without a
//! new `rev`, undo never touches it, and it is not saved. The render thread reads it
//! through a lock-free flag on every tick, so turning it on reaches the next frame
//! without waiting on the engine's lock. The mixer reads a lock-free count of its
//! changes instead, before every block, and the microphones' audio threads on every
//! packet, so it also reaches the next packet, and a change the mixer polls too late to
//! see as it was (on and off again between two polls) still reaches it. The mixer then
//! silences the microphones (`crate::audio_sources` marks their inputs).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use jstream_protocol::{ClientId, Event};

/// The privacy screen's state the engine keeps under its collection lock.
#[derive(Debug, Default)]
pub(crate) struct Privacy {
    /// What the render thread reads. Only [`set`](Self::set) writes it.
    flag: Arc<AtomicBool>,
    /// How many times the screen has changed, odd while it is on: what the mixer and the
    /// microphones' audio threads read (`jstream_audio::Mixer::with_privacy_signal`).
    /// Only [`set`](Self::set) writes it, after the flag.
    changes: Arc<AtomicU64>,
    /// The client that turned it on, while it is on.
    by: Option<ClientId>,
}

impl Privacy {
    /// Whether the screen is on.
    pub(crate) fn on(&self) -> bool {
        self.by.is_some()
    }

    /// Turns the screen on or off for `client_id`. `true` when that changed it.
    pub(crate) fn set(&mut self, on: bool, client_id: &ClientId) -> bool {
        if on == self.on() {
            return false;
        }
        self.by = on.then(|| client_id.clone());
        self.flag.store(on, Ordering::Release);
        self.changes.fetch_add(1, Ordering::Release);
        true
    }

    /// The flag the render thread reads.
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
    }

    /// The count of changes the mixer and the microphones' audio threads read.
    pub(crate) fn changes(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.changes)
    }

    /// The event a client that subscribes while the screen is on gets first, so it
    /// never shows an uncovered picture: `privacy.changed` naming who turned it on.
    pub(crate) fn replay(&self) -> Option<Event> {
        self.by.as_ref().map(|by| Event::PrivacyChanged {
            on: true,
            client_id: by.clone(),
            request_id: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Setting the state it already has changes nothing; the flag follows every change
    /// and the replay names the client that turned it on.
    #[test]
    fn privacy_tracks_who_turned_it_on() {
        let mut privacy = Privacy::default();
        let flag = privacy.flag();
        let changes = privacy.changes();
        let ada = ClientId::new("ada");
        assert!(privacy.replay().is_none());
        assert!(!privacy.set(false, &ada));
        assert!(privacy.set(true, &ada));
        assert!(flag.load(Ordering::Acquire));
        assert!(!privacy.set(true, &ClientId::new("bob")));
        // Odd while on, and only a change counts.
        assert_eq!(changes.load(Ordering::Acquire), 1);
        assert_eq!(
            privacy.replay(),
            Some(Event::PrivacyChanged {
                on: true,
                client_id: ada.clone(),
                request_id: None,
            })
        );
        assert!(privacy.set(false, &ada));
        assert!(!flag.load(Ordering::Acquire));
        assert_eq!(changes.load(Ordering::Acquire), 2);
        assert!(privacy.replay().is_none());
    }
}
