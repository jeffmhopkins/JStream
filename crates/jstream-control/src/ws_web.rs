//! [`WsControl`] on `wasm32`: the [`Control`] trait over the engine's WebSocket API through
//! the browser's `WebSocket` (feature `ws`).
//!
//! One connection is one session, as on native targets, but nothing here runs a runtime
//! or a thread: the browser calls the socket's handlers on its event loop, and they
//! complete the waiting calls and feed the event streams through `futures-channel`, which
//! any executor can poll (the UI's is `wasm-bindgen-futures`). Everything lives on the
//! page's one thread, so the shared state is an `Rc<RefCell<…>>`, and no borrow is held
//! across an `await` or a call into JavaScript that could re-enter a handler.
//!
//! Browsers can't set `Authorization` on a WebSocket, so a control token travels as the
//! `jstream.token.<token>` subprotocol next to `jstream`, which the server selects
//! ([`crate::WS_PROTOCOL`]). With an empty token no subprotocol is offered, and the
//! connection carries whatever credentials the browser already sends for the engine's
//! origin (HTTP Basic, when the engine served the page behind it). The browser sets
//! `Origin` to the page's own and handles `wss://` itself.
//!
//! Binary messages are read as array buffers and parsed as [`BinaryFrame`]s into the
//! channel [`Control::binary`] last opened, which holds at most [`BINARY_BUFFER`] of
//! them and drops up to a keyframe when full ([`KeyframeGate`]), as on native targets.
//!
//! Docs: crates/jstream-control/README.md#design, docs/adr/0006-mcp-full-access.md, docs/adr/0045-leptos-web-ui.md

use crate::WS_PROTOCOL;
use crate::control::{BinaryStream, BoxFuture, Control, EventStream};
use crate::error::ControlError;
use crate::ws_config::{
    SocketEnd, WsConfig, browser_url, disconnected, end_reason, token_protocol,
};
use futures_channel::{mpsc, oneshot};
use futures_core::Stream;
use jstream_protocol::{
    BinaryFrame, Call, ClientMessage, Event, Hello, KeyframeGate, Request, RequestId, Response,
    ServerMessage, Welcome,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, CloseEvent, Event as DomEvent, MessageEvent, WebSocket};

/// One client session with an engine over its WebSocket API, in a browser.
///
/// Dropping it closes the connection. Request IDs are `req_1`, `req_2`, … per session,
/// and the server reports them in the `state.changed` events the calls cause.
pub struct WsControl {
    socket: WebSocket,
    state: Rc<RefCell<State>>,
    next_request: Cell<u64>,
    /// The socket's handlers, kept alive for as long as the socket can call them.
    _handlers: Handlers,
}

impl fmt::Debug for WsControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WsControl")
            .field("closed", &self.state.borrow().closed)
            .finish_non_exhaustive()
    }
}

/// The closures installed as the socket's `on…` handlers.
struct Handlers {
    _open: Closure<dyn FnMut(DomEvent)>,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _close: Closure<dyn FnMut(CloseEvent)>,
    _error: Closure<dyn FnMut(DomEvent)>,
}

/// What the handle and the socket's handlers share.
#[derive(Default)]
struct State {
    /// Told the outcome of the opening handshake, until it is known.
    connecting: Option<oneshot::Sender<Result<(), ControlError>>>,
    /// The calls waiting for an answer, by request ID.
    calls: HashMap<RequestId, oneshot::Sender<Result<Response, ControlError>>>,
    /// `welcome` carries no ID: handshakes are answered in order.
    hellos: VecDeque<oneshot::Sender<Result<Welcome, ControlError>>>,
    /// The live event subscriptions; `None` once the connection has closed.
    subscribers: Option<Vec<mpsc::UnboundedSender<Event>>>,
    /// The binary channel [`Control::binary`] last opened.
    binary: Option<Rc<RefCell<BinaryQueue>>>,
    /// Why the connection closed, once it has.
    closed: Option<String>,
}

