<!-- @doc
id = "ADR-0083"
title = "The server keeps the UI's settings, so a layout follows the person"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0083: The server keeps the UI's settings, so a layout follows the person

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
The workspace's dock (its zones, tab groups, sizes, collapsed groups and saved layouts,
[UI-FEATURES › Docking](../UI-FEATURES.md#docking)) is kept in the browser's storage. The
desktop app's webview and each browser that reaches the daemon then show different
layouts, and a portable install ([ADR-0020](0020-portable-windows-distribution.md)) loses
its layout with the WebView2 profile. DK-10 asks for one layout everywhere. The UI design
review compared three places for it (decision register D35): browser storage, a
server-side store the UI owns the shape of, and the collection, which is wrong because
switching collections would switch layouts and every change would land on the undo stack.
It recommended the server-side store. Where the store lives and how it is reached are
architecture, so agent tier ([R-GOV-005](../RULES.md#R-GOV-005)).

<a id="decision"></a>
## Decision
- **Two calls, answered by the server.** `ui_settings.set {key, value}` (an action) keeps
  any JSON value under a key, and a `null` value removes it; `ui_settings.get {key}` (a
  query) reads it back, omitting `value` when there is none. They ship in protocol 0.51,
  an additive minor ([R-API-001](../RULES.md#R-API-001)). `jstream-server`'s session
  wrapper answers them, as it answers the device calls
  ([ADR-0057](0057-server-answers-credential-calls.md)), with pairing off too, so the
  desktop app's webview is served. A `Control` with no server in front (the engine's own
  session, `MockControl`) answers `unsupported`, and the UI then keeps its settings in the
  browser alone.
- **Opaque values the UI owns.** The server checks only a key's form (1 to 64 of `a`-`z`,
  `0`-`9`, `.`, `_`, `-`) and the sizes (64 KiB of compact JSON per value, 64 keys), then
  stores the value as it came. The web UI keeps its dock under `workspace.dock`, in the
  same shape it already keeps in the browser.
- **Kept in the data folder.** `ServerConfig::ui_settings_file` names the file; the
  desktop app and the daemon both pass `ui-settings.json` in the data folder (ADR-0020),
  so both show the same workspace, and a portable install keeps its layout beside the
  exe. It is rewritten whole through a temporary file on every change, user-only like the
  device file. A file that can't be read is logged and replaced on the next change rather
  than stopping the server: a lost layout costs less than an engine that won't start.
- **Not collection state, and no event.** A change has no `rev`, no undo and no event,
  and outlives switching collections. The UI reads the dock when its workspace opens and
  sends each change once the dock has stayed the same for half a second (a splitter drag
  changes it on every move). Another open client picks the change up the next time it
  opens, not while someone is working in it: rearranging a dock under a person's pointer
  because another device moved a panel would be worse than a stale layout.
- **The browser keeps its copy.** The UI still writes the dock to the browser's storage
  and shows that copy first, so a reload draws at once and works with no server; the
  server's copy then replaces it. When the server keeps none yet, the browser's copy is
  sent, so an existing layout carries over.

<a id="consequences"></a>
## Consequences
- The desktop app and every browser of one engine show the same layout, and saved
  layouts ("My layout 1") follow too. Two clients changing the dock at once: the last
  change sent wins.
- The calls are reachable from MCP as the generated `ui_settings` tool and from any
  client, with no permission tiers (ADR-0044). No plugin may call them.
- Other UI preferences (the mixer's orientation, studio mode's toggle) can move to the
  store under their own keys without a protocol change.
- A UI that changes its dock's shape must still read the old one, or fall back to its
  defaults, as it does for the browser's copy today.
