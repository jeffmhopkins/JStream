//! Local crash reports for the JStream apps (R-SEC-004: crash data is written locally
//! and never uploaded), shared by `jstream-daemon` and `jstream-desktop`
//! (ADR-0067: docs/adr/0067-shared-crash-reports.md).
//!
//! [`install`] sets a panic hook that writes `crash-<UTC time>-<pid>-<n>.txt` into a
//! folder: the program's name and version, the protocol version, the OS and
//! architecture, the time, the process ID, the panicking thread's name, the panic's
//! location and message, and a backtrace. It holds nothing else: no environment, no
//! command line, no settings. Secrets registered with the [`Secrets`] the hook was given
//! (the control token, and every stream key and OAuth refresh token that passes through
//! a [`Redacting`] store) are replaced by `***` wherever they appear (R-SEC-001), and
//! only their BLAKE3 digests are kept for that, never their text. The newest
//! [`MAX_REPORTS`] reports are kept.
//!
//! Native crashes (access violations, aborts) write no report yet: see
//! crates/jstream-crash/README.md#purpose.
//!
//! Docs: docs/ARCHITECTURE.md#crate-map, crates/jstream-crash/README.md#purpose, docs/adr/0067-shared-crash-reports.md

#![forbid(unsafe_code)]

use jstream_platform::{OAuthTokenStore, PlatformError, Secret as StoredSecret, StreamKeyStore};
use jstream_protocol::PROTOCOL_VERSION;
use std::backtrace::Backtrace;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
mod tests;

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

/// The program a report names on its `program:` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Program {
    /// The binary's package name, such as `jstream-daemon`.
    pub name: &'static str,
    /// Its version, usually `env!("CARGO_PKG_VERSION")`.
    pub version: &'static str,
}

/// The secrets to redact from crash reports, as a shared handle: clones share one list.
/// The app makes one, hands a clone to [`install`] and to each [`Redacting`] store, and
/// registers its control token with it. Only digests are kept.
#[derive(Clone, Default)]
pub struct Secrets(Arc<Mutex<Vec<Secret>>>);

impl Secrets {
    /// An empty list.
    #[must_use]
    pub fn new() -> Secrets {
        Secrets::default()
    }

    /// Registers `value` (a control token, a stream key, a refresh token) to be replaced
    /// by `***` in crash reports. Empty values are ignored.
    pub fn register(&self, value: &str) {
        if value.is_empty() {
            return;
        }
        let secret = digest(value);
        let mut secrets = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !secrets.contains(&secret) {
            secrets.push(secret);
        }
    }

    /// Whether `value` is registered.
    #[must_use]
    pub fn is_registered(&self, value: &str) -> bool {
        let secret = digest(value);
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&secret)
    }
}

/// Shows how many secrets are registered, never their digests.
impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.0.lock().unwrap_or_else(PoisonError::into_inner).len();
        f.debug_struct("Secrets").field("count", &count).finish()
    }
}

fn digest(value: &str) -> Secret {
    (value.len(), blake3::hash(value.as_bytes()))
}

/// A credential store that registers every secret it stores or loads with
/// [`Secrets`], so a panic that quotes a stream key or a refresh token writes `***` in
/// its place (#353). An app wraps its key store in it, so a key is registered when
/// `output.set_key` stores it and when the publish opener reads it, before anything could
/// format it, and its YouTube token store too, so [`OAuthTokenStore`] registers a refresh
/// token as YouTube stores or loads it.
pub struct Redacting<S: ?Sized> {
    secrets: Secrets,
    inner: Arc<S>,
}

impl<S: ?Sized> Redacting<S> {
    /// `inner`, registering what passes through it with `secrets`.
    pub fn new(secrets: Secrets, inner: Arc<S>) -> Redacting<S> {
        Redacting { secrets, inner }
    }
}

impl<S: StreamKeyStore + ?Sized> StreamKeyStore for Redacting<S> {
    fn set(&self, output_id: &str, key: &StoredSecret) -> Result<(), PlatformError> {
        self.secrets.register(key.expose());
        self.inner.set(output_id, key)
    }

    fn get(&self, output_id: &str) -> Result<Option<StoredSecret>, PlatformError> {
        let key = self.inner.get(output_id)?;
        if let Some(key) = &key {
            self.secrets.register(key.expose());
        }
        Ok(key)
    }

    fn clear(&self, output_id: &str) -> Result<(), PlatformError> {
        self.inner.clear(output_id)
    }

