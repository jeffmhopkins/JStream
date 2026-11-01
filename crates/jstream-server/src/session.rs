//! One WebSocket connection: the protocol envelope on one side, one `Control` session on
//! the other.
//!
//! The connection task reads client messages, forwards `hello` and each `request` to the
//! session (several requests may be in flight; answers go back as they complete), and
//! forwards the session's events once the handshake has succeeded. It ends when the
//! client closes, the session's event stream ends (the engine went away), the server
//! shuts down (a message read once it is shutting down is not handled, so no request is
//! answered after the shutdown), the paired device whose token admitted it is revoked
//! (close code 1008, ADR-0044), or the handshake hasn't succeeded by its deadline
//! (`ServerConfig::handshake_timeout`, counted from when the connection was accepted;
//! close code 1008), even if the client's `hello` is still waiting for the engine's
//! answer, or the client stays silent too long (below); dropping the session then ends
//! its event streams and expires the client's open live-value gestures (ADR-0032).
//!
//! **Liveness.** The task pings the client every `ServerConfig::ping_interval`, and
//! anything it reads from the client (a message, or the pong a live client's WebSocket
//! layer answers with) counts as hearing from it. A connection the task hasn't heard
//! from for `ServerConfig::liveness_timeout` is closed (close code 1008), so one that
//! died without a close frame, which no read or write may report for a long time,
//! doesn't keep its session. While the task itself has stopped reading (see *Slow
//! clients*) the silence doesn't count, since a pong would wait unread behind the
//! requests. A ping goes out before the next queued message, once the send in progress
//! completes.
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
//!
//! **Preview frames.** Once the handshake succeeds the task also opens the session's
//! binary channel and sends its frames as binary messages, after any JSON message
//! that is waiting ([`preview`](crate::preview)). It reads no binary messages: a client
//! that sends one gets a `malformed` error.
//!
//! **Message size** (ADR-0074). A client message over [`REQUEST_LIMIT`] is refused with
//! `too_large`, answering its `id` when the envelope's `id` can be read, and nothing else
//! of it is parsed: it never reaches the session and never counts against
//! [`MAX_IN_FLIGHT`]. The connection stays open. The WebSocket layer reads messages up to
//! [`FRAME_LIMIT`](crate::FRAME_LIMIT); a larger one closes the connection with close code 1009. `welcome`
//! advertises the request limit as `max_request_bytes`.
//!
//! **Slow clients.** The task never waits on the socket. What it sends goes into an
//! [`Outbox`], and a writer future that owns the socket's sink sends it, one message at a
//! time, while the task keeps reading the client and draining the session's events. So a
//! client that reads slowly never holds events back in the engine's subscription queue,
//! and never slows the engine or other clients. The outbox holds at most
//! [`EVENT_BUFFER`] events. When one more arrives, every queued event is dropped and one
//! `events.dropped` notice with their count takes their place (a notice still queued
//! gets the new count added instead), so the client knows to re-read the state. Replies
//! (`welcome`, `response`, `error`) are never dropped, and what remains keeps its order:
//! a `state.changed` this connection's call caused still follows that call's response.
//! While [`REPLY_BACKLOG`] replies wait unsent, the task stops reading requests, so a
//! client that sends requests without reading the answers is held back by the socket as
//! before. The same goes for calls the engine hasn't answered yet: while
//! [`MAX_IN_FLIGHT`] are in flight, the task reads nothing more until one completes, so a
//! client can't pile up unbounded work in the session (#354).

use crate::REQUEST_LIMIT;
use crate::envelope::{Envelope, Id};
use crate::preview::{self, Frames};
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use futures_util::future::BoxFuture as SendFuture;
use futures_util::stream::{FuturesUnordered, SplitSink};
use futures_util::{SinkExt, StreamExt};
use jstream_control::{BinaryStream, BoxFuture, Control, ControlError, EventStream};
use jstream_protocol::{
    ClientId, ClientMessage, ErrorResponse, Event, EventMessage, Hello, ProtocolError, RequestId,
    Response, ServerMessage, Welcome, WireError,
};
use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior, Sleep};

/// How many session-to-wire request ID pairs a connection remembers. A `state.changed`
/// event normally arrives within a few calls of its answer.
const REMEMBERED_IDS: usize = 1024;

