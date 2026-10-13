//! The [`Control`] trait and the boxed future and stream types it returns.
//!
//! The trait is dyn-compatible: clients hold a `Box<dyn Control>` or `Arc<dyn Control>`
//! and never know whether the engine is in the same process or across a WebSocket. That
//! rules out `async fn` in the trait (not dyn-compatible on stable Rust), so each method
//! returns a [`BoxFuture`] instead. On native targets the futures, the stream and the
//! trait object are `Send` (and `Sync`), so a server can share one `Arc<dyn Control>`
//! across threads. On `wasm32` they are not, because browser WebSocket handles are not
//! `Send`; no executor is required on either side.

use crate::error::ControlError;
use futures_core::Stream;
use jstream_protocol::{Call, Event, Hello, Response, Welcome};
use std::future::Future;
use std::pin::Pin;

/// A boxed future, as returned by [`Control`] methods.
///
/// `Send` on native targets. On `wasm32` it has no `Send` bound, because futures there
/// hold JavaScript handles.
#[cfg(not(target_arch = "wasm32"))]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A boxed future, as returned by [`Control`] methods.
///
/// `Send` on native targets. On `wasm32` it has no `Send` bound, because futures there
/// hold JavaScript handles.
#[cfg(target_arch = "wasm32")]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// The events pushed by the engine, as returned by [`Control::events`].
///
/// `Send` on native targets, not on `wasm32` (see [`BoxFuture`]).
#[cfg(not(target_arch = "wasm32"))]
pub type EventStream = Pin<Box<dyn Stream<Item = Event> + Send>>;

/// The events pushed by the engine, as returned by [`Control::events`].
///
/// `Send` on native targets, not on `wasm32` (see [`BoxFuture`]).
#[cfg(target_arch = "wasm32")]
pub type EventStream = Pin<Box<dyn Stream<Item = Event>>>;

/// `Send + Sync` on native targets, nothing on `wasm32`. The supertrait of [`Control`].
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSendSync: Send + Sync {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> MaybeSendSync for T {}

/// `Send + Sync` on native targets, nothing on `wasm32`. The supertrait of [`Control`].
#[cfg(target_arch = "wasm32")]
pub trait MaybeSendSync {}

#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSendSync for T {}

/// One client session with the engine: the API every client is written against
/// (ADR-0001, R-ARCH-003).
///
/// A `Control` value is one connection identity: after [`hello`](Control::hello) it acts
/// as the `client_id` in the [`Welcome`], which keys its undo history and audit-log
/// entries (ADR-0013). Implementations: in-process by `jstream-core` (`InProcessControl`),
/// over the WebSocket API by this crate (`WsControl`, feature `ws`), and in memory by
/// `MockControl` behind the `mock` feature.
///
/// Every method takes `&self`, so one session can be shared (`Arc<dyn Control>`) and
/// several calls can be in flight at once. Answers to concurrent calls may complete in any
/// order.
pub trait Control: MaybeSendSync {
    /// Opens the session: sends `hello` and waits for `welcome`.
    ///
    /// Fails with `unsupported_version` if the engine can't serve the client's protocol
    /// version. Calling it again on an open session returns the same `client_id` with the
    /// current `rev`.
    fn hello(&self, hello: Hello) -> BoxFuture<'_, Result<Welcome, ControlError>>;

    /// Sends one call (a batch of edits, an action or a query) and waits for its answer.
    ///
    /// The implementation chooses the request ID and returns it in [`Response::id`], so the
    /// caller can match the `state.changed` event its batch causes. Before
    /// [`hello`](Control::hello) it fails with `handshake_required`.
    fn call(&self, call: Call) -> BoxFuture<'_, Result<Response, ControlError>>;

    /// Subscribes to the engine's events.
    ///
    /// The stream yields every event emitted after this call returns, in the order the
    /// engine emitted them, and buffers them until they are read. It ends when the
    /// session or the engine goes away. Each call opens an independent subscription.
    fn events(&self) -> EventStream;
}
