//! Did Frame Server load the media source during this run? S2's headline question.
//!
//! Two sources of evidence, in order of trust:
//! 1. **The control block.** The DLL records its host's identity (session 0, svchost,
//!    LOCAL SERVICE) and bumps counters when it attaches, creates a source or serves a
//!    sample. The producer snapshots those at start and compares at the end, so a DLL that
//!    stayed loaded from an earlier run still counts. Deltas, not totals, because the block
//!    outlives a producer while the DLL holds it open.
//! 2. **The DLL's log.** Each log file starts with a `LOADED` line naming the host process
//!    and account. Used when the DLL never reached the control block (for example when it
//!    can't open the cross-session names) but was loaded.
//!
//! Platform-independent, so its tests run on any host.

use crate::proto::{CONSUMER_KNOWN, CONSUMER_LOCAL_SERVICE, CONSUMER_SESSION0, CONSUMER_SVCHOST};

/// The consumer side of the control block at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsumerSnapshot {
    pub pid: u32,
    pub attaches: u32,
    pub activations: u32,
    pub served: u64,
    pub shared: u64,
    pub session_id: u32,
    pub flags: u32,
}

impl ConsumerSnapshot {
    /// Whether the recorded host looks like Frame Server (a service process).
    pub fn host_is_service(&self) -> bool {
        self.flags & CONSUMER_KNOWN != 0
            && self.flags & (CONSUMER_SESSION0 | CONSUMER_SVCHOST | CONSUMER_LOCAL_SERVICE) != 0
    }

    /// Whether a DLL attached, created a source or served a sample since `before`.
    pub fn active_since(&self, before: &ConsumerSnapshot) -> bool {
        self.attaches > before.attaches
            || self.activations > before.activations
            || self.served > before.served
    }
}

/// What the DLL's logs from this run say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogEvidence {
    /// Some log file has a `LOADED` line at all.
    pub any_loaded: bool,
    /// Some `LOADED` line names a service host (svchost or LOCAL SERVICE).
    pub service_loaded: bool,
}

/// The host named by one `LOADED …` line (see the DLL's `log::identity`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedBy {
    pub process: String,
    pub user: String,
    pub session: Option<u32>,
    pub flags: Option<u32>,
}

impl LoadedBy {
    /// Parses the `LOADED` part of a log line, if it has one.
    pub fn parse(line: &str) -> Option<Self> {
        let at = line.find("LOADED ")?;
        let l = &line[at..];
        let flags = field(l, " flags=", " ")
            .and_then(|f| u32::from_str_radix(f.trim_start_matches("0x"), 16).ok());
        Some(Self {
            process: field(l, " process=", " user=")
                .unwrap_or_default()
                .to_owned(),
            user: field(l, " user=", " session=")
                .unwrap_or_default()
                .to_owned(),
            session: field(l, " session=", " ").and_then(|s| s.parse().ok()),
            flags,
        })
    }

    /// Frame Server is a svchost-hosted service in session 0 running as LOCAL SERVICE.
    /// The Camera app or a browser loading the DLL in-process would show its own exe, the
    /// user's session and the user's account instead.
    pub fn is_service(&self) -> bool {
        let by_flags = self.flags.is_some_and(|f| {
            f & CONSUMER_KNOWN != 0
                && f & (CONSUMER_SESSION0 | CONSUMER_SVCHOST | CONSUMER_LOCAL_SERVICE) != 0
        });
        by_flags
            || self.session == Some(0)
            || self.process.to_ascii_lowercase().ends_with("svchost.exe")
            || self.user.eq_ignore_ascii_case("LOCAL SERVICE")
    }

    pub fn describe(&self) -> String {
        let session = self.session.map_or_else(|| "?".into(), |s| s.to_string());
        format!("{} ({}, session {session})", self.process, self.user)
    }
}

/// The text between `start` and the next `end` (or the end of `s`).
fn field<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let a = s.find(start)? + start.len();
    let b = s[a..].find(end).map_or(s.len(), |i| a + i);
    Some(&s[a..b])
}

impl LogEvidence {
    pub fn from_hosts(hosts: &[LoadedBy]) -> Self {
        Self {
            any_loaded: !hosts.is_empty(),
            service_loaded: hosts.iter().any(LoadedBy::is_service),
        }
    }
}

/// Which evidence decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecidedBy {
    ControlBlock,
    DllLog,
    Nothing,
}

