use super::{
    Facts, MAX_MESSAGE_BYTES, MAX_REPORTS, Program, REPORT_PREFIX, Redacting, Secret, Secrets, Utc,
    digest, redact, redacted_report, write_report,
};
use jstream_platform::fake::{MemoryKeyStore, MemoryTokenStore};
use jstream_platform::{OAuthTokenStore, Secret as StoredSecret, StreamKeyStore};
use std::backtrace::Backtrace;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A fake control token, stream key and refresh token: none may reach a report.
const FAKE_TOKEN: &str = "f4ke-c0ntrol-t0ken-0123456789abcdef";
const FAKE_STREAM_KEY: &str = "live_f4ke-stream-key-9876543210";
const FAKE_REFRESH_TOKEN: &str = "1//f4ke-refresh-t0ken-abcdef0123";

/// Set in the child process that panics.
const CHILD_ENV: &str = "JSTREAM_CRASH_TEST_DIR";

/// The program the tests' reports name.
const PROGRAM: Program = Program {
    name: "jstream-test-app",
    version: "1.2.3",
};

fn secret(value: &str) -> Secret {
    digest(value)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("jstream-crash-{}-{name}", std::process::id()));
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
    let facts = Facts {
        program: PROGRAM,
        now: SystemTime::now(),
        thread: "t",
        location: "here",
    };
    let report = redacted_report(&facts, &message, &Backtrace::disabled(), &secrets);
    assert!(!report.contains("s3cr"), "a prefix of the secret leaked");
}

#[test]
fn times_are_utc() {
    let at = UNIX_EPOCH + Duration::from_secs(1_791_973_500);
    assert_eq!(Utc::from(at).rfc3339(), "2026-10-14T10:25:00Z");
    assert_eq!(Utc::from(at).compact(), "20261014T102500Z");
    assert_eq!(Utc::from(UNIX_EPOCH).rfc3339(), "1970-01-01T00:00:00Z");
}

/// Clones share one list; an empty value is never registered; `Debug` shows no digest.
#[test]
fn secrets_are_shared_by_their_clones() {
    let secrets = Secrets::new();
    let clone = secrets.clone();
    clone.register("shared-secret-0000");
    clone.register("shared-secret-0000");
    secrets.register("");
    assert!(secrets.is_registered("shared-secret-0000"));
    assert!(!secrets.is_registered(""));
    assert!(!Secrets::new().is_registered("shared-secret-0000"));
    assert_eq!(format!("{secrets:?}"), "Secrets { count: 1 }");
}

/// A stream key or refresh token is registered when a [`Redacting`] store stores it or
/// hands it out, and not when the store fails or holds nothing (#353).
#[test]
fn a_redacting_store_registers_what_it_stores_and_loads() {
    let secrets = Secrets::new();
    let keys = MemoryKeyStore::new();
    let loaded = "live_loaded-key-for-redaction-0001";
    keys.set("out_a", &StoredSecret::new(loaded))
        .expect("stored behind the wrapper's back");
    let redacting = Redacting::new(secrets.clone(), Arc::new(keys.clone()));
    assert_eq!(redacting.is_set("out_a").ok(), Some(true));
    assert!(!secrets.is_registered(loaded), "is_set reads no secret");
    let key = redacting.get("out_a").expect("read").expect("stored");
    assert_eq!(key.expose(), loaded);
    assert!(secrets.is_registered(loaded), "a loaded key is registered");
    assert!(redacting.get("out_none").expect("read").is_none());

    let stored = "live_stored-key-for-redaction-0002";
    redacting
        .set("out_b", &StoredSecret::new(stored))
        .expect("stored");
    assert!(secrets.is_registered(stored), "a stored key is registered");
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
    assert!(
        !secrets.is_registered(unread),
        "a failed read registers nothing"
    );
    keys.set_failing(false);

    let tokens = MemoryTokenStore::new();
    let refresh = "1//refresh-token-for-redaction-0004";
    tokens
        .set("someone@example.com", &StoredSecret::new(refresh))
        .expect("stored");
    let redacting = Redacting::new(secrets.clone(), Arc::new(tokens));
    let token = redacting
        .get("someone@example.com")
        .expect("read")
        .expect("stored");
    assert_eq!(token.expose(), refresh);
    assert!(
        secrets.is_registered(refresh),
        "a loaded refresh token is registered"
    );
    let new = "1//refresh-token-for-redaction-0005";
    redacting
        .set("someone@example.com", &StoredSecret::new(new))
        .expect("stored");
    assert!(
        secrets.is_registered(new),
        "a stored refresh token is registered"
    );
}