impl State {
    /// `Disconnected`, with the reason the connection closed if it is known.
    fn disconnected(&self) -> ControlError {
        disconnected(
            self.closed
                .clone()
                .unwrap_or_else(|| "the connection closed".into()),
        )
    }

    /// Records why the connection closed, fails whatever is still waiting and ends the
    /// event streams once they have yielded what they buffered. Only the first reason
    /// is kept, so a `close` after an `error` (or a second of either) changes nothing.
    fn close(&mut self, reason: String) {
        if self.closed.is_some() {
            return;
        }
        // A caller that stopped waiting needs no answer, so failed sends are ignored.
        if let Some(tx) = self.connecting.take() {
            let _ = tx.send(Err(disconnected(reason.clone())));
        }
        for (_, tx) in self.calls.drain() {
            let _ = tx.send(Err(disconnected(reason.clone())));
        }
        for tx in self.hellos.drain(..) {
            let _ = tx.send(Err(disconnected(reason.clone())));
        }
        self.subscribers = None;
        if let Some(queue) = self.binary.take() {
            let waker = queue.borrow_mut().close();
            wake(waker);
        }
        self.closed = Some(reason);
    }
}

impl WsControl {
    /// Connects to the engine and opens a session. `hello` is still the caller's first
    /// call.
    ///
    /// Fails with [`ControlError::Disconnected`] if the URL is not a `ws://` or `wss://`
    /// URL, the token can't be carried in a subprotocol, or the connection fails. A
    /// browser doesn't tell a page why an upgrade was refused, so a wrong token, a
    /// refused `Origin` and an unreachable engine fail alike.
    pub fn connect(config: WsConfig) -> BoxFuture<'static, Result<WsControl, ControlError>> {
        let opened = WsControl::open(&config);
        Box::pin(async move {
            let (control, ready) = opened?;
            ready
                .await
                .unwrap_or_else(|_| Err(disconnected("the connection was abandoned")))?;
            Ok(control)
        })
    }

    /// Creates the socket and installs its handlers. The receiver learns whether the
    /// opening handshake succeeded.
    fn open(
        config: &WsConfig,
    ) -> Result<(WsControl, oneshot::Receiver<Result<(), ControlError>>), ControlError> {
        let url = browser_url(&config.url)?;
        // With a token, offer it as a subprotocol. Without one, offer none: the page relies
        // on credentials the browser already sends for the engine's origin.
        let with_token = !config.token.is_empty();
        let socket = if with_token {
            let protocols =
                js_sys::Array::of2(&WS_PROTOCOL.into(), &token_protocol(&config.token)?.into());
            WebSocket::new_with_str_sequence(&url, &protocols)
        } else {
            WebSocket::new(&url)
        }
        .map_err(|_| disconnected(format!("the browser refused to open `{url}`")))?;
        // Preview frames arrive as array buffers, which are read at once.
        socket.set_binary_type(BinaryType::Arraybuffer);

        let (ready_tx, ready_rx) = oneshot::channel();
        let state = Rc::new(RefCell::new(State {
            connecting: Some(ready_tx),
            subscribers: Some(Vec::new()),
            ..State::default()
        }));

        let open = {
            let state = Rc::clone(&state);
            let socket = socket.clone();
            Closure::<dyn FnMut(DomEvent)>::new(move |_: DomEvent| {
                let selected = socket.protocol();
                if with_token && selected != WS_PROTOCOL {
                    state.borrow_mut().close(format!(
                        "the engine selected subprotocol `{selected}`, not `{WS_PROTOCOL}`"
                    ));
                    // Best effort: the socket may already be closing.
                    let _ = socket.close();
                    return;
                }
                let mut state = state.borrow_mut();
                if let Some(tx) = state.connecting.take() {
                    let _ = tx.send(Ok(()));
                }
            })
        };
        let message = {
            let state = Rc::clone(&state);
            let socket = socket.clone();
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let data = event.data();
                let Some(text) = data.as_string() else {
                    if let Some(buffer) = data.dyn_ref::<js_sys::ArrayBuffer>() {
                        let bytes = js_sys::Uint8Array::new(buffer).to_vec();
                        // Out of the borrow before waking: a waker may poll at once.
                        let queue = state.borrow().binary.clone();
                        if let Some(queue) = queue {
                            deliver(&bytes, &queue);
                        }
                    }
                    return;
                };
                let outcome = dispatch(&text, &mut state.borrow_mut());
                if let Err(reason) = outcome {
                    // Best effort: the socket may already be closing.
                    let _ = socket.close();
                    state.borrow_mut().close(reason);
                }
            })
        };
        let close = {
            let state = Rc::clone(&state);
            let url = url.clone();
            Closure::<dyn FnMut(CloseEvent)>::new(move |event: CloseEvent| {
                let mut state = state.borrow_mut();
                let reason = event.reason();
                let end = SocketEnd::Close { reason: &reason };
                let reason = end_reason(&url, state.connecting.is_some(), end);
                state.close(reason);
            })
        };
        // `error` is usually followed by `close`, but not when the page's Content Security
        // Policy refuses the URL: Chromium then fires only `error` and leaves the socket
        // `CLOSED`. So `error` ends the session itself; `State::close` keeps the first
        // reason, and a later `close` changes nothing.
        let error = {
            let state = Rc::clone(&state);
            Closure::<dyn FnMut(DomEvent)>::new(move |_: DomEvent| {
                let mut state = state.borrow_mut();
                let reason = end_reason(&url, state.connecting.is_some(), SocketEnd::Error);
                state.close(reason);
            })
        };
        socket.set_onopen(Some(open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(close.as_ref().unchecked_ref()));
        socket.set_onerror(Some(error.as_ref().unchecked_ref()));

        let control = WsControl {
            socket,
            state,
            next_request: Cell::new(1),
            _handlers: Handlers {
                _open: open,
                _message: message,
                _close: close,
                _error: error,
            },
        };
        Ok((control, ready_rx))
    }

    /// Sends one message, or explains why it couldn't be sent.
    fn send(&self, message: &ClientMessage) -> Result<(), ControlError> {
        let text = serde_json::to_string(message)
            .map_err(|error| disconnected(format!("could not encode a request: {error}")))?;
        self.socket
            .send_with_str(&text)
            .map_err(|_| disconnected("could not send to the engine"))
    }
}

