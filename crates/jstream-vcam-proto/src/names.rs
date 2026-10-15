//! Names and security of the shared kernel objects (control block, ring textures, fence)
//! and of the consumer's log folder.

/// Base name of the control block's file mapping.
pub const CONTROL_NAME: &str = "JStream-VCam-Control";

/// Name of the consumer's log folder under `%ProgramData%`. Frame Server runs the DLL as
/// LOCAL SERVICE, whose `%TEMP%` the user can't read, so a folder both can reach is used.
pub const LOG_DIR_NAME: &str = "JStreamVCam";

/// DACL (SDDL) for the textures, the fence and the control block: full access for SYSTEM,
/// Administrators, LOCAL SERVICE (Frame Server's account) and interactive users (the
/// producer). Protected (`P`), so nothing is inherited.
pub const SDDL_OBJECTS: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;LS)(A;;GA;;;IU)";

/// DACL (SDDL) for the log folder: the same principals, inherited by the files in it
/// (`OICI`).
pub const SDDL_LOG_DIR: &str =
    "D:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)(A;OICI;GA;;;LS)(A;OICI;GA;;;IU)";

/// Highest session id the consumer probes after the console session when looking for a
/// session-scoped control block ([`control_open_names`]).
pub const SESSION_PROBE_MAX: u32 = 4;

/// The namespace the producer created its named objects in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `Global\…`: visible to every session, including Frame Server's session 0. Creating
    /// a global file mapping needs `SeCreateGlobalPrivilege`, which a non-elevated user
    /// lacks, so a normal engine falls back to [`Scope::Session`].
    Global,
    /// The producer's own session namespace (`Local\…`). The consumer, in session 0,
    /// opens it as `Session\<id>\…`.
    Session,
}

impl Scope {
    /// The value stored in [`Control::scope`](crate::Control::scope).
    pub fn as_u32(self) -> u32 {
        match self {
            Scope::Global => 1,
            Scope::Session => 2,
        }
    }

    /// The scope a [`Control::scope`](crate::Control::scope) value names, if any.
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1 => Some(Scope::Global),
            2 => Some(Scope::Session),
            _ => None,
        }
    }

    /// A short lowercase name for logs and reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Session => "session",
        }
    }

    /// The name the producer passes when creating object `base` in this scope.
    pub fn create_name(self, base: &str) -> String {
        match self {
            Scope::Global => format!("Global\\{base}"),
            Scope::Session => format!("Local\\{base}"),
        }
    }

    /// The name another session (Frame Server) opens object `base` by, given the
    /// producer's session id.
    pub fn open_name(self, session_id: u32, base: &str) -> String {
        match self {
            Scope::Global => format!("Global\\{base}"),
            Scope::Session => format!("Session\\{session_id}\\{base}"),
        }
    }
}

/// Base name of ring texture `slot`. The producer's process id keeps names unique across
/// producer restarts, while the consumer may still hold the previous objects open.
pub fn texture_base(producer_pid: u32, slot: u32) -> String {
    format!("JStream-VCam-Tex{slot}-{producer_pid}")
}

/// Base name of the shared fence.
pub fn fence_base(producer_pid: u32) -> String {
    format!("JStream-VCam-Fence-{producer_pid}")
}

/// The names the consumer tries, in order, to open the control block: the global name,
/// then the console session's (`console`, if there is one), then sessions 1 to
/// [`SESSION_PROBE_MAX`]. No name appears twice.
pub fn control_open_names(console: Option<u32>) -> Vec<String> {
    let mut sessions: Vec<u32> = console.into_iter().collect();
    sessions.extend((1..=SESSION_PROBE_MAX).filter(|id| Some(*id) != console));
    std::iter::once(Scope::Global.open_name(0, CONTROL_NAME))
        .chain(
            sessions
                .into_iter()
                .map(|id| Scope::Session.open_name(id, CONTROL_NAME)),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            Scope::Session.open_name(1, &texture_base(42, 2)),
            "Session\\1\\JStream-VCam-Tex2-42"
        );
        assert_eq!(
            Scope::Session.create_name(&fence_base(42)),
            "Local\\JStream-VCam-Fence-42"
        );
        assert_eq!(
            Scope::Global.create_name(CONTROL_NAME),
            "Global\\JStream-VCam-Control"
        );
        assert_eq!(
            Scope::Global.open_name(7, CONTROL_NAME),
            Scope::Global.create_name(CONTROL_NAME)
        );
    }

    #[test]
    fn scope_round_trips() {
        for s in [Scope::Global, Scope::Session] {
            assert_eq!(Scope::from_u32(s.as_u32()), Some(s));
        }
        // 0 is the zeroed mapping's value and must never name a scope.
        assert_eq!(Scope::from_u32(0), None);
        assert_eq!(Scope::from_u32(3), None);
        assert_eq!(Scope::Global.as_str(), "global");
        assert_eq!(Scope::Session.as_str(), "session");
    }

    #[test]
    fn open_order_puts_the_console_first_without_duplicates() {
        assert_eq!(
            control_open_names(Some(2)),
            [
                "Global\\JStream-VCam-Control",
                "Session\\2\\JStream-VCam-Control",
                "Session\\1\\JStream-VCam-Control",
                "Session\\3\\JStream-VCam-Control",
                "Session\\4\\JStream-VCam-Control",
            ]
        );
        assert_eq!(
            control_open_names(None).len(),
            1 + SESSION_PROBE_MAX as usize
        );
        assert_eq!(
            control_open_names(Some(9)).len(),
            2 + SESSION_PROBE_MAX as usize
        );
    }

    #[test]
    fn sddl_grants_the_same_principals() {
        for sid in ["SY", "BA", "LS", "IU"] {
            assert!(SDDL_OBJECTS.contains(&format!("(A;;GA;;;{sid})")));
            assert!(SDDL_LOG_DIR.contains(&format!("(A;OICI;GA;;;{sid})")));
        }
        assert!(SDDL_OBJECTS.starts_with("D:P"));
        assert!(SDDL_LOG_DIR.starts_with("D:P"));
    }
}
