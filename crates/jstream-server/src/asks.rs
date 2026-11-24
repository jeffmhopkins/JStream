//! Devices asking to pair, answered by the person on the host instead of with a code
//! ([ADR-0235](../../../docs/adr/0235-pair-a-device-by-approving-it-on-the-host.md)).
//!
//! `POST /api/v1/pair/ask` ([`PAIR_ASK_PATH`](crate::PAIR_ASK_PATH), `pair.rs`) opens an
//! ask with [`Asks::open`]: every admitted session hears `pairing.asked`, and the request
//! waits on its [`Pending`] until a session answers (`pairing.answer`, [`Asks::answer`]),
//! [`ASK_TIMEOUT`] passes, or the device goes away (its request is dropped). Then every
//! session hears `pairing.ask_ended`. At most [`MAX_OPEN`] asks are open at once, and one
//! per address, so a device that asks in a loop can't pile prompts up.

use jstream_protocol::{EntityKind, Event, PairingAsk, PairingAskOutcome, ProtocolError};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};

/// How long a device's ask waits for the person's answer.
pub(crate) const ASK_TIMEOUT: Duration = Duration::from_secs(120);

/// The most asks open at once.
pub(crate) const MAX_OPEN: usize = 4;

/// How many ask events a slow session may fall behind by before it misses some.
const EVENT_BUFFER: usize = 32;

/// One open ask.
struct Open {
    ask: PairingAsk,
    ip: IpAddr,
    answer: oneshot::Sender<bool>,
}

#[derive(Default)]
struct State {
    open: Vec<Open>,
    next: u64,
}

/// The open asks, and the events about them.
pub(crate) struct Asks {
    state: Mutex<State>,
    events: broadcast::Sender<Event>,
}

/// Why an ask wasn't opened.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AskRefused {
    /// [`MAX_OPEN`] are open already, or one from this address is.
    Busy,
}

/// An open ask, waiting for its answer. Dropping it before the answer ends the ask as
/// `closed`: the device went away.
pub(crate) struct Pending {
    asks: Weak<Asks>,
    ask_id: String,
    answer: Option<oneshot::Receiver<bool>>,
}

impl Default for Asks {
    fn default() -> Self {
        Asks {
            state: Mutex::new(State::default()),
            events: broadcast::channel(EVENT_BUFFER).0,
        }
    }
}

impl Asks {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The events every admitted session hears: `pairing.asked` and `pairing.ask_ended`.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Opens an ask from a device named `name` (already checked) at `ip`.
    ///
    /// # Errors
    /// [`AskRefused::Busy`] when [`MAX_OPEN`] asks are open, or one from `ip` is.
    pub(crate) fn open(self: &Arc<Self>, name: &str, ip: IpAddr) -> Result<Pending, AskRefused> {
        let mut state = self.lock();
        if state.open.len() >= MAX_OPEN || state.open.iter().any(|open| open.ip == ip) {
            return Err(AskRefused::Busy);
        }
        state.next += 1;
        let ask_id = format!("ask_{}", state.next);
        let ip = ip.to_canonical();
        let ask = PairingAsk::new(ask_id.clone(), name, ip.to_string());
        let (answer, answered) = oneshot::channel();
        state.open.push(Open {
            ask: ask.clone(),
            ip,
            answer,
        });
        drop(state);
        tracing::info!(%ask_id, name, address = %ip, "a device asks to pair");
        let _ = self.events.send(Event::PairingAsked { ask });
        Ok(Pending {
            asks: Arc::downgrade(self),
            ask_id,
            answer: Some(answered),
        })
    }

    /// Answers the open ask `ask_id`. Its device learns the answer; the device's handler
    /// pairs it and ends the ask.
    ///
    /// # Errors
    /// `not_found` for an ask that isn't open.
    pub(crate) fn answer(&self, ask_id: &str, allow: bool) -> Result<(), ProtocolError> {
        let mut state = self.lock();
        let Some(at) = state.open.iter().position(|open| open.ask.ask_id == ask_id) else {
            return Err(ProtocolError::NotFound {
                entity: EntityKind::PairingAsk,
                id: ask_id.to_owned(),
            });
        };
        let open = state.open.remove(at);
        drop(state);
        tracing::info!(ask_id, allow, "a device's ask to pair answered");
        // A device gone meanwhile can't hear it; its `Pending` ends the ask then.
        let _ = open.answer.send(allow);
        Ok(())
    }

