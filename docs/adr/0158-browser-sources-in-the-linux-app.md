<!-- @doc
id = "ADR-0158"
title = "Browser sources in the Linux desktop app"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0158: Browser sources in the Linux desktop app

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0095](0095-browser-helper-on-linux.md) brought browser sources into Linux's
supported scope, through the sandboxed helper executable beside the host, and the daemon
opens them there with its `cef` feature. [ADR-0141](0141-desktop-app-on-linux.md)
decision 3 then gave the Linux desktop app a live engine with "no virtual camera and no
browser sources yet". Its packages already ship everything a browser source needs:
`cargo xtask dist-linux` builds the app with `cef` and lays the helper's `browser` folder
beside it, in the `.deb` (with `chrome-sandbox` setuid root) and in the AppImage. Only
the app's wiring is missing: `jstream-browser` is a Windows-only dependency of the app and
its `browser` module is compiled for Windows alone, so on Linux the `cef` feature turns
nothing on and the app's browser sources show nothing. beta.1 ships the Linux app and names browser
pages among its core flows ([ROADMAP › Beta](../../ROADMAP.md#beta)).

Whether Linux has browser sources, and the sandbox posture that comes with them, was
decided by ADR-0095 at maintainer tier. This ADR only applies that decision to the
second Linux host, so it is agent tier
([R-GOV-005](../RULES.md#R-GOV-005)).

<a id="decision"></a>
## Decision
This amends ADR-0141 decision 3 on browser sources only; the rest of it stands.

1. **The Linux desktop app opens browser sources** with its `cef` feature, as the
   Windows app does, with `jstream-browser` an optional dependency on Linux too: the same `browser::backend`, which finds the helper in the
   `browser` folder beside the app's resolved path (`CefConfig::beside_exe`), keeps
   CEF's cache in the data folder's `cef` and its log in `logs/cef.log`. The daemon's
   cache stays its own folder (`daemon`), so the two can run side by side (#1187).
2. **Everything else is ADR-0095's**: CPU frames through the private slot folder, the
   sandbox on (user namespaces, else the setuid `chrome-sandbox` the `.deb` installs;
   the AppImage has none and needs user namespaces), and no browser sources for root.
3. **Without the helper**, as in a build without `cef` or a folder with the helper
   missing, the app starts and browser sources fail with the backend's error, as on
   Windows.

<a id="consequences"></a>
## Consequences
- The Linux `.deb` and AppImage have working browser sources, which the beta.1 Linux pass
  checks on a desktop (#1357, #1482).
- CI's Linux clippy step type-checks the app's wiring with `dox`, as it does the
  daemon's, and after review `dist-linux` builds the app with `cef` for real. No CI job
  opens a page inside the app; `browser-linux` opens one through the helper end to end.
