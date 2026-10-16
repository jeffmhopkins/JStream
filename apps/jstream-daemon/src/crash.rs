//! Local crash reports (R-SEC-004: crash data is written locally and never uploaded).
//!
//! A panic hook writes `crash-<UTC time>-<pid>-<n>.txt` into the log directory: the
//! daemon and protocol versions, the OS and architecture, the time, the process ID, the
//! panicking thread's name, the panic's location and message, and a backtrace. It holds
//! nothing else: no environment, no command line, no settings. Registered secrets (the
//! control token, and every stream key and OAuth refresh token that passes through a
//! [`Redacting`] credential store) are replaced by `***` wherever they appear
//! (R-SEC-001), and only their BLAKE3 digests are kept for that, never their text. The
//! newest [`MAX_REPORTS`] reports are kept.
//!
//! Native crashes (access violations, aborts) write no report yet: see
//! apps/jstream-daemon/README.md#design.
//!
//! Docs: apps/jstream-daemon/README.md#design

use crate::logs::{self, Utc};
#[cfg(any(windows, test))]
use jstream_platform::{OAuthTokenStore, PlatformError, Secret as StoredSecret, StreamKeyStore};
use jstream_protocol::PROTOCOL_VERSION;
use std::backtrace::Backtrace;
use std::io::{self, Write};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, TryLockError};
use std::time::SystemTime;

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

/// Registers `value` (a control token, a stream key, a refresh token) to be replaced by `***` in crash
/// reports. Empty values are ignored.
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

/// A credential store that registers every secret it stores or loads with the crash
/// reports' redaction ([`register_secret`]), so a panic that quotes a stream key or a
/// refresh token writes `***` in its place (#353). It wraps the daemon's key store, so
/// a key is registered when `output.set_key` stores it and when the publish opener
/// reads it, before anything could format it; [`OAuthTokenStore`] works the same way
/// for a refresh token once the daemon signs in to YouTube. Only digests are kept.
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

#[cfg(any(windows, test))]
impl<S: OAuthTokenStore + ?Sized> OAuthTokenStore for Redacting<S> {
    fn set(&self, account: &str, token: &StoredSecret) -> Result<(), PlatformError> {
        register_secret(token.expose());
        self.0.set(account, token)
    }

    fn get(&self, account: &str) -> Result<Option<StoredSecret>, PlatformError> {
        let token = self.0.get(account)?;
        if let Some(token) = &token {
            register_secret(token.expose());
        }
        Ok(token)
    }

    fn clear(&self, account: &str) -> Result<(), PlatformError> {
        self.0.clear(account)
    }

