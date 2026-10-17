//! Local crash reports (R-SEC-004: crash data is written locally and never uploaded),
//! as the daemon writes them (apps/jstream-daemon/src/crash.rs). The two apps are both
//! layer 6, so neither can use the other's module.
//!
//! A panic hook writes `crash-<UTC time>-<pid>-<n>.txt` into the data folder's `logs`
//! folder: the app and protocol versions, the OS and architecture, the time, the
//! process ID, the panicking thread's name, the panic's location and message, and a
//! backtrace. It holds nothing else: no environment, no command line, no settings.
//! Registered secrets (the control token, and every stream key that passes through a
//! `Redacting` key store) are replaced by `***` wherever they appear (R-SEC-001), and
//! only their BLAKE3 digests are kept for that, never their text. The newest
//! [`MAX_REPORTS`] reports are kept.
//!
//! Docs: apps/jstream-desktop/README.md#design

#[cfg(any(windows, test))]
use jstream_platform::{PlatformError, Secret as StoredSecret, StreamKeyStore};
use jstream_protocol::PROTOCOL_VERSION;
use std::backtrace::Backtrace;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, TryLockError};
use std::time::{SystemTime, UNIX_EPOCH};

/// The crash reports kept, the newest first.
pub const MAX_REPORTS: usize = 10;

/// The file name prefix of crash reports.
pub const REPORT_PREFIX: &str = "crash-";

/// The file name suffix of crash reports.
pub const REPORT_SUFFIX: &str = ".txt";

/// The longest panic message a report quotes, in bytes; the rest is cut.
const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// What the report says in place of a secret.
const REDACTED: &str = "***";

/// A secret to redact: its length in bytes and its BLAKE3 digest.
type Secret = (usize, blake3::Hash);

/// The secrets to redact from reports. Only their digests are kept.
static SECRETS: Mutex<Vec<Secret>> = Mutex::new(Vec::new());

/// Registers `value` (the control token, a stream key) to be replaced by `***` in
/// crash reports. Empty values are ignored.
pub fn register_secret(value: &str) {
    if value.is_empty() {
        return;
    }
    let secret = (value.len(), blake3::hash(value.as_bytes()));
    let mut secrets = SECRETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !secrets.contains(&secret) {
        secrets.push(secret);
    }
}

/// A key store that registers every stream key it stores or loads with the crash
/// reports' redaction ([`register_secret`]), so a panic that quotes a key writes `***`
/// in its place (#353). The live engine's key store is wrapped in it, so a key is
/// registered when `output.set_key` stores it and when the publish opener reads it,
/// before anything could format it. Only digests are kept.
#[cfg(any(windows, test))]
pub struct Redacting<S: ?Sized>(pub std::sync::Arc<S>);

#[cfg(any(windows, test))]
impl<S: StreamKeyStore + ?Sized> StreamKeyStore for Redacting<S> {
    fn set(&self, output_id: &str, key: &StoredSecret) -> Result<(), PlatformError> {
        register_secret(key.expose());
        self.0.set(output_id, key)
    }

    fn get(&self, output_id: &str) -> Result<Option<StoredSecret>, PlatformError> {
        let key = self.0.get(output_id)?;
        if let Some(key) = &key {
            register_secret(key.expose());
        }
        Ok(key)
    }

    fn clear(&self, output_id: &str) -> Result<(), PlatformError> {
        self.0.clear(output_id)
    }

    fn is_set(&self, output_id: &str) -> Result<bool, PlatformError> {
        self.0.is_set(output_id)
    }
}

/// Whether `value` is registered, for tests.
#[cfg(test)]
pub(crate) fn is_registered(value: &str) -> bool {
    let secret = (value.len(), blake3::hash(value.as_bytes()));
    SECRETS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&secret)
}

