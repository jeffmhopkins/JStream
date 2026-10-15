//! Names and security of the shared kernel objects: the control block, the ring textures
//! and the fence.

/// Base name of the control block's file mapping.
pub const CONTROL_NAME: &str = "JStream-VCam-Control";

/// The DACL (SDDL) for the textures, the fence and the control block: full access for
/// SYSTEM, Administrators, LOCAL SERVICE (Frame Server's account) and the producing user,
/// whose SID string (`S-1-5-21-…`) the producer looks up for its own token. Protected
/// (`P`), so nothing is inherited, and no other user is let in: spike S2's grant to every
/// interactive user (`IU`) would let another logged-on user rewrite the control block that
/// steers the DLL inside Frame Server, or read the camera's frames.
///
/// Returns `None` unless `user_sid` is a well-formed SID string, so a malformed value can't
/// add entries to the DACL.
pub fn sddl_objects(user_sid: &str) -> Option<String> {
    is_sid(user_sid).then(|| format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;LS)(A;;GA;;;{user_sid})"))
}

/// Whether `s` is a SID string: `S-1-`, an identifier authority, then one or more
/// sub-authorities, each a decimal number.
fn is_sid(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("S-1-") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() >= 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()))
}

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
    fn sddl_names_the_producing_user_and_no_one_else() {
        let sid = "S-1-5-21-1004336348-1177238915-682003330-1001";
        assert_eq!(
            sddl_objects(sid).as_deref(),
            Some(
                "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;LS)\
                 (A;;GA;;;S-1-5-21-1004336348-1177238915-682003330-1001)"
            )
        );
        let sddl = sddl_objects(sid).unwrap_or_default();
        assert!(!sddl.contains(";IU)"));
        assert!(!sddl.contains(";WD)"));
        assert_eq!(sddl.matches("(A;").count(), 4);
    }

    #[test]
    fn sddl_rejects_anything_but_a_sid() {
        for bad in [
            "",
            "IU",
            "WD",
            "S-1-5",
            "S-1-",
            "S-1-5-",
            "S-1-5--18",
            "S-2-5-18",
            "s-1-5-18",
            "S-1-5-18)(A;;GA;;;WD",
            "S-1-5-18 ",
            "S-1-5-99999999999",
        ] {
            assert_eq!(sddl_objects(bad), None, "{bad:?}");
        }
        assert!(sddl_objects("S-1-5-18").is_some());
    }
}