/// How many events may be held back waiting for an answer before they are sent as they
/// are (with the session's request ID).
const MAX_HELD_EVENTS: usize = 1024;

/// How long a closing connection waits for the client to take what is still queued and
/// the close frame, and to answer it with its own. A client that has stopped reading would otherwise hold the
/// connection, and its `Control` session, open for good (#108 review N1).
pub(crate) const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// How many events a connection's outbox holds before it drops them all and queues one
/// `events.dropped` notice instead.
///
/// Today's events are small (about 100 bytes on the wire), so a full buffer is about
/// 100 KB per connection. A client that reads at all keeps up with the bursts the
/// engine makes now (one `state.changed` per batch, a few `output.state_changed` per
/// action), and 1024 still covers about 17 seconds of a 60 Hz live drag (`live.changed`,
/// ADR-0032) or minutes of a once-a-second `stats.tick`. Past that, re-reading the state
/// with `state.get` is cheaper for the client than catching up event by event.
const EVENT_BUFFER: usize = 1024;

/// How many replies may wait unsent before the connection stops reading requests, until
/// the client reads some answers. Replies are never dropped, so this is what bounds them.
const REPLY_BACKLOG: usize = 1024;

/// How many calls may be in flight on one connection (forwarded to the session and not
/// answered yet) before the connection stops reading, until one completes (#354).
///
/// Each one holds its parsed request, read from at most [`REQUEST_LIMIT`] (4 MiB) of
/// message text, so a client that never waits for answers can make the server hold
/// about 64 times the request limit, while a UI that pipelines its calls stays far
/// below it.
pub(crate) const MAX_IN_FLIGHT: usize = 64;

/// The connection ended (the client went away or a send failed).
struct Closed;

/// The answer to one forwarded request, with the client's ID for it.
type Answer = (RequestId, Result<Response, ControlError>);

/// Forwards `hello` to the engine. Before the first handshake succeeds (`pending`), the
/// engine's answer races `deadline`: `None` if the deadline passed first, so an engine
/// that never answers doesn't hold the connection past the handshake timeout (#273).
async fn handshake(
    control: &dyn Control,
    hello: Hello,
    pending: bool,
    deadline: &mut Pin<&mut Sleep>,
) -> Option<Result<Welcome, ControlError>> {
    if !pending {
        return Some(control.hello(hello).await);
    }
    tokio::select! {
        answer = control.hello(hello) => Some(answer),
        () = deadline.as_mut() => None,
    }
}

/// How often a connection is pinged and how long it may stay silent
/// (`ServerConfig::ping_interval` and `ServerConfig::liveness_timeout`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Liveness {
    /// How often the client is pinged.
    pub(crate) ping_every: Duration,
    /// How long the client may go unheard while the task reads it.
    pub(crate) timeout: Duration,
}

