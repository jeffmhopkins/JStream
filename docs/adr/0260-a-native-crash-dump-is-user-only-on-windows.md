<!-- @doc
id = "ADR-0260"
title = "A native crash dump on Windows is user-only, like a crash report"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0260: A native crash dump on Windows is user-only, like a crash report

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context

[ADR-0258](0258-a-native-crash-writes-a-local-minidump-on-windows.md) has an
unhandled-exception filter write `crash-<pid>-<n>.dmp` beside the panic reports. It
kept the process's default DACL on the dump, because the filter must not allocate,
lock or look up SIDs, and the report file's user-and-SYSTEM DACL needs the user's SID.
A dump holds thread stacks, which can hold fragments of a secret, so it is less private
than the panic report next to it, which gets a protected DACL for SYSTEM and the user.
ADR-0258 named tightening that as later work. The SID lookup only has to stay out of
the filter, not out of the process.

This ADR partially supersedes ADR-0258: it replaces the sentence that the dump keeps
the process's default DACL. The rest of ADR-0258 stands. It brings the dump up to the
protection crash reports already have and changes no security posture, so it is agent
tier ([ADR-0043](0043-narrow-maintainer-tier.md)).

<a id="decision"></a>
## Decision

1. **Built at install.** `install_native_dump` builds the crash report's security
   descriptor, a protected DACL for SYSTEM and the current user, once, outside the
   filter, and keeps it and a `SECURITY_ATTRIBUTES` pointing at it for the life of the
   process.
2. **Passed by the filter.** The filter passes those attributes to `CreateFileW`. It
   still allocates nothing, takes no lock and looks nothing up.
3. **A new file only.** The dump is created with `CREATE_NEW`. A file that is already
   there under the name (left by an earlier process with the same id) is skipped for
   the next number, a few times, rather than replaced, because a replaced file keeps
   its old DACL.
4. **Falling back.** If the descriptor can't be built at install, install logs a
   warning and dumps keep the process's default DACL, as before.

<a id="consequences"></a>
## Consequences

A dump is as private as the crash report beside it: other local users of a shared log
folder can't read it. The descriptor lives for the process, a few hundred bytes. A
failure to build it is visible in the log rather than silent. The minidump unit test
checks the dump's DACL on Windows.