    fn is_set(&self, account: &str) -> Result<bool, PlatformError> {
        self.0.is_set(account)
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

/// Installs the panic hook that writes crash reports into `dir`, which must exist, and
/// prunes old reports there. It replaces the default hook, whose stderr line would
/// quote the message unredacted, with a redacted line naming the report.
pub fn install(dir: PathBuf) {
    logs::prune(&dir, REPORT_PREFIX, REPORT_SUFFIX, MAX_REPORTS, None);
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
    // Best effort: the report is what matters.
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
/// leave its prefix behind (#126 review N2); the whole report is redacted again for the
/// location and backtrace.
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
         program: jstream-daemon {version}, protocol {PROTOCOL_VERSION}\n\
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
/// [`MAX_REPORTS`] remain with it.
fn write_report(dir: &Path, now: SystemTime, report: &str) -> io::Result<PathBuf> {
    logs::prune(
        dir,
        REPORT_PREFIX,
        REPORT_SUFFIX,
        MAX_REPORTS.saturating_sub(1),
        None,
    );
    let stamp = Utc::from(now).compact();
    let pid = std::process::id();
    let mut sequence = 0u32;
    loop {
        let path = dir.join(format!(
            "{REPORT_PREFIX}{stamp}-{pid}-{sequence:03}{REPORT_SUFFIX}"
        ));
        match logs::create_new(&path) {
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

#[cfg(test)]
mod tests {
    use super::{
        MAX_MESSAGE_BYTES, MAX_REPORTS, REPORT_PREFIX, Redacting, Secret, is_registered, redact,
        redacted_report, register_secret, write_report,
    };
    use jstream_platform::fake::{MemoryKeyStore, MemoryTokenStore};
    use jstream_platform::{OAuthTokenStore, Secret as StoredSecret, StreamKeyStore};
    use std::backtrace::Backtrace;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Arc;
    use std::time::SystemTime;

    /// A fake control token, stream key and refresh token: none may reach a report.
    const FAKE_TOKEN: &str = "f4ke-c0ntrol-t0ken-0123456789abcdef";
    const FAKE_STREAM_KEY: &str = "live_f4ke-stream-key-9876543210";
    const FAKE_REFRESH_TOKEN: &str = "1//f4ke-refresh-t0ken-abcdef0123";

    /// Set in the child process that panics.
    const CHILD_ENV: &str = "JSTREAM_DAEMON_CRASH_TEST_DIR";

    fn secret(value: &str) -> Secret {
        (value.len(), blake3::hash(value.as_bytes()))
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "jstream-daemon-crash-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the scratch directory is created");
        dir
    }

    fn reports(dir: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .expect("readable")
            .map(|entry| entry.expect("an entry").path())
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

    /// A secret that straddles the message cut is redacted whole, not left as a prefix
    /// (#126 review N2).
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

    /// A stream key or refresh token is registered when a [`Redacting`] store stores it
    /// or hands it out, and not when the store fails or holds nothing (#353).
    #[test]
    fn a_redacting_store_registers_what_it_stores_and_loads() {
        let keys = MemoryKeyStore::new();
        let loaded = "live_loaded-key-for-redaction-0001";
        keys.set("out_a", &StoredSecret::new(loaded))
            .expect("stored behind the wrapper's back");
        let redacting = Redacting(Arc::new(keys.clone()));
        assert_eq!(redacting.is_set("out_a").ok(), Some(true));
        assert!(!is_registered(loaded), "is_set reads no secret");
        let key = redacting.get("out_a").expect("read").expect("stored");
        assert_eq!(key.expose(), loaded);
        assert!(is_registered(loaded), "a loaded key is registered");
        assert!(redacting.get("out_none").expect("read").is_none());

        let stored = "live_stored-key-for-redaction-0002";
        redacting
            .set("out_b", &StoredSecret::new(stored))
            .expect("stored");
        assert!(is_registered(stored), "a stored key is registered");
        assert_eq!(
            keys.get("out_b")
                .expect("read")
                .map(|k| k.expose().to_owned()),
            Some(stored.to_owned()),
            "the key reached the inner store"
        );
        redacting.clear("out_b").expect("cleared");
        assert!(keys.get("out_b").expect("read").is_none());

        let unread = "live_unread-key-for-redaction-0003";
        keys.set("out_c", &StoredSecret::new(unread))
            .expect("stored");
        keys.set_failing(true);
        assert!(redacting.get("out_c").is_err());
        assert!(!is_registered(unread), "a failed read registers nothing");
        keys.set_failing(false);

        let tokens = MemoryTokenStore::new();
        let refresh = "1//refresh-token-for-redaction-0004";
        tokens
            .set("someone@example.com", &StoredSecret::new(refresh))
            .expect("stored");
        let redacting = Redacting(Arc::new(tokens));
        let token = redacting
            .get("someone@example.com")
            .expect("read")
            .expect("stored");
        assert_eq!(token.expose(), refresh);
        assert!(
            is_registered(refresh),
            "a loaded refresh token is registered"
        );
        let new = "1//refresh-token-for-redaction-0005";
        redacting
            .set("someone@example.com", &StoredSecret::new(new))
            .expect("stored");
        assert!(is_registered(new), "a stored refresh token is registered");
    }

    #[test]
    fn old_reports_are_pruned_to_the_cap() {
        let dir = scratch("prune");
        for n in 0..MAX_REPORTS + 3 {
            std::fs::write(
                dir.join(format!("{REPORT_PREFIX}19990101T0000{n:02}Z-1-000.txt")),
                "old",
            )
            .expect("written");
        }
        let path = write_report(&dir, SystemTime::now(), "new").expect("written");
        let left = reports(&dir);
        assert_eq!(left.len(), MAX_REPORTS);
        assert_eq!(left.last(), Some(&path), "the new report is kept");
        assert!(
            !left
                .iter()
                .any(|p| p.ends_with("crash-19990101T000000Z-1-000.txt")),
            "the oldest went first: {left:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real hook, in a child process (this test binary again, running only this
    /// test), so the hook and the panic don't touch the other tests. The child registers
    /// the fake token, loads the fake stream key and refresh token from credential stores
    /// behind [`Redacting`] (the key's text comes from its environment, so only the store
    /// registers it), and panics with a message quoting all three; the report has the
    /// facts and no secret (#353).
    #[test]
    fn a_panic_writes_a_redacted_report() {
        if let Some(dir) = std::env::var_os(CHILD_ENV) {
            register_secret(FAKE_TOKEN);
            let inner = MemoryKeyStore::new();
            let from_env = std::env::var("JSTREAM_FAKE_STREAM_KEY").unwrap_or_default();
            inner
                .set("out_yt", &StoredSecret::new(from_env))
                .expect("stored");
            let keys = Redacting(Arc::new(inner));
            let tokens = MemoryTokenStore::new();
            tokens
                .set("someone", &StoredSecret::new(FAKE_REFRESH_TOKEN))
                .expect("stored");
            let tokens = Redacting(Arc::new(tokens));
            super::install(PathBuf::from(dir));
            let _ = std::thread::Builder::new()
                .name("crash-test-worker".to_owned())
                .spawn(move || {
                    let key = keys.get("out_yt").ok().flatten();
                    let token = tokens.get("someone").ok().flatten();
                    let key = key.as_ref().map_or("", StoredSecret::expose);
                    let token = token.as_ref().map_or("", StoredSecret::expose);
                    panic!("boom with token={FAKE_TOKEN} and key={key} and refresh={token}");
                })
                .expect("spawned")
                .join();
            return;
        }
        let dir = scratch("hook");
        let output = Command::new(std::env::current_exe().expect("the test binary"))
            .args([
                "crash::tests::a_panic_writes_a_redacted_report",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, &dir)
            .env("JSTREAM_FAKE_STREAM_KEY", FAKE_STREAM_KEY)
            .env("JSTREAM_TOKEN", FAKE_TOKEN)
            .output()
            .expect("the child runs");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        let found = reports(&dir);
        assert_eq!(found.len(), 1, "{found:?} {stderr}");
        let report = std::fs::read_to_string(&found[0]).expect("readable");
        assert!(
            report.starts_with("JStream crash report (panic)\n"),
            "{report}"
        );
        assert!(
            report.contains(&format!(
                "program: jstream-daemon {}",
                env!("CARGO_PKG_VERSION")
            )),
            "{report}"
        );
        assert!(report.contains("thread: crash-test-worker\n"), "{report}");
        // Panic locations use the platform's separator (`apps\jstream-daemon\…` on
        // Windows).
        assert!(
            report
                .replace('\\', "/")
                .contains("location: apps/jstream-daemon/src/crash.rs:"),
            "{report}"
        );
        assert!(
            report.contains("message: boom with token=*** and key=*** and refresh=***\n"),
            "{report}"
        );
        assert!(report.contains("backtrace:\n"), "{report}");
        for text in [&report, &stderr.to_string()] {
            assert!(!text.contains(FAKE_TOKEN), "the token leaked: {text}");
            assert!(!text.contains(FAKE_STREAM_KEY), "the key leaked: {text}");
            assert!(
                !text.contains(FAKE_REFRESH_TOKEN),
                "the refresh token leaked: {text}"
            );
        }
        assert!(
            stderr.contains("error: panic in thread 'crash-test-worker'"),
            "{stderr}"
        );
        assert!(stderr.contains("crash report: "), "{stderr}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
