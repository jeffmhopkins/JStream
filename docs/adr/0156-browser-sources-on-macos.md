<!-- @doc
id = "ADR-0156"
title = "Browser sources on macOS: the Linux helper's software paints, in an unsigned app bundle"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0156: Browser sources on macOS: the Linux helper's software paints, in an unsigned app bundle

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0144](0144-the-desktop-app-on-macos.md) brought the desktop app to macOS as an
unsigned, ad-hoc signed app bundle, and kept the browser source out of it, as
[ADR-0063](0063-macos-platform-support.md) §13 had. The maintainer has since decided that
JStream on macOS stays unsigned for good and that browser sources come there from CEF,
the Chromium the Windows and Linux builds already ship
([ROADMAP › macOS without signing](../../ROADMAP.md#macos-unsigned)). That settled the
scope; this ADR records how it is built, so it is agent tier
([ADR-0043](0043-narrow-maintainer-tier.md)). The security posture is unchanged: pages
run in the sandboxed helper process of [ADR-0048](0048-sandboxed-browser-helper.md),
with Chromium's sandbox on, as on Windows and Linux.

CEF on macOS differs from the other two in four ways. Its library is a framework that
each process loads at run time from where the app bundle keeps it, not a library linked
at build time. Its subprocesses must run from helper app bundles in the app's
`Contents/Frameworks`, named after a plain helper (`<name> Helper.app`) with ` (GPU)`,
` (Renderer)`, ` (Plugin)` and ` (Alerts)` variants beside it. Each subprocess enters
CEF's Seatbelt sandbox itself, through `libcef_sandbox.dylib` in the framework, before it
loads anything else. And the browser process's `NSApp` must be an application class that
speaks Chromium's `CrAppControlProtocol`. The pinned `cef` crate and its sys crate bind
all of it: CEF's scoped library loader and scoped sandbox, and the protocols.

[ADR-0095](0095-browser-helper-on-linux.md) already runs the helper as one executable
that paints in software, driven over a socket pair, with frames crossing as CPU frames
through a private folder. None of that is Linux-only: it is Unix sockets, files and
CEF's `OnPaint`.

<a id="decision"></a>
## Decision
This ADR amends ADR-0144 and ADR-0063 §13 on one point: the browser source is in scope
on macOS, in the desktop app. Everything else in both stands.

1. **The Linux helper's design, on macOS.** The helper is the same executable,
   `jstream-browser-helper`, built from `apps/jstream-browser-helper` with the `cef`
   feature. The engine drives it as on Linux (ADR-0095 decisions 3 to 5): a socket pair
   as its standard input, software paints written into slot files in a private folder
   made fresh for each helper (on macOS under the user's own temp folder), and CPU frames
   in the engine, which needs no GPU device for them. Pages paint at their frame rate
   with a transparent background, as everywhere.
2. **The bundle's layout.** The browser process is `JStream.app/Contents/MacOS/
   jstream-browser-helper`, beside the app, where `CefConfig::beside_exe` finds it. CEF's
   framework is `Contents/Frameworks/Chromium Embedded Framework.framework`, and beside
   it are `JStream Helper.app` and its four variants, each holding a copy of the helper
   executable as its own. The helper names the bundle, the framework and the plain helper
   to CEF (`main_bundle_path`, `framework_dir_path`, `browser_subprocess_path`), and
   Chromium finds the variants by name. The helper apps are agent apps (`LSUIElement`):
   no Dock icon. CEF's credits and version go in `Contents/Resources`
   (`CEF-CREDITS.html`, `CEF-VERSION.txt`), as the Windows zip and Linux packages carry
   them.
3. **Each process loads CEF itself.** A process started with `--type=` is a subprocess:
   it enters CEF's sandbox first and exits if it can't, then loads the framework three
   folders up from its executable. The browser process loads it from `../Frameworks`,
   tells the engine if it can't, then makes `NSApp` an instance of its own application
   class, which speaks CEF's application protocols and is kept out of the Dock and the
   app switcher (activation policy `Prohibited`).
4. **The sandbox stays on**, as ADR-0048 and ADR-0095 have it, with no entitlements: an
   ad-hoc signed app has none, and Seatbelt needs none. Chromium also keeps its in-memory
   profiles' key itself (`--use-mock-keychain`), never the login Keychain, which would
   ask the user again after every update of an ad-hoc signed app.
5. **Signed ad hoc, from the inside out.** `scripts/mac/bundle-app.sh` takes the built
   helper and CEF's distribution folder (`--browser-helper`, `--cef`;
   `scripts/mac/build-browser-helper.sh` builds the one and finds the other) and signs
   the framework's libraries, the framework, each helper app and the helper, then the
   app. Without them it bundles the app without browser sources, as before. It refuses a
   Developer ID identity together with browser sources: the helpers would need
   hardened-runtime entitlements that an unsigned build never has.
6. **The desktop app only.** The app built with `cef` on macOS opens `browser` sources'
   pages through the bundle's helper. The daemon on macOS isn't an app bundle, so it gets
   no browser sources there, as before.
7. **Checked in CI.** The after-review `macos-app` job builds the app with `cef` and the
   helper with CEF, bundles them, and opens a page through the bundled helper, from a
   file and from a server on this computer, checking its paints reach the engine. Whether
   the helpers launch from a downloaded, quarantined image once the user has allowed the
   app, and whether the sandbox holds on real hardware, are checked by hand on a Mac
   before a release (`docs/runbooks/mac-testing.md`).

<a id="consequences"></a>
## Consequences
- **Easier:** a Mac user gets browser sources: overlays, alerts, HTML scenes and the page
  API, as on Windows and Linux, from one helper design shared with Linux.
- **Harder:** the app bundle grows by CEF's framework, a few hundred megabytes before the
  disk image compresses it. Pages paint in software and cross through CPU memory, a copy
  per paint that Windows' shared textures avoid; a GPU path (CEF's IOSurface paints) is
  its own later decision if it is ever needed. Gatekeeper's quarantine and the sandbox
  can only be checked on a Mac, by hand; CI's runner is neither a downloaded image nor
  real hardware. Nothing here was run on Mac hardware when it landed.
- **Amends** ADR-0144's consequences and ADR-0063 §13: the browser source is in the
  macOS desktop app. Signing, notarisation and the virtual camera stay out.
- **Docs to change:** the browser crate's and the helper's READMEs, the desktop app's
  README, ARCH › Browser source and › Packaging, the macOS testing runbook, and
  ROADMAP › macOS without signing.
