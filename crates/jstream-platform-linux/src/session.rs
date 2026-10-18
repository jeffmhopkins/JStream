//! The desktop session JStream runs in, and its support tier under ADR-0060 §2.
//!
//! Only the session type and the desktop are read here, from the environment the session
//! manager sets. Distribution and library versions are checked by the backends that
//! need them (PipeWire's floor is in [`crate::pipewire_version_supported`]).

/// The kind of graphical session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionKind {
    /// A Wayland session.
    Wayland,
    /// An X11 session.
    X11,
    /// No graphical session: a console, SSH or a service.
    Headless,
}

/// How ADR-0060 §2 classifies a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SupportTier {
    /// Supported and tested: GNOME or KDE Plasma on Wayland.
    Supported,
    /// Best effort: anything else with a graphical session, where capture works if the
    /// desktop ships a ScreenCast portal backend.
    BestEffort,
    /// No graphical session, so no screen capture. Audio, encoding and the daemon still
    /// work.
    Headless,
}

/// The session as the environment describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// The session type.
    pub kind: SessionKind,
    /// The desktops `XDG_CURRENT_DESKTOP` names, in its order (for example
    /// `["ubuntu", "GNOME"]`). Empty when it is unset.
    pub desktops: Vec<String>,
}

impl Session {
    /// The current process's session.
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// The session described by `var`, which returns an environment variable's value.
    /// Split out so tests don't touch the process environment.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name: &str| var(name).is_some_and(|v| !v.is_empty());
        let kind = match var("XDG_SESSION_TYPE").as_deref() {
            Some("wayland") => SessionKind::Wayland,
            Some("x11") => SessionKind::X11,
            // Unset or "tty"/"unspecified": fall back to the display variables, which a
            // nested or manually started session sets without a session type.
            _ if set("WAYLAND_DISPLAY") => SessionKind::Wayland,
            _ if set("DISPLAY") => SessionKind::X11,
            _ => SessionKind::Headless,
        };
        let desktops = var("XDG_CURRENT_DESKTOP")
            .map(|v| {
                v.split(':')
                    .filter(|d| !d.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Self { kind, desktops }
    }

    /// Whether the desktop is one ADR-0060 supports: GNOME or KDE Plasma.
    pub fn is_supported_desktop(&self) -> bool {
        self.desktops
            .iter()
            .any(|d| d.eq_ignore_ascii_case("GNOME") || d.eq_ignore_ascii_case("KDE"))
    }

    /// The session's tier under ADR-0060 §2.
    pub fn tier(&self) -> SupportTier {
        match self.kind {
            SessionKind::Headless => SupportTier::Headless,
            SessionKind::Wayland if self.is_supported_desktop() => SupportTier::Supported,
            SessionKind::Wayland | SessionKind::X11 => SupportTier::BestEffort,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Session, SessionKind, SupportTier};

    fn session(vars: &[(&str, &str)]) -> Session {
        Session::from_vars(|name| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| (*v).to_owned())
        })
    }

    #[test]
    fn gnome_and_plasma_on_wayland_are_supported() {
        let s = session(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
        ]);
        assert_eq!(s.kind, SessionKind::Wayland);
        assert_eq!(s.desktops, ["ubuntu", "GNOME"]);
        assert_eq!(s.tier(), SupportTier::Supported);
        let s = session(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
        ]);
        assert_eq!(s.tier(), SupportTier::Supported);
    }

    #[test]
    fn x11_and_other_desktops_are_best_effort() {
        let s = session(&[
            ("XDG_SESSION_TYPE", "x11"),
            ("XDG_CURRENT_DESKTOP", "GNOME"),
        ]);
        assert_eq!(s.tier(), SupportTier::BestEffort);
        let s = session(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "Hyprland"),
        ]);
        assert_eq!(s.tier(), SupportTier::BestEffort);
    }

    #[test]
    fn display_variables_stand_in_for_a_missing_session_type() {
        assert_eq!(
            session(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]).kind,
            SessionKind::Wayland
        );
        assert_eq!(
            session(&[("XDG_SESSION_TYPE", "tty"), ("DISPLAY", ":0")]).kind,
            SessionKind::X11
        );
        let s = session(&[("XDG_SESSION_TYPE", "tty"), ("DISPLAY", "")]);
        assert_eq!(s.kind, SessionKind::Headless);
        assert_eq!(s.tier(), SupportTier::Headless);
        assert!(s.desktops.is_empty());
    }
}