impl DecidedBy {
    pub fn as_str(self) -> &'static str {
        match self {
            DecidedBy::ControlBlock => "control_block",
            DecidedBy::DllLog => "dll_log",
            DecidedBy::Nothing => "none",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Verdict {
    pub frame_server_loaded: bool,
    pub decided_by: DecidedBy,
    /// Set when the two sources disagree; the report shows it.
    pub inconsistency: Option<String>,
}

pub fn decide(before: &ConsumerSnapshot, after: &ConsumerSnapshot, log: LogEvidence) -> Verdict {
    if after.active_since(before) {
        let loaded = after.host_is_service();
        let inconsistency = (loaded != log.service_loaded).then(|| {
            format!(
                "control block: consumer pid {} (session {}, flags {:#x}) active this run; \
                 DLL log: service LOADED line {}",
                after.pid,
                after.session_id,
                after.flags,
                if log.service_loaded {
                    "present"
                } else {
                    "absent"
                }
            )
        });
        return Verdict {
            frame_server_loaded: loaded,
            decided_by: DecidedBy::ControlBlock,
            inconsistency,
        };
    }
    if log.any_loaded {
        return Verdict {
            frame_server_loaded: log.service_loaded,
            decided_by: DecidedBy::DllLog,
            inconsistency: None,
        };
    }
    Verdict {
        frame_server_loaded: false,
        decided_by: DecidedBy::Nothing,
        inconsistency: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(served: u64) -> ConsumerSnapshot {
        ConsumerSnapshot {
            pid: 1234,
            attaches: 1,
            activations: 1,
            served,
            shared: served,
            session_id: 0,
            flags: CONSUMER_KNOWN | CONSUMER_SESSION0 | CONSUMER_SVCHOST,
        }
    }

    #[test]
    fn dll_still_loaded_from_earlier_run_counts_without_log() {
        // Same attach as before (block kept open), but samples were served this run, and
        // the log was cleared so it has no LOADED line: B1's false-negative case.
        let before = service(100);
        let after = service(400);
        let v = decide(&before, &after, LogEvidence::default());
        assert!(v.frame_server_loaded);
        assert_eq!(v.decided_by, DecidedBy::ControlBlock);
        assert!(v.inconsistency.is_some());
    }

    #[test]
    fn fresh_attach_by_service() {
        let log = LogEvidence {
            any_loaded: true,
            service_loaded: true,
        };
        let v = decide(&ConsumerSnapshot::default(), &service(10), log);
        assert!(v.frame_server_loaded);
        assert_eq!(v.inconsistency, None);
    }

    #[test]
    fn in_process_consumer_is_not_frame_server() {
        let after = ConsumerSnapshot {
            attaches: 1,
            served: 5,
            session_id: 1,
            flags: CONSUMER_KNOWN,
            ..Default::default()
        };
        let v = decide(&ConsumerSnapshot::default(), &after, LogEvidence::default());
        assert!(!v.frame_server_loaded);
        assert_eq!(v.decided_by, DecidedBy::ControlBlock);
    }

    #[test]
    fn log_decides_when_block_never_reached() {
        let log = LogEvidence {
            any_loaded: true,
            service_loaded: true,
        };
        let s = ConsumerSnapshot::default();
        let v = decide(&s, &s, log);
        assert!(v.frame_server_loaded);
        assert_eq!(v.decided_by, DecidedBy::DllLog);
    }

    #[test]
    fn parses_identity_lines() {
        let svc = LoadedBy::parse(
            "1.000 pid=9 LOADED dll=C:\\x y\\s2_vcam.dll process=C:\\Windows\\System32\\svchost.exe \
             user=LOCAL SERVICE session=0 flags=0xf version=0.0.0",
        );
        let svc = svc.expect("has a LOADED part");
        assert_eq!(svc.process, "C:\\Windows\\System32\\svchost.exe");
        assert_eq!(svc.user, "LOCAL SERVICE");
        assert_eq!(svc.session, Some(0));
        assert_eq!(svc.flags, Some(0xf));
        assert!(svc.is_service());

        let app = LoadedBy::parse(
            "2.000 pid=7 LOADED dll=d process=C:\\apps\\WindowsCamera.exe user=jeff \
             session=1 flags=0x8 version=0.0.0",
        )
        .expect("has a LOADED part");
        assert!(!app.is_service());
        assert_eq!(LoadedBy::parse("2.000 pid=7 Source::Start"), None);

        let log = LogEvidence::from_hosts(&[app, svc]);
        assert!(log.any_loaded && log.service_loaded);
    }

    #[test]
    fn no_evidence() {
        let s = service(7);
        let v = decide(&s, &s, LogEvidence::default());
        assert!(!v.frame_server_loaded);
        assert_eq!(v.decided_by, DecidedBy::Nothing);
    }
}
