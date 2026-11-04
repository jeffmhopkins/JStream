<!-- @doc
id = "ADR-0112"
title = "Assignable zoom bindings for a follow-cursor camera, heard while another app has the focus"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0112: Assignable zoom bindings for a follow-cursor camera, heard while another app has the focus

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
A follow-cursor camera is a scene camera ([ADR-0094](0094-scene-cameras.md)) that
glides after the pointer over a display capture, close to the screen, with depth of field
([ADR-0108](0108-camera-depth-of-field.md)). The person zooms it in and out while they
work in another application, so the input that zooms must reach JStream while JStream
doesn't have the focus.

The maintainer wants zoom to be **assignable**: Ctrl with the mouse wheel by default, but
any modifiers with the wheel (Ctrl, Shift, Alt, Meta, or a mix), or plain keyboard keys
for zoom in and zoom out, so nobody is locked into one gesture.

Hotkeys already exist as collection state
([ADR-0024 §5](0024-scene-structure.md)): a `Chord` (modifiers and one
`KeyboardEvent.code` key) bound to a protocol action with `hotkey.set`. The engine stores
them and never listens. Today only the web UI listens, and only while its page has the
focus (UI-FEATURES KB-07). Nothing in JStream hears input meant for another application.

Hearing input while another application has the focus is a privacy and security posture
change, so it's maintainer tier
([ADR-0043](0043-narrow-maintainer-tier.md)). The operating systems differ:
- **Keys.** Windows `RegisterHotKey` and macOS's Carbon `RegisterEventHotKey` deliver only
  the registered chord, with no permission prompt. On Linux, X11's `XGrabKey` does the
  same, and on Wayland only the GlobalShortcuts portal can, with the person confirming each
  shortcut in a system dialog.
- **The wheel with modifiers.** No OS has a "register this one wheel chord" call. Windows
  needs a low-level mouse hook (`WH_MOUSE_LL`), which sees every mouse event and needs no
  permission. macOS needs a Quartz event tap, which needs the Input Monitoring permission
  to listen, or the Accessibility permission to also consume the event. X11 can grab
  buttons 4 and 5 with modifiers. Wayland offers no way at all.

<a id="decision"></a>
## Decision
1. **Zoom is a protocol action.** Zooming a follow-cursor camera in or out is an
   action, so a binding, an agent, MCP or a Stream Deck can all do it
   ([R-ARCH-001](../RULES.md#R-ARCH-001)). Its shape (a step in or out, and the zoom
   range) belongs to the follow-camera PR and isn't decided here.
2. **Bindings are hotkeys.** A zoom binding is an ordinary hotkey binding of that action,
   kept in the collection, set and cleared with `hotkey.set` and `hotkey.clear`, and
   undone like any other edit. Plain keys work with no new format: for example, `Minus`
   and `Equal`, or `Ctrl+Alt+ArrowUp` and `Ctrl+Alt+ArrowDown`.
3. **A chord may end in the wheel.** `Chord` gains two keys, `WheelUp` and `WheelDown`,
   which take modifiers like any key: `Ctrl+WheelUp`, `Shift+Alt+WheelDown`. They can only
   be bound to an action that takes one step, so a wheel notch is one step. Each notch, or
   every 120 units of high-resolution wheel delta, runs the action once. It's an additive
   protocol change: a new minor version under [R-API-001](../RULES.md#R-API-001).
4. **The defaults are `Ctrl+WheelUp` to zoom in and `Ctrl+WheelDown` to zoom out.** The UI
   offers to bind them when the person first turns a follow-cursor camera on, if neither
   chord is bound already, and they can rebind or clear them like any hotkey.
5. **The desktop host hears bound chords while another application has the focus.**
   - **Listening is limited to what is bound.** The host listens only for the chords
     bound in the open collection. A keyboard chord is registered with the OS's
     single-chord API, so the host never sees other keys. The wheel hook is installed
     only while at least one wheel chord is bound, and it is removed when the last one is
     cleared or JStream quits.
   - **Nothing leaves.** The wheel hook checks each wheel event against the bound wheel
     chords and keeps no other input. Nothing it sees is stored, logged or sent anywhere.
     Only the bound action runs, as a protocol action from the host.
   - **Consumed only while it's live.** A matching wheel event is consumed, so the
     application under the pointer doesn't zoom too, only while a follow-cursor camera is
     on the program or the preview. The rest of the time it passes through untouched.
     This keeps Ctrl+wheel zoom working in a browser while nothing is being followed.
   - **Permissions are asked for once, when they're needed.** On macOS the first wheel
     binding asks for Accessibility, which covers both listening and consuming. If the
     person declines, the wheel chords work only while JStream has the focus, and the
     Hotkeys list says so. Keyboard chords need no permission.
   - **Wayland:** keyboard chords go through the GlobalShortcuts portal, which shows the
     system's own confirmation. Wheel chords work only while JStream has the focus, and
     the Hotkeys list says so and suggests keys instead.
   - **The headless daemon** listens for nothing, as now. The web UI keeps hearing every
     binding, wheel chords included, while its page has the focus.
6. **Every hotkey, not only zoom.** Once the host listens while another application has
   the focus, it listens for every bound chord, so scene hotkeys (KB-07) work the same
   way. A setting, on by default, turns all of it off and puts JStream back to hearing
   hotkeys only while it has the focus.

<a id="consequences"></a>
## Consequences
- **Easier:**
  - People zoom the follow-cursor camera with whatever gesture suits them, including
    keys on a keyboard, a macro pad or a Stream Deck.
  - Hotkeys stop needing the JStream window to have the focus, which streamers expect
    from other tools.
  - There's one binding system: zoom adds two key names, not a second settings page.
- **Harder:**
  - JStream installs a system-wide mouse hook on Windows and asks for Accessibility on
    macOS. Both are visible to the person and to security tools. The hook has to be
    quick, because a slow low-level hook makes the whole system's pointer lag, so it only
    compares against a list and hands off a match.
  - Wayland users get keyboard zoom but no wheel zoom outside JStream.
  - Wheel chords bring a new protocol minor version, and the follow-camera PR depends on
    it.
- **We must now:**
  - add the wheel keys to `Chord`, the step rule, and a test for each;
  - build the desktop host's listeners: `RegisterHotKey`, Carbon hot keys,
    `XGrabKey`/`XGrabButton`, the GlobalShortcuts portal, `WH_MOUSE_LL` and the Quartz
    event tap;
  - add the off switch to the host's settings, and the focus-only notes to the Hotkeys
    list;
  - document the Accessibility prompt in GETTING-STARTED.
