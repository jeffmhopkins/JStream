<!-- @doc
id = "ADR-0059"
title = "Linux platform support: portal and PipeWire capture, VA-API encode"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0059: Linux platform support: portal and PipeWire capture, VA-API encode

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Windows is JStream's only platform today ([ADR-0008](0008-windows-support-policy.md)).
Linux is planned for M3 as roadmap item LNX1 ([ROADMAP](../../ROADMAP.md#m3)), and
[ADR-0031](0031-source-types-and-decoding-scope.md) leaves its codec path to "a separate
ADR before M3 (VAAPI for video; AAC is open). Until then Linux can't stream." This is that
ADR. It covers **Linux only**: macOS (MAC1) is out of scope here and gets its own ADR.

Which platforms are supported is product scope, and part of this decision touches the
license policy, so it is a maintainer-tier decision
([ADR-0043](0043-narrow-maintainer-tier.md)).

The forces:
- **Screen capture.** On Wayland an application cannot read the screen or other windows
  itself. The only portable path is the
  [ScreenCast portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html):
  the compositor shows its own picker, and the app receives a
  [PipeWire](https://docs.pipewire.org/) node to read frames from. GNOME, KDE Plasma and
  the wlroots compositors all implement it, and it also works in X11 sessions where the
  desktop ships a portal backend. Since interface version 4 the portal can hand back a
  restore token, so a granted choice survives a restart without a new prompt.
- **Audio.** PipeWire is the default sound server on current Ubuntu and Fedora, and
  exposes inputs, outputs, each output's monitor (desktop audio) and each application's
  stream (application audio) through one API. ALSA is what remains on systems without it.
- **Codecs.** Linux ships no OS H.264 or AAC encoder ([ADR-0031](0031-source-types-and-decoding-scope.md)).
  The GPU-vendor path is [VA-API](https://github.com/intel/libva), which Intel's driver
  and Mesa's AMD driver implement for H.264 encode. NVIDIA's driver does not encode
  through VA-API; it offers NVENC. Some distributions build Mesa with H.264 and HEVC encode
  turned off for patent reasons (Fedora does, with a full build in RPM Fusion).
- **Software fallback.** ADR-0031 and [R-DEP-002](../RULES.md#R-DEP-002) ban bundled C
  codec libraries by name, OpenH264 and x264 among them, and `deny.toml` bans their
  crates. x264 is GPL-2.0-or-later, which [R-DEP-001](../RULES.md#R-DEP-001) excludes
  outright. OpenH264's source is BSD-2-Clause, a license `deny.toml` allows, but its H.264
  patent license covers only
  [Cisco's own binary](https://www.openh264.org/BINARY_LICENSE.txt), and only when it is
  downloaded separately to the user's device, not integrated with other software first.
  A copy built or bundled by someone else is not covered.
- **Libraries at build time.** libpipewire and libva are C libraries. Linking them through
  `-sys` crates needs their headers on every machine that builds the workspace, which CI
  and Windows developers don't have. Loading them at run time (`dlopen`) needs nothing at
  build time and degrades to a clear "not installed" error.
- **CI** already runs on Linux ([ADR-0054](0054-platform-ci-after-review.md)): `fmt`,
  `clippy`, `test`, `doc`, `deny`, `lint-deps` and `docs` run on `ubuntu-latest` for every
  PR. Those runners have no GPU, no desktop session and no PipeWire daemon.

<a id="decision"></a>
## Decision

**1. Linux is a supported platform; macOS is not part of this decision.** The Linux
backends live in `jstream-platform-linux` (layer 2, `unsafe` allowed by
[R-UNSAFE-001](../RULES.md#R-UNSAFE-001)), behind the same `jstream-platform` traits as
Windows. Code that calls Linux APIs is under `cfg(target_os = "linux")`; on other targets
the crate builds empty apart from platform-independent helpers, as the Windows crate does.

**2. Support tiers.**

| Tier | What |
|---|---|
| Supported and tested | x86_64; GNOME or KDE Plasma in a Wayland session; Ubuntu LTS releases in standard support whose sound server is PipeWire 1.0 or later (24.04, 26.04<!--fact:ubuntu-lts-supported-->) and maintained Fedora releases (43, 44<!--fact:fedora-maintained-->); PipeWire 1.0 or later; xdg-desktop-portal with the desktop's own backend, ScreenCast interface version 4 or later. |
| Best effort | X11 sessions of those desktops where the distribution still ships one; other desktops with a ScreenCast portal backend (wlroots compositors through xdg-desktop-portal-wlr, Hyprland, COSMIC); other distributions (Debian stable, Arch, openSUSE); systems without PipeWire (ALSA fallback, below); aarch64. Bugs are fixed when reported, but nothing is tested there. |
| Not supported | Capture without a ScreenCast portal (no XShm or XComposite path); PulseAudio without PipeWire beyond what ALSA reaches; anything older than the floors above. |

As with Windows, the supported releases are facts the docs sweep re-checks, so the policy
text needn't change when a release ages out.

**3. Screen and window capture go through the ScreenCast portal and PipeWire.**
- `Capture::targets` lists one target per kind: "a display" and "a window", each chosen in
  the desktop's own picker when the capture starts. JStream never enumerates other
  windows itself; on Wayland it can't.
- A source keeps the portal's restore token (`persist_mode` 2, "until revoked") in the
  data folder ([ADR-0020](0020-portable-windows-distribution.md)'s `paths`), so reopening
  a collection captures the same screen without a new prompt. A token is single-use, so
  each start stores the replacement the portal returns. Revoking the grant in the
  desktop's settings, or deleting the source, forgets it. The token never leaves the
  machine.
- Frames arrive on a PipeWire stream. The GPU path imports DMA-BUF frames into the
  engine's Vulkan device ([ADR-0004](0004-wgpu-with-native-interop.md)); shared-memory
  frames are the CPU fallback. The cursor is requested as metadata where the portal offers
  it, so the source's "show cursor" setting works as on Windows.

**4. Audio goes through PipeWire, with ALSA as the fallback.**
- PipeWire serves audio input, output, desktop audio (the default output's monitor) and
  application audio: `AudioIo::processes` lists the processes that own a PipeWire playback
  stream, and `ProcessLoopback` records that stream.
- Without a PipeWire daemon, ALSA serves input and output only. Desktop and application
  audio report `Unsupported` there.

**5. Media clock.** The clock is `CLOCK_MONOTONIC` in nanoseconds
([ADR-0014](0014-media-clock.md)), the clock PipeWire and V4L2 already stamp their data in,
so mapping a timestamp onto the media clock is a unit change.

**6. Video encode goes through VA-API.**
- H.264 encode uses VA-API (Intel and AMD), loaded at run time. HEVC and AV1 follow the
  roadmap's enhanced-RTMP item. A driver without H.264 encode, such as Fedora's stock
  Mesa, is reported as a missing component with the distribution's remedy.
- NVENC, reached through the NVIDIA driver's own library at run time, is NVIDIA's path and
  a follow-up roadmap item. It is a GPU-vendor API, so ADR-0031 already allows it. Vulkan
  Video encode is the candidate to unify the two later.
- **No software H.264 fallback in this decision.** Without a hardware encoder, Linux can't
  stream; capture, mixing and preview still work. Whether to add one is question 1 below.
  x264 is not an option under any answer: it is GPL.

**7. Audio encode (AAC) stays open.** VA-API encodes no audio, and Linux has no OS AAC
encoder, so Linux can't stream until question 2 below is answered.

**8. Run-time loading, not build-time linking.** libpipewire, libva and libasound are
loaded with `dlopen` (through `libloading`) when a backend first needs them. The workspace
builds and tests on any Linux machine, and in CI, without their headers or `-sys` crates,
and `deny.toml` and the lint-deps native allowlist don't change. A missing library is a
`MissingComponent` error that names the package to install.

**9. Out of scope for now**, each its own later decision:
- the **virtual camera** (v4l2loopback needs a kernel module and root, so it gets its own
  ADR, as the roadmap says); Linux `Backends::vcam` is `None` until then;
- **webcams** beyond a plain V4L2 backend, and the camera portal for sandboxed packages;
- the **media-file source**: Linux has no OS demuxer or audio decoder, which ADR-0031's
  "OS codecs only" assumes;
- the **browser source** on Linux, and a Linux build of the desktop app (Tauri on
  WebKitGTK): the Linux host for M3 is the daemon with the web UI in a browser;
- **packaging** (tarball, AppImage, Flatpak) and its signing;
- the **stream-key and token store** (Secret Service over D-Bus) and what happens where no
  keyring runs, which is a security-posture decision for the maintainer when the crate
  reaches it.

**10. CI.** Linux CI stays as it is and now also builds, lints and unit-tests
`jstream-platform-linux`; nothing in it needs a system library. Tests that need a desktop
session, PipeWire or a VA-API GPU sit behind the crate's `hw-tests` feature and
`#[ignore]`, as on Windows, and run on a Linux machine by hand. A headless PipeWire daemon
in CI for the audio tests is a follow-up.

<a id="questions"></a>
## Questions for the maintainer
Accepting this ADR accepts decisions 1 to 10. These two are left open on purpose; each
answer becomes its own ADR, because either "yes" changes ADR-0031 and R-DEP-002.

1. **A software H.264 fallback on Linux?** The options:
   - **No fallback** (this ADR's default): no hardware encoder means no streaming.
   - **Cisco's OpenH264 binary**, downloaded from Cisco on the user's request and loaded
     at run time, never bundled. Its license (BSD-2-Clause) is already on the allow list,
     and Cisco's patent license covers that binary when it is downloaded separately. It needs an ADR superseding
     ADR-0031 with a bounded exception like CEF's, a change to R-DEP-002, and either own
     bindings or lifting the `openh264` ban in `deny.toml`.
   - **x264**: excluded, GPL.

   Recommendation: Cisco's binary, as a follow-up after VA-API works, because many Linux
   machines (NVIDIA before NVENC lands, Fedora's stock Mesa, virtual machines) have no
   usable hardware encoder.
2. **Where does AAC come from on Linux?** The options:
   - **An AAC-LC encoder written in Rust in this repository**: permissive, no C, but real
     work, and AAC is still patent-licensed (its oldest patents have expired; whether
     JStream needs a license for the rest is a legal question).
   - **fdk-aac**: its license is not on the allow list and the crate is banned.
   - **FFmpeg or GStreamer at run time**: banned by R-DEP-002.
   - **Opus instead of AAC**: only if the target accepts it; YouTube's RTMP ingest expects
     AAC today.

   Recommendation: the Rust AAC-LC encoder as its own roadmap item, after a patent check.

<a id="consequences"></a>
## Consequences
- **Linux can capture, mix, preview and play audio** once the crate lands, on the
  supported desktops, with the portal's picker in place of an in-app list of windows.
- **Linux can't stream yet.** That needs VA-API H.264 (this ADR) and an AAC encoder
  (question 2), and on machines without hardware encode, question 1.
- **Builds stay simple.** No Linux system headers are needed to build or test the
  workspace, and the dependency policy files don't change. The cost is hand-written
  bindings for the few libpipewire, libva and libasound calls the crate makes, all behind
  safe APIs under [R-UNSAFE-002](../RULES.md#R-UNSAFE-002).
- **Untested tiers can break silently.** Only the supported tier is ever run, and only by
  hand until hardware CI exists.
- **Edits that follow acceptance** (none are made while this ADR is proposed): ROADMAP's
  M3 Linux line points here; ARCHITECTURE's crate-map row for `jstream-platform-linux`
  names this ADR instead of "codec policy per future ADR"; the README's platform table
  marks Linux "M3, per ADR-0059".
