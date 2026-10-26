//! The audit log: one entry for every batch, undo, redo and action the engine executes,
//! naming who sent it, what it was, the `rev` before and after, and how it ended
//! (ADR-0006, ADR-0013, ADR-0032). Live values are logged once per gesture, never per
//! `live.set`: a committed gesture through its batch, and a cancelled or expired one in
//! an entry of its own.
//!
//! Entries carry names, IDs, revisions and error codes only, never a call's arguments,
//! so a stream key, a URL or a source's settings cannot reach the log (R-SEC-001).
//! `output.set_key` is recorded as that action on that output, and nothing of the key.
//!
//! The engine records an entry after the collection lock is released, and a sink must
//! not block: [`AuditFile`] hands entries over a bounded channel to its own writer
//! thread, which appends JSON lines to a size-capped file.
//!
//! Docs: crates/jstream-core/README.md#design, docs/adr/0006-mcp-full-access.md, docs/RULES.md#R-SEC-001

use crate::gesture::Ended;
use crate::lock;
use jstream_protocol::{
    Action, Call, CallResult, ClientId, GestureId, LiveOutcome, OutputId, ProtocolError, RequestId,
    Rev, SceneId,
};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

/// One executed batch or action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEntry {
    /// When the call finished.
    pub time: SystemTime,
    /// The client that sent it. IDs are numbered per engine, so they start again at
    /// `client_1` when the engine restarts.
    pub client_id: ClientId,
    /// The name the client gave in its latest `hello` (`claude-desktop`), cut to
    /// [`MAX_CLIENT_NAME_BYTES`]. Chosen by the client, so it says who claims to act,
    /// not who proved it.
    pub client_name: String,
    /// The request, as numbered by the client's session. `None` for a gesture that
    /// expired when its session ended, which no request did.
    pub request_id: Option<RequestId>,
    /// `batch`, `history.undo`, `history.redo`, or the action's name (`output.start`).
    /// A gesture that ended without a commit is `live.cancel` when its client cancelled
    /// it and `live.ended` when it expired.
    pub call: &'static str,
    /// A batch's edits by name (`scene.create`), in order, without their arguments.
    /// For a gesture, the edits it held a value for, one per parameter. Empty for an
    /// undo, a redo or an action.
    pub edits: Vec<&'static str>,
    /// The output an action names.
    pub output_id: Option<OutputId>,
    /// The scene an action names (`scene.switch`).
    pub scene_id: Option<SceneId>,
    /// The gesture a `live.cancel` or `live.ended` entry is about.
    pub gesture_id: Option<GestureId>,
    /// How that gesture ended: `cancelled` or `expired`.
    pub outcome: Option<LiveOutcome>,
    /// The collection `rev` before the call.
    pub rev_before: Rev,
    /// The collection `rev` after it. Equal to `rev_before` unless a batch was applied.
    pub rev_after: Rev,
    /// `None` on success, else the protocol error code (`rev_conflict`).
    pub error: Option<&'static str>,
    /// A batch whose idempotency key this client had already used: it got the `rev` it
    /// produced the first time, and nothing was applied again.
    pub replayed: bool,
}

impl AuditEntry {
    /// The entry as one line of JSON, without the newline. Times are UTC, to the
    /// millisecond (RFC 3339).
    pub fn to_json_line(&self) -> String {
        let mut line = String::with_capacity(160);
        line.push_str("{\"time\":");
        json_string(&mut line, &utc_time(self.time));
        line.push_str(",\"client_id\":");
        json_string(&mut line, self.client_id.as_str());
        line.push_str(",\"client_name\":");
        json_string(&mut line, &self.client_name);
        if let Some(request_id) = &self.request_id {
            line.push_str(",\"request_id\":");
            json_string(&mut line, request_id.as_str());
        }
        line.push_str(",\"call\":");
        json_string(&mut line, self.call);
        if !self.edits.is_empty() {
            line.push_str(",\"edits\":[");
            for (n, edit) in self.edits.iter().enumerate() {
                if n > 0 {
                    line.push(',');
                }
                json_string(&mut line, edit);
            }
            line.push(']');
        }
        if let Some(output_id) = &self.output_id {
            line.push_str(",\"output_id\":");
            json_string(&mut line, output_id.as_str());
        }
        if let Some(scene_id) = &self.scene_id {
            line.push_str(",\"scene_id\":");
            json_string(&mut line, scene_id.as_str());
        }
        if let Some(gesture_id) = &self.gesture_id {
            line.push_str(",\"gesture_id\":");
            json_string(&mut line, gesture_id.as_str());
        }
        if let Some(outcome) = self.outcome {
            line.push_str(",\"outcome\":");
            json_string(&mut line, outcome.name());
        }
        let _ = write!(
            line,
            ",\"rev_before\":{},\"rev_after\":{},\"result\":",
            self.rev_before, self.rev_after
        );
        json_string(&mut line, self.error.unwrap_or("ok"));
        if self.replayed {
            line.push_str(",\"replayed\":true");
        }
        line.push('}');
        line
    }
}