    /// Ends the ask `ask_id` as `outcome`, removing it if it is still open.
    fn end(&self, ask_id: &str, outcome: PairingAskOutcome) {
        self.lock().open.retain(|open| open.ask.ask_id != ask_id);
        let _ = self.events.send(Event::PairingAskEnded {
            ask_id: ask_id.to_owned(),
            outcome,
        });
    }
}

impl Pending {
    /// Waits for the person's answer, at most [`ASK_TIMEOUT`]: `Some(allow)`, or `None`
    /// when no one answered in time (the ask then ends as `timed_out`).
    pub(crate) async fn answer(&mut self, timeout: Duration) -> Option<bool> {
        let answered = self.answer.as_mut()?;
        match tokio::time::timeout(timeout, answered).await {
            Ok(Ok(allow)) => {
                self.answer = None;
                Some(allow)
            }
            // The sender is dropped only with the ask's own removal, after an answer.
            Ok(Err(_)) | Err(_) => {
                self.finish(PairingAskOutcome::TimedOut);
                None
            }
        }
    }

    /// Ends the ask as `outcome`, once.
    pub(crate) fn finish(&mut self, outcome: PairingAskOutcome) {
        self.answer = None;
        if let Some(asks) = self.asks.upgrade() {
            asks.end(&self.ask_id, outcome);
        }
        self.asks = Weak::new();
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if self.asks.strong_count() > 0 {
            self.finish(PairingAskOutcome::Closed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AskRefused, Asks, MAX_OPEN};
    use jstream_protocol::{Event, PairingAskOutcome};
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use std::time::Duration;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, last))
    }

    /// One ask per address, and at most [`MAX_OPEN`] in all.
    #[test]
    fn asks_are_limited_per_address_and_in_all() {
        let asks = Arc::new(Asks::default());
        let first = asks.open("Deck", ip(1)).expect("opens");
        assert_eq!(asks.open("Deck", ip(1)).err(), Some(AskRefused::Busy));
        let mut held = vec![first];
        for n in 2..=u8::try_from(MAX_OPEN).expect("small") {
            held.push(asks.open("Deck", ip(n)).expect("opens"));
        }
        assert_eq!(asks.open("Deck", ip(99)).err(), Some(AskRefused::Busy));
        drop(held);
        assert!(
            asks.open("Deck", ip(1)).is_ok(),
            "a closed ask frees its address"
        );
    }

    /// An answer reaches the waiting ask; a dropped one ends as closed; an unanswered
    /// one times out. Each says so to the sessions.
    #[tokio::test]
    async fn an_ask_ends_answered_closed_or_timed_out() {
        let asks = Arc::new(Asks::default());
        let mut events = asks.subscribe();
        let mut pending = asks.open("Deck", ip(1)).expect("opens");
        let Ok(Event::PairingAsked { ask }) = events.recv().await else {
            panic!("no pairing.asked");
        };
        assert_eq!(
            (ask.name.as_str(), ask.address.as_str()),
            ("Deck", "192.168.1.1")
        );
        asks.answer(&ask.ask_id, true).expect("open");
        assert_eq!(pending.answer(Duration::from_secs(1)).await, Some(true));
        pending.finish(PairingAskOutcome::Allowed);
        assert!(matches!(
            events.recv().await,
            Ok(Event::PairingAskEnded {
                outcome: PairingAskOutcome::Allowed,
                ..
            })
        ));
        assert!(asks.answer(&ask.ask_id, true).is_err(), "answered once");

        drop(asks.open("Deck", ip(2)).expect("opens"));
        let _ = events.recv().await;
        assert!(matches!(
            events.recv().await,
            Ok(Event::PairingAskEnded {
                outcome: PairingAskOutcome::Closed,
                ..
            })
        ));

        let mut late = asks.open("Deck", ip(3)).expect("opens");
        let _ = events.recv().await;
        assert_eq!(late.answer(Duration::from_millis(10)).await, None);
        assert!(matches!(
            events.recv().await,
            Ok(Event::PairingAskEnded {
                outcome: PairingAskOutcome::TimedOut,
                ..
            })
        ));
        drop(late);
        assert!(events.try_recv().is_err(), "ended once");
    }
}
