//! Global hotkeys on Wayland through the xdg-desktop-portal
//! [GlobalShortcuts](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html)
//! interface ([ADR-0112](../../../../docs/adr/0112-assignable-camera-zoom-bindings.md) §5).
//!
//! Wayland lets no client grab keys, so the desktop app asks the compositor instead: it
//! opens a GlobalShortcuts session and binds one shortcut per chord, whose id is the
//! chord as the protocol spells it (`Ctrl+Shift+KeyA`) and whose preferred trigger is
//! the same chord in the specification's shortcut format (`CTRL+SHIFT+a`). The desktop
//! shows its own confirmation, and the person may assign other keys there; whatever they
//! pick, the `Activated` signal names the id, so the press runs the chord's binding.
//! Wheel chords can't be shortcuts: they stay heard only while JStream has the focus.
//!
//! The calls and signals are built and read here without a bus, so they are tested
//! everywhere; [`PortalHotkeys`] runs them on the session bus, on Linux, on a thread of
//! its own.

use jstream_platform::PlatformError;

use super::handshake::{DESTINATION, OBJECT};
use crate::dbus::{Message, Value};

/// The GlobalShortcuts interface.
pub(crate) const GLOBAL_SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";

/// The application id the app registers with the portal's host registry, so a desktop
/// that keys its grants by application (GNOME) remembers them: the Tauri identifier.
#[cfg(target_os = "linux")]
pub(crate) const APP_ID: &str = "dev.jstream.desktop";

/// A chord (as the protocol spells it) as a preferred trigger in the shortcuts
/// specification's format: `CTRL`, `ALT`, `SHIFT` and `LOGO`, then the key's XKB keysym
/// name, joined with `+`. `None` for a wheel chord or a key with no keysym here.
pub fn shortcut_trigger(chord: &str) -> Option<String> {
    let mut parts: Vec<&str> = chord.split('+').collect();
    let key = keysym(parts.pop()?)?;
    let mut out = Vec::with_capacity(parts.len() + 1);
    for (name, spec) in [
        ("Ctrl", "CTRL"),
        ("Alt", "ALT"),
        ("Shift", "SHIFT"),
        ("Meta", "LOGO"),
    ] {
        match parts.iter().filter(|p| **p == name).count() {
            0 => {}
            1 => out.push(spec),
            _ => return None,
        }
    }
    if parts.len() != out.len() {
        return None; // a modifier the protocol doesn't have
    }
    let mut trigger = out.join("+");
    if !trigger.is_empty() {
        trigger.push('+');
    }
    trigger.push_str(&key);
    Some(trigger)
}

/// A chord key's XKB keysym name.
fn keysym(code: &str) -> Option<String> {
    if let Some(letter) = code.strip_prefix("Key") {
        let mut chars = letter.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_uppercase() => Some(c.to_ascii_lowercase().to_string()),
            _ => None,
        };
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        return (digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit())
            .then(|| digit.to_owned());
    }
    if let Some(n) = code.strip_prefix('F')
        && let Ok(n) = n.parse::<u8>()
    {
        return (1..=24).contains(&n).then(|| format!("F{n}"));
    }
    if let Some(n) = code.strip_prefix("Numpad")
        && n.len() == 1
        && n.as_bytes()[0].is_ascii_digit()
    {
        return Some(format!("KP_{n}"));
    }
    Some(
        match code {
            "Escape" => "Escape",
            "Minus" => "minus",
            "Equal" => "equal",
            "Backspace" => "BackSpace",
            "Tab" => "Tab",
            "BracketLeft" => "bracketleft",
            "BracketRight" => "bracketright",
            "Enter" => "Return",
            "Semicolon" => "semicolon",
            "Quote" => "apostrophe",
            "Backquote" => "grave",
            "Backslash" => "backslash",
            "Comma" => "comma",
            "Period" => "period",
            "Slash" => "slash",
            "NumpadMultiply" => "KP_Multiply",
            "Space" => "space",
            "ScrollLock" => "Scroll_Lock",
            "NumpadSubtract" => "KP_Subtract",
            "NumpadAdd" => "KP_Add",
            "NumpadDecimal" => "KP_Decimal",
            "NumpadEnter" => "KP_Enter",
            "NumpadDivide" => "KP_Divide",
            "PrintScreen" => "Print",
            "Home" => "Home",
            "ArrowUp" => "Up",
            "PageUp" => "Page_Up",
            "ArrowLeft" => "Left",
            "ArrowRight" => "Right",
            "End" => "End",
            "ArrowDown" => "Down",
            "PageDown" => "Page_Down",
            "Insert" => "Insert",
            "Delete" => "Delete",
            "AudioVolumeMute" => "XF86AudioMute",
            "AudioVolumeDown" => "XF86AudioLowerVolume",
            "AudioVolumeUp" => "XF86AudioRaiseVolume",
            "Pause" => "Pause",
            "MediaTrackNext" => "XF86AudioNext",
            "MediaPlayPause" => "XF86AudioPlay",
            "MediaTrackPrevious" => "XF86AudioPrev",
            "MediaStop" => "XF86AudioStop",
            _ => return None,
        }
        .to_owned(),
    )
}