/// A report is made by the app's `create` (`install_creating`, #656: on Windows the apps
/// pass one that makes the file user-only), and a name already taken moves on to the next
/// one through that `create` too.
#[test]
fn a_report_is_made_by_the_apps_create() {
    /// The app's `create`: `create_new`, plus a marker beside each file it makes.
    fn marked(path: &Path) -> std::io::Result<std::fs::File> {
        let file = super::create_new(path)?;
        std::fs::write(path.with_extension("made-by-app"), "")?;
        Ok(file)
    }
    let dir = scratch("create-hook");
    let now = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    let stem = format!(
        "{REPORT_PREFIX}{}-{}",
        Utc::from(now).compact(),
        std::process::id()
    );
    std::fs::write(dir.join(format!("{stem}-000.txt")), "taken").expect("written");
    let path = write_report(&dir, now, "report", marked).expect("written");
    assert_eq!(
        path,
        dir.join(format!("{stem}-001.txt")),
        "the next free name"
    );
    assert_eq!(std::fs::read_to_string(&path).expect("readable"), "report");
    assert!(
        path.with_extension("made-by-app").exists(),
        "made by the app's create"
    );
    let _ = std::fs::remove_dir_all(&dir);
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
    // Files that only share the folder are never touched.
    std::fs::write(dir.join("jstream-daemon-1.log"), "log").expect("written");
    let path = write_report(&dir, SystemTime::now(), "new", super::create_new).expect("written");
    let left = reports(&dir);
    assert_eq!(left.len(), MAX_REPORTS + 1, "{left:?}");
    assert!(left.iter().any(|p| p.ends_with("jstream-daemon-1.log")));
    assert!(left.contains(&path), "the new report is kept");
    assert!(
        !left
            .iter()
            .any(|p| p.ends_with("crash-19990101T000000Z-1-000.txt")),
        "the oldest went first: {left:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The folder helper `install` uses creates a missing folder and its parents,
/// user-only on Unix. (The hook test covers `install` itself making a missing folder.)
#[test]
fn install_creates_the_folder() {
    let dir = scratch("create").join("data").join("logs");
    assert!(!dir.exists());
    super::create_dir(&dir).expect("created");
    assert!(dir.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).expect("stat").permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
    let _ = std::fs::remove_dir_all(dir.parent().and_then(Path::parent).expect("scratch"));
}

/// The real hook, in a child process (this test binary again, running only this test),
/// so the hook and the panic don't touch the other tests. The child registers the fake
/// token, loads the fake stream key and refresh token from credential stores behind
/// [`Redacting`] (the key's text comes from its environment, so only the store registers
/// it), and panics with a message quoting all three; the report has the facts and no
/// secret (#353). The folder doesn't exist yet: `install` makes it.
#[test]
fn a_panic_writes_a_redacted_report() {
    if let Some(dir) = std::env::var_os(CHILD_ENV) {
        let secrets = Secrets::new();
        secrets.register(FAKE_TOKEN);
        let inner = MemoryKeyStore::new();
        let from_env = std::env::var("JSTREAM_FAKE_STREAM_KEY").unwrap_or_default();
        inner
            .set("out_yt", &StoredSecret::new(from_env))
            .expect("stored");
        let keys = Redacting::new(secrets.clone(), Arc::new(inner));
        let tokens = MemoryTokenStore::new();
        tokens
            .set("someone", &StoredSecret::new(FAKE_REFRESH_TOKEN))
            .expect("stored");
        let tokens = Redacting::new(secrets.clone(), Arc::new(tokens));
        // The app's notifier: the test reads the line on the child's stderr.
        fn notify(line: &str) {
            use std::io::Write;
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        }
        super::install(PathBuf::from(dir), PROGRAM, secrets, notify);
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
    let scratch = scratch("hook");
    let dir = scratch.join("logs");
    let output = Command::new(std::env::current_exe().expect("the test binary"))
        .args([
            "tests::a_panic_writes_a_redacted_report",
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
            "program: jstream-test-app 1.2.3, protocol {}\n",
            jstream_protocol::PROTOCOL_VERSION
        )),
        "{report}"
    );
    assert!(report.contains("thread: crash-test-worker\n"), "{report}");
    // Panic locations use the platform's separator (`crates\jstream-crash\…` on
    // Windows).
    assert!(
        report
            .replace('\\', "/")
            .contains("location: crates/jstream-crash/src/tests.rs:"),
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
    let _ = std::fs::remove_dir_all(&scratch);
}
