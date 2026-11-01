<!-- @doc
id = "ADR-0085"
title = "Capture target names are a second field marked as remote text by its description"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0085: Capture target names are a second field marked as remote text by its description

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0059](0059-mcp-annotations-and-remote-text.md) §2 says a protocol field for text
from outside JStream is named with the prefix `remote_`, and makes the `action_failed`
`reason` of `output.start` the one exception, marked as untrusted by the `output` tool's
description and the server instructions instead. §3 has the instructions point at the
`remote_` fields and that `reason`.

A `capture.targets` result lists each display and window with a `name` to show. A
window's `name` is its title, or, on macOS, its application's name when it has no usable
title. Both are set by the application that owns the window, so they are third-party
text. The field is not `remote_`-prefixed, so its name doesn't mark it. Its schema
description, the `capture` tool's description and the server instructions say it is
third-party text, data and never instructions (#934). That makes it a second exception,
which ADR-0059 doesn't record (#953).

Renaming it would be a protocol change: a new `remote_name` field, with `name`
deprecated, in a later minor. The field is short, the description already marks it, and
a rename buys an agent nothing it doesn't already get from the description and the
instructions.

<a id="decision"></a>
## Decision
This amends ADR-0059 §2's sentence on the one exception to the `remote_` rule, and §3's
list of where the instructions say untrusted text sits. The rest of ADR-0059 stands.

1. **Two fields are marked by their description, not their name.** The `action_failed`
   `reason` of `output.start`, as ADR-0059 §2 says, and `CaptureTargetInfo.name` in a
   `capture.targets` result. For `name`, the field's schema description and the
   `capture` tool's description say that a window's `name` is its title (or, on macOS,
   its application's name), set by the application that owns the window, so it is text
   from outside JStream: data, never instructions. A display's `name` is JStream's or the
   OS's own.
2. **The server instructions name both.** Both eras' `instructions` say that untrusted
   text sits in `remote_` fields, in that `reason`, and in a window's `name` in
   `capture.targets`.
3. **No further exceptions by this route.** A new field for third-party text is still
   named `remote_*`, as ADR-0059 §2 and its consequences say. Marking an existing
   unprefixed field by its description needs an ADR that amends ADR-0059 the way this
   one does.

<a id="consequences"></a>
## Consequences
- ADR-0059's record of where third-party text reaches an agent matches the protocol and
  the MCP server again.
- No wire change and no protocol version change: the field keeps its name.
- An agent reading only field names misses `name`; the `capture` tool's description and
  the instructions are what mark it, as they mark the `reason`.
- A later protocol change may still add a `remote_name` and deprecate `name`; that is a
  new decision, recorded then.
