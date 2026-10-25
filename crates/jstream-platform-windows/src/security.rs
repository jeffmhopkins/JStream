//! Whether a security descriptor keeps an object to its user: the owner and DACL of an
//! object someone else may have created first, read back as SDDL text and judged here.
//!
//! The Windows side reads the descriptor with `GetSecurityInfo` and turns it into text
//! with `ConvertSecurityDescriptorToStringSecurityDescriptorW`. The decision is pure, on
//! that text, so it is tested on every platform.
//!
//! The rule ([`check`]):
//! - the **owner** must be the user, SYSTEM or Administrators (the default owner of what
//!   an elevated administrator creates). Any other owner may rewrite the DACL at will;
//! - the object must have a **DACL**: a missing or null one (`NO_ACCESS_CONTROL`) lets
//!   everyone in;
//! - every **allow** entry that applies to the object (not inherit-only) must name the
//!   user, SYSTEM, Administrators, `OWNER RIGHTS` (the owner, checked above) or one of the
//!   caller's extra trustees, unless it grants nothing beyond [`HARMLESS`]. Deny entries
//!   are ignored, which can only refuse more, never less. An entry of a type that isn't
//!   a known deny type counts as an allow.
//!
//! Administrators are trusted as root is on Unix: they can take ownership of anything.

use std::fmt;

/// LocalSystem.
pub(crate) const SYSTEM_SID: &str = "S-1-5-18";
/// `BUILTIN\Administrators`.
pub(crate) const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
/// LOCAL SERVICE, Frame Server's account.
pub(crate) const LOCAL_SERVICE_SID: &str = "S-1-5-19";
/// `OWNER RIGHTS`: whoever owns the object.
const OWNER_RIGHTS_SID: &str = "S-1-3-4";

/// Rights another account may hold without being let in: `READ_CONTROL`, `SYNCHRONIZE`
/// and, for files, `FILE_READ_ATTRIBUTES`. They show the descriptor, wait on the object
/// or read its timestamps, never its contents.
pub(crate) const HARMLESS: u32 = 0x0002_0000 | 0x0010_0000 | 0x0000_0080;

/// Why a descriptor was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The SDDL text couldn't be read.
    Unreadable(String),
    /// It names no owner.
    NoOwner,
    /// Its owner is someone else.
    ForeignOwner(String),
    /// It has no DACL, or a null one, so everyone has full access.
    NoDacl,
    /// An allow entry gives `sid` the rights `mask`.
    ForeignGrant {
        /// The trustee, as SDDL names it (a SID string or a two-letter alias).
        sid: String,
        /// The rights it is granted.
        mask: u32,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Unreadable(why) => write!(f, "its security descriptor is unreadable ({why})"),
            Refusal::NoOwner => f.write_str("it has no owner"),
            Refusal::ForeignOwner(sid) => write!(f, "it is owned by another account ({sid})"),
            Refusal::NoDacl => f.write_str("it has no access list, so everyone may open it"),
            Refusal::ForeignGrant { sid, mask } => {
                write!(
                    f,
                    "its access list lets another account in ({sid}, rights {mask:#x})"
                )
            }
        }
    }
}

/// Checks the descriptor `sddl` (owner and DACL, as
/// `ConvertSecurityDescriptorToStringSecurityDescriptorW` writes them) against the rule
/// in the module docs, for the user `user_sid`. `extra` names further trustees the DACL
/// may grant, as SID strings (such as [`LOCAL_SERVICE_SID`] for the vcam's objects).
pub(crate) fn check(sddl: &str, user_sid: &str, extra: &[&str]) -> Result<(), Refusal> {
    let sd = Descriptor::parse(sddl).map_err(Refusal::Unreadable)?;
    let owners = [user_sid, SYSTEM_SID, ADMINISTRATORS_SID];
    let owner = sd.owner.ok_or(Refusal::NoOwner)?;
    if !names_any(&owner, user_sid, &owners) {
        return Err(Refusal::ForeignOwner(owner));
    }
    let dacl = sd.dacl.ok_or(Refusal::NoDacl)?;
    let mut trusted = vec![user_sid, SYSTEM_SID, ADMINISTRATORS_SID, OWNER_RIGHTS_SID];
    trusted.extend_from_slice(extra);
    for ace in dacl {
        let applies = ace.allows && !ace.inherit_only && ace.mask & !HARMLESS != 0;
        if applies && !names_any(&ace.sid, user_sid, &trusted) {
            return Err(Refusal::ForeignGrant {
                sid: ace.sid,
                mask: ace.mask,
            });
        }
    }
    Ok(())
}

