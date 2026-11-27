<!-- @doc
id = "ADR-0258"
title = "A native crash writes a local minidump on Windows"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0258: A native crash writes a local minidump on Windows

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context

An access violation is a Windows exception, not a Rust panic, so `jstream-crash`'s
hook never runs and the process leaves no file. The faulting instruction is then
unknown: a jump through a bad pointer names no module. [ARCH › Persistence](../ARCHITECTURE.md#persistence)
already plans local minidumps, and [R-SEC-004](../RULES.md#R-SEC-004) already says
crash data stays on disk and is never uploaded.
[ADR-0067](0067-shared-crash-reports.md) puts panic reports in `jstream-crash` and
says a native report would go there too. That crate forbids `unsafe`, and it sits
on the same layer as `jstream-platform-windows`, so it cannot call the Windows API
and the Windows crate cannot call it.

<a id="decision"></a>
## Decision

The apps install an unhandled-exception filter from `jstream-platform-windows`,
beside the panic hook, into the same log folder. The filter calls
`MiniDumpWriteDump` with `MiniDumpNormal` and `MiniDumpWithThreadInfo`: each
thread's stack and the loaded modules, not the process heap and not the memory a
stack slot merely points at. The file is `crash-<pid>-<n>.dmp`.

The filter does not allocate, lock, or build a DACL. Looking up the SIDs for the
report file's user-and-SYSTEM DACL can deadlock there, so the dump keeps the
process's default DACL. The folder is the user's log folder. Install deletes the
oldest dumps past three; the filter does not.

The filter returns `EXCEPTION_CONTINUE_SEARCH`, so Windows Error Reporting still
runs.

Panic reports stay in `jstream-crash`. This does not move them. Linux and macOS
still write nothing for a native crash.

<a id="consequences"></a>
## Consequences

The next Windows access violation leaves a dump a debugger can open: the module
and the stack, which a fault offset with no module cannot. A value that was on a
thread's stack can be in the dump, including a secret that was live in a frame.
It stays on disk and is never uploaded. The dump is less private than a panic
report, which gets an explicit user-and-SYSTEM DACL; tightening that is a later
change and cannot be done inside the filter.

A crash on Linux or macOS still writes no dump.
