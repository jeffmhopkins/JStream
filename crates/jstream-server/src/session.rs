//! One WebSocket connection: the protocol envelope on one side, one `Control` session on
//! the other.
//!
//! The connection task reads client messages, forwards `hello` and each `request` to the
//! session (several requests may be in flight; answers go back as they complete), and
//! forwards the session's events once the handshake has succeeded. It ends when the
//! client closes, the session's event stream ends (the engine went away), or the server
//! shuts down; dropping the session then ends its event streams.
//!
//! **Logging.** The task runs in its connection's `connection` span. It logs the
//! handshake's outcome (the `client_id`, or the refusal's code) and why the connection
//! ended; never message contents.
//!
//! **Request IDs.** The client chooses the wire ID of each request, while a `Control`
//! session assigns its own. Answers carry the wire ID, and so do the `state.changed`
//! events this connection's calls cause: the task maps the session's IDs back to the
//! wire IDs. An event can arrive before the answer that tells the task its ID, so while
//! calls are in flight such an event is held back (with every event after it, to keep
//! the order) until the answer arrives or no call is left in flight.

use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures_util::stream::FuturesUnordered;
use futures_util::{SinkExt, StreamExt};
use jstream_control::{BoxFuture, Control, ControlError, EventStream};
use jstream_protocol::{
    ClientId, ClientMessage, ErrorResponse, Event, EventMessage, ProtocolError, RequestId,
    Response, ServerMessage, WireError,
};
use std::collections::VecDeque;
use tokio::sync::watch;

/// How many session-to-wire request ID pairs a connection remembers. A `state.changed`
/// event normally arrives within a few calls of its answer.
const REMEMBERED_IDS: usize = 1024;

/// How many events may be held back waiting for an answer before they are sent as they
/// are (with the session's request ID).
const MAX_HELD_EVENTS: usize = 1024;

/// The connection ended (the client went away or a send failed).
struct Closed;

/// The answer to one forwarded request, with the client's ID for it.
type Answer = (RequestId, Result<Response, ControlError>);

/// Runs one connection until it ends.
pub(crate) async fn run(
    socket: WebSocket,
    control: Box<dyn Control>,
    mut shutdown: watch::Receiver<bool>,
) {
    let (mut sink, mut incoming) = socket.split();
    let control: &dyn Control = &*control;
    let mut calls: FuturesUnordered<BoxFuture<'_, Answer>> = FuturesUnordered::new();
    let mut events: Option<EventStream> = None;
    let mut ids = IdMap::default();
    let end: Result<Option<CloseFrame>, Closed> = loop {
        tokio::select! {
            () = stopping(&mut shutdown) => {
                break Ok(Some(close(close_code::AWAY, "the server is shutting down")));
            }
            message = incoming.next() => {
                let text = match message {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break Err(Closed),
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Binary(_))) => {
                        let error = malformed(None, "binary frames are not part of the protocol yet");
                        if send(&mut sink, &error).await.is_err() {
                            break Err(Closed);
                        }
                        continue;
                    }
                    // Pings are answered by the WebSocket layer.
                    Some(Ok(_)) => continue,
                };
                match ClientMessage::parse(text.as_str()) {
                    Err(error) => {
                        let reply = ServerMessage::Error(ErrorResponse {
                            id: request_id(text.as_str()),
                            error: error.into(),
                        });
                        if send(&mut sink, &reply).await.is_err() {
                            break Err(Closed);
                        }
                    }
                    Ok(ClientMessage::Hello(hello)) => match control.hello(hello).await {
                        Ok(welcome) => {
                            tracing::info!(client_id = %welcome.client_id, "handshake done");
                            ids.client_id = Some(welcome.client_id.clone());
                            if events.is_none() {
                                events = Some(control.events());
                            }
                            if send(&mut sink, &ServerMessage::Welcome(welcome)).await.is_err() {
                                break Err(Closed);
                            }
                        }
                        Err(ControlError::Protocol(error)) => {
                            tracing::info!(code = error.error.code(), "handshake refused");
                            let refused = matches!(error.error, ProtocolError::UnsupportedVersion { .. });
                            let reply = ServerMessage::Error(ErrorResponse { id: None, error });
                            if send(&mut sink, &reply).await.is_err() {
                                break Err(Closed);
                            }
                            // The envelope closes the connection after a refused handshake.
                            if refused && ids.client_id.is_none() {
                                break Ok(Some(close(close_code::POLICY, "unsupported protocol version")));
                            }
                        }
                        Err(error) => break Ok(Some(engine_gone(&error))),
                    },
                    Ok(ClientMessage::Request(request)) => {
                        calls.push(Box::pin(async move {
                            (request.id, control.call(request.call).await)
                        }));
                    }
                    Ok(_) => {
                        let error = malformed(None, "a message type this server doesn't handle");
                        if send(&mut sink, &error).await.is_err() {
                            break Err(Closed);
                        }
                    }
                }
            }
            Some((wire_id, result)) = calls.next(), if !calls.is_empty() => {
                let reply = match result {
                    Ok(response) => {
                        ids.remember(response.id, wire_id.clone());
                        ServerMessage::Response(Response { id: wire_id, result: response.result })
                    }
                    Err(ControlError::Protocol(error)) => {
                        ServerMessage::Error(ErrorResponse { id: Some(wire_id), error })
                    }
                    Err(error) => break Ok(Some(engine_gone(&error))),
                };
                if send(&mut sink, &reply).await.is_err() {
                    break Err(Closed);
                }
                if flush(&mut sink, &mut ids, !calls.is_empty()).await.is_err() {
                    break Err(Closed);
                }
            }
            event = next_event(&mut events) => {
                let Some(event) = event else {
                    break Ok(Some(close(close_code::AWAY, "the engine went away")));
                };
                ids.held.push_back(event);
                if flush(&mut sink, &mut ids, !calls.is_empty()).await.is_err() {
                    break Err(Closed);
                }
            }
        }
    };
    match &end {
        Ok(Some(frame)) if frame.code == close_code::ERROR => {
            tracing::warn!(
                reason = frame.reason.as_str(),
                "connection closed: engine failed"
            );
        }
        Ok(Some(frame)) => {
            tracing::info!(
                code = frame.code,
                reason = frame.reason.as_str(),
                "connection closed"
            );
        }
        Ok(None) => tracing::info!("connection closed"),
        Err(Closed) => tracing::info!("connection closed: client gone or send failed"),
    }
    if let Ok(frame) = end {
        // Best effort: the client may already be gone.
        let _ = sink.send(Message::Close(frame)).await;
    }
}