/// Chords, each with a second string: its trigger, or why it can't be bound.
type ChordPairs = Vec<(String, String)>;

/// Splits `chords` into the shortcuts the portal can take, each with its trigger, and
/// the ones it can't, each with the reason.
pub(crate) fn shortcuts(chords: &[String]) -> (ChordPairs, ChordPairs) {
    let mut bound = Vec::new();
    let mut refused = Vec::new();
    for chord in chords {
        if chord.ends_with("WheelUp") || chord.ends_with("WheelDown") {
            refused.push((
                chord.clone(),
                "Wayland has no global wheel chords: it is heard only while JStream has \
                 the focus"
                    .to_owned(),
            ));
        } else if let Some(trigger) = shortcut_trigger(chord) {
            if !bound.iter().any(|(c, _)| c == chord) {
                bound.push((chord.clone(), trigger));
            }
        } else {
            refused.push((
                chord.clone(),
                "the portal has no name for this key".to_owned(),
            ));
        }
    }
    (bound, refused)
}

/// The host registry's `Register` call, which names the app to the portal. Older portals
/// lack it, which is fine: they key grants by other means.
#[cfg(target_os = "linux")]
pub(crate) fn register_app() -> Message {
    Message::call(
        DESTINATION,
        OBJECT,
        "org.freedesktop.host.portal.Registry",
        "Register",
        vec![Value::Str(APP_ID.into()), Value::dict(Vec::new())],
    )
}

/// `CreateSession`, its request answering on `handle_token` and the session on
/// `session_token`.
pub(crate) fn create_session(handle_token: &str, session_token: &str) -> Message {
    Message::call(
        DESTINATION,
        OBJECT,
        GLOBAL_SHORTCUTS,
        "CreateSession",
        vec![Value::dict(vec![
            ("handle_token", Value::Str(handle_token.into())),
            ("session_handle_token", Value::Str(session_token.into())),
        ])],
    )
}

/// `BindShortcuts` on `session` for `shortcuts` (each chord and its trigger), its
/// request answering on `handle_token`.
pub(crate) fn bind_shortcuts(
    session: &str,
    shortcuts: &[(String, String)],
    handle_token: &str,
) -> Message {
    let list = shortcuts
        .iter()
        .map(|(chord, trigger)| {
            Value::Struct(vec![
                Value::Str(chord.clone()),
                Value::dict(vec![
                    ("description", Value::Str(format!("JStream hotkey {chord}"))),
                    ("preferred_trigger", Value::Str(trigger.clone())),
                ]),
            ])
        })
        .collect();
    Message::call(
        DESTINATION,
        OBJECT,
        GLOBAL_SHORTCUTS,
        "BindShortcuts",
        vec![
            Value::Path(session.into()),
            Value::Array("(sa{sv})".into(), list),
            Value::Str(String::new()), // no parent window
            Value::dict(vec![("handle_token", Value::Str(handle_token.into()))]),
        ],
    )
}

/// The shortcut id an `Activated` signal names, when `msg` is one from `portal` (its
/// unique name) for `session`.
pub(crate) fn activated<'a>(msg: &'a Message, portal: &str, session: &str) -> Option<&'a str> {
    if !msg.is_signal(GLOBAL_SHORTCUTS, "Activated") || msg.sender.as_deref() != Some(portal) {
        return None;
    }
    match msg.body.as_slice() {
        [handle, id, ..] if handle.as_str() == Some(session) => id.as_str(),
        _ => None,
    }
}

