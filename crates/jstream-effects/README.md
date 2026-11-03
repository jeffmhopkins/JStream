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
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-effects", "crates/jstream-render/README.md#filters", "docs/adr/0025-typed-settings-kind-descriptors.md", "docs/adr/0035-clap-plugins-out-of-process.md", "docs/adr/0037-kind-descriptors-for-transitions-and-plugins.md", "docs/adr/0093-shader-sources-from-effect-packs.md"]
-->

Layer-3 loader for third-party **WGSL effect packs** and their **shader sources**, the
data-only extension point of
[ADR-0035](../../docs/adr/0035-clap-plugins-out-of-process.md) (kept from ADR-0005). A
pack is a folder holding an `effect.toml` manifest and the `.wgsl` files it names. Each
effect becomes a `video_filter` kind with the ID `<pack>/<effect>`, so a pack never
shadows a built-in kind ([ADR-0025](../../docs/adr/0025-typed-settings-kind-descriptors.md)).
It depends on [`jstream-protocol`](../jstream-protocol/README.md) for kind descriptors and
settings, [`jstream-render`](../jstream-render/README.md#filters) for the filter harness
that draws an effect, `naga` (the exact version wgpu pins, with only `wgsl-in`) for WGSL, `toml`,
`serde`/`serde_json`, `thiserror`, `tracing`, and `png`, `zune-core` and `zune-jpeg` for a
shader source's images. It opens no GPU device of its own and has
no `unsafe`. Its one thread is each pack load's, below.

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
  256 KiB, and a pack holds at most 64 effects and shader sources together, of at most
  64 parameters each.
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
reported once. Each pack loads on a scoped thread of its own named `LOAD_THREAD`
(`jstream-pack-load`), joined before the poll goes on (or on the poll's own thread if it
can't start). A load that panics (naga, on untrusted WGSL) is caught there, around that
one pack, and reported as `PackError::Panicked`, a failed load like any other, so the other
packs keep loading and the poll keeps every change it found; a test loads one pack with
a loader that panics. A second folder with a pack ID that is already loaded is refused, and
retried in the poll that frees the ID (the first folder removed, or its pack renamed).
Polling needs no dependency and behaves the same on every file system. The cost is the
poll's latency, and an edit that keeps a file's size within the file system's timestamp
resolution is seen on the next change.

**Shader sources** (`source`, [ADR-0093](../../docs/adr/0093-shader-sources-from-effect-packs.md)).
A `[[source]]` table beside the `[[effect]]` tables (a pack has at least one of either,
and at most 64 together, sharing one ID space) becomes a `source` kind
`<pack>/<source>`, a `ShaderSource`. Its fields: `id`, `name`, `version`, `renames` and
`[[source.param]]` as for an effect; `width` and `height` (the defaults of the `width`
and `height` settings every shader source has, live, 16 to 4096 pixels, 1920 by 1080
when left out); an optional `common` WGSL file; `audio` (the `audio` bus and
`audio_source` settings the `spectrum` kind has, not live); `images`, a table of up to
`MAX_IMAGES` (12) names to PNG or JPEG files in the pack, each at most 4 MiB and 2048
pixels each way, decoded with the `png` and `zune-jpeg` crates `jstream-sources` uses;
and 1 to 8 `[[source.pass]]` tables, each an `id`, a `shader`, a `scale` (1/16 to 1, the
last pass's 1) and `inputs`, at most 12 names it reads. A source's parameters may also
be of type `scene` (an effect's may not): a scene reference with no default, which
fills an input slot instead of a uniform field. The names a pass can read, which follow
the parameter-ID rules and are unique per source, are its passes' IDs, its image names,
its `scene` parameters and, with `audio`, `audio`. None of them is a name the prelude
defines (`sys`, `noise`, `luma` and the rest), or another name plus `_tex`, `_wrap`,
`_texel` or `_size`, which that name's header defines. `width`, `height`, `audio` and
`audio_source` are not parameter IDs a pack may declare. Image paths follow the shader
paths' rules.

Each pass's WGSL is a generated header, then the `common` file, then the pass's file,
appended to `jstream_render::PROGRAM_PRELUDE`. The header holds `p` (the parameters
with a uniform form, laid out as for an effect, at binding 3) and, for each input in
order from binding 4, a texture and four functions of its name: `n(uv)`, `n_wrap(uv)`
(repeating), `n_texel(px)` and `n_size()`, which return premultiplied linear colour; the
`audio` input adds `audio_band(i)` and `audio_wave(i)` over its `AUDIO_BANDS` (64)
columns. A pass reads only what it lists. The contract is the effect's, with the header's
own declarations allowed, and an error names the file it is in (the `common` file or the
pass's) with that file's line. Inputs map to `ProgramInput`s: a pass, an image (in name
order), a slot (the `scene` parameters in declaration order), or `audio`, render's audio
data image.
`ShaderSource` implements render's `ExternalProgram`; `video_filter(settings)` makes the
filter that draws it, `size` reads `width` and `height` (a default for a value out of
range), `scene_params` says which slot is which, `hears` whether it reads audio, and `migrate` is an
effect's. The watcher stamps a source's `common`, pass and image files too, and reports a
source kind changed when its descriptor, passes (WGSL, scale, inputs) or images do. Tests cover the descriptor,
the manifest rules, errors naming their file and line, a bad image, the watcher, and a
two-pass source with feedback, an image and a scene slot drawn on the test adapter.

`jstream-core` runs the watcher (`Engine::watch_effect_packs`) and lists pack kinds in
`kind.list`. Not here yet: pack transitions (ADR-0037 §1), and shader sources in the
engine.