/// Installs the panic hook that writes crash reports into `dir`, creating it (with its
/// parents) if it is missing, and prunes old reports there. It replaces the default
/// hook, whose stderr line would quote the message unredacted, with a redacted line
/// naming the report.
pub fn install(dir: PathBuf) {
    // Best effort: if the folder can't be made, each report says so on stderr.
    let _ = fs::create_dir_all(&dir);
    prune(&dir, MAX_REPORTS);
    std::panic::set_hook(Box::new(move |info| on_panic(&dir, info)));
}

/// The hook. It must not panic: a panic here aborts the process.
fn on_panic(dir: &Path, info: &PanicHookInfo<'_>) {
    let now = SystemTime::now();
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("<unnamed>");
    let location = info.location().map_or_else(
        || "<unknown>".to_owned(),
        |at| format!("{}:{}:{}", at.file(), at.line(), at.column()),
    );
    let message = payload_text(info.payload());
    let backtrace = Backtrace::force_capture();
    let (report, summary) = match SECRETS.try_lock() {
        Ok(secrets) => (
            redacted_report(now, thread, &location, &message, &backtrace, &secrets),
            redact(&message, &secrets),
        ),
        Err(TryLockError::Poisoned(secrets)) => {
            let secrets = secrets.into_inner();
            (
                redacted_report(now, thread, &location, &message, &backtrace, &secrets),
                redact(&message, &secrets),
            )
        }
        // The panicking thread holds the list, so nothing can be redacted: leave the
        // message out entirely.
        Err(TryLockError::WouldBlock) => {
            let withheld = "<withheld: the secrets to redact were unavailable>";
            (
                report(now, thread, &location, withheld, &backtrace),
                withheld.to_owned(),
            )
        }
    };
    let written = write_report(dir, now, &report);
    let mut err = io::stderr().lock();
    let summary = truncate(&summary, 512);
    // Best effort: the report is what matters. A release build has no console.
    let _ = match &written {
        Ok(path) => writeln!(
            err,
            "error: panic in thread '{thread}' at {location}: {summary}\n\
             crash report: {}",
            path.display()
        ),
        Err(error) => writeln!(
            err,
            "error: panic in thread '{thread}' at {location}: {summary}\n\
             could not write a crash report in `{}`: {error}",
            dir.display()
        ),
    };
}

/// The panic message: the payload when it is a string.
fn payload_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "<not a string>".to_owned()
    }
}

/// The report with every registered secret redacted. The message is redacted before
/// [`report`] cuts it to [`MAX_MESSAGE_BYTES`], so a secret straddling the cut can't
/// leave its prefix behind; the whole report is redacted again for the location and
/// backtrace.
fn redacted_report(
    now: SystemTime,
    thread: &str,
    location: &str,
    message: &str,
    backtrace: &Backtrace,
    secrets: &[Secret],
) -> String {
    let message = redact(message, secrets);
    redact(&report(now, thread, location, &message, backtrace), secrets)
}

/// The report's text, before redaction.
fn report(
    now: SystemTime,
    thread: &str,
    location: &str,
    message: &str,
    backtrace: &Backtrace,
) -> String {
    format!(
        "JStream crash report (panic)\n\
         program: jstream-desktop {version}, protocol {PROTOCOL_VERSION}\n\
         os: {os} {arch}\n\
         time: {time}\n\
         process: {pid}\n\
         thread: {thread}\n\
         location: {location}\n\
         message: {message}\n\
         \n\
         backtrace:\n{backtrace}\n",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        time = Utc::from(now).rfc3339(),
        pid = std::process::id(),
        message = truncate(message, MAX_MESSAGE_BYTES),
    )
}

/// `text` cut to at most `max` bytes, on a character boundary, marked when cut.
fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (cut)", &text[..end])
}

