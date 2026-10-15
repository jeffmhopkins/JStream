//! The engine session the UI works through: connecting, the handshake, and the engine URL
//! a page derives from where it was served.
//!
//! Docs: crates/jstream-ui/README.md#design

use jstream_control::protocol::{Hello, PROTOCOL_VERSION, Welcome};
use jstream_control::{Control, ControlError, WS_PATH, WsConfig, WsControl};
use std::rc::Rc;

/// The `client_name` the UI sends in `hello`, shown in the engine's audit log.
pub const CLIENT_NAME: &str = "jstream-ui";

/// An open session: the engine connection after a successful `hello`.
///
/// Panels reach it through Leptos context (see [`crate::App`]) and call the engine with
/// [`Session::control`]. Cloning shares the connection; it closes when the last clone
/// goes.
#[derive(Clone)]
pub struct Session {
    /// The connection. Every call the UI makes goes through it.
    pub control: Rc<dyn Control>,
    /// The engine's answer to `hello`: its protocol version, this session's client ID
    /// and the revision the UI starts from.
    pub welcome: Welcome,
}

impl Session {
    /// Opens a session on `control`: sends `hello` with this UI's protocol version and
    /// client name.
    pub async fn open(control: Rc<dyn Control>) -> Result<Session, ControlError> {
        let welcome = control
            .hello(Hello {
                protocol: PROTOCOL_VERSION,
                client_name: CLIENT_NAME.into(),
            })
            .await?;
        Ok(Session { control, welcome })
    }

    /// Connects to the engine at `url` with the control token, then opens a session.
    pub async fn connect(url: &str, token: &str) -> Result<Session, ControlError> {
        let control = WsControl::connect(WsConfig::new(url, token)).await?;
        Session::open(Rc::new(control)).await
    }
}

/// The engine URL for a page served from `protocol` (`http:` or `https:`, as
/// `location.protocol` gives it) and `host` (`location.host`): the same host, `wss://`
/// for an `https:` page, and the API path. `None` when the page wasn't served over HTTP
/// (a `file:` page), so the user types the URL.
pub fn engine_url_for_page(protocol: &str, host: &str) -> Option<String> {
    let scheme = match protocol {
        "http:" => "ws",
        "https:" => "wss",
        _ => return None,
    };
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}{WS_PATH}"))
}

#[cfg(test)]
mod tests {
    use super::{CLIENT_NAME, Session, engine_url_for_page};
    use jstream_control::MockEngine;
    use jstream_control::protocol::PROTOCOL_VERSION;
    use std::pin::pin;
    use std::rc::Rc;
    use std::task::{Context, Poll, Waker};

    /// Polls a future that is ready at once, as the mock's are.
    fn now<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the mock's futures are ready at once"),
        }
    }

    #[test]
    fn a_session_opens_with_hello() {
        let engine = MockEngine::new();
        let session = now(Session::open(Rc::new(engine.connect())));
        let session = session.unwrap_or_else(|error| panic!("hello failed: {error}"));
        assert_eq!(session.welcome.protocol, PROTOCOL_VERSION);
        assert!(!session.welcome.client_id.as_str().is_empty());
        assert_eq!(CLIENT_NAME, "jstream-ui");
    }

    #[test]
    fn the_engine_url_follows_the_page() {
        assert_eq!(
            engine_url_for_page("http:", "127.0.0.1:4455").as_deref(),
            Some("ws://127.0.0.1:4455/api/v1/ws")
        );
        assert_eq!(
            engine_url_for_page("https:", "studio.example").as_deref(),
            Some("wss://studio.example/api/v1/ws")
        );
        assert_eq!(engine_url_for_page("file:", ""), None);
        assert_eq!(engine_url_for_page("http:", ""), None);
    }
}
