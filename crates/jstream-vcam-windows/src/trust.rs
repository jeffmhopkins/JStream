//! Whether a control block found in a session namespace may be used (#180).
//!
//! The DLL probes `Global\`, then the console session, then sessions 1 to 4. A
//! non-elevated user can only create objects in their own session's namespace, so a block
//! found as `Session\<id>\…` was made by whoever is logged on to that session. Without a
//! check, a user in another session (fast user switching, or a second remote session)
//! could publish a block there and have the DLL serve their frames to a camera someone
//! else created, whenever the console session has none.
//!
//! The rule, applied to every `Session\` block before the DLL writes to it:
//! - the session must be **active** now (connected to the console, or an active remote
//!   connection), so a disconnected user's engine can't feed the person at the machine;
//! - the mapping's **owner** must be that session's logged-on user, or SYSTEM or
//!   Administrators (the default owner of what an elevated administrator creates).
//!
//! The Windows side reads the owner with `GetSecurityInfo`, and the session's user and
//! state with `WTSQuerySessionInformationW` plus `LookupAccountNameW`, all of which work
//! in Frame Server's LOCAL SERVICE process. (`WTSQueryUserToken` would give the user's
//! token directly, but needs `SeTcbPrivilege`, which LOCAL SERVICE lacks.) The decision
//! itself is pure, on SID strings, so it is tested on every platform.
//!
//! `Global\` blocks are not checked: creating a global section needs
//! `SeCreateGlobalPrivilege`, which only administrators and services hold, and they can
//! already reach Frame Server's process.

/// LocalSystem's SID.
pub const LOCAL_SYSTEM_SID: &str = "S-1-5-18";
/// `BUILTIN\Administrators`' SID: the default owner of objects an elevated administrator
/// creates.
pub const ADMINISTRATORS_SID: &str = "S-1-5-32-544";

/// Why a session-scoped control block was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distrust {
    /// The session isn't active now (disconnected, or its state couldn't be read).
    Inactive,
    /// No user is logged on to the session, or their SID couldn't be found.
    NoUser,
    /// The mapping's owner is neither the session's user nor SYSTEM or Administrators.
    ForeignOwner,
}

impl Distrust {
    /// A short reason for logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Distrust::Inactive => "the session is not active",
            Distrust::NoUser => "the session's user is unknown",
            Distrust::ForeignOwner => "the block's owner is not the session's user",
        }
    }
}

/// The session a consumer-side control-block name (`Session\<id>\…`) opens, or `None` for
/// any other name, such as `Global\…`.
pub fn session_of(name: &str) -> Option<u32> {
    let (id, _) = name.strip_prefix("Session\\")?.split_once('\\')?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    id.parse().ok()
}

/// Whether a block found in a session's namespace may be used, given its owner's SID, the
/// SID of the user logged on to that session (`None` if none or unknown), and whether the
/// session is active. SIDs are compared as strings, ignoring ASCII case.
pub fn check_session_block(
    owner_sid: &str,
    session_user_sid: Option<&str>,
    session_active: bool,
) -> Result<(), Distrust> {
    if !session_active {
        return Err(Distrust::Inactive);
    }
    let user = session_user_sid
        .filter(|s| !s.is_empty())
        .ok_or(Distrust::NoUser)?;
    let trusted = [user, LOCAL_SYSTEM_SID, ADMINISTRATORS_SID]
        .iter()
        .any(|s| !owner_sid.is_empty() && owner_sid.eq_ignore_ascii_case(s));
    if trusted {
        Ok(())
    } else {
        Err(Distrust::ForeignOwner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: &str = "S-1-5-21-1004336348-1177238915-682003330-1001";
    const BOB: &str = "S-1-5-21-1004336348-1177238915-682003330-1002";

    #[test]
    fn session_of_reads_only_session_names() {
        assert_eq!(session_of("Session\\1\\JStream-VCam-Control"), Some(1));
        assert_eq!(session_of("Session\\12\\JStream-VCam-Control"), Some(12));
        assert_eq!(session_of("Global\\JStream-VCam-Control"), None);
        assert_eq!(session_of("Session\\\\JStream-VCam-Control"), None);
        assert_eq!(session_of("Session\\+1\\JStream-VCam-Control"), None);
        assert_eq!(session_of("Session\\1"), None);
        assert_eq!(session_of("Local\\JStream-VCam-Control"), None);
    }

    #[test]
    fn the_session_user_owning_an_active_session_block_is_trusted() {
        assert_eq!(check_session_block(ALICE, Some(ALICE), true), Ok(()));
        assert_eq!(
            check_session_block(&ALICE.to_ascii_lowercase(), Some(ALICE), true),
            Ok(())
        );
        assert_eq!(
            check_session_block(LOCAL_SYSTEM_SID, Some(ALICE), true),
            Ok(())
        );
        assert_eq!(
            check_session_block(ADMINISTRATORS_SID, Some(ALICE), true),
            Ok(())
        );
    }

    /// #180: another user's block must not feed this camera.
    #[test]
    fn another_users_block_is_refused() {
        // Bob, switched away from (his session is disconnected), left his engine running.
        assert_eq!(
            check_session_block(BOB, Some(BOB), false),
            Err(Distrust::Inactive)
        );
        // A block in Alice's active session that Bob owns.
        assert_eq!(
            check_session_block(BOB, Some(ALICE), true),
            Err(Distrust::ForeignOwner)
        );
        // A session with no user, or an owner that couldn't be read.
        assert_eq!(check_session_block(BOB, None, true), Err(Distrust::NoUser));
        assert_eq!(
            check_session_block(BOB, Some(""), true),
            Err(Distrust::NoUser)
        );
        assert_eq!(
            check_session_block("", Some(ALICE), true),
            Err(Distrust::ForeignOwner)
        );
        // Other well-known accounts are not enough.
        for sid in ["S-1-5-19", "S-1-5-20", "S-1-1-0", "S-1-5-32-545"] {
            assert_eq!(
                check_session_block(sid, Some(ALICE), true),
                Err(Distrust::ForeignOwner)
            );
        }
    }
}