/// Completes once the server is shutting down (or gone).
async fn stopping(shutdown: &mut watch::Receiver<bool>) {
    // An error means the server was dropped: that stops the connection too.
    let _ = shutdown.wait_for(|stopping| *stopping).await;
}

/// The next event, or never if the handshake hasn't subscribed yet.
async fn next_event(events: &mut Option<EventStream>) -> Option<Event> {
    match events {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

/// The session-to-wire request ID map, and the events held back until it can rewrite
/// them.
#[derive(Default)]
struct IdMap {
    /// This connection's identity, once the handshake succeeded.
    client_id: Option<ClientId>,
    /// (session ID, wire ID), oldest first.
    pairs: VecDeque<(RequestId, RequestId)>,
    /// Events not sent yet, in emission order.
    held: VecDeque<Event>,
}

impl IdMap {
    fn remember(&mut self, session: RequestId, wire: RequestId) {
        if self.pairs.len() == REMEMBERED_IDS {
            self.pairs.pop_front();
        }
        self.pairs.push_back((session, wire));
    }

    /// The event ready to send next, rewritten to the wire ID, or `None` if the front
    /// event must wait for an answer still in flight.
    fn next_ready(&mut self, calls_in_flight: bool) -> Option<Event> {
        let front = self.held.front()?;
        let waiting = match front {
            Event::StateChanged {
                client_id,
                request_id: Some(session),
                ..
            } if Some(client_id) == self.client_id.as_ref() => {
                !self.pairs.iter().any(|(s, _)| s == session)
                    && calls_in_flight
                    && self.held.len() <= MAX_HELD_EVENTS
            }
            _ => false,
        };
        if waiting {
            return None;
        }
        let mut event = self.held.pop_front()?;
        if let Event::StateChanged {
            client_id,
            request_id: Some(request_id),
            ..
        } = &mut event
            && Some(&*client_id) == self.client_id.as_ref()
            && let Some((_, wire)) = self.pairs.iter().find(|(s, _)| s == request_id)
        {
            *request_id = wire.clone();
        }
        Some(event)
    }
}

/// Sends every held event that is ready.
async fn flush<S>(sink: &mut S, ids: &mut IdMap, calls_in_flight: bool) -> Result<(), Closed>
where
    S: SinkExt<Message> + Unpin,
{
    while let Some(event) = ids.next_ready(calls_in_flight) {
        send(sink, &ServerMessage::Event(EventMessage { event })).await?;
    }
    Ok(())
}

/// Sends one message as a JSON text frame.
async fn send<S>(sink: &mut S, message: &ServerMessage) -> Result<(), Closed>
where
    S: SinkExt<Message> + Unpin,
{
    // The protocol's types always serialize; a failure would be a bug, so the
    // connection ends rather than send something else.
    let text = serde_json::to_string(message).map_err(|_| Closed)?;
    sink.send(Message::Text(text.into()))
        .await
        .map_err(|_| Closed)
}

/// A `malformed` error, for a message that is valid JSON text but can't be served.
fn malformed(id: Option<RequestId>, reason: &str) -> ServerMessage {
    ServerMessage::Error(ErrorResponse {
        id,
        error: WireError::from(ProtocolError::Malformed {
            reason: reason.into(),
        }),
    })
}

/// The request ID of a message that failed to parse, if it is a `request` whose `id` is
/// still readable, so the client can match the error to its call.
fn request_id(text: &str) -> Option<RequestId> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("type")?.as_str()? != "request" {
        return None;
    }
    Some(RequestId::new(value.get("id")?.as_str()?))
}

fn close(code: u16, reason: &str) -> CloseFrame {
    CloseFrame {
        code,
        reason: reason.into(),
    }
}

/// The close frame for a session whose engine connection failed.
fn engine_gone(error: &ControlError) -> CloseFrame {
    let reason = match error {
        ControlError::Disconnected { reason } => reason.as_str(),
        _ => "the engine failed",
    };
    // Close reasons are limited to 123 bytes.
    let mut end = reason.len().min(120);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    close(close_code::ERROR, &reason[..end])
}

#[cfg(test)]
mod tests {
    use super::{IdMap, request_id};
    use jstream_protocol::{ClientId, Event, OutputId, OutputState, RequestId, Rev};

    fn changed(client: &str, request: &str) -> Event {
        Event::StateChanged {
            rev: Rev(1),
            client_id: ClientId::new(client),
            request_id: Some(RequestId::new(request)),
        }
    }

    fn map() -> IdMap {
        IdMap {
            client_id: Some(ClientId::new("me")),
            ..IdMap::default()
        }
    }

    #[test]
    fn own_events_get_the_wire_id() {
        let mut ids = map();
        ids.remember(RequestId::new("req_1"), RequestId::new("c1"));
        ids.held.push_back(changed("me", "req_1"));
        assert_eq!(ids.next_ready(true), Some(changed("me", "c1")));
        assert_eq!(ids.next_ready(true), None);
    }

    #[test]
    fn other_clients_events_pass_unchanged() {
        let mut ids = map();
        ids.remember(RequestId::new("req_1"), RequestId::new("c1"));
        ids.held.push_back(changed("other", "req_1"));
        assert_eq!(ids.next_ready(true), Some(changed("other", "req_1")));
    }

    #[test]
    fn unanswered_own_events_wait_and_keep_the_order() {
        let mut ids = map();
        let stopped = Event::OutputStateChanged {
            output_id: OutputId::new("o"),
            state: OutputState::Stopped,
        };
        ids.held.push_back(changed("me", "req_2"));
        ids.held.push_back(stopped.clone());
        assert_eq!(ids.next_ready(true), None);
        ids.remember(RequestId::new("req_2"), RequestId::new("c2"));
        assert_eq!(ids.next_ready(true), Some(changed("me", "c2")));
        assert_eq!(ids.next_ready(true), Some(stopped));
        // With no call in flight, nothing can map it: it goes as it is.
        ids.held.push_back(changed("me", "req_9"));
        assert_eq!(ids.next_ready(false), Some(changed("me", "req_9")));
    }

    #[test]
    fn unparseable_requests_keep_their_id() {
        assert_eq!(
            request_id(r#"{"type":"request","id":"c7","call":{"kind":"nope"}}"#),
            Some(RequestId::new("c7"))
        );
        assert_eq!(request_id(r#"{"type":"hello","id":"c7"}"#), None);
        assert_eq!(request_id("not json"), None);
    }
}
