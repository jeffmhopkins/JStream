<!-- @doc
id = "ADR-0063"
title = "macOS platform support: ScreenCaptureKit, Core Audio taps, VideoToolbox"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0063: macOS platform support: ScreenCaptureKit, Core Audio taps, VideoToolbox

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
Windows is JStream's primary platform ([ADR-0008](0008-windows-support-policy.md)), and
Linux joins it in M3 ([ADR-0060](0060-linux-platform-support.md)). macOS is roadmap item
MAC1 ([ROADMAP](../../ROADMAP.md#m3)): "macOS platform crate (VideoToolbox/AudioToolbox;
a signed camera extension needs its own ADR). A Syphon receiver, the macOS follow-up to
Spout2, is considered with it." This ADR decides what macOS support is. It covers **macOS
only**.

Which platforms are supported, and on what hardware, is product scope, and two of the
open questions spend money, so this is a maintainer-tier decision
([ADR-0043](0043-narrow-maintainer-tier.md), [R-GOV-005](../RULES.md#R-GOV-005)).

The forces:
- **Codecs are already settled.** [ADR-0061](0061-source-types-and-decoding-scope-v2.md)
  names VideoToolbox and AudioToolbox as the macOS baseline. Apple ships hardware H.264
  and HEVC encoders and an AAC encoder in the OS, so, unlike Linux, nothing about codec
  licences is open here.
- **Screen capture.** [ScreenCaptureKit](https://developer.apple.com/documentation/screencapturekit)
  (macOS 12.3 and later) is the supported capture API; the older CoreGraphics capture
  calls are deprecated. It can list displays, applications and windows itself, or let the
  user choose in the
  [system picker](https://developer.apple.com/documentation/screencapturekit/sccontentsharingpicker)
  (macOS 14 and later). Either way the user has to grant the Screen Recording permission
  (TCC) in System Settings. The app can check for it and ask for it with
  [`CGPreflightScreenCaptureAccess` and `CGRequestScreenCaptureAccess`](https://developer.apple.com/documentation/coregraphics/cgrequestscreencaptureaccess())
  (macOS 10.15 and later). Since macOS 15, apps that capture without the system picker
  [get a periodic re-confirmation prompt](https://tidbits.com/2024/08/19/apple-reduces-excessive-sequoia-permission-requests-shifts-to-monthly),
  now monthly.
- **Agents drive JStream too.** An agent over MCP picks a capture target by its ID, as on
  Windows. It can't click a system picker. On Wayland there's no choice (ADR-0060 §3); on
  macOS there is.
- **Audio.** Core Audio serves inputs and outputs. For desktop and application audio,
  [Core Audio process taps](https://developer.apple.com/documentation/coreaudio/audiohardwarecreateprocesstap(_:_:))
  (macOS 14.2 and later) record the output of one process, a set of them, or everything
  except a set. They need their own "system audio recording" permission and an
  [`NSAudioCaptureUsageDescription`](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps)
  string, not Screen Recording. The older path is
  [ScreenCaptureKit's audio](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturesaudio)
  (macOS 13 and later), which hangs audio off a screen-capture stream and needs the Screen
  Recording permission.
- **Encode.** VideoToolbox compression sessions encode H.264 and HEVC in hardware on
  every Apple Silicon Mac, and offer Apple's software encoders too.
  [Constant bit rate](https://developer.apple.com/documentation/videotoolbox/kvtcompressionpropertykey_constantbitrate),
  which RTMP ingest wants, needs macOS 13. VideoToolbox has no AV1 encoder.
- **GPU frames.** ScreenCaptureKit, AVFoundation and VideoToolbox all pass frames as
  `CVPixelBuffer`s backed by an `IOSurface`. Metal can
  [wrap an IOSurface as a texture](https://developer.apple.com/documentation/metal/mtldevice/maketexture(descriptor:iosurface:plane:))
  without a copy, and wgpu's Metal backend can adopt a Metal texture through its
  wgpu-hal escape hatch ([ADR-0004](0004-wgpu-with-native-interop.md)).
- **Secrets.** The Keychain ([`SecItemAdd`](https://developer.apple.com/documentation/security/secitemadd(_:_:))
  and friends) is macOS's equivalent of the Windows Credential Manager store the Windows
  crate already uses for stream keys and tokens.
- **Webcams.** AVFoundation lists built-in, Continuity and
  [external](https://developer.apple.com/documentation/avfoundation/avcapturedevice/devicetype-swift.struct/external)
  (USB, macOS 14 and later) cameras, and decodes MJPEG itself. It needs the Camera
  permission.
- **Virtual camera.** macOS's only supported virtual-camera mechanism is a
  [Core Media I/O camera extension](https://developer.apple.com/documentation/coremediaio/creating-a-camera-extension-with-core-media-i-o)
  (macOS 12.3 and later): a system extension inside an app bundle in `/Applications`,
  with the System Extension and App Group entitlements, activated with an
  administrator's approval. Those entitlements need a paid developer account and
  signing.
- **Signing costs money.** Developer ID signing and notarisation need the
  [Apple Developer Program](https://developer.apple.com/programs/), $99 a year. Without
  them, Gatekeeper blocks a downloaded build, and
  [since macOS 15](https://developer.apple.com/news/?id=saqachfa) the user can't
  Control-click past it: they have to allow it in System Settings › Privacy & Security.
  TCC grants follow the code signature, so an ad-hoc-signed build loses its Screen
  Recording, Camera and Microphone grants each time it is rebuilt.
- **Hardware.** macOS 26 is
  [the last release for Intel Macs](https://petapixel.com/2025/06/10/macos-26-tahoe-is-the-end-of-the-road-for-intel-macs/).
  In practice Apple ships security updates for the three newest releases
  ([Apple security releases](https://support.apple.com/kb/HT1222)): today macOS
  15, 26, 27<!--fact:macos-security-updated-->. macOS 14 got no update in September
  2026's round.
- **CI.** A non-required `macos` job already runs `cargo test --workspace` on
  `macos-latest` after review ([ADR-0054](0054-platform-ci-after-review.md)). The
  repository is public, so
  [GitHub's standard runners are free](https://docs.github.com/en/billing/concepts/product-billing/github-actions);
  on a private repository a macOS minute costs about ten times a Linux one ($0.062 against
  $0.006). Hosted runners are virtual machines with no way to grant TCC permissions, so
  capture, camera and microphone can't be tested there.
- **Bindings.** [R-UNSAFE-004](../RULES.md#R-UNSAFE-004) prefers maintained bindings
  and names `objc2-*`. The [objc2](https://github.com/madsmtm/objc2) framework crates
  (Zlib OR Apache-2.0 OR MIT) are generated from Apple's SDK headers and published with
  the generated code, so building them needs no SDK, `bindgen` or libclang, and they have
  no C build step. wgpu's Metal backend already depends on `objc2`, `objc2-foundation`,
  `objc2-metal` and `objc2-core-foundation` at 0.3.2, so the Metal and IOSurface types
  the interop passes are the same types.

<a id="decision"></a>
## Decision
The decision is written with each maintainer question's recommended answer, marked
*(question N)*. A different answer changes the marked text before the ADR is accepted.

**1. macOS is a supported platform.** Its backends live in `jstream-platform-macos`
(layer 2, `unsafe` allowed by [R-UNSAFE-001](../RULES.md#R-UNSAFE-001)), behind the
`jstream-platform` traits. Code that calls Apple APIs is under `cfg(target_os = "macos")`;
elsewhere the crate builds only its platform-independent helpers, as the Windows and Linux
crates do.

**2. Support tiers.** *(questions 2 and 3)*

| Tier | What |
|---|---|
| Supported and tested | Apple Silicon (`aarch64-apple-darwin`) on the macOS releases Apple still ships security updates for (15, 26, 27<!--fact:macos-security-updated-->). |
| Best effort | macOS 14 (Sonoma) from 14.0, which Apple no longer patches; Intel Macs (`x86_64-apple-darwin`) on any release from 14 to 26, the last one Intel gets. Builds and bug fixes, no testing. |
| Not supported | Anything older than macOS 14.0. |

The floor is **macOS 14.0**: it has the system picker, USB cameras in AVFoundation and
everything below except process taps, which need 14.2. On 14.0 and 14.1, desktop and
application audio report a missing component whose remedy is the free 14.2 update. The
engine checks the running version at start-up and reports an older one as unsupported.
As with Windows and Linux, the patched releases are a fact the docs sweep re-checks.

**3. Screen and window capture use ScreenCaptureKit.**
- `Capture::targets` lists displays and windows from `SCShareableContent`, with stable
  IDs, so agents and the UI choose a target the same way as on Windows. The system picker
  is an optional convenience in the UI later; it is never the only way to pick.
- Before listing, the backend checks the Screen Recording permission. Without it, it asks
  once and reports a permission error that names the System Settings pane, instead of an
  empty list. Whether that is a new `PlatformError` variant is the implementing PR's call.
- JStream does not apply for Apple's restricted persistent-content-capture entitlement,
  so on macOS 15 and later the user sees the monthly re-confirmation.
- Frames are IOSurface-backed `CVPixelBuffer`s. The GPU path wraps the IOSurface as a
  Metal texture and hands it to wgpu (decision 8). The CPU path locks the buffer and
  copies it. "Show cursor" maps to `showsCursor`. Frame timestamps are already on the
  media clock (decision 5).

**4. Audio uses Core Audio.**
- Inputs and outputs go through the Core Audio HAL. Microphones need the Microphone
  permission.
- **Desktop audio** is a process tap on every process except JStream's own, and
  **application audio** (`AudioIo::processes`, `ProcessLoopback`) is a tap on the chosen
  process. Both need macOS 14.2 and the system audio recording permission, not Screen
  Recording.
- ScreenCaptureKit's audio is not used: it would tie audio to a screen-capture stream and
  to the Screen Recording permission, and taps cover every release from 14.2.

**5. Media clock.** The clock is `mach_absolute_time` ([ADR-0014](0014-media-clock.md)),
read in the 100 ns ticks the other backends use, through `mach_timebase_info` (1/1 on
Intel, 125/3 on Apple Silicon). Core Media's host clock, ScreenCaptureKit, AVFoundation
and Core Audio's host times are on that time line, so mapping one of their timestamps is
a multiplication.

**6. Video uses VideoToolbox.**
- H.264 and HEVC encode through `VTCompressionSession`, in hardware by default, with
  Apple's software encoder for `HardwarePreference::Avoid`. Rate control is CBR for RTMP.
  AV1 encode reports `Unsupported`.
- Its input is `CVPixelBuffer`s made from IOSurfaces, so the GPU encode target (decision
  8) reaches the encoder without a copy.
- Its output is converted from length-prefixed NAL units to Annex B, the engine's framing
  (`jstream-media`).
- Video decode (`VTDecompressionSession`) and the media-file source (AVFoundation's asset
  reader) are in scope, as later parts.

**7. Audio encode uses AudioToolbox.** AAC-LC through an `AudioConverter`, at 48 kHz
stereo and the bit rates YouTube's ingest asks for. So, unlike Linux, macOS can stream as
soon as decisions 3, 6 and 7 are built.

**8. GPU interop through IOSurface and Metal** ([ADR-0004](0004-wgpu-with-native-interop.md)).
The engine's wgpu device is on the Metal backend. Capture and webcam frames are imported by
wrapping their IOSurface as a Metal texture and adopting it into wgpu through wgpu-hal.
Encode targets are IOSurfaces the platform crate allocates, which wgpu renders into and
VideoToolbox reads. Synchronisation (a command buffer's completion, or a shared event) is
specified in [ARCH › GPU pipeline](../ARCHITECTURE.md#gpu-pipeline) once a spike on a
real Mac has proven it, as spike S1 did on Windows. Until then the crate offers CPU
frames only.

**9. Stream keys and tokens go in the Keychain.** `StreamKeyStore` and `OAuthTokenStore`
are generic-password items in the user's login keychain, through the Security framework.
The posture is the Windows one: secrets stay on the machine, protected by the user's
login. Unsigned builds use the login keychain rather than the data-protection keychain,
which needs a signed app with an access-group entitlement. The cost is that macOS asks
the user to allow access again after each rebuild.

**10. Webcams use AVFoundation.** Built-in, Continuity and external cameras through a
`AVCaptureDevice` discovery session. AVFoundation decodes MJPEG itself, so the backend
receives decoded, IOSurface-backed frames. Cameras need the Camera permission.

**11. Permission strings.** Camera, microphone and system audio recording prompts need
usage strings in the responsible app's `Info.plist`. The daemon embeds one in its binary
(a linker `__info_plist` section, no app bundle needed). A process started from a terminal
is attributed to the terminal app by TCC, so the runbook says to grant the terminal or
run the bundled app.

**12. Bindings: the objc2 framework crates.** This is an agent-tier choice, made here:
- The crate calls Apple's frameworks through `objc2` and its framework crates
  (`objc2-screen-capture-kit`, `-core-media`, `-core-video`, `-video-toolbox`,
  `-audio-toolbox`, `-core-audio`, `-av-foundation`, `-io-surface`, `-metal`,
  `-security`), as R-UNSAFE-004 prefers. The Mach time calls come from `mach2`, which is
  already in the tree.
- They are pinned to the versions wgpu's Metal backend uses, so there is one copy, and
  added with `default-features = false` and only the framework features the crate calls.
  The `objc2` `exception` feature is never enabled, because it compiles C.
- Hand-written `extern` blocks are a last resort, for a symbol no maintained crate
  declares, each with a `SAFETY` comment, as the Linux crate does.
- Because the crates carry their generated code and link frameworks with `#[link]`, the
  crate type-checks from Linux with `cargo clippy --target aarch64-apple-darwin`. Linking
  still needs a Mac.
- Hand-written bindings throughout were rejected: ScreenCaptureKit, AVFoundation and
  VideoToolbox are Objective-C and block-based APIs, and writing their message sends and
  blocks by hand is more `unsafe` code for no gain. Single-framework crates such as
  `screencapturekit` were rejected because they bring their own bindings of types wgpu
  already takes from objc2.

**13. Out of scope for now**, each its own later decision:
- the **virtual camera**, a Core Media I/O camera extension, which needs signing and
  its own ADR *(question 5)*. `Backends::vcam` is `None` on macOS until then;
- the **Syphon receiver**. Syphon's framework is Objective-C under BSD-3-Clause, so its
  licence is allowed, and it shares frames as IOSurfaces, which decision 8 already
  imports. It would be a bundled non-media dependency or a reimplementation of its
  protocol. It is its own roadmap item "when wanted", like the Spout2 receiver;
- the **browser source** on macOS (CEF's helper-app bundles) and the desktop app (Tauri
  on WKWebView): the macOS host for M3 is the daemon with the web UI in a browser, as on
  Linux;
- **packaging**, an app bundle and disk image, and its signing and notarisation
  *(question 1)*.

**14. CI.** *(question 4)*
- The `macos` job becomes a required check where the Windows jobs are: on PRs into `main`,
  wave landings included, after review ([ADR-0054](0054-platform-ci-after-review.md),
  [ADR-0055](0055-wave-branches.md)), and on `main`. It also runs clippy on
  `jstream-platform-macos` for the native target.
- The Linux `clippy` job also checks `jstream-platform-macos` for `aarch64-apple-darwin`,
  so a macOS-only compile error shows before review.
- Tests that need permissions, a camera or the hardware encoder sit behind the crate's
  `hw-tests` feature and `#[ignore]`, and run by hand on a Mac.
- The workflow edits are a governance change ([R-GOV-001](../RULES.md#R-GOV-001)) and land
  in their own PR after acceptance.

<a id="questions"></a>
## Questions for the maintainer
Accepting this ADR as written accepts decisions 1 to 14 with the recommended answers
below.

1. **Pay for the Apple Developer Program ($99 a year) for signing and notarisation?**
   Options: now, later, or never. Without it, builds are ad-hoc signed: Gatekeeper blocks
   a downloaded copy until the user allows it in System Settings, and TCC grants reset on
   every rebuild. With it, JStream can ship a notarised app, and the camera extension
   becomes possible.
   **Recommended: later.** Unsigned developer builds until there is a release to ship;
   the camera extension needs the account anyway, so the two decisions go together.
2. **Minimum macOS version?** Options: 13, 14 or 15.
   **Recommended: 14 (Sonoma).** It has the system picker and USB cameras in
   AVFoundation, and 14.2, a free update on the same Macs, adds process audio taps. 13
   would need ScreenCaptureKit audio as a second audio path; 15 would drop Macs that
   can't update past 14 for nothing JStream uses.
3. **Which Macs?** Options: Apple Silicon only, or Apple Silicon and Intel.
   **Recommended: Apple Silicon only** as the supported and tested tier, with Intel
   builds best effort and untested. Intel Macs stop at macOS 26, and every Apple Silicon
   Mac has the hardware encoders this ADR relies on.
4. **macOS CI?** Options: yes or no.
   **Recommended: yes**, on PRs into `main` and on wave landings only, like the Windows
   jobs and Linux's full set in ADR-0060. It is free while the repository is public; on
   a private one a macOS minute costs about ten times a Linux minute.
5. **The virtual camera (a CMIO camera extension)?** Options: now or later.
   **Recommended: later**, in its own ADR once signing (question 1) exists, because the
   extension can't be installed without it.

<a id="consequences"></a>
## Consequences
- **macOS can capture, mix, preview, play audio and stream** once the crate's parts land,
  because the OS has every codec. It is the one M3 platform with no licence work.
- **Agents can drive capture** on macOS as on Windows, by target ID. The cost is the
  Screen Recording grant and, on macOS 15 and later, the monthly re-confirmation.
- **Unsigned builds are rough to use.** Gatekeeper's Settings detour, permission grants
  lost on rebuild, and Keychain prompts after rebuild, until question 1 is answered "now".
- **Only Apple Silicon is tested,** and only by hand until a Mac with permissions runs
  the hardware tests. Intel and macOS 14 can break silently.
- **The crate can be type-checked from Linux** through the objc2 crates, so builders
  without a Mac can work on it, but linking and every test that touches a framework need
  macOS.
- **Edits that follow acceptance** (none are made while this ADR is proposed): ROADMAP's
  M3 macOS line points here; ARCHITECTURE's crate-map row for `jstream-platform-macos`
  names this ADR; the README's platform table marks macOS "M3, per ADR-0063"; the CI
  change in decision 14.
