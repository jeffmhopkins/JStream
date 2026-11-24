//! The network listener's state as the request handlers read it: whether it is open, the
//! gate's rules for it, and what `pairing.start` tells a device. The host opens and
//! closes the listener while the server runs (`NetworkSwitch`, ADR-0104 decision 1 as
//! amended by ADR-0234), so every request reads this instead of a value fixed at bind.
//!
//! - **Open**, the gate admits device tokens on the listener's side and the server pairs
//!   devices, whatever `ServerConfig::pairing` says; `pairing.start` names the listener's
//!   addresses and fingerprint.
//! - **Closed**, the gate has no network side, so a request still arriving on a closing
//!   listener fails its `Host` check, and the server pairs devices only if
//!   `ServerConfig::pairing` is on. Each WebSocket session admitted on the listener holds
//!   [`NetworkState::stopping`]'s receiver of the time it opened, and closes when that
//!   listener closes.

use crate::gate::NetworkSide;
use jstream_protocol::NetworkAccess;
use std::sync::{Arc, PoisonError, RwLock};
use tokio::sync::watch;

/// The network listener's state: open or not, and its rules while it is.
#[derive(Default)]
pub(crate) struct NetworkState {
    open: RwLock<Option<Arc<Open>>>,
}

/// An open network listener, as the handlers see it.
struct Open {
    side: Arc<NetworkSide>,
    access: Option<NetworkAccess>,
    /// Turns true when this listener closes, for the sessions admitted on it.
    stopping: watch::Receiver<bool>,
}

impl NetworkState {
    fn current(&self) -> Option<Arc<Open>> {
        self.open
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Opens it: the gate's side, what `pairing.start` answers, and the receiver that
    /// turns true when it closes.
    #[cfg(feature = "tls")]
    pub(crate) fn open(
        &self,
        side: NetworkSide,
        access: Option<NetworkAccess>,
        stopping: watch::Receiver<bool>,
    ) {
        *self.open.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(Open {
            side: Arc::new(side),
            access,
            stopping,
        }));
    }

    /// Closes it.
    #[cfg(feature = "tls")]
    pub(crate) fn close(&self) {
        *self.open.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Whether it is open.
    pub(crate) fn is_open(&self) -> bool {
        self.current().is_some()
    }

    /// The gate's rules for it, while it is open.
    pub(crate) fn side(&self) -> Option<Arc<NetworkSide>> {
        self.current().map(|open| Arc::clone(&open.side))
    }

    /// What `pairing.start` tells a device, while it is open.
    pub(crate) fn access(&self) -> Option<NetworkAccess> {
        self.current().and_then(|open| open.access.clone())
    }

    /// The receiver a session admitted on it holds, which turns true when it closes;
    /// `None` while it is closed.
    pub(crate) fn stopping(&self) -> Option<watch::Receiver<bool>> {
        self.current().map(|open| open.stopping.clone())
    }
}
