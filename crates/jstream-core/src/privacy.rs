//! The privacy screen (ADR-0027 §4): runtime state that covers every output.
//!
//! Like the program scene it is not collection state: `privacy.set` changes it without a
//! new `rev`, undo never touches it, and it is not saved. The render thread reads it
//! through a lock-free flag on every tick, so turning it on reaches the next frame
//! without waiting on the engine's lock.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jstream_protocol::{ClientId, Event};

/// The privacy screen's state the engine keeps under its collection lock.
#[derive(Debug, Default)]
pub(crate) struct Privacy {
    /// What the render thread reads. Only [`set`](Self::set) writes it.
    flag: Arc<AtomicBool>,
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
        true
    }

    /// The flag the render thread reads.
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.flag)
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
        let ada = ClientId::new("ada");
        assert!(privacy.replay().is_none());
        assert!(!privacy.set(false, &ada));
        assert!(privacy.set(true, &ada));
        assert!(flag.load(Ordering::Acquire));
        assert!(!privacy.set(true, &ClientId::new("bob")));
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
        assert!(privacy.replay().is_none());
    }
}
