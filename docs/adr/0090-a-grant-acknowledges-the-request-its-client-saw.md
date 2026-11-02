<!-- @doc
id = "ADR-0090"
title = "A plugin grant acknowledges the request its client was shown"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0090: A plugin grant acknowledges the request its client was shown

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0076](0076-saved-wasm-plugins-and-manifest-requests.md) says, under **Updates**,
that "a `plugin.grant` acknowledges what the manifest asks for now". The engine does
that by marking the request it holds when the grant arrives as granted.

A person grants from a dialog that shows the request `plugin.list` gave their client.
If another client's `plugin.enable` loads an updated manifest between that list and the
grant, the grant acknowledges the newer request, which the person never saw (#978). The
grant still can't give more than the person ticked, and never more than the manifest
asks for, so nothing is escalated. But the update's new asks lose the `needs_grant`
prompt that ADR-0076 promises them.

The issue proposed letting `plugin.grant` carry the request it acknowledges, or a
revision from `plugin.list`. That changes the protocol, and the engine can already tell
without it: every call arrives on a client's session, and `plugin.list` is the only way
a client learns a plugin's request.

<a id="decision"></a>
## Decision
This amends ADR-0076's **Updates** bullet on what a `plugin.grant` acknowledges. The
rest of ADR-0076 stands.

1. **The engine remembers what it showed.** Each `plugin.list` notes, for each plugin,
   the request it showed the calling client. The note is kept in memory only, is never
   saved, and is dropped when the client's session ends.
2. **A grant acknowledges that request.** A `plugin.grant` marks as granted the request
   its client was last shown. When the plugin's manifest has since been updated to ask
   for more, the next load still finds an update beyond what was granted, and the
   plugin waits as `needs_grant` until the person, shown the update by a new
   `plugin.list`, grants it again. The grant itself is applied, and checked against what
   the manifest asks for now, as before.
3. **A client that never listed it** acknowledges what the manifest asks for now, as
   ADR-0076 says. It was shown nothing, so there is nothing older to hold it to, and a
   scripted grant keeps working.
4. **No protocol change.** `plugin.grant` and `plugin.list` keep their wire forms. Only
   the doc comment of `plugin.grant`, and so the schema descriptions, say what it
   acknowledges.

<a id="consequences"></a>
## Consequences
- A person's grant can no longer acknowledge an update they weren't shown, so the
  update's new asks always reach a dialog.
- A UI that grants after a stale list sees `plugin.enable` answer `action_failed` naming
  what is new, lists again, and shows the update. The jstream UI's grant dialog already
  shows the request from its own `plugin.list`.
- The engine keeps one request per client and plugin listed, until the session ends.
- A client that grants without listing still acknowledges whatever the manifest asks
  for now. A client that shows a person a request must get it from `plugin.list`.