/// `text` with every occurrence of a secret replaced by `***`. Each secret is found by
/// hashing every window of its length, so only digests are needed.
fn redact(text: &str, secrets: &[Secret]) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut start = 0;
    let mut at = 0;
    'scan: while at < bytes.len() {
        for (len, digest) in secrets {
            let Some(window) = bytes.get(at..at + len) else {
                continue;
            };
            if blake3::hash(window) == *digest
                && text.is_char_boundary(at)
                && text.is_char_boundary(at + len)
            {
                out.push_str(&text[start..at]);
                out.push_str(REDACTED);
                at += len;
                start = at;
                continue 'scan;
            }
        }
        at += 1;
    }
    out.push_str(&text[start..]);
    out
}

/// Writes `report` to a new file in `dir`, after pruning the oldest reports so that
/// [`MAX_REPORTS`] remain with it. The file is user-only on Unix (mode 0600), and gets
/// its folder's ACL on Windows.
fn write_report(dir: &Path, now: SystemTime, report: &str) -> io::Result<PathBuf> {
    prune(dir, MAX_REPORTS.saturating_sub(1));
    let stamp = Utc::from(now).compact();
    let pid = std::process::id();
    let mut sequence = 0u32;
    loop {
        let path = dir.join(format!(
            "{REPORT_PREFIX}{stamp}-{pid}-{sequence:03}{REPORT_SUFFIX}"
        ));
        match create_new(&path) {
            Ok(mut file) => {
                file.write_all(report.as_bytes())?;
                file.sync_all()?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && sequence < 999 => {
                sequence += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Opens a new file at `path` for writing, failing if anything is there; user-only on
/// Unix.
fn create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Deletes the oldest crash reports in `dir`, so at most `keep` remain. The names start
/// with a UTC time, so name order is age order to the second. Best effort: a report that
/// can't be deleted is left for a later prune. The log file and the audit log share the
/// folder; only names with the report prefix and suffix are touched.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(REPORT_PREFIX) && name.ends_with(REPORT_SUFFIX))
        .collect();
    names.sort();
    let excess = names.len().saturating_sub(keep);
    for name in &names[..excess] {
        let _ = fs::remove_file(dir.join(name));
    }
}

/// A UTC date and time, to the second, for report names and reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Utc {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

impl Utc {
    /// `time` in UTC; times before 1970 read as 1970-01-01.
    fn from(time: SystemTime) -> Utc {
        let seconds = time
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX / 2);
        let of_day = u32::try_from(seconds % 86_400).unwrap_or(0);
        // Howard Hinnant's `civil_from_days`, for days since 1970-01-01.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Utc {
            year,
            month: u32::try_from(month).unwrap_or(1),
            day: u32::try_from(day).unwrap_or(1),
            hour: of_day / 3600,
            minute: of_day / 60 % 60,
            second: of_day % 60,
        }
    }

    /// `20261014T101500Z`: sortable, and valid in a file name everywhere.
    fn compact(&self) -> String {
        let Utc {
            year,
            month,
            day,
            hour,
            minute,
            second,
        } = self;
        format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
    }

    /// `2026-10-14T10:15:00Z` (RFC 3339).
    fn rfc3339(&self) -> String {
        let Utc {
            year,
            month,
            day,
            hour,
            minute,
            second,
        } = self;
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_MESSAGE_BYTES, MAX_REPORTS, REPORT_PREFIX, Redacting, Secret, Utc, is_registered,
        redact, redacted_report, register_secret, write_report,
    };
    use jstream_platform::fake::MemoryKeyStore;
    use jstream_platform::{Secret as StoredSecret, StreamKeyStore};
    use std::backtrace::Backtrace;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// A fake control token and stream key: neither may reach a report.
    const FAKE_TOKEN: &str = "f4ke-desktop-t0ken-0123456789abcdef";
    const FAKE_STREAM_KEY: &str = "live_f4ke-desktop-key-9876543210";

    /// Set in the child process that panics.
    const CHILD_ENV: &str = "JSTREAM_DESKTOP_CRASH_TEST_DIR";

    fn secret(value: &str) -> Secret {
        (value.len(), blake3::hash(value.as_bytes()))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jstream-desktop-crash-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        dir
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{e}"))
            .map(|entry| entry.unwrap_or_else(|e| panic!("{e}")).path())
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn secrets_are_redacted_wherever_they_appear() {
        let secrets = [secret("s3cret"), secret("äöü")];
        assert_eq!(
            redact("a s3cret, s3crets3cret and äöü!", &secrets),
            "a ***, ****** and ***!"
        );
        assert_eq!(redact("nothing here", &secrets), "nothing here");
        assert_eq!(redact("s3cre", &secrets), "s3cre");
        assert_eq!(redact("", &secrets), "");
    }

    /// A secret that straddles the message cut is redacted whole, not left as a prefix.
    #[test]
    fn a_secret_at_the_message_cut_is_redacted_first() {
        let secrets = [secret("s3cretKEY")];
        let mut message = "x".repeat(MAX_MESSAGE_BYTES - 4);
        message.push_str("s3cretKEY and more");
        let report = redacted_report(
            SystemTime::now(),
            "t",
            "here",
            &message,
            &Backtrace::disabled(),
            &secrets,
        );
        assert!(!report.contains("s3cr"), "a prefix of the secret leaked");
    }

    #[test]
    fn times_are_utc() {
        let at = UNIX_EPOCH + Duration::from_secs(1_791_973_500);
        assert_eq!(Utc::from(at).rfc3339(), "2026-10-14T10:25:00Z");
        assert_eq!(Utc::from(at).compact(), "20261014T102500Z");
        assert_eq!(Utc::from(UNIX_EPOCH).rfc3339(), "1970-01-01T00:00:00Z");
    }

    /// A stream key is registered when a [`Redacting`] store stores it or hands it out,
    /// and not when the store fails or holds nothing (#353).
    #[test]
    fn a_redacting_store_registers_what_it_stores_and_loads() {
        let keys = MemoryKeyStore::new();
        let loaded = "live_desktop-loaded-key-0001";
        keys.set("out_a", &StoredSecret::new(loaded))
            .unwrap_or_else(|e| panic!("{e}"));
        let redacting = Redacting(Arc::new(keys.clone()));
        assert_eq!(redacting.is_set("out_a").ok(), Some(true));
        assert!(!is_registered(loaded), "is_set reads no secret");
        let key = redacting.get("out_a").ok().flatten();
        assert_eq!(key.as_ref().map(StoredSecret::expose), Some(loaded));
        assert!(is_registered(loaded), "a loaded key is registered");
        assert!(matches!(redacting.get("out_none"), Ok(None)));

        let stored = "live_desktop-stored-key-0002";
        assert!(redacting.set("out_b", &StoredSecret::new(stored)).is_ok());
        assert!(is_registered(stored), "a stored key is registered");
        assert_eq!(
            keys.get("out_b")
                .ok()
                .flatten()
                .map(|k| k.expose().to_owned()),
            Some(stored.to_owned()),
            "the key reached the inner store"
        );
        assert!(redacting.clear("out_b").is_ok());
        assert!(matches!(keys.get("out_b"), Ok(None)));

        let unread = "live_desktop-unread-key-0003";
        keys.set("out_c", &StoredSecret::new(unread))
            .unwrap_or_else(|e| panic!("{e}"));
        keys.set_failing(true);
        assert!(redacting.get("out_c").is_err());
        assert!(!is_registered(unread), "a failed read registers nothing");
        keys.set_failing(false);
    }

    /// Only crash reports are pruned: the log file and the audit log share the folder.
    #[test]
    fn old_reports_are_pruned_to_the_cap() {
        let dir = scratch("prune");
        for n in 0..MAX_REPORTS + 3 {
            std::fs::write(
                dir.join(format!("{REPORT_PREFIX}19990101T0000{n:02}Z-1-000.txt")),
                "old",
            )
            .unwrap_or_else(|e| panic!("{e}"));
        }
        let log = dir.join("jstream-desktop.log");
        std::fs::write(&log, "log").unwrap_or_else(|e| panic!("{e}"));
        let path = write_report(&dir, SystemTime::now(), "new").unwrap_or_else(|e| panic!("{e}"));
        let left = files(&dir);
        assert_eq!(left.len(), MAX_REPORTS + 1, "{left:?}");
        assert!(left.contains(&path), "the new report is kept");
        assert!(left.contains(&log), "the log file is not a report");
        assert!(
            !left
                .iter()
                .any(|p| p.ends_with("crash-19990101T000000Z-1-000.txt")),
            "the oldest went first: {left:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real hook, in a child process (this test binary again, running only this
    /// test), so the hook and the panic don't touch the other tests. The child installs
    /// the hook into a folder that doesn't exist yet, registers the fake token, loads
    /// the fake stream key from a key store behind [`Redacting`] (the key's text comes
    /// from its environment, so only the store registers it), and panics with a message
    /// quoting both. The report has the facts and neither secret, and so does the
    /// stderr line that replaces the default hook's (#353 item 4: the app had no hook,
    /// so a panic wrote nothing and printed the message unredacted).
    #[test]
    fn a_panic_writes_a_redacted_report() {
        if let Some(dir) = std::env::var_os(CHILD_ENV) {
            let inner = MemoryKeyStore::new();
            let from_env = std::env::var("JSTREAM_FAKE_STREAM_KEY").unwrap_or_default();
            inner
                .set("out_live", &StoredSecret::new(from_env))
                .unwrap_or_else(|e| panic!("{e}"));
            let keys = Redacting(Arc::new(inner));
            super::install(PathBuf::from(dir).join("logs"));
            register_secret(FAKE_TOKEN);
            let _ = std::thread::Builder::new()
                .name("crash-test-worker".to_owned())
                .spawn(move || {
                    let key = keys.get("out_live").ok().flatten();
                    let key = key.as_ref().map_or("", StoredSecret::expose);
                    panic!("boom with token={FAKE_TOKEN} and key={key}");
                })
                .map(std::thread::JoinHandle::join);
            return;
        }
        let root = scratch("hook");
        let output = Command::new(std::env::current_exe().unwrap_or_else(|e| panic!("{e}")))
            .args([
                "crash::tests::a_panic_writes_a_redacted_report",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, &root)
            .env("JSTREAM_FAKE_STREAM_KEY", FAKE_STREAM_KEY)
            .output()
            .unwrap_or_else(|e| panic!("the child runs: {e}"));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        let found = files(&root.join("logs"));
        assert_eq!(found.len(), 1, "{found:?} {stderr}");
        let report = std::fs::read_to_string(&found[0]).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            report.starts_with("JStream crash report (panic)\n"),
            "{report}"
        );
        assert!(
            report.contains(&format!(
                "program: jstream-desktop {}",
                env!("CARGO_PKG_VERSION")
            )),
            "{report}"
        );
        assert!(report.contains("thread: crash-test-worker\n"), "{report}");
        // Panic locations use the platform's separator on Windows.
        assert!(
            report
                .replace('\\', "/")
                .contains("location: apps/jstream-desktop/src/crash.rs:"),
            "{report}"
        );
        assert!(
            report.contains("message: boom with token=*** and key=***\n"),
            "{report}"
        );
        assert!(report.contains("backtrace:\n"), "{report}");
        for text in [&report, &stderr.to_string()] {
            assert!(!text.contains(FAKE_TOKEN), "the token leaked: {text}");
            assert!(!text.contains(FAKE_STREAM_KEY), "the key leaked: {text}");
        }
        assert!(
            stderr.contains("error: panic in thread 'crash-test-worker'"),
            "{stderr}"
        );
        assert!(stderr.contains("crash report: "), "{stderr}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
