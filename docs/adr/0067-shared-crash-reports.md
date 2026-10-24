<!-- @doc
id = "ADR-0067"
title = "Crash reports live in one crate, jstream-crash, with no global state"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0067: Crash reports live in one crate, jstream-crash, with no global state

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
`jstream-daemon` and `jstream-desktop` each write local crash reports on a panic
([R-SEC-004](../RULES.md#R-SEC-004)), and each had its own copy of the code, about 300
lines: the panic hook, the redaction of registered secrets by BLAKE3 digest
([R-SEC-001](../RULES.md#R-SEC-001), #353), the report format, pruning and UTC
formatting, and `Redacting`, the credential-store wrapper that registers every stream key
and refresh token it stores or loads. A fix to one copy, such as to the redaction scan,
had to be made in the other too (#558).

Both apps are layer 6, so neither may depend on the other
([R-ARCH-002](../RULES.md#R-ARCH-002)). Sharing the code means a new crate or a module
in an existing lower crate, and either is a structural decision
([R-ARCH-004](../RULES.md#R-ARCH-004), [ARCH › Crate map](../ARCHITECTURE.md#crate-map)).
It changes no product scope, security posture or compatibility promise: the reports, what
they hold, and where they go stay as they are. So it is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)).

Both copies kept the secrets in a process-wide `static Mutex<Vec<…>>`. That was allowed
in an app, but [R-RS-008](../RULES.md#R-RS-008) forbids global mutable state in
`crates/**`.

<a id="decision"></a>
## Decision
- **A new crate, `jstream-crash`, at layer 2**, holds the shared code. It depends on
  `jstream-platform` (L1) for `StreamKeyStore`, `OAuthTokenStore` and their `Secret`,
  which `Redacting` implements, on `jstream-protocol` (L1) for the `PROTOCOL_VERSION`
  each report names, and on `blake3`. No `unsafe`.
- **Not a module in `jstream-platform`.** That crate is the backend traits, which every
  engine crate and client sees. A panic hook, the report files and their pruning are
  process-level app code with nothing to do with backends, and would widen it for two
  layer-6 callers.
- **No global state.** The secrets live in a `Secrets` handle, a clone-shared list that
  keeps only digests. The app makes one at startup, hands a clone to `install` (whose hook
  owns it) and to every `Redacting` store, and registers its control token with it. The
  panic hook itself is the only process-wide thing, as `std::panic::set_hook` makes it.
- **The program is named by the caller.** `install(dir, Program { name, version },
  secrets, notify)`: each app passes its own package name and version for the report's
  `program:` line. `install` creates the folder if it is missing, as the desktop app's
  copy did, and now user-only on Unix, which the desktop's copy didn't do; the daemon
  creates it first anyway.
- **The library never prints.** As a library crate it falls under
  [R-RS-005](../RULES.md#R-RS-005), so the hook builds the redacted line naming the
  report and hands it to `notify`, a plain `fn(&str)` each app supplies. Both apps pass
  one that writes the line to stderr, which keeps their output unchanged. The redaction
  and the cut stay in the crate, so no app can print an unredacted message.
- **What stays in the apps:** wiring only, where the `Secrets` is made and which stores
  `Redacting` wraps. The daemon's rotating log files (`logs.rs`) stay its own.

<a id="consequences"></a>
## Consequences
- A fix to the hook, the redaction or the report format is made once, and its tests (the
  hook in a child process, redaction at the message cut, `Redacting`, pruning) run once,
  in `jstream-crash`.
- Every function on the path from startup to the token and the credential stores takes
  the `Secrets` it registers with, so tests check registration on a list of their own
  instead of a process-wide one.
- The crate map gains a layer-2 row, and both apps depend on `jstream-crash`; the
  desktop app no longer needs `blake3` directly.
- Native crashes (access violations, aborts) still write no report; when they do, it
  goes in this crate.