/// Where the engine sends its audit entries. The host injects one with
/// [`Engine::with_audit`](crate::Engine::with_audit); without one, nothing is recorded.
///
/// `record` is called on the thread that executed the call, with no engine lock held.
/// It must return quickly and must not call back into the engine.
pub trait AuditSink: Send + Sync {
    /// Takes one entry.
    fn record(&self, entry: AuditEntry);
}

/// An in-memory sink, for tests and for hosts that show the log themselves.
#[derive(Debug, Default)]
pub struct AuditMemory {
    entries: Mutex<Vec<AuditEntry>>,
}

impl AuditMemory {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every entry recorded so far, oldest first.
    pub fn entries(&self) -> Vec<AuditEntry> {
        lock(&self.entries).clone()
    }
}

impl AuditSink for AuditMemory {
    fn record(&self, entry: AuditEntry) {
        lock(&self.entries).push(entry);
    }
}

/// The audit file's name in its directory. The previous file is [`AUDIT_FILE_OLD`].
pub const AUDIT_FILE: &str = "audit.jsonl";
/// The previous audit file, replaced each time [`AUDIT_FILE`] fills up.
pub const AUDIT_FILE_OLD: &str = "audit.1.jsonl";
/// How large [`AuditFile`] lets the current file grow before starting a new one.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// The longest client name an entry keeps; a longer one is cut at a character boundary.
pub const MAX_CLIENT_NAME_BYTES: usize = 128;
/// How many entries wait for the writer thread before new ones are dropped (and counted).
pub const QUEUE_ENTRIES: usize = 1024;

/// A sink that appends one JSON line per entry to `audit.jsonl` in a directory (the
/// data directory, in a host).
///
/// Entries go over a bounded channel to a writer thread, so recording never waits on
/// the disk. If the writer falls [`QUEUE_ENTRIES`] behind, further entries are dropped
/// and counted, and the next line written is `{"time":…,"dropped":n}`.
///
/// When a line would take the file past its size limit, the file is renamed to
/// `audit.1.jsonl` (replacing the one before) and a new one is started, so the log uses
/// at most about twice the limit. A failed write is logged at `warn` and the entry lost:
/// the audit log never fails a call. The file is created user-only: mode `0600` on Unix,
/// a protected DACL for SYSTEM and the user on Windows.
///
/// Dropping it lets the writer finish what is queued, then joins it.
pub struct AuditFile {
    tx: Option<SyncSender<AuditEntry>>,
    dropped: Arc<AtomicU64>,
    writer: Option<JoinHandle<()>>,
}

impl AuditFile {
    /// Opens (or creates) `audit.jsonl` in `dir`, which must exist, with
    /// [`DEFAULT_MAX_FILE_BYTES`] as its limit, and starts the writer thread.
    ///
    /// # Errors
    /// The file can't be opened, or the thread can't be spawned.
    pub fn open(dir: &Path) -> io::Result<Self> {
        Self::with_limit(dir, DEFAULT_MAX_FILE_BYTES)
    }

