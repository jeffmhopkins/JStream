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
| [ADR-0018](0018-ci-supply-chain-policy.md) | CI supply-chain policy: pinned actions, Dependabot, current runtimes | accepted | maintainer |  |
| [ADR-0019](0019-review-gate-v2.md) | Agent review gate v2: owner-only, round-aware verdicts | accepted | maintainer |  |
| [ADR-0020](0020-portable-windows-distribution.md) | Portable Windows distribution: run the exe, no installer | accepted | maintainer |  |
| [ADR-0021](0021-browser-source.md) | Browser (web page) source | accepted | maintainer |  |
| [ADR-0022](0022-item-transform-and-canvas.md) | Item transform and canvas coordinates | accepted | agent |  |
| [ADR-0023](0023-placements-references-and-clones.md) | Placements, references and clones | accepted | maintainer |  |
| [ADR-0024](0024-scene-structure.md) | Scene structure: groups, keyframe scenes, folders and organisation | accepted | maintainer |  |
| [ADR-0025](0025-typed-settings-kind-descriptors.md) | Typed settings through kind descriptors | accepted | agent |  |
| [ADR-0027](0027-program-master-and-output-chains.md) | Program master, per-output chains and the privacy screen | accepted | maintainer |  |
| [ADR-0028](0028-audio-model-v2.md) | Audio model v2: bus matrix with faders, scene-following audio, keys, meters | accepted | maintainer |  |
| [ADR-0029](0029-transitions.md) | Transitions | accepted | maintainer |  |
| [ADR-0030](0030-studio-state-and-scene-switching.md) | Studio state and scene switching | accepted | agent |  |
| [ADR-0031](0031-source-types-and-decoding-scope.md) | Source types for v1 and decoding scope | accepted | maintainer |  |
| [ADR-0032](0032-live-edits-and-undo.md) | Live parameter edits and undo on the wire | accepted | agent |  |
| [ADR-0034](0034-youtube-interaction-scope-v2.md) | YouTube interaction scope v2 | superseded | maintainer | [ADR-0041](0041-youtube-interaction-scope-v3.md) |
| [ADR-0035](0035-clap-plugins-out-of-process.md) | CLAP audio plugins, out of process | accepted | maintainer |  |
| [ADR-0036](0036-mcp-events-polling-tool.md) | Engine events over MCP through a polling tool | accepted | agent |  |
| [ADR-0037](0037-kind-descriptors-for-transitions-and-plugins.md) | Kind descriptors for transitions and CLAP plugins | accepted | agent |  |
| [ADR-0038](0038-adr-status-banner-and-index.md) | Generated ADR status banners and index | accepted | maintainer |  |
| [ADR-0039](0039-hash-ignores-metadata-and-row-dependencies.md) | Doc hashes ignore metadata blocks; row-level crate-map dependencies | accepted | maintainer |  |
| [ADR-0040](0040-scene-structure-overrides-shared-groups-and-hotkey-targets.md) | Scene structure: orphaned overrides, shared groups and deleted hotkey targets | accepted | maintainer |  |
| [ADR-0041](0041-youtube-interaction-scope-v3.md) | YouTube interaction scope v3: one sign-in | accepted | maintainer |  |
| [ADR-0042](0042-narrow-hot-architecture-sections.md) | Narrow the hot architecture sections: manifest views, app entry points, threading model | accepted | maintainer |  |
| [ADR-0043](0043-narrow-maintainer-tier.md) | Decision tiers v2: the maintainer decides product-level calls only | accepted | maintainer |  |
| [ADR-0044](0044-pairing-remote-browsers.md) | Pairing remote browsers with a one-time code | accepted | maintainer |  |
| [ADR-0045](0045-leptos-web-ui.md) | One web UI on Leptos, confirmed for M2 | accepted | agent |  |
| [ADR-0047](0047-browser-pairing-page-and-device-cookie.md) | A browser pairs on a token-free page and keeps its device token in a cookie | accepted | maintainer |  |
<!-- @generated end adr-index -->
