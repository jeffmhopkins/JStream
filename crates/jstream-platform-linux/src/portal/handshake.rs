//! The ScreenCast portal's session flow, without I/O: `CreateSession`, `SelectSources`,
//! `Start`, then `OpenPipeWireRemote`.
//!
//! [`Handshake`] says which call to make next and takes the portal's answers; the Linux
//! backend moves the messages. Each of the first three calls returns a request handle
//! at once and answers later with a `Response` signal on it (the dialog can take
//! minutes), so a step's response path is computed before the call, per
//! [`request_path`], and the signal is matched to it.

use std::time::Duration;

use jstream_platform::PlatformError;

use super::{CursorModes, PersistMode, PortalPick, SourceTypes, handle_token, request_path};
use crate::dbus::{Message, Value, lookup};

/// The portal's bus name.
pub(crate) const DESTINATION: &str = "org.freedesktop.portal.Desktop";
/// The portal's object.
pub(crate) const OBJECT: &str = "/org/freedesktop/portal/desktop";
/// The ScreenCast interface.
pub(crate) const SCREENCAST: &str = "org.freedesktop.portal.ScreenCast";
/// The interface of a request's handle.
pub(crate) const REQUEST: &str = "org.freedesktop.portal.Request";
/// The interface of a session's handle.
pub(crate) const SESSION: &str = "org.freedesktop.portal.Session";

/// How long the portal may take to answer a call that shows no dialog.
pub(crate) const QUICK: Duration = Duration::from_secs(30);
/// How long the user may take in the picker `Start` opens.
pub(crate) const PICKER: Duration = Duration::from_secs(300);

/// The ScreenCast interface version that added `persist_mode` and restore tokens.
const PERSIST_VERSION: u32 = 4;
/// The version that added `cursor_mode`.
const CURSOR_VERSION: u32 = 2;

/// What the ScreenCast portal offers, from its properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PortalInfo {
    /// The interface version.
    pub(crate) version: u32,
    /// `AvailableSourceTypes`.
    pub(crate) source_types: SourceTypes,
    /// `AvailableCursorModes` (0 before version 2).
    pub(crate) cursor_modes: CursorModes,
}

impl PortalInfo {
    /// The `org.freedesktop.DBus.Properties.GetAll` call for the ScreenCast interface.
    pub(crate) fn query() -> Message {
        Message::call(
            DESTINATION,
            OBJECT,
            "org.freedesktop.DBus.Properties",
            "GetAll",
            vec![Value::Str(SCREENCAST.into())],
        )
    }

    /// Reads the `GetAll` reply's body.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when it is not an `a{sv}` with a version.
    pub(crate) fn from_reply(body: &[Value]) -> Result<Self, PlatformError> {
        let props = body
            .first()
            .and_then(Value::as_dict)
            .ok_or_else(|| bad_reply("org.freedesktop.DBus.Properties.GetAll", "no properties"))?;
        let get = |k| lookup(&props, k).and_then(Value::as_u32);
        Ok(Self {
            version: get("version").ok_or_else(|| {
                bad_reply(
                    "org.freedesktop.DBus.Properties.GetAll",
                    "no ScreenCast version",
                )
            })?,
            source_types: SourceTypes(get("AvailableSourceTypes").unwrap_or(0)),
            cursor_modes: CursorModes(get("AvailableCursorModes").unwrap_or(0)),
        })
    }
}

/// The cursor mode to ask for, for CPU frames: drawn into them where the portal can,
/// since nothing composites a metadata cursor yet; else hidden.
pub(crate) fn cursor_mode_for_cpu_frames(available: CursorModes) -> CursorModes {
    if available.0 & CursorModes::EMBEDDED.0 != 0 {
        CursorModes::EMBEDDED
    } else {
        CursorModes::HIDDEN
    }
}

/// One stream `Start` granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PortalStream {
    /// The PipeWire node to connect to.
    pub(crate) node_id: u32,
    /// The source's size in logical pixels, when the portal says.
    pub(crate) size: Option<(i32, i32)>,
    /// What was picked (a [`SourceTypes`] bit), when the portal says.
    pub(crate) source_type: Option<u32>,
}

