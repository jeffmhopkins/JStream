<!-- @doc
id = "ADR-0141"
title = "The desktop app on Linux: Tauri on WebKitGTK, behind a feature"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0141: The desktop app on Linux: Tauri on WebKitGTK, behind a feature

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0060](0060-linux-platform-support.md) §9 left "a Linux build of the desktop app
(Tauri on WebKitGTK)" out of scope, with the daemon and the web UI in a browser as the
Linux host. The engine has since become live on Linux in the daemon (the backends, the
Secret Service key store), the desktop host gained an X11 global hotkey listener that
waits for a window to run in ([ADR-0112](0112-assignable-camera-zoom-bindings.md) §5),
and the UI is one code path for every host
([ADR-0045](0045-leptos-web-ui.md)). The maintainer asked for the platform-specific
desktop apps to be built: "All right. Go ahead and start out the threads to build up the
platform. Specific desktop apps as well then that should be done."

Tauri 2 already runs on Linux through wry on
WebKitGTK 4.1 ([Tauri's Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux)),
and `jstream-desktop` already compiles everything but the window on Linux. What stops a
Linux window is that WebKitGTK's Rust bindings link the system's GTK and WebKitGTK
through pkg-config, so a Linux build of Tauri needs their development packages, which
every Linux CI job that builds the workspace would then have to install.

The window has one native piece beyond the webview: the Browse dialogs (`pick_path`).
On Windows it is a common-item dialog; a GTK dialog would tie the app to GTK's own file
chooser, which a KDE or a sandboxed session replaces with the portal anyway.

<a id="decision"></a>
## Decision
This amends ADR-0060 §9 for the desktop app only; the rest of it stands.

1. **The desktop app has a Linux window: Tauri on WebKitGTK 4.1.** The same `shell.rs`,
   scheme, navigation guard, projector windows, bootstrap script and the two commands
   (`pick_path`, `set_network`) run on both platforms; the code is gated on one cfg,
   `webview`, which `build.rs` sets for Windows and for Linux with the feature below.
   The scheme's origin is `jstream://localhost` there, as `ui.rs` already says for a
   webview other than WebView2. Links the app hands to the system browser go through
   `xdg-open` instead of Explorer, with the URL as one argument.
2. **Behind a Cargo feature, `webkitgtk`, off by default.** Tauri and `tauri-build` are
   optional Linux dependencies the feature turns on, so `cargo build --workspace` and
   the existing Linux CI jobs need no GTK. Without it the Linux binary says how to build
   the window and to use the daemon meanwhile. A new CI job, `desktop-linux`, installs
   WebKitGTK and Xvfb, lints and tests the app with the feature, and opens the window on
   Xvfb (`tests/webkitgtk.rs`), checking that WebKitGTK loads the UI, that the UI
   connects to the in-process engine with its token, and that the X11 hotkey listener
   starts. It runs on wave PRs too, since it is the only check of the Linux window, and
   is not required, like `browser-linux`. Linux packages (decision 5) build with the
   feature.
3. **The engine in the Linux window is live, wired as the daemon's**: the Linux backends
   (`LinuxBackends`, ADR-0060), stream keys in the Secret Service or its `0600` file
   ([ADR-0071](0071-linux-secret-store.md)), RTMP(S) publishing, the process and thread
   CPU readers, and the ScreenCast portal's restore tokens in the data folder's
   `capture-tokens`, the daemon's folder. No virtual camera and no browser sources yet,
   as in ADR-0060 §9 and [ADR-0095](0095-browser-helper-on-linux.md) for the app.
4. **Browse dialogs go through the FileChooser portal**
   ([org.freedesktop.portal.FileChooser](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html)),
   on `jstream-platform-linux`'s own D-Bus client, as screen capture does: the desktop's
   own dialog in its own process, on X11 and Wayland alike, with no toolkit linked. Only
   the portal's own bus connection may answer, and only a local `file://` path is
   accepted. The command runs off the main thread, so the window keeps painting.
5. **Packaging is a `.deb` and an AppImage, built in CI as artifacts**, from a follow-up
   PR; publishing them with a release, and signing them, stay later decisions, as
   ADR-0060 §9 says for packaging in general.

<a id="consequences"></a>
## Consequences
- Linux gets the same app as Windows, global hotkeys included on X11; Wayland's need the
  GlobalShortcuts portal (#1403).
- One more CI job installs GTK and WebKitGTK; the rest stay as they are.
- `deny.toml`'s ignore of RUSTSEC-2024-0370 (`proc-macro-error`, a build-time proc-macro
  under gtk-rs 0.18) now covers a crate that is built, with the feature; it stays until
  Tauri moves to GTK 4 or gtk-rs drops it. The lint-deps notes for the GTK `-sys` crates
  say they are built with the feature.
- Nothing is tested on real Linux hardware yet: the window is checked on Xvfb only, and
  the portal dialog by its request and answer, without a running portal.
