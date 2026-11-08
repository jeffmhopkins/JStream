<!-- @doc
id = "ADR-INDEX"
title = "Architecture decision records"
kind = "generated"
status = "active"
-->

# Architecture decision records

Every decision recorded in `docs/adr/`, with its status. This file is generated from each
ADR's `@doc` block by `cargo xtask docs generate`; never edit it by hand
([DOCS › Generated content](../DOCS.md#generated)). New ADRs start from
[`TEMPLATE.md`](TEMPLATE.md).

<!-- @generated begin adr-index -->
| ADR | Title | Status | Tier | Superseded by |
|---|---|---|---|---|
| [ADR-0001](0001-library-first-engine.md) | Library-first engine with thin hosts | accepted | — |  |
| [ADR-0002](0002-web-ui-everywhere.md) | One web UI for desktop and browser | superseded | — | [ADR-0045](0045-leptos-web-ui.md) |
| [ADR-0003](0003-media-stack-and-codecs.md) | Rust media stack on OS codecs, no FFmpeg | superseded | — | [ADR-0031](0031-source-types-and-decoding-scope.md) |
| [ADR-0004](0004-wgpu-with-native-interop.md) | wgpu compositor with native interop | accepted | — |  |
| [ADR-0005](0005-plugins-wgsl-and-wasm.md) | Extensibility through WGSL effects and WASM plugins | superseded | — | [ADR-0035](0035-clap-plugins-out-of-process.md) |
| [ADR-0006](0006-mcp-full-access.md) | MCP agents get full, authenticated access | accepted | — |  |
| [ADR-0007](0007-bus-matrix-audio.md) | Bus-matrix audio and scene-independent outputs | superseded | — | [ADR-0028](0028-audio-model-v2.md) |
| [ADR-0008](0008-windows-support-policy.md) | Windows support policy | accepted | — |  |
| [ADR-0009](0009-youtube-rtmps-and-api.md) | YouTube via RTMPS plus optional Live API | superseded | — | [ADR-0034](0034-youtube-interaction-scope-v2.md) |
| [ADR-0010](0010-headless-first-milestone.md) | Build the headless engine and MCP before the GUI | accepted | — |  |
| [ADR-0011](0011-docs-as-checked-graph.md) | Documentation as a hash-checked graph | superseded | — | [ADR-0016](0016-docs-as-checked-graph-v2.md) |
| [ADR-0012](0012-virtual-camera-windows.md) | Windows virtual camera as a COM media source | superseded | — | [ADR-0020](0020-portable-windows-distribution.md) |
| [ADR-0013](0013-command-model.md) | Command model: edits, actions, revisions, per-client undo | accepted | — |  |
| [ADR-0014](0014-media-clock.md) | Monotonic media clock with adaptive resampling | accepted | — |  |
| [ADR-0015](0015-review-and-governance.md) | Agent review gate and governance with a single GitHub account | superseded | — | [ADR-0019](0019-review-gate-v2.md) |
| [ADR-0016](0016-docs-as-checked-graph-v2.md) | Documentation as a hash-checked graph (v2: metadata in comments) | accepted | — |  |
| [ADR-0017](0017-adr-decision-tiers.md) | Decision tiers: which ADRs need the maintainer | superseded | maintainer | [ADR-0043](0043-narrow-maintainer-tier.md) |
| [ADR-0018](0018-ci-supply-chain-policy.md) | CI supply-chain policy: pinned actions, Dependabot, current runtimes | superseded | maintainer | [ADR-0056](0056-ci-supply-chain-policy-v2.md) |
| [ADR-0019](0019-review-gate-v2.md) | Agent review gate v2: owner-only, round-aware verdicts | accepted | maintainer |  |
| [ADR-0020](0020-portable-windows-distribution.md) | Portable Windows distribution: run the exe, no installer | superseded | maintainer | [ADR-0066](0066-portable-windows-distribution-v2.md) |
| [ADR-0021](0021-browser-source.md) | Browser (web page) source | accepted | maintainer |  |
| [ADR-0022](0022-item-transform-and-canvas.md) | Item transform and canvas coordinates | accepted | agent |  |
| [ADR-0023](0023-placements-references-and-clones.md) | Placements, references and clones | accepted | maintainer |  |
| [ADR-0024](0024-scene-structure.md) | Scene structure: groups, keyframe scenes, folders and organisation | accepted | maintainer |  |
| [ADR-0025](0025-typed-settings-kind-descriptors.md) | Typed settings through kind descriptors | accepted | agent |  |
| [ADR-0026](0026-source-filter-chains.md) | Source filter chains and the filter edits | accepted | agent |  |
| [ADR-0027](0027-program-master-and-output-chains.md) | Program master, per-output chains and the privacy screen | superseded | maintainer | [ADR-0080](0080-program-master-and-output-chains-v2.md) |
| [ADR-0028](0028-audio-model-v2.md) | Audio model v2: bus matrix with faders, scene-following audio, keys, meters | accepted | maintainer |  |
| [ADR-0029](0029-transitions.md) | Transitions | accepted | maintainer |  |
| [ADR-0030](0030-studio-state-and-scene-switching.md) | Studio state and scene switching | accepted | agent |  |
| [ADR-0031](0031-source-types-and-decoding-scope.md) | Source types for v1 and decoding scope | superseded | maintainer | [ADR-0061](0061-source-types-and-decoding-scope-v2.md) |
| [ADR-0032](0032-live-edits-and-undo.md) | Live parameter edits and undo on the wire | accepted | agent |  |
| [ADR-0034](0034-youtube-interaction-scope-v2.md) | YouTube interaction scope v2 | superseded | maintainer | [ADR-0041](0041-youtube-interaction-scope-v3.md) |
| [ADR-0035](0035-clap-plugins-out-of-process.md) | CLAP audio plugins, out of process | accepted | maintainer |  |
| [ADR-0036](0036-mcp-events-polling-tool.md) | Engine events over MCP through a polling tool | accepted | agent |  |
| [ADR-0037](0037-kind-descriptors-for-transitions-and-plugins.md) | Kind descriptors for transitions and CLAP plugins | accepted | agent |  |
| [ADR-0038](0038-adr-status-banner-and-index.md) | Generated ADR status banners and index | accepted | maintainer |  |
| [ADR-0039](0039-hash-ignores-metadata-and-row-dependencies.md) | Doc hashes ignore metadata blocks; row-level crate-map dependencies | accepted | maintainer |  |
| [ADR-0040](0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md) | Scene structure: orphaned overrides, shared groups and deleted hotkey targets | accepted | maintainer |  |
| [ADR-0041](0041-youtube-interaction-scope-v3.md) | YouTube interaction scope v3: one sign-in | superseded | maintainer | [ADR-0051](0051-youtube-without-a-required-oauth-client.md) |
| [ADR-0042](0042-narrow-hot-architecture-sections.md) | Narrow the hot architecture sections: manifest views, app entry points, threading model | accepted | maintainer |  |
| [ADR-0043](0043-narrow-maintainer-tier.md) | Decision tiers v2: the maintainer decides product-level calls only | accepted | maintainer |  |
| [ADR-0044](0044-pairing-remote-browsers.md) | Pairing remote browsers with a one-time code | accepted | maintainer |  |
| [ADR-0045](0045-leptos-web-ui.md) | One web UI on Leptos, confirmed for M2 | accepted | agent |  |
| [ADR-0046](0046-browser-crate-and-helper.md) | Browser sources live in their own crate, with a helper exe | accepted | agent |  |
| [ADR-0047](0047-browser-pairing-page-and-device-cookie.md) | A browser pairs on a token-free page and keeps its device token in a cookie | accepted | maintainer |  |
| [ADR-0048](0048-sandboxed-browser-helper.md) | Browser pages run in a sandboxed helper process | accepted | agent |  |
| [ADR-0049](0049-keyframe-scene-model-and-evaluation.md) | Keyframe scenes: the override model, live values and switching between keyframes | accepted | agent |  |
| [ADR-0050](0050-3d-transforms-rendering.md) | Drawing 3D item transforms | accepted | agent |  |
| [ADR-0051](0051-youtube-without-a-required-oauth-client.md) | YouTube v4: no required OAuth client | superseded | maintainer | [ADR-0058](0058-youtube-sign-in-with-a-jstream-client.md) |
| [ADR-0052](0052-transition-mechanics-and-studio-preview.md) | Transition mechanics as built, and the studio's preview scene | accepted | agent |  |
| [ADR-0053](0053-per-input-verification-ledger.md) | Per-input verification stamps in one ledger file | accepted | agent |  |
| [ADR-0054](0054-platform-ci-after-review.md) | Run the Windows, macOS and end-to-end CI jobs after review | accepted | agent |  |
| [ADR-0055](0055-wave-branches.md) | Wave branches: PRs batch into a wave, and only the wave's PR into main runs full CI | accepted | agent |  |
| [ADR-0056](0056-ci-supply-chain-policy-v2.md) | CI supply-chain policy v2: Dependabot also updates the end-to-end tests' npm packages | accepted | agent |  |
| [ADR-0057](0057-server-answers-credential-calls.md) | The server answers the credential calls, and actions may answer with data | accepted | agent |  |
| [ADR-0058](0058-youtube-sign-in-with-a-jstream-client.md) | YouTube sign-in with JStream's own OAuth client, for people only | accepted | maintainer |  |
| [ADR-0059](0059-mcp-annotations-and-remote-text.md) | MCP tool annotations, and third-party text marked as data | accepted | agent |  |
| [ADR-0060](0060-linux-platform-support.md) | Linux platform support: portal and PipeWire capture, VA-API encode | accepted | maintainer |  |
| [ADR-0061](0061-source-types-and-decoding-scope-v2.md) | Source types and decoding scope v2: Linux's software H.264 and AAC | accepted | maintainer |  |
| [ADR-0062](0062-aac-lc-encoder-patent-check.md) | AAC-LC encoder patent check: may JStream ship its own encoder? | accepted | maintainer |  |
| [ADR-0063](0063-macos-platform-support.md) | macOS platform support: ScreenCaptureKit, Core Audio taps, VideoToolbox | accepted | maintainer |  |
| [ADR-0064](0064-nested-scene-folders.md) | Nested scene folders | accepted | agent |  |
| [ADR-0065](0065-scene-chains-and-item-effects.md) | Scene filter chains and item effects | accepted | agent |  |
| [ADR-0066](0066-portable-windows-distribution-v2.md) | Portable Windows distribution v2: an optional virtual camera, registered from an administrators-only copy | accepted | maintainer |  |
| [ADR-0067](0067-shared-crash-reports.md) | Crash reports live in one crate, jstream-crash, with no global state | accepted | agent |  |
| [ADR-0068](0068-item-effects-in-v1.md) | Item effects are in v1 | accepted | maintainer |  |
| [ADR-0069](0069-wasm-plugin-limits-and-capabilities.md) | WASM plugin resource limits and the Control calls a plugin may make | accepted | maintainer |  |
| [ADR-0070](0070-wave-prs-run-linux-tests.md) | PRs into a wave run the Linux tests | accepted | agent |  |
| [ADR-0071](0071-linux-secret-store.md) | Linux secret store: the Secret Service, else a 0600 file | accepted | maintainer |  |
| [ADR-0072](0072-linux-media-file-decoding.md) | Linux media-file decoding: VA-API and pure Rust first, the system's FFmpeg for the gaps | accepted | maintainer |  |
| [ADR-0073](0073-sources-drawn-at-their-shown-size.md) | Sources drawn at the size they are shown | accepted | agent |  |
| [ADR-0074](0074-too-large-requests.md) | A too_large error for oversized requests | accepted | maintainer |  |
| [ADR-0075](0075-wasm-plugin-host-in-the-engine.md) | The WASM plugin host runs in the engine, and a plugin's package | accepted | agent |  |
| [ADR-0076](0076-saved-wasm-plugins-and-manifest-requests.md) | Saved WASM plugins, what a manifest asks for, and updates that need a grant | accepted | agent |  |
| [ADR-0077](0077-bundle-ibm-plex-fonts.md) | Bundle the IBM Plex fonts with the web UI under the SIL OFL 1.1 | accepted | maintainer |  |
| [ADR-0078](0078-scenes-carry-labels.md) | Scenes carry labels | accepted | agent |  |
| [ADR-0079](0079-text-density-bounded-by-pixels.md) | Text density bounded by its frame's pixels, not at 8 | accepted | agent |  |
| [ADR-0080](0080-program-master-and-output-chains-v2.md) | Program master and per-output chains v2: no privacy screen | accepted | maintainer |  |
| [ADR-0081](0081-effects-pair-by-their-look.md) | Items pair in a Move when their enabled effects look the same | accepted | agent |  |
| [ADR-0082](0082-blend-modes-drawn.md) | Blend modes: how each is drawn, and in which colour space | accepted | agent |  |
| [ADR-0083](0083-activity-log-over-the-protocol.md) | The activity log over the protocol | accepted | agent |  |
| [ADR-0084](0084-ui-settings-kept-by-the-server.md) | The server keeps the UI's settings, so a layout follows the person | accepted | agent |  |
| [ADR-0085](0085-capture-target-names-marked-as-remote-text.md) | Capture target names are a second field marked as remote text by its description | accepted | agent |  |
| [ADR-0086](0086-master-clap-filter-one-host-per-output-bus.md) | A master-chain CLAP filter runs one plugin host per bus that feeds an output | accepted | agent |  |
| [ADR-0087](0087-effect-quarantine-after-gpu-device-loss.md) | A GPU device loss quarantines the effect-pack kinds drawn before it, until a person re-enables them | accepted | agent |  |
| [ADR-0088](0088-keyframe-scenes-override-item-effects.md) | Keyframe scenes override an item's effects | accepted | agent |  |
| [ADR-0089](0089-scene-options-and-nested-audio.md) | Scene options: a scene's audio left out of nesting, and its background | accepted | agent |  |
| [ADR-0090](0090-a-grant-acknowledges-the-request-its-client-saw.md) | A plugin grant acknowledges the request its client was shown | accepted | agent |  |
| [ADR-0091](0091-source-settings-as-live-values.md) | A source's live settings are live values, so a Properties dialog applies as you edit | accepted | agent |  |
| [ADR-0092](0092-installed-fonts-query.md) | The engine lists the installed fonts, so a font is picked by name | accepted | agent |  |
| [ADR-0093](0093-shader-sources-from-effect-packs.md) | Shader sources from effect packs | accepted | agent |  |
| [ADR-0094](0094-scene-cameras.md) | Scene cameras: items in one 3D space | accepted | agent |  |
| [ADR-0095](0095-browser-helper-on-linux.md) | Browser sources on Linux: the helper paints in software and hands the engine CPU frames | accepted | maintainer |  |
| [ADR-0096](0096-item-lock-and-png-preview-frames.md) | Items can be locked, and a preview stream can send PNG for a screenshot | accepted | agent |  |
| [ADR-0097](0097-agents-write-effect-packs.md) | Agents write effect packs over the protocol and MCP | accepted | maintainer |  |
| [ADR-0098](0098-mcp-destructive-hint-for-what-undo-cant-take-back.md) | MCP's destructiveHint marks only what undo can't take back | accepted | agent |  |
| [ADR-0099](0099-native-motion-modifiers-and-timelines.md) | Native motion: one animation clock, motion modifiers and timelines | accepted | maintainer |  |
| [ADR-0100](0100-audio-reactive-motion-modifiers.md) | Audio-reactive bindings are a motion modifier kind that follows a bus or a source | accepted | agent |  |
| [ADR-0101](0101-item-show-hide-actions.md) | Item show, hide and toggle actions, with the item's own effects | accepted | agent |  |
| [ADR-0102](0102-page-api-for-browser-sources.md) | Browser source pages can opt in to reading the engine through window.jstream | accepted | maintainer |  |
| [ADR-0103](0103-per-item-performance-profile.md) | A performance profile per item, source and thread, readable over the protocol and MCP | accepted | agent |  |
| [ADR-0104](0104-network-listener-over-tls.md) | Devices on the network reach JStream over TLS, with a self-signed certificate they pin | accepted | maintainer |  |
| [ADR-0105](0105-browser-matching-srgb-blend-modes.md) | Blend modes that mix in sRGB-encoded values, to match a browser | superseded | agent | [ADR-0126](0126-one-set-of-blend-modes-and-a-blending-space.md) |
| [ADR-0106](0106-item-effects-in-canvas-space.md) | An item's effects can run over the canvas, after it is placed | accepted | agent |  |
| [ADR-0107](0107-3d-models.md) | 3D models: meshes, materials and live textures in a scene | accepted | agent |  |
| [ADR-0108](0108-camera-depth-of-field.md) | Depth of field for scene cameras | accepted | agent |  |
| [ADR-0109](0109-review-pass-carries-across-base-merges.md) | A review pass carries across merges of the base branch | accepted | maintainer |  |
| [ADR-0110](0110-mcp-values-tool-naming.md) | Shared values reach MCP through one values tool | accepted | agent |  |
| [ADR-0111](0111-camera-up-at-the-poles.md) | A scene camera's up at either pole | accepted | agent |  |
| [ADR-0112](0112-assignable-camera-zoom-bindings.md) | Assignable zoom bindings for a follow-cursor camera, heard while another app has the focus | accepted | maintainer |  |
| [ADR-0113](0113-camera-motion-and-follow-pointer.md) | Camera motion, and a scene camera that follows the pointer over a capture | accepted | agent |  |
| [ADR-0114](0114-browsers-keep-no-certificate-pin.md) | A paired browser keeps no certificate pin, so a new warning on its host means stop | superseded | agent | [ADR-0130](0130-browsers-keep-no-certificate-pin-v2.md) |
| [ADR-0115](0115-timelines-on-the-wire.md) | Timelines on the wire: global IDs, inert cues, studio-state playback and source.restart | accepted | agent |  |
| [ADR-0116](0116-keyframe-tracks.md) | Keyframe tracks: a placed scene switches its own keyframe, as studio state, with a Move | accepted | agent |  |
| [ADR-0117](0117-real-time-video-thread-model.md) | Real-time video thread model: a compositor-only render thread, producers that hand it GPU-ready frames, and one always-on stage | accepted | agent |  |
| [ADR-0118](0118-placed-keyframes-on-the-graph.md) | Placed keyframes live on the engine's graph, not on a copy rebuilt each batch | accepted | agent |  |
| [ADR-0119](0119-base-frame-rate-option.md) | The base frame rate stays 30 fps, and 60 fps is an option | accepted | agent |  |
| [ADR-0120](0120-follow-pointer-is-a-filter.md) | Follow pointer is a filter on a capture, with a safe margin | accepted | agent |  |
| [ADR-0121](0121-projects-open-new-and-save-as.md) | Projects: open, new and save as, over the protocol | accepted | agent |  |
| [ADR-0122](0122-controller-input-midi-and-one-binding-system.md) | Controller input: MIDI and JTunnel, one binding system for every trigger | accepted | agent |  |
| [ADR-0123](0123-lan-inputs-jtunnel-and-visca.md) | LAN inputs without TLS: JTunnel senders and VISCA, opt-in and accepted one peer at a time | accepted | maintainer |  |
| [ADR-0124](0124-page-api-read-review.md) | What a page may read: one checkbox and one warning per kind of data | accepted | maintainer |  |
| [ADR-0125](0125-item-effects-layer-grows-where-it-is-needed.md) | An item's effects layer grows at the first effect that draws past the box | accepted | agent |  |
| [ADR-0126](0126-one-set-of-blend-modes-and-a-blending-space.md) | One set of blend modes, and a project setting for the space they mix in | accepted | agent |  |
| [ADR-0127](0127-moves-interpolate-effect-settings.md) | A Move blends the settings of effects that differ only in their settings | accepted | agent |  |
| [ADR-0128](0128-moves-between-2d-and-3d-glide.md) | A Move between a 2D side and a 3D one glides through the framing camera | accepted | agent |  |
| [ADR-0129](0129-keyframe-timing-and-more-easings.md) | A keyframe times its items in moves into it, and three more easings | accepted | agent |  |
| [ADR-0130](0130-browsers-keep-no-certificate-pin-v2.md) | A paired browser keeps no certificate pin, so a new warning on its host means checking the fingerprint again | accepted | agent |  |
| [ADR-0131](0131-ask-before-a-new-agent-connects.md) | The desktop app asks the person before a new agent's first connection | accepted | agent |  |
| [ADR-0132](0132-bench-gates-views-and-quality-probe.md) | Release gates from the profile: the client's views report back, a quality probe, and the bench's gates | accepted | agent |  |
| [ADR-0133](0133-labels-on-hotkeys-and-timelines.md) | Labels on hotkeys and timelines, and a label's members | accepted | agent |  |
| [ADR-0134](0134-html-pages-load-from-an-origin-of-their-own.md) | A page given as HTML loads from an origin of its own | accepted | agent |  |
| [ADR-0136](0136-an-effects-outset-follows-its-settings.md) | An effect's outset follows its settings | accepted | agent |  |
| [ADR-0137](0137-midi-visualization-sources.md) | MIDI visualization sources draw a lock-free per-input state on the CPU | accepted | agent |  |
| [ADR-0141](0141-desktop-app-on-linux.md) | The desktop app on Linux: Tauri on WebKitGTK, behind a feature | accepted | maintainer |  |
| [ADR-0142](0142-feeding-the-midi-sources-and-their-beat.md) | The engine feeds the MIDI sources on the media clock, with each input's tempo | accepted | agent |  |
<!-- @generated end adr-index -->