/// What a finished handshake hands over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Granted {
    /// The session's object path, to close it.
    pub(crate) session: String,
    /// The granted streams; never empty.
    pub(crate) streams: Vec<PortalStream>,
    /// The token that restores this choice next time, when the portal gave one. Each
    /// token is single-use, so this replaces the one passed in.
    pub(crate) restore_token: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    CreateSession,
    SelectSources,
    Start,
    OpenRemote,
    Done,
}

impl Step {
    fn method(self) -> &'static str {
        match self {
            Self::CreateSession => "org.freedesktop.portal.ScreenCast.CreateSession",
            Self::SelectSources => "org.freedesktop.portal.ScreenCast.SelectSources",
            Self::Start => "org.freedesktop.portal.ScreenCast.Start",
            Self::OpenRemote | Self::Done => "org.freedesktop.portal.ScreenCast.OpenPipeWireRemote",
        }
    }
}

/// The next thing the backend does.
#[derive(Debug, PartialEq)]
pub(crate) enum Next {
    /// Call `call` (named `what` in errors). Its reply is a request handle; then wait up
    /// to `timeout` for the `Response` signal that [`Handshake::is_response`] accepts.
    Request {
        call: Message,
        what: &'static str,
        timeout: Duration,
    },
    /// Call `OpenPipeWireRemote`; its reply carries the PipeWire socket. Then
    /// [`Handshake::granted`].
    OpenRemote { call: Message, what: &'static str },
}

/// The session flow's state.
#[derive(Debug)]
pub(crate) struct Handshake {
    step: Step,
    pick: PortalPick,
    info: PortalInfo,
    unique_name: String,
    restore_token: Option<String>,
    /// Counts handle tokens, so each request's is fresh.
    tokens: u64,
    /// Paths a `Response` for the current step may arrive on: the one computed before
    /// the call, and the handle the call returned (they differ on portals older than
    /// version 0.9's handle convention).
    expected: Vec<String>,
    session: Option<String>,
    streams: Vec<PortalStream>,
    new_token: Option<String>,
}

fn bad_reply(what: &'static str, why: &str) -> PlatformError {
    PlatformError::Os {
        call: what,
        code: None,
        message: format!("unexpected reply from the desktop portal: {why}"),
    }
}

impl Handshake {
    /// A handshake for `pick`, on the connection named `unique_name`, reusing
    /// `restore_token` when the portal supports tokens.
    ///
    /// # Errors
    /// [`PlatformError::Unsupported`] when the portal can't capture that kind of source.
    pub(crate) fn new(
        pick: PortalPick,
        info: PortalInfo,
        unique_name: &str,
        restore_token: Option<String>,
    ) -> Result<Self, PlatformError> {
        if !info.source_types.contains(pick.source_types()) {
            return Err(PlatformError::Unsupported {
                what: format!(
                    "{pick:?} capture: this desktop's ScreenCast portal offers source types {:#x}",
                    info.source_types.0
                ),
            });
        }
        Ok(Self {
            step: Step::CreateSession,
            pick,
            info,
            unique_name: unique_name.to_owned(),
            restore_token: restore_token.filter(|_| info.version >= PERSIST_VERSION),
            tokens: 0,
            expected: Vec::new(),
            session: None,
            streams: Vec::new(),
            new_token: None,
        })
    }

    /// The session's path, once `CreateSession` has answered, so a failed handshake can
    /// close it.
    pub(crate) fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    fn token(&mut self) -> Result<String, PlatformError> {
        self.tokens += 1;
        let token = handle_token(self.tokens);
        let path = request_path(&self.unique_name, &token)
            .ok_or_else(|| bad_reply("org.freedesktop.DBus.Hello", "an unusable unique name"))?;
        self.expected = vec![path];
        Ok(token)
    }

    fn session_path(&self) -> Result<Value, PlatformError> {
        self.session
            .clone()
            .map(Value::Path)
            .ok_or_else(|| bad_reply(self.step.method(), "no session"))
    }

