//! Screen and window capture through the xdg-desktop-portal ScreenCast interface
//! (ADR-0060 §3).
//!
//! The desktop's own picker chooses what is captured, so [`PortalCapture`] lists one
//! target per kind rather than every display and window. This module holds the
//! platform-independent parts of the portal protocol: the option values `SelectSources`
//! takes, request handle tokens and the request object path a reply arrives on. The
//! D-Bus transport and the PipeWire stream behind `start` are follow-ups; until then
//! `start` reports `Unsupported`.

/// The `types` option of `SelectSources`: which kinds of source the picker offers. A
/// bit set, as the portal defines it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceTypes(pub u32);

impl SourceTypes {
    /// A monitor.
    pub const MONITOR: Self = Self(1);
    /// A window.
    pub const WINDOW: Self = Self(2);
    /// A virtual monitor the compositor creates for the cast.
    pub const VIRTUAL: Self = Self(4);

    /// Whether every bit of `other` is set here.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// The `cursor_mode` option of `SelectSources`, and the portal's `AvailableCursorModes`
/// property, as a bit set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CursorModes(pub u32);

impl CursorModes {
    /// The cursor is not in the stream.
    pub const HIDDEN: Self = Self(1);
    /// The cursor is drawn into the frames.
    pub const EMBEDDED: Self = Self(2);
    /// The cursor is sent as PipeWire stream metadata, beside the frames.
    pub const METADATA: Self = Self(4);

    /// The mode to ask for, out of the modes the portal offers: metadata, so the source
    /// can show or hide the cursor itself, else embedded, else hidden.
    pub fn preferred(available: Self) -> Self {
        [Self::METADATA, Self::EMBEDDED]
            .into_iter()
            .find(|m| available.0 & m.0 != 0)
            .unwrap_or(Self::HIDDEN)
    }
}

/// The `persist_mode` option of `SelectSources` (interface version 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum PersistMode {
    /// The grant ends with the session.
    None = 0,
    /// The grant lasts while JStream runs.
    WhileRunning = 1,
    /// The grant lasts until the user revokes it: what ADR-0060 asks for, so a collection
    /// reopens on the same screen without a prompt.
    UntilRevoked = 2,
}

/// A request's `handle_token`: a fresh path element per request, so its `Response`
/// signal can be matched before the call returns. Only `[A-Za-z0-9_]`.
pub fn handle_token(serial: u64) -> String {
    format!("jstream_{serial}")
}

/// The object path a request's `Response` signal arrives on, for the caller's D-Bus
/// unique name (such as `:1.42`) and the request's `handle_token`:
/// `/org/freedesktop/portal/desktop/request/1_42/<token>`. `None` when the name isn't a
/// unique name or the token has characters a path element can't hold.
pub fn request_path(unique_name: &str, token: &str) -> Option<String> {
    let sender = unique_name.strip_prefix(':')?;
    let element_ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
    };
    if !element_ok(sender) || !element_ok(token) || token.contains('.') {
        return None;
    }
    Some(format!(
        "/org/freedesktop/portal/desktop/request/{}/{token}",
        sender.replace('.', "_")
    ))
}

/// The kind of target the user picks in the portal's dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortalPick {
    /// A display.
    Display,
    /// A window.
    Window,
}

impl PortalPick {
    /// The device ID [`PortalCapture`](crate::PortalCapture) lists this pick under.
    pub fn device_id(self) -> &'static str {
        match self {
            Self::Display => "portal:display",
            Self::Window => "portal:window",
        }
    }

    /// The picker's source type for this pick.
    pub fn source_types(self) -> SourceTypes {
        match self {
            Self::Display => SourceTypes::MONITOR,
            Self::Window => SourceTypes::WINDOW,
        }
    }
}

#[cfg(target_os = "linux")]
pub use backend::PortalCapture;

#[cfg(target_os = "linux")]
mod backend {
    use jstream_platform::{
        Capture, CaptureStream, CaptureTarget, CaptureTargetInfo, DeviceId, PlatformError,
    };

    use super::PortalPick;
    use crate::{Session, SessionKind};

    /// Display and window capture through the ScreenCast portal (ADR-0060 §3). Draft:
    /// lists its targets; starting one reports `Unsupported` until the D-Bus transport
    /// and the PipeWire stream land.
    #[derive(Debug)]
    pub struct PortalCapture {
        session: Session,
    }

    impl PortalCapture {
        /// A capture backend for `session`. A headless session lists no targets.
        pub fn new(session: Session) -> Self {
            Self { session }
        }

