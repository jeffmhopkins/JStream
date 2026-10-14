<!-- @doc
id = "ADR-0025"
title = "Typed settings through kind descriptors"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0025: Typed settings through kind descriptors

<a id="context"></a>
## Context
Sources need settings: a URL and a frame-rate cap for a browser source
([ADR-0021](0021-browser-source.md)), a file for an image, a colour and size for a colour
source. Filters need parameters. Today the protocol has neither.
`SourceKind::Builtin { kind }` names a kind by string, as a placeholder until
`jstream-sources` defines kinds with settings. The protocol's
[open question 11](../../crates/jstream-protocol/README.md#open-questions) asks whether typed
kinds replace it (a breaking change) or arrive beside it as new variants, which also
breaks older clients today.

These forces decide the shape:

- **Filters are partly data.** Third-party effects are WGSL packs with an `effect.toml`
  manifest, hot-reloadable ([ADR-0005](0005-plugins-wgsl-and-wasm.md)). The protocol can't
  hold a closed Rust type for parameters that a pack defines at run time.
- **Every client is generic.** The web UI, the CLI and MCP agents all speak the same
  protocol ([R-ARCH-001](../RULES.md#R-ARCH-001)), and agents have full access
  ([ADR-0006](0006-mcp-full-access.md)). A settings dialog and an agent both need to know
  each parameter's type, range and default without code per kind.
- **Settings refer to other objects.** Masks and mattes take a scene, an audio
  visualiser or a ducking filter takes a source, and images, LUTs, shaders and media take
  a file. The graph rejects nesting cycles and refuses to delete a referenced object
  ([jstream-graph](../../crates/jstream-graph/README.md)). It can only extend those checks
  to settings if it can see the references without knowing each kind.
- **Edits stay edits.** Every settings change must be an undoable edit with an inverse
  computed from the state before it ([ADR-0013](0013-command-model.md)).
- **Deletes multiply.** As reference kinds grow, "remove the blockers first" becomes a
  long, kind-specific algorithm in every client. Protocol
  [open question 12](../../crates/jstream-protocol/README.md#open-questions) asks whether
  to add a cascading delete.
- **Collections are portable.** A portable install moves between machines and drive
  letters ([ADR-0020](0020-portable-windows-distribution.md)), so absolute file paths in
  settings break.

This is an agent-tier decision ([ADR-0017](0017-adr-decision-tiers.md)). It is a protocol
and data-model shape within ADR-0005, ADR-0006 and ADR-0013. It changes no product scope
or platform, and supersedes no ADR. Every protocol change it implies is additive under
[R-API-001](../RULES.md#R-API-001).

<a id="decision"></a>
## Decision
**Settings are values described by kind descriptors that the engine reports.** The
protocol defines the descriptor and value shapes once. It never defines a type per kind.

- **Kinds.** A kind is a source kind or a filter kind. Built-in kinds come from
  `jstream-sources` and the built-in effects, and pack kinds come from effect packs. Kind
  IDs are opaque strings. Built-in kinds use bare names (`image`, `browser`), and pack
  kinds are namespaced by pack (`<pack>/<effect>`), so a pack can never shadow a built-in.
- **Queries.** `kind.list` returns a summary of every available kind: ID, category
  (source, video filter or audio filter), name and descriptor version.
  `kind.describe { kind_id }` returns one full descriptor, plus a JSON Schema of its
  settings that the engine generates from it. Descriptors are not collection state, so
  neither query carries a `rev`. A `kind.changed` event reports a kind that appears,
  changes or disappears, for example when a pack reloads.
- **Descriptor.** A `KindDescriptor` has an ID, a category, a name, an integer
  `version`, whether the kind produces video, audio or both, and an ordered list of
  parameters. Each `ParamDescriptor` has:
  - an `id`, unique within the kind and stable across versions;
  - a `type` with its constraints:
    - `bool`;
    - `int` and `float` (finite only) with `min`, `max`, `step` and a `unit`;
    - `enum` with its options, each a value and a label;
    - `color`, with or without alpha;
    - `string` (single or multi-line, with a maximum length);
    - `asset` with the file types it accepts;
    - `scene` (a scene reference);
    - `source` (a source reference, optionally restricted to audio or video);
  - a `default`;
  - a `label` and an optional description;
  - hints for UIs: a `group`, an `advanced` flag, and a `visible_if` condition on another
    parameter's value. Hints never affect validation;
  - `live`: whether a change applies without restarting the source or filter (a colour,
    an opacity) or restarts it (a capture device, a canvas size).
    How continuous edits travel and undo is decided by
    [ADR-0032](0032-live-edits-and-undo.md): a `live` parameter can be overridden with
    `live.set` during a gesture, and one batch commits it as one undo step.

  `ParamType` gets an `unknown` fallback, so adding a parameter type is a minor change. A
  client shows a parameter of an unknown type read-only.
- **Values.** Settings are a map from parameter ID to value. Scalars are plain JSON:
  booleans, numbers, strings, and colours as `#rrggbb` or `#rrggbbaa` strings. **References
  are tagged objects**: `{"type":"scene","scene_id":…}`, `{"type":"source","source_id":…}`
  and `{"type":"asset","root":…,"path":…}`. Because of the tag, the graph finds every
  reference in any settings map without the descriptor, even when a pack is missing.
  Reference parameters always accept `null` (no reference).
- **Edits.** `source.create` gains an optional `settings` map. `source.set_settings
  { source_id, patch }` merges a patch, in which `null` on a non-reference parameter
  resets it to its default. Its inverse is the patch of the old values, so the edit set
  stays closed under inversion. Filter settings use the same map and patch shape. The
  filter edits themselves belong to the filter-chain design. `SourceInfo` and the filter
  shapes report settings.
- **`SourceKind` stays.** `builtin { kind }` keeps its meaning: `kind` is a kind ID from
  `kind.list`. Settings travel beside it, not inside it. `scene { scene_id }` stays as
  well, because nesting is structural, not a setting. This settles open question 11
  without a breaking change and without new `SourceKind` variants.
- **Validation in the engine.** The engine checks every edit against the kind's current
  descriptor. An unknown parameter ID, a wrong type, a number out of range, an unknown
  enum option or an over-long string is rejected with `invalid_argument` and
  `field = edits.N.settings.<id>` (or `edits.N.patch.<id>`). Values are never clamped. A
  reference to a missing scene or source is `not_found`. Validation reads only state and
  descriptors. It never touches files, so an asset that doesn't exist yet is accepted.
- **The engine stores the complete map.** On create, missing parameters take their
  defaults, and the collection stores every parameter. A collection then renders the same
  after a kind changes a default.
- **Typed references are dependency edges.** The graph indexes every scene and source
  reference in every settings map, next to the edges it already has (items placing
  sources, `scene` sources showing scenes). Asset references are not edges, since files
  are outside the state.
  - **Cycles.** One shared dependency function covers all edges. A scene reference on a
    source makes every scene that places that source depend on the referenced scene.
    Every edit that can add an edge runs the cycle check: `item.add`, `source.create`,
    `source.set_settings` and the filter edits. A cycle is rejected with
    `invalid_argument` naming it, as nesting cycles are today. A test asserts that every
    edge-adding edit is checked.
  - **Deletes (open question 12).** Rejection stays the primitive. There is no cascading
    delete edit. `scene.delete` and `source.delete` are rejected while anything refers to
    the target, and the error lists **every** blocker: the referring object and the
    field. That list is a new optional detail on `invalid_argument`. One engine-side
    query, `delete.plan { target }`, returns the batch that removes the target and its
    blockers, together with the `rev` it was planned at. The plan:
    - removes the items that place a deleted source;
    - deletes the `scene` sources that show a deleted scene, and their items;
    - clears reference parameters that point at the target (sets them to `null`), and
      keeps the filter or source that holds them.

    The client shows the plan ("this also clears the mask scene on Webcam") and sends it
    as an ordinary batch with that `expected_rev`. Undo is then the batch's normal
    inverse.
- **Asset references** name a root and a relative path. The roots are `collection` (the
  collection file's folder), `app` (the app's asset folder, inside the portable folder in
  portable mode) and `absolute`.
  - A relative path uses `/` separators and has no `..` segments, drive letter or leading
    `/`. Anything else is rejected.
  - When a client sends an absolute path under the collection or app folder, the engine
    stores it in the relative form. A setup that is moved, or carried on a stick under
    another drive letter, still finds its files.
  - A missing file is a runtime error on the source or filter, never a load or edit
    failure.
- **Scoped file listing.** A remote UI can't open a local file dialog, so the protocol
  gets `asset.list { root, dir, accept? }`. It returns the names, types and sizes of the
  entries in one folder under a listing root. The listing roots are the collection
  folder, the app asset folder, and extra folders named in the local app settings. The
  protocol can't add a root, so a token holder can't widen what is listable. The query:
  - resolves the path and rejects anything that escapes its root, including through
    symlinks and junctions;
  - returns no file contents;
  - never names a host path outside a root in an error.

  It requires the control token like every call ([R-SEC-002](../RULES.md#R-SEC-002)). A
  WASM plugin without a granted filesystem permission is refused it
  ([R-SEC-003](../RULES.md#R-SEC-003)).
- **No secrets in settings.** Settings are saved in the collection file in plain text, so
  no kind declares a secret parameter. Credentials stay in the OS credential store
  ([R-SEC-001](../RULES.md#R-SEC-001)).
- **Versioning a kind.** A descriptor's `version` is an integer, and every stored source
  and filter records the version it was written with.
  - Adding a parameter needs no bump if its default reproduces the old behaviour. On
    load, the engine fills it in.
  - Renaming or removing a parameter, or changing its type or meaning, bumps the version
    and ships a migration that runs on load. Built-in kinds migrate in Rust, with a
    fixture test that loads the previous version, as collections do
    ([R-API-003](../RULES.md#R-API-003)). Pack manifests can declare renames and
    defaults only. Any other breaking change in a pack is a new kind ID.
  - A kind version bump is not a protocol change and leaves `PROTOCOL_VERSION` alone.
  - **Unknown data is preserved, not dropped.** The engine keeps the stored settings of
    a source or filter verbatim, saves them back, and reports it with an error instead
    of rendering it, when:
    - its kind is missing (a pack that isn't installed);
    - it was written by a newer kind version;
    - it holds a parameter the kind doesn't know.

    Opening a collection in an older build never destroys data.
- **MCP.** Tools stay generated from the protocol schema
  ([ARCH › MCP](../ARCHITECTURE.md#mcp)). The new queries and edits become ops of the
  `kind`, `source` and `asset` tools, with names derived as
  [R-API-002](../RULES.md#R-API-002) requires. The static schema of a settings map is
  generic, so an agent calls `kind.describe` for one kind's parameters and generated
  settings schema. That is the "describe on demand" the MCP design aims for, and adding a
  kind never grows `tools/list`.

<a id="consequences"></a>
## Consequences
- **Protocol follow-up.** A follow-up PR adds the shapes above. Every change is additive
  (one minor version under [R-API-001](../RULES.md#R-API-001)):
  - queries: `kind.list`, `kind.describe`, `delete.plan` and `asset.list`;
  - the `kind.changed` event;
  - edits: the `settings` field on `source.create`, and `source.set_settings`;
  - settings in `SourceInfo`;
  - the `blockers` detail on `invalid_argument`.

  It updates `schema/protocol.snap`, `schema/examples.snap` and the MCP `tools.snap`,
  and those diffs are the API review ([R-TEST-002](../RULES.md#R-TEST-002)). The protocol
  README's open questions 11 and 12 point here.
- **Kinds get their own snapshot.** Adding or changing a kind changes no protocol
  schema, so R-TEST-002 doesn't see it. The built-in descriptors are therefore
  snapshot-tested in `jstream-sources`, and that diff is reviewed like an API diff.
- **`jstream-graph`** replaces its nesting-only cycle check with one dependency function
  over all edges, and builds the reference index and `delete.plan` there. Persistence
  stores settings maps with their kind version, and runs kind migrations on load next to
  collection migrations.
- **Effect packs.** The `effect.toml` manifest must declare its parameters in descriptor
  form, with a version and any renames.
- **Clients render generically.** One settings form serves every kind, built-in or pack.
  UIs and agents learn new kinds without a release. The price: clients get no compile-time
  types for a kind's settings, so the engine's validation is the only guard, and its
  errors must name the parameter precisely.
- **Rejected alternatives:**
  - **A typed Rust enum per kind in the protocol** gives compile-time checking for Rust
    clients, but:
    - pack kinds can't be expressed at all (ADR-0005);
    - every kind change is a protocol release;
    - the schema snapshot grows with every kind;
    - each new variant breaks older clients until
      [open question 8](../../crates/jstream-protocol/README.md#open-questions) is
      settled.
  - **Typed enums for built-ins and descriptors for packs** has two code paths in every
    client and keeps the drawbacks of the first.
  - **Untyped JSON blobs** validated per kind give UIs and agents nothing to render or
    reason with. They also hide references, so cycle and delete checks would need code
    per kind.
  - **A cascading delete edit** would hide a large, kind-dependent removal behind one
    name. `delete.plan` shows the same removal as ordinary edits that the client can
    show, change or refuse.
  - **Asset paths resolved by search order** (collection folder, then app folder, then
    absolute) were rejected for an explicit root. Then a file in one root can't silently
    shadow another.
- **Live edits.** The `live` hint marks the parameters that
  [ADR-0032](0032-live-edits-and-undo.md)'s `live.set` may override during a gesture.
- **Security review.** `asset.list` is the first query that exposes the host filesystem,
  even in part. Its implementation PR gets a security review against the constraints
  above.
