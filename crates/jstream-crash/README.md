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
- `install(dir, program, secrets, notify)` creates `dir` if it is missing (user-only on Unix),
  prunes old reports there and sets the panic hook. On a panic the hook writes
  `crash-<UTC time>-<pid>-<n>.txt` (user-only on Unix, through `create_new`): the
  program's name and version, the protocol version, the OS and architecture, the time,
  the process ID, the panicking thread's name, the panic's location and message (cut at
  16 KiB), and a backtrace. Nothing else: no environment, no command line, no settings.
  In place of the default hook's stderr line, it hands `notify` (`Notify`, the app's own
  `fn(&str)`) a redacted line naming the report. The crate itself never prints
  ([R-RS-005](../../docs/RULES.md#R-RS-005)); both apps write that line to stderr.
  The newest 10 reports are kept (`MAX_REPORTS`); other files in the folder, such as log
  files and the audit log, are never touched.
- `install_creating(dir, program, secrets, notify, create)` is `install` with the app's
  own `CreateFile` making each report (a new file, `AlreadyExists` if the name is taken,
  so the hook tries the next one). On Windows `create_new` leaves a report its folder's
  ACL, and this crate can't do better: it has no `unsafe`, and `jstream-platform-windows`
  is on its own layer (R-ARCH-002). So both apps pass
  `jstream_platform_windows::create_private_file` there (the daemon through its
  `logs::create_new`), which gives each report a protected DACL for SYSTEM and the user
  (#656).
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
- `Secrets::register_in(slot, value)` registers a secret that supersedes its slot's
  earlier ones, such as an OAuth access token, refreshed about every hour. A slot keeps
  its newest two values, so the one just superseded, which a call made before the switch
  may still hold, is redacted too, and drops older ones, which their holder no longer
  has. So a slot only takes a secret that supersedes its holder's last one; a value
  that might not, such as a sign-in's token the store may refuse, is kept. The list
  then stays bounded however often a token is refreshed (#844). `register` keeps a
  secret for the life of the process, as the control token, stream keys and refresh
  tokens need.
- `Secrets::hook(slot)` is a closure that registers what it is handed with its kind
  `K`, for a client that hands its secrets on as it gets them. `slot` maps a kind to
  the slot it supersedes in, or `None` to keep it. The daemon gives
  `hook(jstream_youtube::SecretKind::slot)` to the YouTube setup
  (`jstream_youtube::SecretHook`), for the access tokens and the stream keys YouTube
  sends (#353). The crate doesn't depend on `jstream-youtube`, which is on its layer:
  the kind is the caller's type.

Not yet: native crashes (access violations, aborts) write no report; a minidump would
need its own ADR.

**Tests** (`src/tests.rs`): redaction wherever a secret appears and at the message cut,
UTC dates, `Secrets` shared by clones, a slot staying at two values across a thousand
refreshes through the hook while kept secrets stay, `Redacting` on key and token stores, pruning to the
cap without touching other files, a report made by the app's `create` (and the next name
tried through it), the folder created user-only, and the real hook in a
child process (this test binary again), whose report holds the facts and none of a fake
token, stream key and refresh token, nor does the line it hands the test's notifier
(written to the child's stderr).
