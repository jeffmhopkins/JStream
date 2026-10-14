//! [`InProcessControl`]: one client session with an [`Engine`], in the same process.

use crate::engine::{Engine, Executed};
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
use tracing::Instrument;
use tracing::field;

/// One client session with an [`Engine`], opened by [`Engine::connect`]: the
/// [`Control`] trait served in-process, with no serialization and no transport.
///
/// It follows the same protocol as the WebSocket API: `hello` first (the version check
/// applies too, since a host may hand the session to a client built against another
/// protocol version), then calls. Its `client_id` keys its idempotency keys. Request IDs
/// are `req_1`, `req_2`, … per session, as `WsControl` numbers them. Every future it
/// returns is already complete, except a `video.snapshot` call's: that one completes,
/// and wakes its waker, when the engine's snapshot thread has drawn the frame, so the
/// GPU wait never blocks the polling thread. Any executor drives it, and so does a
/// poll loop that parks until woken. It never fails with
/// [`ControlError::Disconnected`].
///
/// Dropping the session ends the event streams it opened, after what they buffered.
///
/// Each session has a `session` span (R-RS-005) that records its `client_id` once
/// `hello` succeeds. Its `hello`, its calls and its end are logged inside that span, so
/// the engine's `command` spans nest under the session that sent them.
pub struct InProcessControl {
    engine: Engine,
    span: tracing::Span,
    client_id: Mutex<Option<ClientId>>,
    next_request: AtomicU64,
    /// This session's event subscriptions, closed when it is dropped.
    subscriptions: Mutex<Vec<QueueRef>>,
}

impl InProcessControl {
    pub(crate) fn new(engine: Engine) -> Self {
        InProcessControl {
            engine,
            span: tracing::info_span!("session", client_id = field::Empty),
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
        let _entered = self.span.enter();
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
            drop(session);
            self.span.record("client_id", field::display(&client_id));
            tracing::debug!(
                client_name = %hello.client_name,
                protocol = %hello.protocol,
                "hello accepted"
            );
            Ok(Welcome {
                protocol: PROTOCOL_VERSION,
                client_id,
                rev,
            })
        } else {
            tracing::debug!(
                client_name = %hello.client_name,
                protocol = %hello.protocol,
                "hello refused: unsupported protocol version"
            );
            Err(ProtocolError::UnsupportedVersion {
                client: hello.protocol,
                server: PROTOCOL_VERSION,
            }
            .into())
        };
        Box::pin(future::ready(result))
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        let _entered = self.span.enter();
        let id = RequestId::new(format!(
            "req_{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        ));
        let executed = match self.client_id() {
            None => {
                tracing::debug!("call refused: no hello yet");
                Executed::Ready(Err(ProtocolError::HandshakeRequired))
            }
            Some(client_id) => self.engine.execute(&client_id, &id, call),
        };
        match executed {
            Executed::Ready(result) => Box::pin(future::ready(
                result
                    .map(|result| Response { id, result })
                    .map_err(ControlError::from),
            )),
            Executed::Pending(reply, span) => Box::pin(
                async move {
                    let result = reply.await;
                    match &result {
                        Ok(_) => tracing::debug!("call done"),
                        Err(error) => tracing::debug!(code = error.code(), "call failed"),
                    }
                    result
                        .map(|result| Response {
                            id,
                            result: jstream_protocol::CallResult::Query { result },
                        })
                        .map_err(ControlError::from)
                }
                .instrument(span),
            ),
        }
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
        let _entered = self.span.enter();
        tracing::debug!("session closed");
        let mine = std::mem::take(&mut *lock(&self.subscriptions));
        events::close(mine);
    }
}