/// The owner and DACL of the object `handle` opens, as SDDL text for [`check`]. `kind` is
/// `SE_KERNEL_OBJECT` for a section or `SE_FILE_OBJECT` for a file. The handle must carry
/// `READ_CONTROL`.
#[cfg(windows)]
pub(crate) fn sddl_of(
    handle: windows::Win32::Foundation::HANDLE,
    kind: windows::Win32::Security::Authorization::SE_OBJECT_TYPE,
) -> Result<String, jstream_platform::PlatformError> {
    use crate::com::OsResult;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };
    use windows::core::PWSTR;

    /// Frees what the system allocated with `LocalAlloc`, on drop.
    struct Local(*mut core::ffi::c_void);
    impl Drop for Local {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `LocalFree(HLOCAL) -> HLOCAL`: the block came from LocalAlloc
                // (GetSecurityInfo's descriptor, or the SDDL conversion's string) and is
                // freed exactly once, after its last use.
                unsafe {
                    let _ = LocalFree(Some(HLOCAL(self.0)));
                }
            }
        }
    }

    let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `GetSecurityInfo(HANDLE, SE_OBJECT_TYPE, SECURITY_INFORMATION, PSID*, PSID*,
    // PACL*, PACL*, PSECURITY_DESCRIPTOR*) -> DWORD`: `handle` is a live handle the caller
    // owns, the per-part out-pointers are null (not wanted) and `sd` is a valid
    // out-pointer, released by `_sd`.
    let status =
        unsafe { GetSecurityInfo(handle, kind, info, None, None, None, None, Some(&mut sd)) };
    let _sd = Local(sd.0);
    status.to_hresult().ok().os("GetSecurityInfo")?;
    let mut text = PWSTR::null();
    // SAFETY: `ConvertSecurityDescriptorToStringSecurityDescriptorW(PSECURITY_DESCRIPTOR,
    // DWORD, SECURITY_INFORMATION, LPWSTR*, PULONG) -> BOOL`: `sd` is the live
    // self-relative descriptor GetSecurityInfo returned; `text` is a valid out-pointer,
    // released by `_text`; the length is not wanted.
    unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            sd,
            SDDL_REVISION_1,
            info,
            &mut text,
            None,
        )
    }
    .os("ConvertSecurityDescriptorToStringSecurityDescriptorW")?;
    let _text = Local(text.0.cast());
    // SAFETY: on success `text` is a NUL-terminated wide string, freed by `_text` only
    // after this read.
    unsafe { text.to_string() }.map_err(|_| jstream_platform::PlatformError::Unsupported {
        what: "a security descriptor whose text isn't UTF-16".into(),
    })
}

/// Whether the SDDL trustee `sid` (a SID string or an alias) is one of `sids`.
fn names_any(sid: &str, user_sid: &str, sids: &[&str]) -> bool {
    let sid = resolve(sid, user_sid);
    sids.iter()
        .any(|s| !sid.is_empty() && sid.eq_ignore_ascii_case(s))
}

/// The SID string an SDDL alias stands for; anything else as it is. Only the aliases the
/// rule cares about are resolved: an unknown alias stays two letters, matches no SID
/// string, and so is never trusted. `LA`, the machine's own Administrator account, is the
/// user when the user is that account (relative ID 500).
fn resolve<'a>(sid: &'a str, user_sid: &'a str) -> &'a str {
    match sid.to_ascii_uppercase().as_str() {
        "SY" => SYSTEM_SID,
        "BA" => ADMINISTRATORS_SID,
        "LS" => LOCAL_SERVICE_SID,
        "OW" => OWNER_RIGHTS_SID,
        "LA" if user_sid.starts_with("S-1-5-21-") && user_sid.ends_with("-500") => user_sid,
        _ => sid,
    }
}

/// The parts of a security descriptor the rule reads.
#[derive(Debug, Default, PartialEq, Eq)]
struct Descriptor {
    /// The owner, as SDDL writes it.
    owner: Option<String>,
    /// The DACL's entries; `None` if there is none or it is null.
    dacl: Option<Vec<Ace>>,
}

/// One access-control entry.
#[derive(Debug, PartialEq, Eq)]
struct Ace {
    /// An allow entry (any type that isn't a known deny type).
    allows: bool,
    /// Inherit-only (`IO`): it applies to children, not to the object.
    inherit_only: bool,
    /// The rights it grants or denies.
    mask: u32,
    /// The trustee, as SDDL writes it.
    sid: String,
}