    /// The next call.
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] once the handshake is done.
    pub(crate) fn next(&mut self) -> Result<Next, PlatformError> {
        let screencast =
            |member: &str, body| Message::call(DESTINATION, OBJECT, SCREENCAST, member, body);
        let what = self.step.method();
        Ok(match self.step {
            Step::CreateSession => {
                let token = self.token()?;
                let session_token = format!("jstream_session_{}", self.tokens);
                Next::Request {
                    call: screencast(
                        "CreateSession",
                        vec![Value::dict(vec![
                            ("handle_token", Value::Str(token)),
                            ("session_handle_token", Value::Str(session_token)),
                        ])],
                    ),
                    what,
                    timeout: QUICK,
                }
            }
            Step::SelectSources => {
                let token = self.token()?;
                let mut options = vec![
                    ("handle_token", Value::Str(token)),
                    ("types", Value::U32(self.pick.source_types().0)),
                    ("multiple", Value::Bool(false)),
                ];
                if self.info.version >= CURSOR_VERSION {
                    let mode = cursor_mode_for_cpu_frames(self.info.cursor_modes);
                    options.push(("cursor_mode", Value::U32(mode.0)));
                }
                if self.info.version >= PERSIST_VERSION {
                    options.push(("persist_mode", Value::U32(PersistMode::UntilRevoked as u32)));
                    if let Some(t) = &self.restore_token {
                        options.push(("restore_token", Value::Str(t.clone())));
                    }
                }
                Next::Request {
                    call: screencast(
                        "SelectSources",
                        vec![self.session_path()?, Value::dict(options)],
                    ),
                    what,
                    timeout: QUICK,
                }
            }
            Step::Start => {
                let token = self.token()?;
                Next::Request {
                    call: screencast(
                        "Start",
                        vec![
                            self.session_path()?,
                            Value::Str(String::new()), // no parent window
                            Value::dict(vec![("handle_token", Value::Str(token))]),
                        ],
                    ),
                    what,
                    timeout: PICKER,
                }
            }
            Step::OpenRemote => Next::OpenRemote {
                call: screencast(
                    "OpenPipeWireRemote",
                    vec![self.session_path()?, Value::dict(Vec::new())],
                ),
                what,
            },
            Step::Done => {
                return Err(PlatformError::InvalidState {
                    what: "the ScreenCast handshake",
                    state: "already done",
                });
            }
        })
    }

    /// Takes a request call's reply: the request handle, on which the response may
    /// arrive instead of the computed path.
    pub(crate) fn on_handle(&mut self, reply: &Message) {
        if let Some(Value::Path(p)) = reply.body.first()
            && !self.expected.contains(p)
        {
            self.expected.push(p.clone());
        }
    }

    /// Whether `msg` is the `Response` to the current step's request.
    pub(crate) fn is_response(&self, msg: &Message) -> bool {
        msg.is_signal(REQUEST, "Response")
            && msg.path.as_ref().is_some_and(|p| self.expected.contains(p))
    }

    /// Takes the current step's `Response` signal and moves on.
    ///
    /// # Errors
    /// [`PlatformError::Os`] when the user cancelled (code 1), the portal failed (code
    /// 2), or the results lack what the step needs.
    pub(crate) fn on_response(&mut self, msg: &Message) -> Result<(), PlatformError> {
        let what = self.step.method();
        let (code, results) = match msg.body.as_slice() {
            [code, results] => (
                code.as_u32()
                    .ok_or_else(|| bad_reply(what, "no response code"))?,
                results
                    .as_dict()
                    .ok_or_else(|| bad_reply(what, "no results"))?,
            ),
            _ => return Err(bad_reply(what, "a malformed Response")),
        };
        match code {
            0 => {}
            1 => {
                return Err(PlatformError::Os {
                    call: what,
                    code: Some(1),
                    message: "screen sharing was cancelled in the system picker".into(),
                });
            }
            other => {
                return Err(PlatformError::Os {
                    call: what,
                    code: Some(other),
                    message: "the desktop portal ended the request".into(),
                });
            }
        }
        self.step = match self.step {
            Step::CreateSession => {
                let handle = lookup(&results, "session_handle")
                    .and_then(Value::as_str)
                    .ok_or_else(|| bad_reply(what, "no session_handle"))?;
                self.session = Some(handle.to_owned());
                Step::SelectSources
            }
            Step::SelectSources => Step::Start,
            Step::Start => {
                self.streams = lookup(&results, "streams")
                    .map(parse_streams)
                    .transpose()?
                    .unwrap_or_default();
                if self.streams.is_empty() {
                    return Err(bad_reply(what, "no streams were granted"));
                }
                self.new_token = lookup(&results, "restore_token")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                Step::OpenRemote
            }
            Step::OpenRemote | Step::Done => {
                return Err(PlatformError::InvalidState {
                    what: "the ScreenCast handshake",
                    state: "no request is pending",
                });
            }
        };
        self.expected.clear();
        Ok(())
    }