    /// As [`open`](Self::open), with a size limit of `max_file_bytes` for one file.
    ///
    /// # Errors
    /// The file can't be opened, or the thread can't be spawned.
    pub fn with_limit(dir: &Path, max_file_bytes: u64) -> io::Result<Self> {
        let mut writer = Writer::open(dir, max_file_bytes)?;
        let (tx, rx) = mpsc::sync_channel::<AuditEntry>(QUEUE_ENTRIES);
        let dropped = Arc::new(AtomicU64::new(0));
        let lost = Arc::clone(&dropped);
        let writer = thread::Builder::new()
            .name("jstream-audit".to_owned())
            .spawn(move || {
                for entry in rx {
                    let missed = lost.swap(0, Ordering::Relaxed);
                    if missed > 0 {
                        writer.write(&dropped_line(entry.time, missed));
                    }
                    writer.write(&entry.to_json_line());
                }
                let missed = lost.swap(0, Ordering::Relaxed);
                if missed > 0 {
                    writer.write(&dropped_line(SystemTime::now(), missed));
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            dropped,
            writer: Some(writer),
        })
    }
}

impl AuditSink for AuditFile {
    fn record(&self, entry: AuditEntry) {
        let Some(tx) = &self.tx else { return };
        match tx.try_send(entry) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Drop for AuditFile {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(writer) = self.writer.take()
            && writer.join().is_err()
        {
            tracing::warn!("the audit writer thread panicked");
        }
    }
}

/// The writer thread's side: the open file, its size, and where to rotate it.
struct Writer {
    current: PathBuf,
    old: PathBuf,
    file: Option<File>,
    len: u64,
    max: u64,
}

impl Writer {
    fn open(dir: &Path, max: u64) -> io::Result<Self> {
        let current = dir.join(AUDIT_FILE);
        let file = append(&current)?;
        let len = file.metadata()?.len();
        Ok(Self {
            old: dir.join(AUDIT_FILE_OLD),
            current,
            file: Some(file),
            len,
            max,
        })
    }

    /// Appends `line` and a newline, starting a new file first if the line would take
    /// this one past the limit. A file holds at least one line, however long.
    fn write(&mut self, line: &str) {
        let bytes = line.len() as u64 + 1;
        if self.len > 0 && self.len + bytes > self.max {
            self.rotate();
        }
        if self.file.is_none() {
            // The last rotation could not reopen the file: try again for this line.
            match append(&self.current) {
                Ok(file) => {
                    self.len = file.metadata().map_or(0, |meta| meta.len());
                    self.file = Some(file);
                }
                Err(error) => {
                    tracing::warn!(%error, "audit file could not be opened; entry lost");
                    return;
                }
            }
        }
        let Some(file) = &mut self.file else { return };
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        match file.write_all(buf.as_bytes()) {
            Ok(()) => self.len += bytes,
            Err(error) => tracing::warn!(%error, "audit entry could not be written"),
        }
    }

    fn rotate(&mut self) {
        drop(self.file.take());
        if let Err(error) = fs::rename(&self.current, &self.old) {
            tracing::warn!(%error, "audit file could not be rotated");
        }
        self.len = 0;
        match append(&self.current) {
            Ok(file) => {
                self.len = file.metadata().map_or(0, |meta| meta.len());
                self.file = Some(file);
            }
            Err(error) => tracing::warn!(%error, "audit file could not be reopened"),
        }
    }
}

fn append(path: &Path) -> io::Result<File> {
    crate::private_file::open_append(path)
}

fn dropped_line(time: SystemTime, dropped: u64) -> String {
    let mut line = String::from("{\"time\":");
    json_string(&mut line, &utc_time(time));
    let _ = write!(line, ",\"dropped\":{dropped}}}");
    line
}

/// The engine's side: the sink, if any, behind a lock taken only to clone it, so a sink
/// is never called under an engine lock.
#[derive(Default)]
pub(crate) struct Audit {
    sink: Mutex<Option<Arc<dyn AuditSink>>>,
}

impl Audit {
    pub(crate) fn set(&self, sink: Arc<dyn AuditSink>) {
        *lock(&self.sink) = Some(sink);
    }

    /// The sink, if `call` is one the log records (a batch, an undo or redo, or an
    /// action other than `live.set` and `live.cancel`, whose gestures are logged when
    /// they end) and the host gave one.
    pub(crate) fn sink_for(&self, call: &Call) -> Option<Arc<dyn AuditSink>> {
        match call {
            Call::Action {
                action: Action::LiveSet { .. } | Action::LiveCancel { .. },
            } => None,
            Call::Batch(_) | Call::History(_) | Call::Action { .. } => self.sink(),
            _ => None,
        }
    }