impl Descriptor {
    /// Parses `O:…G:…D:…S:…` text. Components may come in any order; `G:` and `S:` are
    /// skipped.
    fn parse(sddl: &str) -> Result<Descriptor, String> {
        let mut sd = Descriptor::default();
        for (tag, body) in components(sddl)? {
            match tag {
                'O' => sd.owner = Some(body.to_owned()).filter(|s| !s.is_empty()),
                'D' => sd.dacl = parse_dacl(body)?,
                _ => {}
            }
        }
        Ok(sd)
    }
}

/// Splits SDDL text into its `X:` components, outside parentheses.
fn components(sddl: &str) -> Result<Vec<(char, &str)>, String> {
    let bytes = sddl.as_bytes();
    let mut starts = Vec::new();
    let mut depth = 0usize;
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth = depth.checked_sub(1).ok_or("unbalanced parentheses")?,
            b'O' | b'G' | b'D' | b'S' if depth == 0 && bytes.get(i + 1) == Some(&b':') => {
                starts.push(i);
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err("unbalanced parentheses".into());
    }
    if starts.first() != Some(&0) && !sddl.is_empty() {
        return Err("it doesn't start with a component".into());
    }
    let mut out = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(sddl.len());
        let tag = char::from(bytes[start]);
        out.push((tag, &sddl[start + 2..end]));
    }
    Ok(out)
}

/// A DACL's body: flags (`P`, `AI`, `AR`, or `NO_ACCESS_CONTROL` for a null DACL), then
/// its entries in parentheses. `None` for a null DACL.
fn parse_dacl(body: &str) -> Result<Option<Vec<Ace>>, String> {
    let flags_end = body.find('(').unwrap_or(body.len());
    if body[..flags_end].contains("NO_ACCESS_CONTROL") {
        return Ok(None);
    }
    let mut aces = Vec::new();
    let mut rest = &body[flags_end..];
    while !rest.is_empty() {
        let inner = rest.strip_prefix('(').ok_or("text between entries")?;
        // Conditional entries nest parentheses, so the entry ends where depth returns to 0.
        let mut depth = 1usize;
        let mut end = None;
        for (i, c) in inner.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.ok_or("an unterminated entry")?;
        aces.push(parse_ace(&inner[..end])?);
        rest = &inner[end + 1..];
    }
    Ok(Some(aces))
}

/// One entry: `type;flags;rights;object;inherited object;trustee[;…]`.
fn parse_ace(text: &str) -> Result<Ace, String> {
    let fields: Vec<&str> = text.splitn(7, ';').collect();
    let [kind, flags, rights, _, _, sid, ..] = fields.as_slice() else {
        return Err(format!("a malformed entry ({text})"));
    };
    if sid.is_empty() {
        return Err(format!("an entry with no trustee ({text})"));
    }
    let allows = !matches!(kind.to_ascii_uppercase().as_str(), "D" | "OD" | "XD");
    let inherit_only = pairs(flags)?.any(|f| f.eq_ignore_ascii_case("IO"));
    Ok(Ace {
        allows,
        inherit_only,
        mask: parse_rights(rights)?,
        sid: (*sid).to_owned(),
    })
}

/// A string of two-letter codes, two letters at a time.
fn pairs(s: &str) -> Result<impl Iterator<Item = &str>, String> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return Err(format!("odd flags ({s})"));
    }
    Ok((0..s.len()).step_by(2).map(move |i| &s[i..i + 2]))
}

/// An entry's rights: a number (`0x…` or decimal) or two-letter codes.
fn parse_rights(s: &str) -> Result<u32, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).map_err(|_| format!("bad rights ({s})"));
    }
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().map_err(|_| format!("bad rights ({s})"));
    }
    let mut mask = 0u32;
    for code in pairs(s)? {
        mask |= right(code).ok_or_else(|| format!("unknown right {code}"))?;
    }
    Ok(mask)
}