    /// The result, once `OpenPipeWireRemote` has answered with `reply`, which must
    /// hold one file descriptor (the caller takes it from the message).
    ///
    /// # Errors
    /// [`PlatformError::InvalidState`] before `Start` has answered, and
    /// [`PlatformError::Os`] when the reply carries no descriptor.
    pub(crate) fn granted(&mut self, reply: &Message) -> Result<Granted, PlatformError> {
        if self.step != Step::OpenRemote {
            return Err(PlatformError::InvalidState {
                what: "the ScreenCast handshake",
                state: "not ready to open the PipeWire remote",
            });
        }
        if !matches!(reply.body.first(), Some(Value::Fd(_))) || reply.unix_fds == 0 {
            return Err(bad_reply(self.step.method(), "no PipeWire file descriptor"));
        }
        self.step = Step::Done;
        Ok(Granted {
            session: self.session.clone().unwrap_or_default(),
            streams: std::mem::take(&mut self.streams),
            restore_token: self.new_token.take(),
        })
    }
}

/// The `Session.Close` call for `session`, sent without waiting for a reply.
pub(crate) fn close_session(session: &str) -> Message {
    let mut msg = Message::call(DESTINATION, session, SESSION, "Close", Vec::new());
    msg.flags |= crate::dbus::NO_REPLY_EXPECTED;
    msg
}

/// Whether `msg` is the portal closing `session` (the user stopped sharing, or the
/// grant was revoked).
pub(crate) fn is_session_closed(msg: &Message, session: &str) -> bool {
    msg.is_signal(SESSION, "Closed") && msg.path.as_deref() == Some(session)
}