    fn is_set(&self, output_id: &str) -> Result<bool, PlatformError> {
        self.inner.is_set(output_id)
    }
}

impl<S: OAuthTokenStore + ?Sized> OAuthTokenStore for Redacting<S> {
    fn set(&self, account: &str, token: &StoredSecret) -> Result<(), PlatformError> {
        self.secrets.register(token.expose());
        self.inner.set(account, token)
    }

    fn get(&self, account: &str) -> Result<Option<StoredSecret>, PlatformError> {
        let token = self.inner.get(account)?;
        if let Some(token) = &token {
            self.secrets.register(token.expose());
        }
        Ok(token)
    }

    fn clear(&self, account: &str) -> Result<(), PlatformError> {
        self.inner.clear(account)
    }

    fn is_set(&self, account: &str) -> Result<bool, PlatformError> {
        self.inner.is_set(account)
    }
}

/// Installs the panic hook that writes `program`'s crash reports into `dir`, redacting
/// `secrets`. It creates `dir` and its parents if they are missing (user-only on Unix,
/// mode 0700) and prunes old reports there. It replaces the default hook, whose stderr
/// line would quote the message unredacted, with a redacted line naming the report.
pub fn install(dir: PathBuf, program: Program, secrets: Secrets) {
    // Best effort: if the folder can't be made, each report says so on stderr.
    let _ = create_dir(&dir);
    prune(&dir, MAX_REPORTS);
    std::panic::set_hook(Box::new(move |info| {
        on_panic(&dir, program, &secrets, info);
    }));
}

/// The hook. It must not panic: a panic here aborts the process.
fn on_panic(dir: &Path, program: Program, secrets: &Secrets, info: &PanicHookInfo<'_>) {
    let now = SystemTime::now();
    let thread = std::thread::current();
    let thread = thread.name().unwrap_or("<unnamed>");
    let location = info.location().map_or_else(
        || "<unknown>".to_owned(),
        |at| format!("{}:{}:{}", at.file(), at.line(), at.column()),
    );
    let message = payload_text(info.payload());
    let backtrace = Backtrace::force_capture();
    let facts = Facts {
        program,
        now,
        thread,
        location: &location,
    };
    let (report, summary) = match secrets.0.try_lock() {
        Ok(secrets) => (
            redacted_report(&facts, &message, &backtrace, &secrets),
            redact(&message, &secrets),
        ),
        Err(TryLockError::Poisoned(secrets)) => {
            let secrets = secrets.into_inner();
            (
                redacted_report(&facts, &message, &backtrace, &secrets),
                redact(&message, &secrets),
            )
        }
        // The panicking thread holds the list, so nothing can be redacted: leave the
        // message out entirely.
        Err(TryLockError::WouldBlock) => {
            let withheld = "<withheld: the secrets to redact were unavailable>";
            (report(&facts, withheld, &backtrace), withheld.to_owned())
        }
    };
    let written = write_report(dir, now, &report);
    let mut err = io::stderr().lock();
    let summary = truncate(&summary, 512);
    // Best effort: the report is what matters. A release desktop build has no console.
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

/// What a report says about the panic, apart from its message and backtrace.
struct Facts<'a> {
    program: Program,
    now: SystemTime,
    thread: &'a str,
    location: &'a str,
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
    facts: &Facts<'_>,
    message: &str,
    backtrace: &Backtrace,
    secrets: &[Secret],
) -> String {
    let message = redact(message, secrets);
    redact(&report(facts, &message, backtrace), secrets)
}

/// The report's text, before redaction.
fn report(facts: &Facts<'_>, message: &str, backtrace: &Backtrace) -> String {
    let Facts {
        program,
        now,
        thread,
        location,
    } = facts;
    format!(
        "JStream crash report (panic)\n\
         program: {name} {version}, protocol {PROTOCOL_VERSION}\n\
         os: {os} {arch}\n\
         time: {time}\n\
         process: {pid}\n\
         thread: {thread}\n\
         location: {location}\n\
         message: {message}\n\
         \n\
         backtrace:\n{backtrace}\n",
        name = program.name,
        version = program.version,
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        time = Utc::from(*now).rfc3339(),
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

/// Creates `dir` and its missing parents; on Unix the directories it creates are
/// user-only (mode 0700). An existing directory is left as it is.
fn create_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
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
/// can't be deleted is left for a later prune. Log files and the audit log may share the
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