/// Runs one connection until it ends. A connection whose handshake hasn't succeeded by
/// `hello_by` is closed, and so is one whose device `revoked` signals, and one not heard
/// from within `liveness`'s timeout.
pub(crate) async fn run(
    socket: WebSocket,
    control: Box<dyn Control>,
    mut shutdown: watch::Receiver<bool>,
    mut revoked: Option<watch::Receiver<bool>>,
    hello_by: Instant,
    liveness: Liveness,
) {
    let (sink, mut incoming) = socket.split();
    let mut writer = Writer::new(sink);
    let mut outbox = Outbox::default();
    let control: &dyn Control = &*control;
    let mut calls: FuturesUnordered<BoxFuture<'_, Answer>> = FuturesUnordered::new();
    let mut events: Option<EventStream> = None;
    let mut binary: Option<BinaryStream> = None;
    let mut frames = Frames::default();
    let mut ids = IdMap::default();
    let handshake_deadline = tokio::time::sleep_until(hello_by);
    tokio::pin!(handshake_deadline);
    let mut pings =
        tokio::time::interval_at(Instant::now() + liveness.ping_every, liveness.ping_every);
    pings.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let silence = tokio::time::sleep(liveness.timeout);
    tokio::pin!(silence);
    let end: Result<Option<CloseFrame>, Closed> = loop {
        let reading = outbox.reads_requests() && calls.len() < MAX_IN_FLIGHT;
        if !reading {
            // The client can't be heard while nothing reads it: its silence counts again
            // from when reading resumes.
            silence.as_mut().reset(Instant::now() + liveness.timeout);
        }
        if writer.start(&mut outbox, &mut frames).is_err() {
            break Err(Closed);
        }
        tokio::select! {
            () = stopping(&mut shutdown) => break Ok(Some(shutting_down())),
            () = revocation(&mut revoked) => {
                break Ok(Some(close(close_code::POLICY, "the device was revoked")));
            }
            () = &mut handshake_deadline, if ids.client_id.is_none() => {
                break Ok(Some(close(close_code::POLICY, "no handshake in time")));
            }
            () = &mut silence, if reading => {
                break Ok(Some(close(close_code::POLICY, "no answer to pings in time")));
            }
            _ = pings.tick() => writer.ping = true,
            sent = writer.sent() => {
                if sent.is_err() {
                    break Err(Closed);
                }
            }
            message = incoming.next(), if reading => {
                // `select!` picks among ready branches at random, so a message can be
                // taken here although the shutdown is ready too. Once the server is
                // shutting down nothing read is handled: a request is not answered
                // after `serve_with_shutdown` has returned (#294).
                if stopped(&shutdown) {
                    break Ok(Some(shutting_down()));
                }
                if let Some(Ok(_)) = message {
                    silence.as_mut().reset(Instant::now() + liveness.timeout);
                }
                let text = match message {
                    Some(Err(error)) if over_frame_limit(&error) => {
                        break Ok(Some(close(close_code::SIZE, "the message is over the frame limit")));
                    }
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break Err(Closed),
                    Some(Ok(Message::Text(text))) if text.len() > REQUEST_LIMIT => {
                        let id = request_id(text.as_str());
                        outbox.push(too_large(id, text.len()));
                        continue;
                    }
                    Some(Ok(Message::Binary(bytes))) if bytes.len() > REQUEST_LIMIT => {
                        outbox.push(too_large(None, bytes.len()));
                        continue;
                    }
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Binary(_))) => {
                        outbox.push(malformed(None, "the server reads no binary messages"));
                        continue;
                    }
                    // Pings are answered by the WebSocket layer; a pong has done its job
                    // by being read.
                    Some(Ok(_)) => continue,
                };
                match ClientMessage::parse(text.as_str()) {
                    Err(error) => {
                        outbox.push(ServerMessage::Error(ErrorResponse {
                            id: request_id(text.as_str()),
                            error: error.into(),
                        }));
                    }
                    Ok(ClientMessage::Hello(hello)) => match handshake(
                        control,
                        hello,
                        ids.client_id.is_none(),
                        &mut handshake_deadline,
                    )
                    .await
                    {
                        // The engine hasn't answered by the deadline: the same close as a
                        // client that never said hello.
                        None => break Ok(Some(close(close_code::POLICY, "no handshake in time"))),
                        Some(Ok(welcome)) => {
                            tracing::info!(client_id = %welcome.client_id, "handshake done");
                            ids.client_id = Some(welcome.client_id.clone());
                            let welcome = Welcome {
                                max_request_bytes: Some(REQUEST_LIMIT as u64),
                                ..welcome
                            };
                            if events.is_none() {
                                events = Some(control.events());
                                binary = Some(control.binary());
                            }
                            outbox.push(ServerMessage::Welcome(welcome));
                        }
                        Some(Err(ControlError::Protocol(error))) => {
                            tracing::info!(code = error.error.code(), "handshake refused");
                            let refused = matches!(error.error, ProtocolError::UnsupportedVersion { .. });
                            outbox.push(ServerMessage::Error(ErrorResponse { id: None, error }));
                            // The envelope closes the connection after a refused handshake.
                            if refused && ids.client_id.is_none() {
                                break Ok(Some(close(close_code::POLICY, "unsupported protocol version")));
                            }
                        }
                        Some(Err(error)) => break Ok(Some(engine_gone(&error))),
                    },
                    Ok(ClientMessage::Request(request)) => {
                        calls.push(Box::pin(async move {
                            (request.id, control.call(request.call).await)
                        }));
                    }
                    Ok(_) => {
                        outbox.push(malformed(None, "a message type this server doesn't handle"));
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
                outbox.push(reply);
                flush(&mut outbox, &mut ids, !calls.is_empty());
            }
            frame = preview::next_frame(&mut binary) => frames.push(frame),
            event = next_event(&mut events) => {
                let Some(event) = event else {
                    break Ok(Some(close(close_code::AWAY, "the engine went away")));
                };
                ids.held.push_back(event);
                flush(&mut outbox, &mut ids, !calls.is_empty());
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
    // Nothing reads the session's events or frames from here on: end the subscriptions
    // now, so the engine stops queueing for this connection while the last messages go
    // out.
    drop(events);
    drop(binary);
    if let Ok(frame) = end {
        // What is queued (a refused handshake's error, say) goes before the close frame.
        // Then the closing handshake: read, and drop, what the client still sends until
        // its own close frame or the end of the stream. Closing the socket with client
        // data unread makes the kernel reset the connection, and a reset can discard
        // what the client hasn't read yet, the close frame and last replies included
        // (macOS does, #294). Best effort: the client may already be gone, or have
        // stopped reading.
        //
        // Not after a message over the frame limit: the WebSocket layer would go on
        // buffering the rest of that message, however long, to read past it. Nothing
        // more is read: the close frame is sent, and the socket is held, unread, for
        // `CLOSE_TIMEOUT` so the client can take the close frame before the reset that
        // closing over unread data sends, and then dropped.
        let oversized = frame
            .as_ref()
            .is_some_and(|frame| frame.code == close_code::SIZE);
        let started = Instant::now();
        let close = async {
            writer.finish(&mut outbox, frame).await?;
            if oversized {
                return Ok(());
            }
            while let Some(Ok(message)) = incoming.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
            Ok::<(), Closed>(())
        };
        if tokio::time::timeout(CLOSE_TIMEOUT, close).await.is_err() {
            tracing::info!("connection dropped: the client did not finish the close");
        } else if oversized {
            tokio::time::sleep_until(started + CLOSE_TIMEOUT).await;
        }
    }
}

/// Completes once the server is shutting down (or gone).
async fn stopping(shutdown: &mut watch::Receiver<bool>) {
    // An error means the server was dropped: that stops the connection too.
    let _ = shutdown.wait_for(|stopping| *stopping).await;
}

/// Completes once the device that admitted the connection is revoked or lapses; never
/// for a connection the control token admitted.
async fn revocation(revoked: &mut Option<watch::Receiver<bool>>) {
    match revoked {
        // An error means the watch was dropped, which only revocation does.
        Some(watch) => {
            let _ = watch.wait_for(|revoked| *revoked).await;
        }
        None => std::future::pending().await,
    }
}

/// Whether the server is shutting down (or gone) now: what [`stopping`] waits for.
fn stopped(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow() || shutdown.has_changed().is_err()
}

/// The close frame of a connection the server's shutdown ends.
fn shutting_down() -> CloseFrame {
    close(close_code::AWAY, "the server is shutting down")
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
        let front = self.held.front_mut()?;
        let waiting = match caused_by(front) {
            Some((client_id, Some(session))) if Some(client_id) == self.client_id.as_ref() => {
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
        if let Some((client_id, Some(request_id))) = caused_by(&mut event)
            && Some(client_id) == self.client_id.as_ref()
            && let Some((_, wire)) = self.pairs.iter().find(|(s, _)| s == request_id)
        {
            *request_id = wire.clone();
        }
        Some(event)
    }
}

/// The client and request an event names as its cause: `state.changed` and
/// `studio.changed`. Their request IDs are the session's, rewritten to the
/// wire's.
fn caused_by(event: &mut Event) -> Option<(&ClientId, &mut Option<RequestId>)> {
    match event {
        Event::StateChanged {
            client_id,
            request_id,
            ..
        }
        | Event::StudioChanged {
            client_id,
            request_id,
            ..
        } => Some((client_id, request_id)),
        _ => None,
    }
}

/// Moves every held event that is ready to the outbox.
fn flush(outbox: &mut Outbox, ids: &mut IdMap, calls_in_flight: bool) {
    while let Some(event) = ids.next_ready(calls_in_flight) {
        outbox.push_event(event);
    }
}

/// The messages waiting to be sent, oldest first: replies, which are never dropped, and
/// at most `limit` events ([`EVENT_BUFFER`]), which are.
struct Outbox {
    queue: VecDeque<ServerMessage>,
    /// The most events the queue holds; one more drops them all.
    limit: usize,
    /// How many of the queued messages are events that may be dropped.
    events: usize,
    /// How many are replies (not counting an `events.dropped` notice).
    replies: usize,
}

impl Default for Outbox {
    fn default() -> Self {
        Outbox::with_limit(EVENT_BUFFER)
    }
}

impl Outbox {
    fn with_limit(limit: usize) -> Self {
        Outbox {
            queue: VecDeque::new(),
            limit,
            events: 0,
            replies: 0,
        }
    }

    /// Queues a reply (or any message that isn't an event). Never dropped.
    fn push(&mut self, message: ServerMessage) {
        match kind(&message) {
            Kind::Event => self.events += 1,
            Kind::Reply => self.replies += 1,
            Kind::Notice => {}
        }
        self.queue.push_back(message);
    }

    /// Queues an event. If the queue already holds `limit` events, they are dropped
    /// first and one `events.dropped` notice counts them (see [`Outbox::drop_events`]).
    fn push_event(&mut self, event: Event) {
        if self.events >= self.limit {
            self.drop_events();
        }
        self.push(ServerMessage::Event(EventMessage { event }));
    }

    /// Drops every queued event, keeping the replies in their order, and adds the count
    /// to the queued `events.dropped` notice, or queues a new one at the end.
    fn drop_events(&mut self) {
        let before = self.queue.len();
        self.queue
            .retain(|message| !matches!(kind(message), Kind::Event));
        let dropped = (before - self.queue.len()) as u64;
        self.events = 0;
        let queued = self.queue.iter_mut().find_map(|message| match message {
            ServerMessage::Event(EventMessage {
                event: Event::EventsDropped { count },
            }) => Some(count),
            _ => None,
        });
        match queued {
            Some(count) => *count += dropped,
            None => self.push(ServerMessage::Event(EventMessage {
                event: Event::EventsDropped { count: dropped },
            })),
        }
        tracing::warn!(count = dropped, "client reads too slowly: events dropped");
    }

    /// The next message to send.
    fn pop(&mut self) -> Option<ServerMessage> {
        let message = self.queue.pop_front()?;
        match kind(&message) {
            Kind::Event => self.events -= 1,
            Kind::Reply => self.replies -= 1,
            Kind::Notice => {}
        }
        Some(message)
    }

    /// Whether the connection may read another request: not while [`REPLY_BACKLOG`]
    /// replies wait unsent.
    fn reads_requests(&self) -> bool {
        self.replies < REPLY_BACKLOG
    }
}

/// What an outbox message is, for the outbox's bounds.
enum Kind {
    /// An event, which the outbox may drop.
    Event,
    /// The `events.dropped` notice, which is never dropped and bounded by being one.
    Notice,
    /// A reply (`welcome`, `response`, `error`), which is never dropped.
    Reply,
}

fn kind(message: &ServerMessage) -> Kind {
    match message {
        ServerMessage::Event(EventMessage {
            event: Event::EventsDropped { .. },
        }) => Kind::Notice,
        ServerMessage::Event(_) => Kind::Event,
        _ => Kind::Reply,
    }
}

/// The socket's sending half, owned by the connection.
type Sink = SplitSink<WebSocket, Message>;

/// Sends the outbox's messages one at a time. While a send is in progress the future
/// owns the sink and hands it back when done, so the connection task polls it alongside
/// everything else instead of waiting on it.
struct Writer {
    /// The sink, while no send is in progress.
    idle: Option<Sink>,
    /// The send in progress.
    sending: Option<SendFuture<'static, (Sink, Result<(), Closed>)>>,
    /// Whether a ping is due: it goes before the next queued message.
    ping: bool,
}

impl Writer {
    fn new(sink: Sink) -> Self {
        Writer {
            idle: Some(sink),
            sending: None,
            ping: false,
        }
    }

    /// Starts sending a due ping, else the outbox's next message, or if it is empty the
    /// next binary frame, unless a send is in progress or there is nothing to send.
    fn start(&mut self, outbox: &mut Outbox, frames: &mut Frames) -> Result<(), Closed> {
        if self.sending.is_some() {
            return Ok(());
        }
        let Some(mut sink) = self.idle.take() else {
            return Err(Closed);
        };
        let message = if std::mem::take(&mut self.ping) {
            Message::Ping(Vec::new().into())
        } else {
            match outbox.pop() {
                Some(message) => Message::Text(encode(&message)?.into()),
                None => match frames.pop() {
                    Some(bytes) => Message::Binary(bytes.into()),
                    None => {
                        self.idle = Some(sink);
                        return Ok(());
                    }
                },
            }
        };
        self.sending = Some(Box::pin(async move {
            let sent = sink.send(message).await.map_err(|_| Closed);
            (sink, sent)
        }));
        Ok(())
    }

    /// Completes when the send in progress does; never, if none is. Cancel-safe: the
    /// send stays in progress if this is dropped first.
    async fn sent(&mut self) -> Result<(), Closed> {
        let Some(sending) = &mut self.sending else {
            return std::future::pending().await;
        };
        let (sink, sent) = sending.await;
        self.sending = None;
        self.idle = Some(sink);
        sent
    }

    /// Sends every JSON message still queued (not the binary frames), then the close
    /// message.
    async fn finish(
        &mut self,
        outbox: &mut Outbox,
        frame: Option<CloseFrame>,
    ) -> Result<(), Closed> {
        self.ping = false;
        loop {
            if self.sending.is_some() {
                self.sent().await?;
            }
            self.start(outbox, &mut Frames::default())?;
            if self.sending.is_none() {
                break;
            }
        }
        let sink = self.idle.as_mut().ok_or(Closed)?;
        sink.send(Message::Close(frame)).await.map_err(|_| Closed)
    }
}

/// One message as JSON text.
fn encode(message: &ServerMessage) -> Result<String, Closed> {
    // The protocol's types always serialize; a failure would be a bug, so the
    // connection ends rather than send something else.
    serde_json::to_string(message).map_err(|_| Closed)
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

/// A `too_large` error, for a message of `size` bytes over [`REQUEST_LIMIT`].
fn too_large(id: Option<RequestId>, size: usize) -> ServerMessage {
    ServerMessage::Error(ErrorResponse {
        id,
        error: WireError::from(ProtocolError::TooLarge {
            size_bytes: size as u64,
            limit_bytes: REQUEST_LIMIT as u64,
        }),
    })
}

/// Whether a read failed because the message was over [`FRAME_LIMIT`](crate::FRAME_LIMIT).
///
/// axum doesn't re-export `tungstenite`'s error type, so its `Capacity` error is told
/// by its message (`Space limit exceeded: Message too long: …`).
/// `over_frame_limit_closes_with_1009` pins this against the `tungstenite` in the tree.
fn over_frame_limit(error: &axum::Error) -> bool {
    error.to_string().contains("Message too long")
}

/// The request ID of a message that failed to parse or was too large, if it is a
/// `request` whose `id` is a readable string of at most
/// [`MAX_ECHOED_ID`](crate::envelope::MAX_ECHOED_ID) bytes, so the client can match the
/// error to its call. Only the envelope is read ([`Envelope`]), and the cap keeps what a
/// queued error holds small (#981 review B1).
fn request_id(text: &str) -> Option<RequestId> {
    match Envelope::read(text.as_bytes())? {
        Envelope {
            is_request: true,
            id: Some(Id::String(id)),
        } => Some(RequestId::new(id)),
        _ => None,
    }
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
    use super::{IdMap, Outbox, REPLY_BACKLOG, request_id};
    use jstream_protocol::{
        CallResult, ClientId, Event, EventMessage, OutputId, OutputState, RequestId, Response, Rev,
        ServerMessage,
    };

    fn changed(client: &str, request: &str) -> Event {
        Event::StateChanged {
            rev: Rev(1),
            client_id: ClientId::new(client),
            request_id: Some(RequestId::new(request)),
            cause: None,
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

    /// An event numbered `n`, so tests can tell which ones were kept.
    fn numbered(n: usize) -> Event {
        Event::OutputStateChanged {
            output_id: OutputId::new(format!("o{n}")),
            state: OutputState::Active,
        }
    }

    fn reply(id: &str) -> ServerMessage {
        ServerMessage::Response(Response {
            id: RequestId::new(id),
            result: CallResult::action(),
        })
    }

    fn event(event: Event) -> ServerMessage {
        ServerMessage::Event(EventMessage { event })
    }

    fn dropped(count: u64) -> ServerMessage {
        event(Event::EventsDropped { count })
    }

    fn drain(outbox: &mut Outbox) -> Vec<ServerMessage> {
        std::iter::from_fn(|| outbox.pop()).collect()
    }

    #[test]
    fn events_up_to_the_limit_are_all_kept() {
        let mut outbox = Outbox::with_limit(3);
        for n in 0..3 {
            outbox.push_event(numbered(n));
        }
        let expected: Vec<_> = (0..3).map(|n| event(numbered(n))).collect();
        assert_eq!(drain(&mut outbox), expected);
    }

    #[test]
    fn an_overflow_drops_only_events_and_keeps_the_order() {
        let mut outbox = Outbox::with_limit(3);
        outbox.push_event(numbered(0));
        outbox.push(reply("r1"));
        outbox.push_event(numbered(1));
        outbox.push(reply("r2"));
        outbox.push_event(numbered(2));
        // The fourth event overflows: the three queued are dropped, the replies stay in
        // their order, one notice counts the drop, and the new event follows it.
        outbox.push_event(numbered(3));
        assert_eq!(
            drain(&mut outbox),
            [reply("r1"), reply("r2"), dropped(3), event(numbered(3))]
        );
        // Emptied, the outbox counts afresh.
        for n in 4..7 {
            outbox.push_event(numbered(n));
        }
        let expected: Vec<_> = (4..7).map(|n| event(numbered(n))).collect();
        assert_eq!(drain(&mut outbox), expected);
    }

    #[test]
    fn a_queued_notice_sums_later_drops() {
        let mut outbox = Outbox::with_limit(2);
        outbox.push_event(numbered(0));
        outbox.push_event(numbered(1));
        outbox.push_event(numbered(2)); // drops 0 and 1
        outbox.push(reply("r1"));
        outbox.push_event(numbered(3));
        outbox.push_event(numbered(4)); // drops 2 and 3
        outbox.push_event(numbered(5));
        outbox.push_event(numbered(6)); // drops 4 and 5
        // One notice, where the first drop put it, with every dropped event counted.
        assert_eq!(
            drain(&mut outbox),
            [dropped(6), reply("r1"), event(numbered(6))]
        );
        // A notice already sent isn't updated: the next drop queues a new one.
        outbox.push_event(numbered(7));
        outbox.push_event(numbered(8));
        outbox.push_event(numbered(9));
        assert_eq!(drain(&mut outbox), [dropped(2), event(numbered(9))]);
    }

    #[test]
    fn replies_are_never_dropped_but_stop_requests_when_too_many_wait() {
        let mut outbox = Outbox::with_limit(1);
        for n in 0..REPLY_BACKLOG {
            assert!(outbox.reads_requests());
            outbox.push(reply(&format!("r{n}")));
            outbox.push_event(numbered(n));
        }
        assert!(!outbox.reads_requests());
        let sent = drain(&mut outbox);
        let replies = sent
            .iter()
            .filter(|m| matches!(m, ServerMessage::Response(_)))
            .count();
        assert_eq!(replies, REPLY_BACKLOG);
        assert!(outbox.reads_requests());
    }

    #[test]
    fn unparseable_requests_keep_their_id() {
        assert_eq!(
            request_id(r#"{"type":"request","id":"c7","call":{"kind":"nope"}}"#),
            Some(RequestId::new("c7"))
        );
        assert_eq!(request_id(r#"{"type":"hello","id":"c7"}"#), None);
        assert_eq!(request_id("not json"), None);
        assert_eq!(request_id(r#"["request","c7"]"#), None);
        let long = "i".repeat(crate::envelope::MAX_ECHOED_ID + 1);
        assert_eq!(
            request_id(&format!(r#"{{"type":"request","id":"{long}"}}"#)),
            None
        );
    }
}
