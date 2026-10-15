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
//! Browsers can't set `Authorization` on a WebSocket, so the control token travels as the
//! `jstream.token.<token>` subprotocol next to `jstream`, which the server selects
//! ([`crate::WS_PROTOCOL`]). The browser sets `Origin` to the page's own and handles
//! `wss://` itself.
//!
//! Docs: crates/jstream-control/README.md#design, docs/adr/0006-mcp-full-access.md

use crate::WS_PROTOCOL;
use crate::control::{BoxFuture, Control, EventStream};
use crate::error::ControlError;
use crate::ws_config::{WsConfig, browser_url, disconnected, token_protocol};
use futures_channel::{mpsc, oneshot};
use jstream_protocol::{
    Call, ClientMessage, Event, Hello, Request, RequestId, Response, ServerMessage, Welcome,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::rc::Rc;
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
    /// is kept.
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
        let protocols =
            js_sys::Array::of2(&WS_PROTOCOL.into(), &token_protocol(&config.token)?.into());
        let socket = WebSocket::new_with_str_sequence(&url, &protocols)
            .map_err(|_| disconnected(format!("the browser refused to open `{url}`")))?;
        // Binary frames (the preview stream) are not designed yet; array buffers keep
        // them out of the blob store until they are.
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
                if selected != WS_PROTOCOL {
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
                // Binary frames are not designed yet, and are ignored.
                let Some(text) = event.data().as_string() else {
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
                let reason = if state.connecting.is_some() {
                    format!(
                        "could not connect to the engine at `{url}`: it is unreachable, or \
                         it refused the token or this page's origin"
                    )
                } else if event.reason().is_empty() {
                    "the engine closed the connection".to_owned()
                } else {
                    format!("the engine closed the connection: {}", event.reason())
                };
                state.close(reason);
            })
        };
        socket.set_onopen(Some(open.as_ref().unchecked_ref()));
        socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(close.as_ref().unchecked_ref()));
        // An `error` event is always followed by `close`, which reports it.

        let control = WsControl {
            socket,
            state,
            next_request: Cell::new(1),
            _handlers: Handlers {
                _open: open,
                _message: message,
                _close: close,
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