        fn pick(target: &CaptureTarget) -> Option<PortalPick> {
            match target {
                CaptureTarget::Display(id) if id.as_str() == PortalPick::Display.device_id() => {
                    Some(PortalPick::Display)
                }
                CaptureTarget::Window(id) if id.as_str() == PortalPick::Window.device_id() => {
                    Some(PortalPick::Window)
                }
                _ => None,
            }
        }
    }

    impl Capture for PortalCapture {
        fn targets(&self) -> Result<Vec<CaptureTargetInfo>, PlatformError> {
            if self.session.kind == SessionKind::Headless {
                return Ok(Vec::new());
            }
            Ok(vec![
                CaptureTargetInfo {
                    target: CaptureTarget::Display(DeviceId::new(PortalPick::Display.device_id())),
                    name: "A display, chosen in the system picker".into(),
                    size: None,
                    primary: false,
                },
                CaptureTargetInfo {
                    target: CaptureTarget::Window(DeviceId::new(PortalPick::Window.device_id())),
                    name: "A window, chosen in the system picker".into(),
                    size: None,
                    primary: false,
                },
            ])
        }

        fn start(&self, target: &CaptureTarget) -> Result<Box<dyn CaptureStream>, PlatformError> {
            let Some(pick) = Self::pick(target) else {
                let (kind, id) = match target {
                    CaptureTarget::Display(id) => ("display", id.as_str()),
                    CaptureTarget::Window(id) => ("window", id.as_str()),
                    _ => ("capture target", ""),
                };
                return Err(PlatformError::NotFound {
                    kind,
                    id: id.to_owned(),
                });
            };
            Err(PlatformError::Unsupported {
                what: format!(
                    "{pick:?} capture through the ScreenCast portal (draft: the portal's D-Bus \
                     transport and PipeWire stream are not built yet, ADR-0060)"
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CursorModes, PersistMode, PortalPick, SourceTypes, handle_token, request_path};

    #[test]
    fn option_values_match_the_portal() {
        assert_eq!(SourceTypes::MONITOR.0 | SourceTypes::WINDOW.0, 3);
        assert!(SourceTypes(7).contains(SourceTypes::VIRTUAL));
        assert!(!SourceTypes(1).contains(SourceTypes::WINDOW));
        assert_eq!(PersistMode::UntilRevoked as u32, 2);
        assert_eq!(PortalPick::Window.source_types(), SourceTypes::WINDOW);
    }

    #[test]
    fn cursor_mode_prefers_metadata_then_embedded() {
        assert_eq!(
            CursorModes::preferred(CursorModes(7)),
            CursorModes::METADATA
        );
        assert_eq!(
            CursorModes::preferred(CursorModes(3)),
            CursorModes::EMBEDDED
        );
        assert_eq!(CursorModes::preferred(CursorModes(1)), CursorModes::HIDDEN);
        assert_eq!(CursorModes::preferred(CursorModes(0)), CursorModes::HIDDEN);
    }

    #[test]
    fn request_paths_follow_the_portal_convention() {
        let token = handle_token(7);
        assert_eq!(token, "jstream_7");
        assert_eq!(
            request_path(":1.42", &token).as_deref(),
            Some("/org/freedesktop/portal/desktop/request/1_42/jstream_7")
        );
        assert_eq!(request_path("org.example", &token), None);
        assert_eq!(request_path(":1.42", "bad-token"), None);
        assert_eq!(request_path(":1.42", "a.b"), None);
        assert_eq!(request_path(":", &token), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn capture_lists_one_target_per_kind_and_refuses_others() {
        use jstream_platform::{Capture, CaptureTarget, DeviceId, PlatformError};

        use crate::{PortalCapture, Session, SessionKind};

        let capture = PortalCapture::new(Session {
            kind: SessionKind::Wayland,
            desktops: vec!["GNOME".into()],
        });
        let targets = capture.targets().expect("lists without D-Bus");
        assert_eq!(targets.len(), 2);
        assert_eq!(
            targets[0].target,
            CaptureTarget::Display(DeviceId::new("portal:display"))
        );
        assert!(matches!(
            capture.start(&targets[1].target),
            Err(PlatformError::Unsupported { .. })
        ));
        assert!(matches!(
            capture.start(&CaptureTarget::Display(DeviceId::new("HDMI-1"))),
            Err(PlatformError::NotFound {
                kind: "display",
                ..
            })
        ));

        let headless = PortalCapture::new(Session {
            kind: SessionKind::Headless,
            desktops: Vec::new(),
        });
        assert!(headless.targets().expect("lists").is_empty());
    }
}