/// Parses `Start`'s `streams`: `a(ua{sv})`.
fn parse_streams(v: &Value) -> Result<Vec<PortalStream>, PlatformError> {
    let what = Step::Start.method();
    let Value::Array(_, items) = v.unwrap_variant() else {
        return Err(bad_reply(what, "streams is not an array"));
    };
    items
        .iter()
        .map(|item| {
            let Value::Struct(fields) = item else {
                return Err(bad_reply(what, "a stream is not a struct"));
            };
            let [node, props] = fields.as_slice() else {
                return Err(bad_reply(what, "a stream is not (ua{sv})"));
            };
            let node_id = node
                .as_u32()
                .ok_or_else(|| bad_reply(what, "a stream has no node"))?;
            let props = props.as_dict().unwrap_or_default();
            let size = lookup(&props, "size").and_then(|s| match s {
                Value::Struct(wh) => match wh.as_slice() {
                    [w, h] => Some((w.as_i32()?, h.as_i32()?)),
                    _ => None,
                },
                _ => None,
            });
            Ok(PortalStream {
                node_id,
                size,
                source_type: lookup(&props, "source_type").and_then(Value::as_u32),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        Granted, Handshake, Next, PICKER, PortalInfo, PortalStream, QUICK, close_session,
        cursor_mode_for_cpu_frames, is_session_closed,
    };
    use crate::dbus::{Kind, Message, Value, lookup};
    use crate::portal::{CursorModes, PortalPick, SourceTypes};

    const ME: &str = ":1.42";

    fn info(version: u32) -> PortalInfo {
        PortalInfo {
            version,
            source_types: SourceTypes(3),
            cursor_modes: CursorModes(7),
        }
    }

    fn signal(path: &str, code: u32, results: Vec<(&str, Value)>) -> Message {
        Message {
            kind: Kind::Signal,
            flags: 0,
            serial: 9,
            path: Some(path.into()),
            interface: Some("org.freedesktop.portal.Request".into()),
            member: Some("Response".into()),
            error_name: None,
            reply_serial: None,
            destination: Some(ME.into()),
            sender: Some(":1.3".into()),
            unix_fds: 0,
            body: vec![Value::U32(code), Value::dict(results)],
        }
    }

    fn reply(body: Vec<Value>, unix_fds: u32) -> Message {
        Message {
            kind: Kind::Return,
            reply_serial: Some(1),
            unix_fds,
            ..Message::call("x", "/", "i.f", "m", body)
        }
    }

    /// The options dict of a call, by key.
    fn options(call: &Message) -> Vec<(String, Value)> {
        let dict = call.body.last().and_then(Value::as_dict).expect("options");
        dict.into_iter()
            .map(|(k, v)| (k.to_owned(), v.clone()))
            .collect()
    }

    fn request(h: &mut Handshake) -> (Message, std::time::Duration) {
        match h.next().expect("a next step") {
            Next::Request { call, timeout, .. } => (call, timeout),
            other => panic!("expected a request, got {other:?}"),
        }
    }

    const SESSION: &str = "/org/freedesktop/portal/desktop/session/1_42/jstream_session_1";

    #[test]
    fn the_whole_flow_with_a_restore_token() {
        let mut h =
            Handshake::new(PortalPick::Display, info(5), ME, Some("old".into())).expect("new");

        let (create, timeout) = request(&mut h);
        assert_eq!(create.member.as_deref(), Some("CreateSession"));
        assert_eq!(timeout, QUICK);
        let o = options(&create);
        assert_eq!(
            o[0],
            ("handle_token".into(), Value::Str("jstream_1".into()))
        );
        assert_eq!(o[1].0, "session_handle_token");
        let path1 = "/org/freedesktop/portal/desktop/request/1_42/jstream_1";
        assert!(h.is_response(&signal(path1, 0, vec![])));
        assert!(!h.is_response(&signal("/elsewhere", 0, vec![])));
        h.on_response(&signal(
            path1,
            0,
            vec![("session_handle", Value::Str(SESSION.into()))],
        ))
        .expect("session created");
        assert_eq!(h.session(), Some(SESSION));

        let (select, _) = request(&mut h);
        assert_eq!(select.member.as_deref(), Some("SelectSources"));
        assert_eq!(select.body[0], Value::Path(SESSION.into()));
        let o = options(&select);
        let get = |k: &str| o.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("types"), Some(Value::U32(1)));
        assert_eq!(get("multiple"), Some(Value::Bool(false)));
        assert_eq!(
            get("cursor_mode"),
            Some(Value::U32(2)),
            "embedded, for CPU frames"
        );
        assert_eq!(get("persist_mode"), Some(Value::U32(2)));
        assert_eq!(get("restore_token"), Some(Value::Str("old".into())));
        let path2 = "/org/freedesktop/portal/desktop/request/1_42/jstream_2";
        h.on_response(&signal(path2, 0, vec![])).expect("selected");

        let (start, timeout) = request(&mut h);
        assert_eq!(start.member.as_deref(), Some("Start"));
        assert_eq!(start.body[1], Value::Str(String::new()));
        assert_eq!(timeout, PICKER);
        // An older portal answers on the handle it returns rather than the computed one.
        h.on_handle(&reply(vec![Value::Path("/legacy/request".into())], 0));
        let streams = Value::Array(
            "(ua{sv})".into(),
            vec![Value::Struct(vec![
                Value::U32(57),
                Value::dict(vec![
                    (
                        "size",
                        Value::Struct(vec![Value::I32(2560), Value::I32(1440)]),
                    ),
                    ("source_type", Value::U32(1)),
                ]),
            ])],
        );
        let started = signal(
            "/legacy/request",
            0,
            vec![
                ("streams", streams),
                ("restore_token", Value::Str("new".into())),
            ],
        );
        assert!(h.is_response(&started));
        h.on_response(&started).expect("started");

        let Next::OpenRemote { call, .. } = h.next().expect("next") else {
            panic!("expected OpenPipeWireRemote");
        };
        assert_eq!(call.member.as_deref(), Some("OpenPipeWireRemote"));
        assert_eq!(call.signature(), "oa{sv}");
        assert!(
            h.granted(&reply(vec![], 0)).is_err(),
            "a reply with no descriptor"
        );
        let granted = h.granted(&reply(vec![Value::Fd(0)], 1)).expect("granted");
        assert_eq!(
            granted,
            Granted {
                session: SESSION.into(),
                streams: vec![PortalStream {
                    node_id: 57,
                    size: Some((2560, 1440)),
                    source_type: Some(1),
                }],
                restore_token: Some("new".into()),
            }
        );
        assert!(h.next().is_err(), "nothing after the end");
    }

    #[test]
    fn older_portals_get_only_the_options_they_know() {
        let mut h =
            Handshake::new(PortalPick::Window, info(1), ME, Some("old".into())).expect("new");
        let (_, _) = request(&mut h);
        h.on_response(&signal(
            "/org/freedesktop/portal/desktop/request/1_42/jstream_1",
            0,
            vec![("session_handle", Value::Path(SESSION.into()))],
        ))
        .expect("created");
        let (select, _) = request(&mut h);
        let o = options(&select);
        let keys: Vec<&str> = o.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["handle_token", "types", "multiple"]);
        assert_eq!(o[1].1, Value::U32(2), "a window");
    }

    #[test]
    fn cancelling_and_failures_are_errors() {
        let path1 = "/org/freedesktop/portal/desktop/request/1_42/jstream_1";
        let mut h = Handshake::new(PortalPick::Display, info(5), ME, None).expect("new");
        let _ = request(&mut h);
        let err = h
            .on_response(&signal(path1, 1, vec![]))
            .expect_err("cancelled");
        assert!(matches!(
            err,
            jstream_platform::PlatformError::Os { code: Some(1), .. }
        ));

        let mut h = Handshake::new(PortalPick::Display, info(5), ME, None).expect("new");
        let _ = request(&mut h);
        assert!(h.on_response(&signal(path1, 2, vec![])).is_err());

        let mut h = Handshake::new(PortalPick::Display, info(5), ME, None).expect("new");
        let _ = request(&mut h);
        assert!(
            h.on_response(&signal(path1, 0, vec![])).is_err(),
            "no session handle"
        );

        // A portal that offers only monitors can't capture a window.
        let monitors = PortalInfo {
            source_types: SourceTypes::MONITOR,
            ..info(5)
        };
        assert!(Handshake::new(PortalPick::Window, monitors, ME, None).is_err());
        // An unusable unique name fails at the first token.
        let mut h = Handshake::new(PortalPick::Display, info(5), "bogus", None).expect("new");
        assert!(h.next().is_err());
    }

    #[test]
    fn a_start_without_streams_is_an_error() {
        let mut h = Handshake::new(PortalPick::Display, info(5), ME, None).expect("new");
        let _ = request(&mut h);
        h.on_response(&signal(
            "/org/freedesktop/portal/desktop/request/1_42/jstream_1",
            0,
            vec![("session_handle", Value::Str(SESSION.into()))],
        ))
        .expect("created");
        let _ = request(&mut h);
        h.on_response(&signal(
            "/org/freedesktop/portal/desktop/request/1_42/jstream_2",
            0,
            vec![],
        ))
        .expect("selected");
        let _ = request(&mut h);
        let empty = Value::Array("(ua{sv})".into(), Vec::new());
        assert!(
            h.on_response(&signal(
                "/org/freedesktop/portal/desktop/request/1_42/jstream_3",
                0,
                vec![("streams", empty)],
            ))
            .is_err()
        );
    }

    #[test]
    fn properties_and_session_messages() {
        let q = PortalInfo::query();
        assert_eq!(q.member.as_deref(), Some("GetAll"));
        let body = vec![Value::dict(vec![
            ("version", Value::U32(5)),
            ("AvailableSourceTypes", Value::U32(7)),
            ("AvailableCursorModes", Value::U32(3)),
        ])];
        assert_eq!(
            PortalInfo::from_reply(&body).expect("parses"),
            PortalInfo {
                version: 5,
                source_types: SourceTypes(7),
                cursor_modes: CursorModes(3),
            }
        );
        assert!(PortalInfo::from_reply(&[Value::dict(vec![])]).is_err());
        assert!(PortalInfo::from_reply(&[]).is_err());

        assert_eq!(
            cursor_mode_for_cpu_frames(CursorModes(5)),
            CursorModes::HIDDEN
        );
        assert_eq!(
            cursor_mode_for_cpu_frames(CursorModes(7)),
            CursorModes::EMBEDDED
        );

        let close = close_session(SESSION);
        assert_eq!(close.path.as_deref(), Some(SESSION));
        assert_eq!(close.member.as_deref(), Some("Close"));
        assert_eq!(close.flags & crate::dbus::NO_REPLY_EXPECTED, 1);
        let closed = Message {
            kind: Kind::Signal,
            interface: Some("org.freedesktop.portal.Session".into()),
            member: Some("Closed".into()),
            ..Message::call("x", SESSION, "i", "m", vec![Value::dict(vec![])])
        };
        assert!(is_session_closed(&closed, SESSION));
        assert!(!is_session_closed(&closed, "/other"));
        let _ = lookup;
    }
}
