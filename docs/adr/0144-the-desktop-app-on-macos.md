<!-- @doc
id = "ADR-0144"
title = "The desktop app on macOS, as an unsigned app bundle"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0144: The desktop app on macOS, as an unsigned app bundle

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0063](0063-macos-platform-support.md) §13 left two macOS items for later, each its
own decision: the desktop app (Tauri on WKWebView) and packaging, an app bundle and disk
image with its signing and notarisation. Until now the macOS host was the daemon with the
web UI in a browser. The maintainer has since asked for the desktop app on macOS, as on
Windows. Signing and notarisation need the Apple Developer Program, which ADR-0063's
question 1 answered "later", so that answer still holds.

The app's code is mostly platform-neutral already: the engine, its server, the UI's
scheme and the bootstrap script. What was Windows-only is the window (WebView2), the
Browse dialogs, the live engine's backends and key store, and the global hotkeys
([ADR-0112](0112-assignable-camera-zoom-bindings.md) §5). Each has a macOS counterpart in
`jstream-platform-macos` or the system.

<a id="decision"></a>
## Decision
1. **The desktop app builds and runs on macOS**: the Tauri window on WKWebView, the engine
   in-process and live on the backends the daemon uses there (ADR-0063), stream keys in
   the Keychain, AppKit's open and save panels for Browse, and no virtual camera
   (ADR-0063 §13 keeps it out). The app is windowed where `tauri` is a dependency, and its
   build script says so to the code (`cfg(webview)`), so another platform joins by
   adding itself in one place. Linux joins the same way behind its `webkitgtk` feature
   ([ADR-0141](0141-desktop-app-on-linux.md)).
2. **Global hotkeys on macOS** follow ADR-0112 §5: keys through Carbon's
   `RegisterEventHotKey`, which needs no permission, and wheel chords through a Quartz
   event tap installed only while a wheel chord is set. The tap needs the Accessibility
   permission, asked for at the first wheel chord. Without it, wheel chords are heard only
   while JStream has the focus, and the listener reports them so. Carbon's Event Manager
   has no maintained binding, so its few declarations are written by hand in the platform
   crate (R-UNSAFE-004 prefers bindings where they exist).
3. **Packaging is an unsigned app bundle in a disk image**: `JStream.app` with the binary,
   the UI in `Contents/Resources`, the usage strings and the icon, inside `JStream.dmg`,
   built by `scripts/mac/bundle-app.sh` and in CI after review. The bundle is ad-hoc
   signed, as an unsigned build on Apple Silicon must be. The script takes a Developer ID
   identity and a notary profile when there are some, so signing and notarisation plug in
   without changing the bundle once question 1 is answered "now".

<a id="consequences"></a>
## Consequences
- **Easier:** a Mac user gets the same window as a Windows user, with hotkeys heard while
  another app has the focus, and nothing to run from Terminal.
- **Harder:** an ad-hoc signed app is blocked by Gatekeeper until the user allows it in
  System Settings, and macOS forgets its camera, microphone, screen recording and
  Accessibility grants, and asks for the Keychain again, after each new build (ADR-0063's
  consequences). The window can only be checked by hand on a Mac and by CI's launch of
  it; nothing here was run on Mac hardware.
- **Amends** ADR-0063 §13: the desktop app and the unsigned bundle are in scope; signing,
  notarisation, the virtual camera and the browser source stay out. The rest of
  ADR-0063 holds.
- **Docs to change:** the desktop app's README (purpose, usage, design),
  `jstream-platform-macos`' README, the macOS testing runbook.