/// The access mask an SDDL right code stands for (`sddl.h`).
fn right(code: &str) -> Option<u32> {
    Some(match code.to_ascii_uppercase().as_str() {
        "GA" => 0x1000_0000,
        "GR" => 0x8000_0000,
        "GW" => 0x4000_0000,
        "GX" => 0x2000_0000,
        "RC" => 0x0002_0000,
        "SD" => 0x0001_0000,
        "WD" => 0x0004_0000,
        "WO" => 0x0008_0000,
        "CC" | "NR" => 0x1,
        "DC" | "NW" => 0x2,
        "LC" | "NX" => 0x4,
        "SW" => 0x8,
        "RP" => 0x10,
        "WP" => 0x20,
        "DT" => 0x40,
        "LO" => 0x80,
        "CR" => 0x100,
        "FA" => 0x001F_01FF,
        "FR" => 0x0012_0089,
        "FW" => 0x0012_0116,
        "FX" => 0x0012_00A0,
        "KA" => 0x000F_003F,
        "KR" | "KX" => 0x0002_0019,
        "KW" => 0x0002_0006,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: &str = "S-1-5-21-1004336348-1177238915-682003330-1001";
    const BOB: &str = "S-1-5-21-1004336348-1177238915-682003330-1002";

    fn grant(sddl: &str) -> Result<(), Refusal> {
        check(sddl, ALICE, &[])
    }

    #[test]
    fn the_vcam_contract_dacl_passes_with_local_service_trusted() {
        let sddl = jstream_vcam_proto::sddl_objects(ALICE).unwrap_or_default();
        let read_back = format!("O:{ALICE}G:{ALICE}{sddl}");
        assert_eq!(check(&read_back, ALICE, &[LOCAL_SERVICE_SID]), Ok(()));
        // Without LOCAL SERVICE among the trustees, its entry is foreign.
        assert!(matches!(
            check(&read_back, ALICE, &[]),
            Err(Refusal::ForeignGrant { sid, .. }) if sid == "LS"
        ));
        // As Windows writes it back: generic rights mapped, owner an alias.
        let mapped = format!(
            "O:BAD:P(A;;0xf001f;;;SY)(A;;0xf001f;;;BA)(A;;0xf001f;;;LS)(A;;0xf001f;;;{ALICE})"
        );
        assert_eq!(check(&mapped, ALICE, &[LOCAL_SERVICE_SID]), Ok(()));
    }

    #[test]
    fn a_user_only_file_passes() {
        assert_eq!(
            grant(&format!("O:{ALICE}D:P(A;;FA;;;SY)(A;;FA;;;{ALICE})")),
            Ok(())
        );
        // What `%APPDATA%` hands down: inherited entries for SYSTEM, Administrators and the
        // user.
        assert_eq!(
            grant(&format!(
                "O:{ALICE}G:{ALICE}D:AI(A;ID;FA;;;SY)(A;ID;FA;;;BA)(A;ID;FA;;;{ALICE})"
            )),
            Ok(())
        );
        // An empty DACL lets no one in.
        assert_eq!(grant(&format!("O:{ALICE}D:P")), Ok(()));
        // SIDs compare without regard to case.
        assert_eq!(
            grant(&format!("O:{}D:P", ALICE.to_ascii_lowercase())),
            Ok(())
        );
    }

    /// #344, #356: a looser DACL is refused.
    #[test]
    fn other_accounts_with_real_rights_are_refused() {
        for (trustee, rights) in [
            ("WD", "FA"),
            ("IU", "GA"),
            ("BU", "FR"),
            ("AU", "0x1301bf"),
            (BOB, "GR"),
            (BOB, "WD"),
            (BOB, "CC"),
            ("BU", "0x1200a9"),
            ("LS", "GA"),
        ] {
            let sddl = format!("O:{ALICE}D:(A;;FA;;;{ALICE})(A;;{rights};;;{trustee})");
            assert!(
                matches!(grant(&sddl), Err(Refusal::ForeignGrant { ref sid, .. }) if sid == trustee),
                "{sddl}"
            );
        }
        // Object and callback allow entries count too, as does a type the rule doesn't know.
        for kind in ["OA", "XA", "ZA", "AU"] {
            let sddl = format!("O:{ALICE}D:({kind};;GA;;;WD)");
            assert!(grant(&sddl).is_err(), "{sddl}");
        }
    }

    #[test]
    fn harmless_inherit_only_and_deny_entries_pass() {
        // Read the descriptor, synchronize, read the attributes: nothing of the contents.
        assert_eq!(grant(&format!("O:{ALICE}D:(A;;0x120080;;;WD)")), Ok(()));
        assert_eq!(grant(&format!("O:{ALICE}D:(A;;RC;;;WD)")), Ok(()));
        // Inherit-only entries apply to children, not to the object.
        assert_eq!(
            grant(&format!("O:{ALICE}D:(A;OICIIO;GA;;;CO)(A;CIIO;FA;;;BU)")),
            Ok(())
        );
        // Deny entries never let anyone in.
        assert_eq!(
            grant(&format!("O:{ALICE}D:(D;;FA;;;WD)(D;;GA;;;{BOB})")),
            Ok(())
        );
        // OWNER RIGHTS is the owner, who is checked separately.
        assert_eq!(grant(&format!("O:{ALICE}D:(A;;FA;;;OW)")), Ok(()));
    }

    #[test]
    fn a_foreign_or_missing_owner_is_refused() {
        assert_eq!(
            grant(&format!("O:{BOB}D:P(A;;FA;;;{ALICE})")),
            Err(Refusal::ForeignOwner(BOB.into()))
        );
        assert_eq!(grant("O:WDD:P"), Err(Refusal::ForeignOwner("WD".into())));
        assert_eq!(
            grant(&format!("D:P(A;;FA;;;{ALICE})")),
            Err(Refusal::NoOwner)
        );
        // An elevated administrator's objects are owned by Administrators, or SYSTEM's.
        assert_eq!(grant(&format!("O:BAD:P(A;;FA;;;{ALICE})")), Ok(()));
        assert_eq!(grant(&format!("O:SYD:P(A;;FA;;;{ALICE})")), Ok(()));
    }

    #[test]
    fn a_missing_or_null_dacl_is_refused() {
        assert_eq!(grant(&format!("O:{ALICE}")), Err(Refusal::NoDacl));
        assert_eq!(
            grant(&format!("O:{ALICE}D:NO_ACCESS_CONTROL")),
            Err(Refusal::NoDacl)
        );
        assert_eq!(
            grant(&format!("O:{ALICE}G:SYD:NO_ACCESS_CONTROL")),
            Err(Refusal::NoDacl)
        );
    }

    #[test]
    fn the_builtin_administrator_is_recognised_by_its_alias() {
        let admin = "S-1-5-21-1004336348-1177238915-682003330-500";
        assert_eq!(check("O:BAD:P(A;;FA;;;LA)(A;;FA;;;SY)", admin, &[]), Ok(()));
        // For anyone else `LA` is another account.
        assert!(check("O:BAD:P(A;;FA;;;LA)", ALICE, &[]).is_err());
    }

    #[test]
    fn components_split_outside_parentheses() {
        let sd = Descriptor::parse(
            "O:BAG:DGD:PAI(A;;FA;;;SY)(XA;;FX;;;WD;(@User.Title==\"D:S:\"))S:(AU;FA;FA;;;WD)",
        )
        .unwrap_or_default();
        assert_eq!(sd.owner.as_deref(), Some("BA"));
        let dacl = sd.dacl.unwrap_or_default();
        assert_eq!(dacl.len(), 2);
        assert_eq!(dacl[1].sid, "WD");
        assert_eq!(dacl[1].mask, 0x0012_00A0);
    }

    #[test]
    fn malformed_text_is_refused() {
        for bad in [
            "garbage",
            "O:BAD:(A;;FA;;SY)",
            "O:BAD:(A;;FA;;;SY",
            "O:BAD:(A;;ZZ;;;SY)",
            "O:BAD:(A;;0xfffffffff;;;SY)",
            "O:BAD:(A;;FA;;;)",
            "O:BAD:(A;I;FA;;;SY)",
            "O:BAD:(A;;FA;;;SY)x(A;;FA;;;SY)",
            "O:BAD:(A;;FA;;;SY))",
        ] {
            assert!(
                matches!(grant(bad), Err(Refusal::Unreadable(_))),
                "{bad}: {:?}",
                grant(bad)
            );
        }
    }

    #[test]
    fn rights_read_as_numbers_or_codes() {
        assert_eq!(parse_rights("0x1f01ff"), Ok(0x1F_01FF));
        assert_eq!(parse_rights("2032127"), Ok(0x1F_01FF));
        assert_eq!(parse_rights("FA"), Ok(0x1F_01FF));
        assert_eq!(parse_rights("RCSDWDWO"), Ok(0x000F_0000));
        assert_eq!(parse_rights(""), Ok(0));
    }

    #[test]
    fn refusals_read_as_sentences() {
        let r = Refusal::ForeignGrant {
            sid: "WD".into(),
            mask: 0x1F_01FF,
        };
        assert_eq!(
            r.to_string(),
            "its access list lets another account in (WD, rights 0x1f01ff)"
        );
    }
}