    /// The sink, if the host gave one.
    pub(crate) fn sink(&self) -> Option<Arc<dyn AuditSink>> {
        lock(&self.sink).clone()
    }
}

/// The entry for a gesture that ended at `rev` without a commit: cancelled by the
/// request `request_id`, or expired with its session.
pub(crate) fn gesture_entry(
    ended: Ended,
    client_name: String,
    request_id: Option<RequestId>,
    rev: Rev,
) -> AuditEntry {
    AuditEntry {
        time: SystemTime::now(),
        client_id: ended.client_id,
        client_name,
        request_id,
        call: match ended.outcome {
            LiveOutcome::Cancelled => "live.cancel",
            _ => "live.ended",
        },
        edits: ended.edits,
        output_id: None,
        scene_id: None,
        gesture_id: Some(ended.gesture_id),
        outcome: Some(ended.outcome),
        rev_before: rev,
        rev_after: rev,
        error: None,
        replayed: false,
    }
}

/// `name` cut to [`MAX_CLIENT_NAME_BYTES`], at a character boundary.
pub(crate) fn client_name(name: &str) -> String {
    let mut end = name.len().min(MAX_CLIENT_NAME_BYTES);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

/// What an entry records of a call, taken before the call consumes it.
pub(crate) struct Pending {
    client_name: String,
    call: &'static str,
    edits: Vec<&'static str>,
    output_id: Option<OutputId>,
    scene_id: Option<SceneId>,
    rev_before: Rev,
}

impl Pending {
    /// `rev_before` is read just before the call runs. Another client's batch can land
    /// between that read and the call, so for an applied batch the entry uses the
    /// batch's own `rev` minus one instead.
    pub(crate) fn new(call: &Call, rev_before: Rev, client_name: String) -> Self {
        let (name, edits, output_id, scene_id) = match call {
            Call::Batch(batch) => (
                "batch",
                batch.edits.iter().map(|e| e.name()).collect(),
                None,
                None,
            ),
            Call::History(history) => (history.op.name(), Vec::new(), None, None),
            Call::Action { action } => (
                action.name(),
                Vec::new(),
                action_output(action),
                action_scene(action),
            ),
            _ => ("unknown", Vec::new(), None, None),
        };
        Self {
            client_name,
            call: name,
            edits,
            output_id,
            scene_id,
            rev_before,
        }
    }

    /// The finished entry.
    pub(crate) fn finish(
        self,
        client_id: &ClientId,
        request_id: &RequestId,
        result: &Result<CallResult, ProtocolError>,
    ) -> AuditEntry {
        let (rev_before, rev_after, replayed) = match result {
            // Applied now: the rev it produced is newer than any read before it ran.
            Ok(CallResult::Batch { rev, .. } | CallResult::History { rev, .. })
                if *rev > self.rev_before =>
            {
                (Rev(rev.0.saturating_sub(1)), *rev, false)
            }
            // An idempotent retry: it changed nothing.
            Ok(CallResult::Batch { .. } | CallResult::History { .. }) => {
                (self.rev_before, self.rev_before, true)
            }
            // A failed batch, undo or redo applies nothing, and actions never change `rev`.
            _ => (self.rev_before, self.rev_before, false),
        };
        AuditEntry {
            time: SystemTime::now(),
            client_id: client_id.clone(),
            client_name: self.client_name,
            request_id: Some(request_id.clone()),
            call: self.call,
            edits: self.edits,
            output_id: self.output_id,
            scene_id: self.scene_id,
            gesture_id: None,
            outcome: None,
            rev_before,
            rev_after,
            error: result.as_ref().err().map(ProtocolError::code),
            replayed,
        }
    }
}

fn action_output(action: &Action) -> Option<OutputId> {
    match action {
        Action::OutputStart { output_id }
        | Action::OutputStop { output_id }
        | Action::OutputSetKey { output_id, .. }
        | Action::OutputClearKey { output_id } => Some(output_id.clone()),
        _ => None,
    }
}

fn action_scene(action: &Action) -> Option<SceneId> {
    match action {
        Action::SceneSwitch { scene_id, .. } | Action::StudioSetPreview { scene_id } => {
            Some(scene_id.clone())
        }
        _ => None,
    }
}

/// Appends `text` as a JSON string literal.
fn json_string(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `time` as `YYYY-MM-DDThh:mm:ss.sssZ`. Times before 1970 print as the epoch.
fn utc_time(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2);
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        since.subsec_millis()
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    // Both are in range by construction: day 1..=31, month 1..=12.
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        AUDIT_FILE, AUDIT_FILE_OLD, AuditEntry, AuditFile, AuditSink, MAX_CLIENT_NAME_BYTES,
        civil_from_days, client_name, json_string, utc_time,
    };
    use jstream_protocol::{ClientId, OutputId, RequestId, Rev};
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    fn entry(n: u64) -> AuditEntry {
        AuditEntry {
            time: UNIX_EPOCH + Duration::from_millis(1_760_488_935_007),
            client_id: ClientId::new("client_1"),
            client_name: "claude-desktop".to_owned(),
            request_id: Some(RequestId::new(format!("req_{n}"))),
            call: "output.set_key",
            edits: Vec::new(),
            output_id: Some(OutputId::new("out_yt")),
            scene_id: None,
            gesture_id: None,
            outcome: None,
            rev_before: Rev(n),
            rev_after: Rev(n),
            error: None,
            replayed: false,
        }
    }

    /// A fresh directory for one test, removed first if a previous run left it.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("jstream-core-audit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn times_are_utc_rfc3339_to_the_millisecond() {
        assert_eq!(utc_time(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            utc_time(UNIX_EPOCH + Duration::from_millis(1_760_488_935_007)),
            "2025-10-15T00:42:15.007Z"
        );
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn strings_are_escaped() {
        let mut out = String::new();
        json_string(&mut out, "a\"b\\c\nd\u{1}é");
        assert_eq!(out, "\"a\\\"b\\\\c\\nd\\u0001é\"");
    }

    /// A key action names the action and the output only.
    #[test]
    fn an_action_line_has_no_arguments() {
        assert_eq!(
            entry(4).to_json_line(),
            "{\"time\":\"2025-10-15T00:42:15.007Z\",\"client_id\":\"client_1\",\
             \"client_name\":\"claude-desktop\",\"request_id\":\"req_4\",\"call\":\"output.set_key\",\"output_id\":\"out_yt\",\
             \"rev_before\":4,\"rev_after\":4,\"result\":\"ok\"}"
        );
    }

    /// Lines are appended to `audit.jsonl`; when the next line would pass the limit the
    /// file becomes `audit.1.jsonl` and a new one starts, so at most two files remain.
    #[test]
    fn the_file_rotates_at_its_limit() {
        let dir = scratch("rotate");
        let line = entry(0).to_json_line().len() as u64 + 1;
        {
            let audit = AuditFile::with_limit(&dir, line * 3).expect("open");
            for n in 0..7 {
                audit.record(entry(n));
            }
        }
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect(name);
        let current = read(AUDIT_FILE);
        let old = read(AUDIT_FILE_OLD);
        assert_eq!(current.lines().count(), 1, "{current}");
        assert!(current.contains("\"req_6\""), "{current}");
        assert_eq!(old.lines().count(), 3, "{old}");
        assert!(
            old.contains("\"req_3\"") && old.contains("\"req_5\""),
            "{old}"
        );
        let names = std::fs::read_dir(&dir).expect("dir").count();
        assert_eq!(names, 2);

        // Reopening appends to what is there.
        {
            let audit = AuditFile::with_limit(&dir, line * 3).expect("reopen");
            audit.record(entry(7));
        }
        assert_eq!(read(AUDIT_FILE).lines().count(), 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(AUDIT_FILE))
                .expect("meta")
                .permissions();
            assert_eq!(mode.mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_directory_fails_to_open() {
        let scratch = scratch("missing");
        assert!(AuditFile::open(&scratch.join("not-there")).is_err());
        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn long_client_names_are_cut_at_a_character_boundary() {
        assert_eq!(client_name("jstream-cli"), "jstream-cli");
        let long = "é".repeat(MAX_CLIENT_NAME_BYTES);
        let cut = client_name(&long);
        assert_eq!(cut.len(), MAX_CLIENT_NAME_BYTES);
        assert!(long.starts_with(&cut));
        let odd = format!("a{long}");
        assert_eq!(client_name(&odd).len(), MAX_CLIENT_NAME_BYTES - 1);
    }
}