/// Reads a request's `Response` body, `(u, a{sv})`: its results, or why it failed.
pub(crate) fn response_results(
    body: &[Value],
    what: &'static str,
) -> Result<Vec<(String, Value)>, PlatformError> {
    let bad = |message: &str| PlatformError::Os {
        call: what,
        code: None,
        message: message.into(),
    };
    let [code, results] = body else {
        return Err(bad("a malformed Response"));
    };
    match code.as_u32() {
        Some(0) => {}
        Some(1) => {
            return Err(PlatformError::Os {
                call: what,
                code: Some(1),
                message: "the person declined the shortcuts".into(),
            });
        }
        Some(other) => {
            return Err(PlatformError::Os {
                call: what,
                code: Some(other),
                message: "the desktop portal ended the request".into(),
            });
        }
        None => return Err(bad("no response code")),
    }
    let results = results.as_dict().ok_or_else(|| bad("no results"))?;
    Ok(results
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.clone()))
        .collect())
}

/// What the Wayland listener reports to its caller, which logs it: this crate logs
/// nothing itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(target_os = "linux")]
pub enum PortalHotkeyEvent<'a> {
    /// A shortcut was activated: its chord, as the protocol spells it.
    Pressed(&'a str),
    /// The desktop bound this many shortcuts.
    Bound(usize),
    /// The desktop didn't bind them (the person declined, or the portal failed): they
    /// are heard only while JStream has the focus.
    NotBound(&'a str),
    /// The bus connection broke: the listener stopped.
    Stopped(&'a str),
}

#[cfg(target_os = "linux")]
pub use listener::PortalHotkeys;

#[cfg(target_os = "linux")]
mod listener {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use jstream_platform::PlatformError;

    use super::super::handshake::{PICKER, QUICK, REQUEST, close_session, portal_owner_query};
    use super::super::{handle_token, request_path};
    use super::{
        DESTINATION, GLOBAL_SHORTCUTS, PortalHotkeyEvent, activated, bind_shortcuts,
        create_session, register_app, response_results, shortcuts,
    };
    use crate::dbus::bus::{BUS, Bus};
    use crate::dbus::{Message, Value, add_match, match_rule};

    /// How often the thread looks for a new set of chords between signals.
    const POLL: Duration = Duration::from_millis(100);

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn token(what: &str) -> String {
        format!(
            "{}_{what}",
            handle_token(NEXT.fetch_add(1, Ordering::Relaxed))
        )
    }

    fn no_answer(call: &'static str, message: &str) -> PlatformError {
        PlatformError::Os {
            call,
            code: None,
            message: message.into(),
        }
    }

    /// The desktop app's global hotkeys on Wayland: a GlobalShortcuts session on a
    /// thread of its own, which binds the chords [`set`](Self::set) asks for and calls
    /// back with each one activated. Dropping it closes the session.
    ///
    /// The thread does one thing at a time: while the desktop's confirmation for a new
    /// set of chords is open (up to five minutes), it neither reports presses nor takes
    /// the next set, which it binds once the confirmation is answered. Dropping it during
    /// a confirmation stops the wait within a poll.
    pub struct PortalHotkeys {
        chords: Option<Sender<Vec<String>>>,
        stopping: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl std::fmt::Debug for PortalHotkeys {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PortalHotkeys").finish_non_exhaustive()
        }
    }

    impl PortalHotkeys {
        /// Connects to the session bus and checks that the portal offers
        /// GlobalShortcuts, then listens on a thread of its own, calling `on_event` with
        /// the chord of each shortcut activated and with what happens to a binding.
        ///
        /// # Errors
        /// Without a session bus, a running portal or its GlobalShortcuts interface.
        pub fn start(
            on_event: impl Fn(PortalHotkeyEvent<'_>) + Send + 'static,
        ) -> Result<Self, PlatformError> {
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let (chords_tx, chords_rx) = mpsc::channel();
            let stopping = Arc::new(AtomicBool::new(false));
            let stop = Arc::clone(&stopping);
            let thread = std::thread::Builder::new()
                .name("jstream-portal-hotkeys".into())
                .spawn(move || {
                    let mut bus = match connect() {
                        Ok(connected) => {
                            let _ = ready_tx.send(Ok(()));
                            connected
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error));
                            return;
                        }
                    };
                    listen(&mut bus, &chords_rx, &stop, &on_event);
                })
                .map_err(|e| no_answer("starting the hotkey thread", &e.to_string()))?;
            match ready_rx.recv_timeout(QUICK + QUICK) {
                Ok(Ok(())) => Ok(Self {
                    chords: Some(chords_tx),
                    stopping,
                    thread: Some(thread),
                }),
                Ok(Err(error)) => {
                    let _ = thread.join();
                    Err(error)
                }
                Err(_) => Err(no_answer(
                    GLOBAL_SHORTCUTS,
                    "the portal didn't answer in time",
                )),
            }
        }

        /// Binds `chords` (as the protocol spells them) in place of the ones before, and
        /// returns each one the portal can't take (wheel chords, keys it has no name for),
        /// with the reason. The binding itself happens on the listener's thread, which
        /// may show the desktop's confirmation; a refusal there is logged.
        pub fn set(&self, chords: Vec<String>) -> Vec<(String, String)> {
            let (_, refused) = shortcuts(&chords);
            if let Some(tx) = &self.chords {
                let _ = tx.send(chords);
            }
            refused
        }
    }

    impl Drop for PortalHotkeys {
        fn drop(&mut self) {
            // Closing the channel ends the thread's loop within a poll, and the flag ends a
            // wait for the desktop's confirmation.
            self.stopping.store(true, Ordering::Relaxed);
            self.chords.take();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The thread's bus: connected, the app registered (best effort), the portal found
    /// and checked to offer GlobalShortcuts, and the signals it needs matched.
    struct Connected {
        bus: Bus,
        portal: String,
        session: Option<String>,
    }

    fn connect() -> Result<Connected, PlatformError> {
        let mut bus = Bus::session()?;
        // Must come before any other portal call on this connection; an older portal
        // answers with an error, which changes nothing.
        let _ = bus.call_reply(&register_app(), None, "Register", QUICK);
        let owner = bus.call(
            portal_owner_query(),
            Some(BUS),
            "org.freedesktop.DBus.GetNameOwner",
            QUICK,
        )?;
        let portal = owner
            .msg
            .body
            .first()
            .and_then(Value::as_str)
            .filter(|name| name.starts_with(':'))
            .map(str::to_owned)
            .ok_or_else(|| no_answer(GLOBAL_SHORTCUTS, "the desktop portal isn't running"))?;
        let version = Message::call(
            DESTINATION,
            super::OBJECT,
            "org.freedesktop.DBus.Properties",
            "Get",
            vec![
                Value::Str(GLOBAL_SHORTCUTS.into()),
                Value::Str("version".into()),
            ],
        );
        bus.call(version, Some(&portal), GLOBAL_SHORTCUTS, QUICK)
            .map_err(|_| {
                no_answer(
                    GLOBAL_SHORTCUTS,
                    "the desktop portal has no GlobalShortcuts interface",
                )
            })?;
        for rule in [
            match_rule(DESTINATION, REQUEST, "Response"),
            match_rule(DESTINATION, GLOBAL_SHORTCUTS, "Activated"),
        ] {
            bus.call(
                add_match(&rule),
                Some(BUS),
                "org.freedesktop.DBus.AddMatch",
                QUICK,
            )?;
        }
        Ok(Connected {
            bus,
            portal,
            session: None,
        })
    }

    /// Runs until `chords` closes: binds each new set, and calls `on_press` with each
    /// activated chord.
    fn listen(
        c: &mut Connected,
        chords: &Receiver<Vec<String>>,
        stop: &AtomicBool,
        on_event: &dyn Fn(PortalHotkeyEvent<'_>),
    ) {
        loop {
            // The newest set wins; older ones queued behind it are skipped.
            let mut wanted = None;
            loop {
                match chords.try_recv() {
                    Ok(set) => wanted = Some(set),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        close(c);
                        return;
                    }
                }
            }
            if let Some(set) = wanted {
                bind(c, &set, stop, on_event);
            }
            let deadline = Instant::now() + POLL;
            let Some(session) = c.session.clone() else {
                // Nothing bound: wait for a set.
                match chords.recv_timeout(POLL) {
                    Ok(set) => bind(c, &set, stop, on_event),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                continue;
            };
            let portal = c.portal.clone();
            match c
                .bus
                .wait_signal(deadline, |m| activated(m, &portal, &session).is_some())
            {
                Ok(Some(msg)) => {
                    if let Some(chord) = activated(&msg, &portal, &session) {
                        on_event(PortalHotkeyEvent::Pressed(chord));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    on_event(PortalHotkeyEvent::Stopped(&error.to_string()));
                    return;
                }
            }
        }
    }

    /// [`rebind`]s `chords` and reports how it went.
    fn bind(
        c: &mut Connected,
        chords: &[String],
        stop: &AtomicBool,
        on_event: &dyn Fn(PortalHotkeyEvent<'_>),
    ) {
        match rebind(c, chords, stop) {
            Ok(0) => {}
            Ok(count) => on_event(PortalHotkeyEvent::Bound(count)),
            // Stopping: the wait was cut short, which is no refusal.
            Err(_) if stop.load(Ordering::Relaxed) => {}
            Err(error) => on_event(PortalHotkeyEvent::NotBound(&error.to_string())),
        }
    }

    /// Closes the current session and, when `chords` has any the portal can take, opens a
    /// new one and binds them, answering how many. A new session each time, as not every
    /// desktop lets a session bind twice.
    fn rebind(
        c: &mut Connected,
        chords: &[String],
        stop: &AtomicBool,
    ) -> Result<usize, PlatformError> {
        close(c);
        let (bound, _) = shortcuts(chords);
        if bound.is_empty() {
            return Ok(0);
        }
        let results = request(c, create_session(&token("h"), &token("s")), QUICK, stop)?;
        let session = results
            .iter()
            .find(|(k, _)| k == "session_handle")
            .and_then(|(_, v)| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| no_answer(GLOBAL_SHORTCUTS, "no session_handle"))?;
        c.session = Some(session.clone());
        if let Err(error) = request(
            c,
            bind_shortcuts(&session, &bound, &token("b")),
            PICKER,
            stop,
        ) {
            // Nothing is bound in it, so the session goes too.
            close(c);
            return Err(error);
        }
        Ok(bound.len())
    }

    /// Calls a portal request and waits up to `timeout` for its `Response`, giving up
    /// within a poll once `stop` is set.
    fn request(
        c: &mut Connected,
        call: Message,
        timeout: Duration,
        stop: &AtomicBool,
    ) -> Result<Vec<(String, Value)>, PlatformError> {
        let token = match call.body.last().and_then(Value::as_dict) {
            Some(options) => options
                .iter()
                .find(|(k, _)| *k == "handle_token")
                .and_then(|(_, v)| v.as_str())
                .map(str::to_owned),
            None => None,
        };
        let mut expected: Vec<String> = token
            .and_then(|t| request_path(c.bus.unique_name(), &t))
            .into_iter()
            .collect();
        let reply = c.bus.call(call, Some(&c.portal), GLOBAL_SHORTCUTS, QUICK)?;
        if let Some(handle) = reply.msg.body.first().and_then(Value::as_str) {
            expected.push(handle.to_owned());
        }
        let portal = c.portal.clone();
        let deadline = Instant::now() + timeout;
        // Waited for a poll at a time, so a stop doesn't wait out the confirmation; a
        // signal that arrives meanwhile stays queued on the bus.
        let response = loop {
            if stop.load(Ordering::Relaxed) {
                return Err(no_answer(GLOBAL_SHORTCUTS, "the listener is stopping"));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(no_answer(
                    GLOBAL_SHORTCUTS,
                    "no answer from the desktop in time",
                ));
            }
            if let Some(response) = c.bus.wait_signal((now + POLL).min(deadline), |m| {
                m.is_signal(REQUEST, "Response")
                    && m.sender.as_deref() == Some(portal.as_str())
                    && m.path.as_ref().is_some_and(|p| expected.contains(p))
            })? {
                break response;
            }
        };
        response_results(&response.body, GLOBAL_SHORTCUTS)
    }

    /// Closes the current session, if there is one. Best effort: it also closes with the
    /// connection.
    fn close(c: &mut Connected) {
        if let Some(session) = c.session.take() {
            let _ = c.bus.send(&close_session(&session));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        activated, bind_shortcuts, create_session, response_results, shortcut_trigger, shortcuts,
    };
    use crate::dbus::{Kind, Message, Value};

    #[test]
    fn chords_become_the_specifications_triggers() {
        assert_eq!(shortcut_trigger("KeyA").as_deref(), Some("a"));
        assert_eq!(
            shortcut_trigger("Ctrl+Shift+KeyA").as_deref(),
            Some("CTRL+SHIFT+a")
        );
        assert_eq!(
            shortcut_trigger("Ctrl+Alt+Shift+Meta+F12").as_deref(),
            Some("CTRL+ALT+SHIFT+LOGO+F12")
        );
        assert_eq!(shortcut_trigger("Alt+Digit1").as_deref(), Some("ALT+1"));
        assert_eq!(shortcut_trigger("Numpad5").as_deref(), Some("KP_5"));
        assert_eq!(
            shortcut_trigger("Ctrl+PageDown").as_deref(),
            Some("CTRL+Page_Down")
        );
        assert_eq!(
            shortcut_trigger("MediaPlayPause").as_deref(),
            Some("XF86AudioPlay")
        );
        assert_eq!(shortcut_trigger("Ctrl+WheelUp"), None);
        assert_eq!(shortcut_trigger("Ctrl+Ctrl+KeyA"), None);
        assert_eq!(shortcut_trigger("Hyper+KeyA"), None);
        assert_eq!(shortcut_trigger("Keya"), None);
        assert_eq!(shortcut_trigger("F25"), None);
        assert_eq!(shortcut_trigger("Unknown"), None);
    }

    #[test]
    fn every_chord_key_has_a_trigger() {
        for code in jstream_protocol::Chord::KEYS {
            assert!(shortcut_trigger(code).is_some(), "{code} has no keysym");
        }
    }

    #[test]
    fn wheel_chords_and_unnamed_keys_are_refused_with_a_reason() {
        let chords: Vec<String> = ["Ctrl+KeyA", "Ctrl+WheelUp", "Bogus", "Ctrl+KeyA"]
            .into_iter()
            .map(String::from)
            .collect();
        let (bound, refused) = shortcuts(&chords);
        assert_eq!(bound, [("Ctrl+KeyA".to_owned(), "CTRL+a".to_owned())]);
        assert_eq!(refused.len(), 2);
        assert!(refused[0].1.contains("only while JStream has the focus"));
    }

    #[test]
    fn the_calls_carry_their_tokens_and_shortcuts() {
        let create = create_session("h1", "s1");
        assert_eq!(create.member.as_deref(), Some("CreateSession"));
        assert_eq!(create.signature(), "a{sv}");
        create.encode(1).expect("encodes");
        let bind = bind_shortcuts(
            "/org/freedesktop/portal/desktop/session/1_42/s1",
            &[("Ctrl+KeyA".into(), "CTRL+a".into())],
            "b1",
        );
        assert_eq!(bind.member.as_deref(), Some("BindShortcuts"));
        assert_eq!(bind.signature(), "oa(sa{sv})sa{sv}");
        let Value::Array(_, list) = &bind.body[1] else {
            panic!("a list");
        };
        let Value::Struct(fields) = &list[0] else {
            panic!("a shortcut");
        };
        assert_eq!(fields[0], Value::Str("Ctrl+KeyA".into()));
        let props = fields[1].as_dict().expect("props");
        assert!(
            props
                .iter()
                .any(|(k, v)| *k == "preferred_trigger" && v.as_str() == Some("CTRL+a"))
        );
        bind.encode(1).expect("encodes");
    }

    fn signal(sender: &str, session: &str, id: &str) -> Message {
        let mut msg = Message::call(
            "",
            "/org/freedesktop/portal/desktop",
            super::GLOBAL_SHORTCUTS,
            "Activated",
            vec![
                Value::Path(session.into()),
                Value::Str(id.into()),
                Value::U64(1),
                Value::dict(Vec::new()),
            ],
        );
        msg.kind = Kind::Signal;
        msg.destination = None;
        msg.sender = Some(sender.into());
        msg
    }

    #[test]
    fn only_the_portals_activation_of_this_session_counts() {
        let s = "/org/freedesktop/portal/desktop/session/1_42/s1";
        assert_eq!(
            activated(&signal(":1.3", s, "Ctrl+KeyA"), ":1.3", s),
            Some("Ctrl+KeyA")
        );
        assert_eq!(activated(&signal(":1.9", s, "Ctrl+KeyA"), ":1.3", s), None);
        assert_eq!(
            activated(&signal(":1.3", "/other", "Ctrl+KeyA"), ":1.3", s),
            None
        );
    }

    #[test]
    fn a_declined_or_failed_request_is_an_error() {
        let ok = vec![
            Value::U32(0),
            Value::dict(vec![("session_handle", Value::Str("/s".into()))]),
        ];
        let results = response_results(&ok, "x").expect("ok");
        assert_eq!(results[0].0, "session_handle");
        assert!(response_results(&[Value::U32(1), Value::dict(vec![])], "x").is_err());
        assert!(response_results(&[Value::U32(2), Value::dict(vec![])], "x").is_err());
        assert!(response_results(&[Value::U32(0)], "x").is_err());
    }
}
