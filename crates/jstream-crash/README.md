<!-- @doc
id = "CRATE-jstream-crash"
title = "jstream-crash"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-crash

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-crash/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-crash", "docs/adr/0067-shared-crash-reports.md", "docs/RULES.md#R-SEC-001", "docs/RULES.md#R-SEC-004", "docs/RULES.md#R-RS-008"]
-->

Layer-2 crash reports for the apps, `jstream-daemon` and `jstream-desktop`
([ADR-0067](../../docs/adr/0067-shared-crash-reports.md)). Reports are written locally and
never uploaded ([R-SEC-004](../../docs/RULES.md#R-SEC-004)). No `unsafe`. Its JStream
dependencies are `jstream-platform`, for the credential-store traits `Redacting`
implements, and `jstream-protocol`, for `PROTOCOL_VERSION`; its one other is `blake3`.

Contents:
- `install(dir, program, secrets)` creates `dir` if it is missing (user-only on Unix),
  prunes old reports there and sets the panic hook. On a panic the hook writes
  `crash-<UTC time>-<pid>-<n>.txt` (user-only on Unix, its folder's ACL on Windows): the
  program's name and version, the protocol version, the OS and architecture, the time,
  the process ID, the panicking thread's name, the panic's location and message (cut at
  16 KiB), and a backtrace. Nothing else: no environment, no command line, no settings.
  It prints a redacted line naming the report to stderr in place of the default hook's.
  The newest 10 reports are kept (`MAX_REPORTS`); other files in the folder, such as log
  files and the audit log, are never touched.
- `Secrets`: the secrets to redact, as a shared handle. The app makes one at startup and
  hands clones to `install` and to each `Redacting` store; it registers its control token
  with it. Only each secret's length and BLAKE3 digest are kept, never its text, and its
  `Debug` shows only how many there are. There is no global list
  ([R-RS-008](../../docs/RULES.md#R-RS-008)).
- Redaction replaces every occurrence of a registered secret with `***`
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)), found by hashing every window of its
  length. The message is redacted before it is cut, so a secret straddling the cut leaves
  no prefix, then the whole report again for the location and backtrace. If the panicking
  thread holds the list, the message is withheld.
- `Redacting::new(secrets, store)` wraps a `StreamKeyStore` or an `OAuthTokenStore` and
  registers every key or refresh token it stores or loads (not on `is_set`, a failed
  read or an empty slot), before anything could format it (#353).

Not yet: native crashes (access violations, aborts) write no report; a minidump would
need its own ADR.

**Tests** (`src/tests.rs`): redaction wherever a secret appears and at the message cut,
UTC dates, `Secrets` shared by clones, `Redacting` on key and token stores, pruning to the
cap without touching other files, the folder created user-only, and the real hook in a
child process (this test binary again), whose report holds the facts and none of a fake
token, stream key and refresh token, nor does its stderr line.