impl Drop for WsControl {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        self.socket.set_onerror(None);
        // Best effort: the socket may already be closed.
        let _ = self.socket.close();
        self.state
            .borrow_mut()
            .close("the session was closed".to_owned());
    }
}

impl Control for WsControl {
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>> {
        let (tx, rx) = oneshot::channel();
        let sent = {
            let mut state = self.state.borrow_mut();
            if state.closed.is_some() {
                Err(state.disconnected())
            } else {
                state.hellos.push_back(tx);
                Ok(())
            }
        };
        let sent = sent.and_then(|()| {
            self.send(&ClientMessage::Hello(hello)).inspect_err(|_| {
                self.state.borrow_mut().hellos.pop_back();
            })
        });
        let state = Rc::clone(&self.state);
        Box::pin(async move {
            sent?;
            rx.await
                .unwrap_or_else(|_| Err(state.borrow().disconnected()))
        })
    }

    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>> {
        let number = self.next_request.get();
        self.next_request.set(number + 1);
        let id = RequestId::new(format!("req_{number}"));
        let (tx, rx) = oneshot::channel();
        let sent = {
            let mut state = self.state.borrow_mut();
            if state.closed.is_some() {
                Err(state.disconnected())
            } else {
                state.calls.insert(id.clone(), tx);
                Ok(())
            }
        };
        let sent = sent.and_then(|()| {
            let request = ClientMessage::Request(Request {
                id: id.clone(),
                call,
            });
            self.send(&request).inspect_err(|_| {
                self.state.borrow_mut().calls.remove(&id);
            })
        });
        let state = Rc::clone(&self.state);
        Box::pin(async move {
            sent?;
            rx.await
                .unwrap_or_else(|_| Err(state.borrow().disconnected()))
        })
    }

