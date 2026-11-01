<!-- @doc
id = "CRATE-jstream-effects"
title = "jstream-effects"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-effects

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-effects/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-effects", "crates/jstream-render/README.md#filters", "docs/adr/0025-typed-settings-kind-descriptors.md", "docs/adr/0035-clap-plugins-out-of-process.md", "docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md"]
-->

Layer-3 loader for third-party **WGSL effect packs**, the data-only extension point of
[ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md) (kept from ADR-0005). A
pack is a folder holding an `effect.toml` manifest and the `.wgsl` files it names. Each
effect becomes a `video_filter` kind with the ID `<pack>/<effect>`, so a pack never
shadows a built-in kind ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)).
It depends on [`jstream-protocol`](../jstream-protocol/README.md) for kind descriptors and
settings, [`jstream-render`](../jstream-render/README.md#filters) for the filter harness
that draws an effect, `naga` (the exact version wgpu pins, with only `wgsl-in`) for WGSL, `toml`,
`serde`/`serde_json`, `thiserror` and `tracing`. It opens no GPU device of its own, starts
no threads and has no `unsafe`.

**The manifest.** A `[pack]` table (`id`, `name`, optional `description`) and one or more
`[[effect]]` tables (`id`, `name`, `version`, `shader`, optional `renames`), each with its
`[[effect.param]]` list in descriptor form: `id`, `label`, `type`, `default`, the type's
constraints, and the optional `description`, `group` and `advanced` hints. The crate
docs show a full example.
- Pack and effect IDs use `a-z`, `0-9` and `-`. Parameter IDs are WGSL field names
  (`a-z`, `0-9`, single `_`, starting with a letter). IDs are at most 32 characters.
- `clap` is not a pack ID: `clap/…` belongs to CLAP plugin kinds
  ([ADR-0037](../../docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md) §2).
- Parameter types are the ones a shader can read: `bool`, `int` (with `min` and `max`
  inside `i32`), `float`, `enum` and `color`. Text, files and references have no uniform
  form. Every pack parameter is `live`. Each default must pass the protocol's own check.
- A shader path ends in `.wgsl` and follows the protocol's asset-path rules under the
  `collection` root ([`jstream-protocol`](../jstream-protocol/README.md#design), the
  Rejections bullet): relative, with `/` separators, and no empty or `..` segment, drive
  letter, leading `/`, backslash or `:`, and its last component is not a DOS device name
  (`con.wgsl` and `shaders/nul.wgsl` are refused). A symlink out of the pack folder is
  refused. The manifest is read up to 64 KiB, a shader up to
  256 KiB, and a pack holds at most 64 effects of at most 64 parameters each.
- **Versions** follow ADR-0025: `renames = { old = "new" }` comes with a version above 1,
  and `Effect::migrate` moves an older version's values to the new IDs. Settings from a
  newer version are not migrated, so the engine keeps them verbatim. Any other breaking
  change is a new effect ID.

**The shader contract** (`shader`). The host, the render filter harness, compiles its
prelude, then the generated `Params` struct (one field per parameter in declaration
order, at group 0, binding 4, as `p`), then the effect's file. `bool` and `enum` are
`u32` (an enum is its option's index), `int` is `i32`, `float` is `f32`, and `color` is a
`vec4<f32>` of linear-light RGB with straight alpha. The effect defines
`@fragment fn fs(in: VsOut) -> @location(0) vec4<f32>` and may add functions, constants
and `var<private>` globals. It may not add bindings, immediates, overrides (the host has
no value for them) or entry points. Its memory is capped at `MAX_VALUE_BYTES` (16 KiB) in
three ways: any one value it names, declares or constructs (`array<f32, N>()` included),
all its `var<private>` globals together, and each function's local variables together. A
huge array is a cheap way to make a driver spill to scratch memory or stall while
compiling (#349).
`Pack::load` validates the whole text with naga (all validation flags, no optional
capabilities, so a pack runs on every adapter) and checks that contract. The prelude is
validated on its own first, so a host mistake is named as one. Errors carry the
line in the effect's own file. `Effect::uniforms` writes the `Params` bytes for a
settings map, reading a missing or invalid value as its default. A test checks the
layout against the offsets naga computes.

**Drawing.** `Effect` implements render's `ExternalFilter` (label: the kind ID), and
`Effect::video_filter(settings)` makes the `VideoFilter` a video chain keeps;
`jstream-core`'s chain runtime draws pack kinds with it once their pack is loaded. The
host's prelude is `jstream_render::FILTER_PRELUDE`. A filter made from a reloaded effect
draws the new WGSL, because the render side rebuilds a label's pipeline when its source
changes. A test draws an effect on the test adapter, changes its settings in place, and
draws it again after a reload.

**Hot reload** (`PackWatcher`). The owner polls (`DEFAULT_POLL_INTERVAL`, 500 ms). Each
poll lists the sub-folders of the packs folder that hold an `effect.toml`. It stamps
each folder's manifest and the shader files that manifest named when last read, by
modification time and size (or absence, so a named shader that appears later is seen),
and reloads a folder whose stamp changed. Other files in the folder (a `.git`, notes)
are never read. A poll reports kinds added, changed or removed, for `kind.changed`, and
the errors of packs that failed. A failed reload keeps the last good version live and is
reported once. A second folder with a pack ID that is already loaded is refused, and
retried in the poll that frees the ID (the first folder removed, or its pack renamed).
Polling needs no dependency and behaves the same on every file system. The cost is the
poll's latency, and an edit that keeps a file's size within the file system's timestamp
resolution is seen on the next change.

`jstream-core` runs the watcher (`Engine::watch_effect_packs`) and lists pack kinds in
`kind.list`. Not here yet: pack transitions (ADR-0037 §1).
