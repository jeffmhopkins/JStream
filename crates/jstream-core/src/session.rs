//! [`InProcessControl`]: one client session with an [`Engine`], in the same process.

use crate::engine::Engine;
use crate::events;
use crate::events::QueueRef;
use crate::lock;
use jstream_control::{BoxFuture, Control, ControlError, EventStream};
use jstream_protocol::{
    Call, ClientId, Hello, PROTOCOL_VERSION, ProtocolError, RequestId, Response, Welcome,
};
use std::future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// One client session with an [`Engine`], opened by [`Engine::connect`]: the
/// [`Control`] trait served in-process, with no serialization and no transport.
///
/// It follows the same protocol as the WebSocket API: `hello` first (the version check
/// applies too, since a host may hand the session to a client built against another
/// protocol version), then calls. Its `client_id` keys its idempotency keys. Request IDs
/// are `req_1`, `req_2`, … per session, as `WsControl` numbers them. Every future it
/// returns is already complete, so any executor, or a plain poll loop, drives it, and it
/// never fails with [`ControlError::Disconnected`].
///
/// Dropping the session ends the event streams it opened, after what they buffered.
pub struct InProcessControl {
    engine: Engine,
    client_id: Mutex<Option<ClientId>>,
    next_request: AtomicU64,
    /// This session's event subscriptions, closed when it is dropped.
    subscriptions: Mutex<Vec<QueueRef>>,
}

impl InProcessControl {
    pub(crate) fn new(engine: Engine) -> Self {
        InProcessControl {
            engine,
            client_id: Mutex::new(None),
            next_request: AtomicU64::new(1),
            subscriptions: Mutex::new(Vec::new()),
        }
    }

    /// The session's client ID, once `hello` has succeeded.
    pub fn client_id(&self) -> Option<ClientId> {
        lock(&self.client_id).clone()
    }
}

impl Control for InProcessControl {
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        let result = if PROTOCOL_VERSION.can_serve(hello.protocol) {
            let mut session = lock(&self.client_id);
            let (client_id, rev) = match &*session {
                Some(client_id) => (client_id.clone(), self.engine.rev()),
                None => {
                    let (client_id, rev) = self.engine.register_client();
                    *session = Some(client_id.clone());
                    (client_id, rev)
                }
            };
            Ok(Welcome {
                protocol: PROTOCOL_VERSION,
                client_id,
                rev,
            })
        } else {
            Err(ProtocolError::UnsupportedVersion {
                client: hello.protocol,
                server: PROTOCOL_VERSION,
            }
            .into())
        };
        Box::pin(future::ready(result))
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        let id = RequestId::new(format!(
            "req_{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        ));
        let result = match self.client_id() {
            None => Err(ProtocolError::HandshakeRequired),
            Some(client_id) => self.engine.execute(&client_id, &id, call),
        };
        Box::pin(future::ready(
            result
                .map(|result| Response { id, result })
                .map_err(ControlError::from),
        ))
    }

    fn events(&self) -> EventStream {
        let (stream, queue) = self.engine.subscribe();
        let mut mine = lock(&self.subscriptions);
        mine.retain(|weak| weak.strong_count() > 0);
        mine.push(queue);
        Box::pin(stream)
    }
}

impl Drop for InProcessControl {
    /// Ends this session's event streams: the session is gone, as when a socket closes.
    fn drop(&mut self) {
        let mine = std::mem::take(&mut *lock(&self.subscriptions));
        events::close(mine);
    }
}