    fn events(&self) -> EventStream {
        let (tx, rx) = mpsc::unbounded();
        if let Some(subscribers) = self.state.borrow_mut().subscribers.as_mut() {
            subscribers.push(tx);
        }
        // Once closed, `tx` is dropped here and the stream ends at once.
        Box::pin(rx)
    }

    fn binary(&self) -> BinaryStream {
        let queue = Rc::new(RefCell::new(BinaryQueue::default()));
        let old = {
            let mut state = self.state.borrow_mut();
            if state.closed.is_some() {
                queue.borrow_mut().closed = true;
                None
            } else {
                state.binary.replace(Rc::clone(&queue))
            }
        };
        if let Some(old) = old {
            let waker = old.borrow_mut().close();
            wake(waker);
        }
        Box::pin(BinaryChannel(queue))
    }
}

/// How many binary frames the channel holds before it drops up to a keyframe.
const BINARY_BUFFER: usize = 16;

/// The binary channel's frames, shared by the handle's stream and the socket's handler.
#[derive(Default)]
struct BinaryQueue {
    frames: VecDeque<BinaryFrame>,
    gate: KeyframeGate,
    waker: Option<Waker>,
    closed: bool,
}

impl BinaryQueue {
    /// Queues `frame` unless the gate drops it, and returns the waker to wake.
    fn push(&mut self, frame: BinaryFrame) -> Option<Waker> {
        let room = self.frames.len() < BINARY_BUFFER;
        if self.gate.admit(&frame.header, room) {
            self.frames.push_back(frame);
            self.waker.take()
        } else {
            None
        }
    }

    /// Ends the stream once it has yielded what it holds.
    fn close(&mut self) -> Option<Waker> {
        self.closed = true;
        self.waker.take()
    }
}

/// The binary channel's stream.
struct BinaryChannel(Rc<RefCell<BinaryQueue>>);

impl Stream for BinaryChannel {
    type Item = BinaryFrame;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<BinaryFrame>> {
        let mut queue = self.0.borrow_mut();
        if let Some(frame) = queue.frames.pop_front() {
            Poll::Ready(Some(frame))
        } else if queue.closed {
            Poll::Ready(None)
        } else {
            queue.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

/// Wakes `waker`, if there is one, with no borrow held: a waker may poll at once.
fn wake(waker: Option<Waker>) {
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// Queues one binary message for the binary channel. A frame this client can't read
/// is skipped, as the protocol asks.
fn deliver(bytes: &[u8], queue: &RefCell<BinaryQueue>) {
    let Ok(frame) = BinaryFrame::parse(bytes) else {
        return;
    };
    let waker = queue.borrow_mut().push(frame);
    wake(waker);
}

/// Routes one message from the engine. An unreadable message ends the session (`Err`
/// with the reason).
fn dispatch(text: &str, state: &mut State) -> Result<(), String> {
    let message: ServerMessage = serde_json::from_str(text)
        .map_err(|error| format!("the engine sent an unreadable message: {error}"))?;
    // A caller that stopped waiting needs no answer, so failed sends are ignored.
    match message {
        ServerMessage::Welcome(welcome) => {
            if let Some(tx) = state.hellos.pop_front() {
                let _ = tx.send(Ok(welcome));
            }
        }
        ServerMessage::Response(response) => {
            if let Some(tx) = state.calls.remove(&response.id) {
                let _ = tx.send(Ok(response));
            }
        }
        ServerMessage::Error(error) => match error.id {
            Some(id) => {
                if let Some(tx) = state.calls.remove(&id) {
                    let _ = tx.send(Err(ControlError::Protocol(error.error)));
                }
            }
            // An error without an ID answers a handshake (or a message too broken to
            // read, which this client never sends).
            None => {
                if let Some(tx) = state.hellos.pop_front() {
                    let _ = tx.send(Err(ControlError::Protocol(error.error)));
                }
            }
        },
        ServerMessage::Event(message) => {
            if let Some(subscribers) = state.subscribers.as_mut() {
                subscribers.retain(|tx| tx.unbounded_send(message.event.clone()).is_ok());
            }
        }
        // A message type from a newer server: ignored, as the protocol asks.
        _ => {}
    }
    Ok(())
}
