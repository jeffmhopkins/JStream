<!-- @doc
id = "DESIGN-UI-DECISIONS"
title = "UI prototype: decision register"
kind = "reference"
status = "active"
review_every = "90d"
-->

# UI prototype: decision register

What the UI prototype (direction D) forces on the engine, protocol and data model. Each
entry is something to decide **before** the UI is written, ideally before the M1b shapes
in `jstream-graph` and `jstream-protocol` harden.

It is analysis, not a decision: each entry ends in a recommendation, and the decisions
themselves land as ADRs and `docs/ARCHITECTURE.md` edits. Line numbers in file references
are as of the commit that added this register; use `git log` on this file to find it.

<a id="sources-read"></a>
## Sources read
<!-- @section
-->

- Prototype: `design/ui-prototype/README.md` (all), `Docking.dc.html` (script part, lines
  896–3506, and the header and canvas markup), `Panel.dc.html` (script part, lines
  724–1909, and the chat, mixer, outputs, transitions and agent markup, lines 548–720).
  Main/CanvasFirst/ShowControl were not read in depth, since README marks D as chosen.
- Docs: `docs/ARCHITECTURE.md` (all), `ROADMAP.md`, ADR-0001 to ADR-0021, `docs/RULES.md`
  (R-ARCH, R-API, R-RT, R-GOV-005).
- Code: `crates/jstream-protocol/src/{command,query,event,version}.rs` and its README
  (Design, Open questions), `crates/jstream-graph/src/model/mod.rs`, `apply.rs` and its
  README, `crates/jstream-platform/src/{capture,audio}.rs` plus the trait list, and the
  open-question sections of every crate README.

<a id="notation"></a>
## Notation
<!-- @section
-->

- **Status:** *covered* (docs or code already decide it), *gap* (the docs say nothing),
  or *conflict* (it contradicts an accepted ADR, ARCH text or the maintainer-set ROADMAP).
- **Tier** follows ADR-0017. *M* means maintainer (product scope, platforms, process, or
  superseding an ADR). *A* means agent (technical, within accepted ADRs). Borderline
  cases are marked M, as ADR-0017 requires.
- **Urgency:** *now* (affects M1b shapes being built: graph model, protocol edits, mixer,
  undo), *pre-M2* (must be settled before UI work starts), *later*.
- File references: `D:` = `design/ui-prototype/Docking.dc.html`, `P:` = `Panel.dc.html`.
  Line numbers are on origin/main.

The protocol is at **0.3** (`version.rs:12`), and its README applies R-API-001 "at 0.x
exactly as after 1.0". So any change to a field's *meaning* (for example the rotation
pivot) already needs an ADR plus a deprecation release. That is why the "now" items
matter.

---

<a id="summary-table"></a>
## Summary table
<!-- @section
-->

| ID | Decision | Status | Tier | Urgency |
|---|---|---|---|---|
| D1 | Item transform: size vs scale, rotation pivot, crop units, canvas size | conflict (protocol shape vs prototype) | A | **now** |
| D2 | What belongs to a placement and what to the source (blend, opacity, item effects) | **conflict** (ARCH data model, ROADMAP M1b) | **M** | **now** |
| D3 | Reference ↔ clone conversion and "original" ownership | gap | A (UI meaning M) | **now** |
| D4 | Groups are nested scenes: scene canvas, `item.move`, group flag | gap | M | **now** |
| D5 | Keyframe scenes (variants that override placement only) | gap | M | pre-M2 (shape now) |
| D6 | Scene folders and scene-tree order | gap | A | now (before `scene.reorder` is frozen) |
| D7 | Organisation metadata (labels, favourites, icons, colours, hotkeys, multiview, background) | gap | A | pre-M2 |
| D8 | Source settings and filter params: typed enums or kind descriptors | gap (protocol OQ 11) | A | **now** |
| D9 | Filter model: separate video and audio chains on sources, scenes, outputs and buses | gap (ARCH has one `filters[]`) | A | **now** |
| D10 | Filters that use another scene: graph edges, cycle checks, delete rejection | gap | A | **now** |
| D11 | Render graph: per-tick DAG, caching, render plan in the snapshot | gap | A | now (design), M1b build |
| D12 | 3D per item: perspective per item, no depth buffer | gap (ARCH: "Transform3D", M3) | A | now (shape), M3 build |
| D13 | Blend modes in linear FP16 | decided ([ADR-0082](../../docs/adr/0082-blend-modes-drawn.md)) | A | pre-M2 |
| D14 | Program master chain and per-output chains (video) | gap; strains encoder sharing | M | pre-M2 (shape now) |
| D15 | Latency compensation, audio/video delay filters | gap | A | now (mixer timestamps) |
| D16 | Privacy screen (blur + mute) | dropped ([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md) §4) | M | — |
| D17 | Mixer UI (master + source faders) vs ADR-0007 bus matrix | gap / tension with ADR-0007 consequences | M | **now** (M1b mixer) |
| D18 | Does audio follow scenes? (nested scene audio, per-scene mixer list) | **conflict** (ADR-0007) | **M** | **now** |
| D19 | Per-source audio switch | gap | A | now |
| D20 | Sidechain and ducking edges in the audio graph | gap | A | now (graph shape) |
| D21 | Loudness metering (LUFS, true peak) and the meter stream | gap | A | now (meter stream shape) |
| D22 | Monitoring pre or post FX | gap (ARCH: `monitor_device?`) | A | pre-M2 |
| D23 | CLAP plugin hosting | **conflict** (ADR-0005) | **M** | later |
| D24 | Transition presets, default, per-scene in/out overrides | gap | M | pre-M2 (shape now) |
| D25 | Move transition: pairing, retargeting, per-item settings | gap (README asks for an ADR) | M | pre-M2 |
| D26 | Scene switching is an action; program and preview are runtime state | gap (protocol OQ 5) | A | now |
| D27 | Each client's editing scene is local; no studio-mode coupling | gap | A | pre-M2 |
| D28 | Source type list (Spout2, media file, waveform, spectrum, app audio…) | gap (README asks for an ADR) | **M** | now (M1b list) |
| D29 | Source lifecycle: what "shown", "active" and "visible" mean | gap | A | **now** |
| D30 | Stable capture targets (window and app by executable) and device modes | gap (platform traits) | A | now |
| D31 | Asset file references (images, LUTs, shaders, media) and portability | gap | A | now (persistence) |
| D32 | Image, GIF and font decoding vs ADR-0003's "OS codecs only" | ambiguity in ADR-0003 | M | now (image source is M1b) |
| D33 | Continuous edits: scrubbing, drags, live dialogs with Cancel; undo granularity | gap (protocol OQ 4) | A | **now** |
| D34 | Multi-client: selection, presence, conflicting edits | partly covered (ADR-0013) | A | pre-M2 |
| D35 | Workspace layout persistence: client side or server side | decided ([ADR-0083](../../docs/adr/0083-ui-settings-kept-by-the-server.md)) | A | pre-M2 |
| D36 | Preview streams for any target, pop-outs, projectors, multiview | partly covered (ADR-0002) | A (projector host: M) | pre-M2 (stream shape now) |
| D37 | Item geometry for the canvas overlay: source sizes as runtime state | gap | A | now |
| D38 | Audit log over the protocol (Agent activity panel) | gap | A | pre-M2 |
| D39 | Header and stream stats; event subscriptions | partly covered (`stats.tick`) | A | M1d |
| D40 | YouTube chat: send as channel, feature on stream, audience stats | gap beyond ADR-0009 | **M** | later (M3) |
| D41 | Go Live and broadcast creation | covered (ADR-0009) | — | M3 |
| D42 | Hotkeys (scene hotkeys; Pause for privacy dropped by ADR-0080) | gap | A | pre-M2 |
| D43 | Copy, paste and duplicate scene | covered by batches (client IDs) | A | pre-M2 |
| D44 | Deleting referenced objects (protocol OQ 12) from the UI's point of view | gap | A | now |

---

<a id="a-scene-graph-and-item-model-m1b-shapes"></a>
## A. Scene graph and item model (M1b shapes)
<!-- @section
-->

### D1. Item transform: size vs scale, rotation pivot, crop units, canvas size

**Prototype.** Transforms are "in canvas pixels (1920 x 1080). x/y/w/h are the visible
(post-crop) box; cl/cr/ct/cb are how much is cropped away on each side, in the same units"
(`D:1243-1244`, `XF0` at `D:908`). Rotation turns about the item's **centre**
(`D:1480-1487`, `D:1529-1532`). The Inspector shows W/H in px and again as a % of the
source's native size (`P:1747-1751`, `NATIVE` at `P:878`). Resize keeps the aspect ratio,
Ctrl stretches, Alt crops (`D:1489-1539`). The canvas is a fixed 1920×1080 (`CW, CH` at
`D:906`).

**Today.** `ItemTransform` is `position` (top-left), `scale`, and `rotation_deg` "about the
item's top-left corner" (`command.rs`, `ItemTransform` doc). `Crop` is in whole **source**
pixels. The doc says the 3D transform will be added "as added fields with defaults", but
a pivot change is a change of meaning, and so major under R-API-001. Scenes have no size.
`VideoMix` has a `resolution` (ARCH data model, line 175). No collection-level canvas
exists.

**Status.** Conflict between the protocol's semantics and the prototype's interaction
model. The UI *could* convert, but only if it knows each source's native size (D37), and
a centre pivot can't be expressed exactly by a top-left pivot without that size.

**Consequences.**
- Rotating about the top-left means every UI rotation also rewrites `position`, which
  needs the current native size. When a window capture is resized, a top-left-pivot item
  appears to swing.
- Scale-based storage means a resized window grows on the canvas. Size-based (bounds)
  storage keeps the box, like OBS's "bounding box". The prototype assumes size-based (w/h
  in canvas px).
- Crop in source px is stable under resize-by-handle, and the UI can convert. Keep it.
- A **collection canvas size** (base resolution) is needed for: nested scenes' coordinate
  space (D4), Fit/Stretch to canvas (`D:3171-3172`), snapping to centres, and mask scenes
  (D10).
- 3D (D12) adds tilt, turn, depth and perspective, which need a defined pivot too.

**Options.**
1. Keep top-left and scale. The UI converts using native sizes. Cheapest now, but
   fragile with resizing sources and with 3D.
2. **Centre pivot, explicit size.** `position` names the centre (or an `anchor` field,
   defaulting to centre), with `size` in canvas px plus `scale_mode: stretch | fit | fill`
   to say how the source fills its box. The crop stays in source px, and rotation turns
   about the anchor.
3. Like option 2, but keep both `scale` and an optional `bounds` (OBS model). More
   expressive, and harder for agents to reason about.

**Recommendation.** Option 2, decided now with one breaking protocol change (0.3 → 0.4,
with ADR and deprecation per R-API-001). The golden tests in M1b (ROADMAP "transform +
crop, with golden tests") would otherwise freeze the wrong semantics. Add
`Collection.canvas { width, height }` (default 1920×1080), and mixes scale from it.

**Tier** A (technical within the data model; it is an ADR because protocol meaning
changes per R-ARCH-004). **Urgency:** now.

---

### D2. What belongs to a placement, and what to the source

**Prototype.** "What a reference shares with its source. Everything else (position,
size, rotation, crop) is the reference's own placement": `SHARED_XF = ['opacity',
'blend']` (`D:1098-1100`). `setXfMany` routes opacity and blend to the **source**
(`D:1250-1256`). `switchScene` copies them into every scene (`D:2686-2691`). In the
Inspector, blend and opacity are bound to the source id (`P:1769-1777`), while 3D and
crop are bound to the item (`P:1761-1767`). Filters are always "Applies everywhere this
source appears" (`P:1100`, `P:1873`: "SOURCE FILTERS · EVERYWHERE"). **There is no
per-placement effects chain.** To vary a look per placement, you clone. Visibility is
per placement (`P:1455-1460`), and keyframes may change it (`PLACE_KEYS`, `D:1109`).

**Today.** ARCH: `SceneItem { id, source_id, transform: Transform3D, crop, visible,
blend, effects[] }` (line 173). "`SceneItem.effects[]` and `crop` belong to that one
placement" (line 193). ROADMAP M1b (expanded by the maintainer, per its verify note):
"source filters apply everywhere the source appears, and **item effects apply to one
placement**". Opacity isn't modelled anywhere.

**Status.** **Conflict** with ARCH and with the maintainer-set M1b scope. Blend moves
from the item to the source, and item effects disappear.

**Consequences.**
- Graph model: whether `Item` gets `blend`, `opacity` and `effects`, or `Source` gets
  them. This changes the edits (`item.set_blend` or `source.set_blend`), the inverses,
  and what `item.add`'s inverse must restore.
- Render: a per-source blend means every placement blends the same way. That is odd for
  a nested scene used both as a background and as an overlay.
- Move transition (D25) interpolates per-item alpha (`sampleAnim`, `D:2758`, `D:2775`),
  so the compositor needs a per-item opacity regardless.
- Keyframes may not change opacity (`PLACE_KEYS`). If opacity is per item, keyframes
  could override it, which is a cheap and useful extension.

**Options.**
1. **Adopt the prototype:** blend and opacity on the source, no item effects. This drops
   an M1b deliverable and amends ARCH.
2. **Keep ARCH:** blend, opacity and `effects[]` on the item. The UI gets a "This
   placement" section, and a "reference" then shares less than the prototype says.
3. **Hybrid:** opacity and blend on the **item** (they are compositing parameters of a
   placement, and Move animates them). No item effect chain in v1, but keep an
   `effects[]` slot in the model so it can come later without a migration. The README's
   reference definition becomes "shares everything except placement, opacity and blend".

**Recommendation.** Option 3. It keeps the compositor model clean, since per-item alpha
is needed anyway, and it matches how users expect blend to behave. Item effects are the
expensive part (a render pass per placement), and the prototype shows nobody missed
them. The maintainer must decide, because it rewrites an M1b bullet and the prototype's
stated reference semantics.

**Tier** M. **Urgency:** now. The filters/effects split is an M1b deliverable.

---

### D3. Reference ↔ clone conversion and "original" ownership

**Prototype.** `dups[id] = { of, mode }`. A reference follows `of`, and a clone starts
as a copy (`D:1260-1308`). Right-click toggles between the two (`check('Reference to
original')` / `check('Independent clone')`). Toggling clone → reference "discard[s] local
changes" and makes the item follow the original again (`D:1310-1326`). "Add existing" in
the add-source dialog creates a reference (`D:2411`). References show in italics because
they "reference a source owned by another scene" (`REF_KEYS`, `P:865-867`; `D:1096-1097`).
Names, labels, favourites, properties and filters belong to the source, and a clone copies
them (`copyShared`, `D:1268-1282`).

**Today.** This maps almost exactly onto the existing model. `item.add` of an existing
`source_id` is a reference, and `source.duplicate` plus `item.add` is a clone (`command.rs`
`ItemAdd` and `SourceDuplicate` docs; ARCH line 186-189). What is missing:
- no edit changes which source an item places (needed for the toggle);
- no record of which source a clone came from (needed for "back to reference");
- sources have no **owner scene**. ADR-0007: sources exist independently of scenes.

**Status.** Gap. The model is covered, the conversion isn't, and "owned by another scene"
contradicts the scene-independent source model.

**Consequences.**
- New edit `item.set_source { item_id, source_id }`, with inverse `item.set_source` back.
  Clone = batch `source.duplicate` + `item.set_source`. Back to reference = batch
  `item.set_source(original)` + `source.delete(clone)` if nothing else places it.
- Optional `Source.duplicated_from: Option<SourceId>`, kept as a plain link that dangles
  harmlessly. Deleting the original is then allowed, and the link reads as "none".
- Italics: with no owner, the honest display is "this source is placed N times" on every
  placement, rather than marking all but one as references.

**Options.**
1. Add `item.set_source` plus `duplicated_from`, and display "shared ×N". No ownership.
2. Add an owner (`Source.home_scene`). This contradicts ADR-0007's scene-independent
   sources, and it complicates deleting the home scene.
3. No conversion edit: the UI removes and re-adds the item. That loses the item ID,
   which breaks keyframe overrides and per-item transition settings (D5, D25).

**Recommendation.** Option 1. The protocol additions are agent tier. Changing the italic
rule ("shared" on every placement instead of "reference to another scene's") is a UI
semantics change worth a line to the maintainer.

**Tier** A, plus a note to M on the display rule. **Urgency:** now (`item.set_source`
belongs with the M1b shared-source work).

---

### D4. Groups are nested scenes

**Prototype.** "In Scene objects a group is a scene of its own, nested here. (In Scenes,
folders are only for organising.)" `newGroup` moves the selected objects into a new
scene "Group N", nests it in place, and lists it in Scenes too (`D:2462-2486`). The
children of a group are drawn and **edited inline on the parent canvas**: `drawOrder`
flattens the objects tree, including group children (`D:1780-1784`). A nested scene's
data with no saved state is built "from its children here" (`D:2539-2544`). Scene
properties have "Include its audio when nested" (`D:1032-1037`). "A nested scene is a
reference: edit it in its own scene" (`D:2422`).

**Today.** Scenes as sources exist (`SourceKind::Scene`; cycle check on `item.add`,
`apply.rs`; graph README "No cycles"). Scenes have no size (D1), no group flag, and there
is no way to move an item between scenes.

**Status.** Gap.

**Consequences.**
- **Coordinate space.** Inline editing of group children on the parent canvas works only
  if a group scene shares the parent's canvas and the group item has the identity
  transform at creation. Then the children keep their canvas coordinates. This needs D1's
  collection canvas.
- **"Group selection" is one atomic batch:** `scene.create(G)`, `source.create(kind:
  scene G)`, `item.add(G-source)` in the parent at the old position, and for each
  selected item an `item.move { item_id, scene_id: G, index }` that **keeps the item ID**.
  Keyframe overrides and per-item transition settings are keyed by item ID (D5, D25).
  `item.move`'s inverse is `item.move` back with the old index.
- **Render:** a group renders to its own texture (it is a scene), so scene filters and
  blend apply to the group as a whole. That costs one canvas-sized texture per group per
  tick (D11). Option: a `flatten: false` hint, so a group with no filters and normal
  blend composites its children directly into the parent.
- **Scene list:** groups appear in Scenes (toast at `D:2485`). Add `Scene.role: scene |
  group` so switchers, hotkeys and agents can tell them apart.
- **Move transition** pairs flattened items, group children included (`sceneSources`,
  `D:2653-2656`). The engine must decide whether Move looks inside nested scenes (D25).

**Options.**
1. **Groups are scenes with `role = group`**, sharing the collection canvas, plus
   `item.move`. Rendering may flatten as an optimisation.
2. Groups as a separate item container (OBS groups): no scene, no reuse, no scene
   filters. This contradicts the prototype's "a group is a nested scene".
3. Groups are scenes with no special support (no `item.move`): groups lose item identity.

**Recommendation.** Option 1. It is a product decision (the prototype states it, but no
doc does), so the maintainer confirms it in the scene-structure ADR.

**Tier** M (product semantics); the shape decisions are A. **Urgency:** now. The
coordinate space and `item.move` change what M1b's scenes-as-sources are built on.

---

### D5. Keyframe scenes

**Prototype.** A keyframe is "a variant of a scene, nested under it, that may change
only placement and visibility; everything else follows the parent. It stores only what
differs" (README; `SCENE_DATA0` at `D:919-925`; `saveCurrent`, `D:2547-2562`). Keyframes
are scene-list children of their parent (`TREES0`, `D:949`). A keyframe of a keyframe is
made on the parent (`newKeyframe`, `D:2572-2586`). Editing anything except placement is
refused in a keyframe (`kfLocked`, `D:2564-2570`; Inspector note at `P:1700-1702`). It
can be reset or deleted. It switches like a scene, usually with a Move transition, which
is its point. Filter scene-choices treat a keyframe like its parent for loop checks
(`D:2284`).

**Today.** Nothing.

**Status.** Gap.

**Consequences.**
- Model: `Scene { …, variant_of: Option<SceneId>, overrides: BTreeMap<ItemId,
  PlacementPatch> }`. A variant has no `items` of its own. `PlacementPatch` holds the
  optional fields of D1's transform plus crop and visible (and opacity if D2 option 3).
- Invariants: `variant_of` names a non-variant scene. Overrides only name the parent's
  items. Deleting the parent is rejected while variants exist (or cascades, D44). Removing
  a parent item drops its overrides, and the inverse must restore them, so `item.remove`'s
  inverse grows. Adding a parent item shows it in every variant at the parent's placement.
- Edits: `scene.create_variant`, `variant.set_override` / `variant.clear_override`, and
  `variant.reset`. Or reuse the `item.set_*` edits with a `scene_id` context, which is
  cleaner for agents ("set this item's transform in scene X"). That second form changes
  the item edits' addressing, which is why the shape is decided now.
- Cycle checks: the variant → parent edge counts in the scene DAG (D10).
- Render: a variant renders the parent's items with patched placements. Its scene filters
  are the parent's.
- A variant can be nested and used by masks (prototype allows it).

**Options.**
1. **First-class variant scenes** (above).
2. Snapshot presets on a scene ("layouts") that are not scenes, so they can't be
   program, nested or hotkeyed. That contradicts the prototype.
3. Plain duplicate scenes, with Move pairing by source. Loses "everything else follows
   the parent".

**Recommendation.** Option 1, with item placement edits addressed by `(scene_id,
item_id)`. In a normal scene that is the item's own scene; in a variant it writes an
override. Decide the addressing now; build later.

**Tier** M (new product concept). **Urgency:** pre-M2 for the feature; **now** for the
addressing of `item.set_*` edits.

---

### D6. Scene folders and scene-tree order

**Prototype.** Scenes is a tree: folders (organising only) holding scenes, and scenes
holding their keyframes (`TREES0`, `D:948-952`). Drag into, before or after (`treeMove`,
`D:1795-1814`). Keyframes stay under their scene.

**Today.** Scenes have a flat display order. `scene.reorder { index }` is in protocol 0.3.
`SceneSummary` has no folder.

**Status.** Gap.

**Options.**
1. Keep the flat order and add `Scene.folder: Option<FolderId>`. Folders are a separate
   ordered list `{id, name}`, and a folder's scenes are the scenes naming it, in flat
   order. `scene.reorder` stays valid. New edits: `folder.create/rename/delete/reorder`
   and `scene.set_folder`.
2. A real tree, with `scene.move { parent, index }` replacing `scene.reorder` (breaking).
3. Folders client-side only. Agents and other clients don't see them, which goes against
   R-ARCH-001's "no UI-only features".

**Recommendation.** Option 1: additive, shared by every client, visible to agents.
Keyframes nest under scenes through `variant_of` (D5), not through folders.

**Tier** A. **Urgency:** now-ish (cheap to add; it only must not be contradicted by
other M1b work).

---

### D7. Organisation metadata: collection state or client state?

**Prototype.** Labels with name, icon and colour; label assignments; favourites
(`DEFAULT_META`, `P:831-844`; `D:1189-1202`). They are keyed by **source** key, so
references share them and clones copy them (`copyShared`, `D:1270-1272`). Trees filter by
search, favourites and labels (`P:1416-1441`). Scene properties: hotkey, background
colour, "Show in multiview", "Include its audio when nested" (`D:1032-1037`). Scenes have
an icon (`SCENE_INFO`, `P:856-864`). Scenes carry labels too: the prototype also assigns
labels to `scene:` keys ([ADR-0078](../../docs/adr/0078-scenes-carry-labels.md)).

**Today.** Nothing. R-ARCH-001: "There are no UI-only features".

**Status.** Gap.

**Options.**
1. **Collection state, edited by undoable edits** (`label.create/update/delete`,
   `label.assign`, `source.set_meta`, `scene.set_meta { icon, color, favourite }`).
   Agents can use it ("hide everything labelled Sponsor"), and every client sees the same.
2. A per-client UI blob. Cheap, but agents can't see it and two UIs disagree.
3. Split: labels, icons and colours in the collection; favourites per client.

**Recommendation.** Option 1, all in the collection: JStream is single-user (ADR-0006),
so favourites are not personal. Scene **background colour** belongs in the scene model,
because the compositor needs it (clear colour). Whether a nested scene's background is
transparent must be stated. **Hotkeys** (D42) and **multiview** go in the collection too.

**Tier** A. **Urgency:** pre-M2. Leave room in `Source` and `Scene` now: the model's
private fields already allow it (`model/mod.rs` header).

---

### D8. Source settings and filter parameters: typed enums or kind descriptors?

**Prototype.** Each property dialog is generated from a **schema per kind**
(`PROP_SCHEMA`, `D:993-1086`). Each field has a key, label, type (`num`, `select`,
`toggle`, `text`, `area`, `color`), default, range, step and unit. Filters are described
the same way (`FILTER_TYPES`, `D:1122-1155`), including a `scene` param type, and the UI
renders any of them generically (`feVals`, `D:2198-2235`; `flVals` field builder,
`P:1021-1029`). "Easy" filters (three settings or fewer) expand in place (`P:772`).

**Today.** `SourceKind::Builtin { kind: String }` is a placeholder "until
`jstream-sources` defines typed kinds with settings" (`command.rs`). Protocol open
question 11 asks whether typed kinds replace it (breaking) or arrive beside it. ADR-0005:
effect packs are WGSL plus an `effect.toml` manifest, so third-party filters are
**data-defined**.

**Status.** Gap (open question 11, unresolved).

**Consequences.**
- If filters can be data-defined (effect packs, ADR-0005), the protocol can't have a
  closed Rust enum of filter params. It needs a descriptor that the engine publishes.
- One generic shape for sources and filters keeps the UI and MCP generic: query
  `kind.list` → `[KindDescriptor { id, category, name, params: [ParamDescriptor] }]`,
  and settings `BTreeMap<String, ParamValue>`, validated by the engine against the
  descriptor (`invalid_argument` with `field = settings.<key>`).
- Typed references inside params: `ParamValue::Scene(SceneId)`,
  `ParamValue::Source(SourceId)` (waveform/spectrum "Audio source", ducking "Triggered
  by"), and `ParamValue::Asset(path)` (D31). The graph can then index references for
  cycle checks and delete rejection (D10, D44) without knowing each kind.
- Edits: `source.set_settings { source_id, patch }` (merge patch; inverse = patch of
  the old values), and the same shape for filters.
- MCP: descriptors are exactly the "describe on demand" that ARCH › MCP wants.

**Options.**
1. **Descriptor-driven settings for all kinds**, built-in and pack. `SourceKind` becomes
   `{ type: "builtin" | "scene", … }` with a settings map.
2. Typed Rust enums for built-in kinds, descriptors only for packs. Two code paths, and
   the schema snapshot grows with every kind.
3. Typed everything. Effect packs then need protocol releases, which contradicts
   ADR-0005's hot-reloadable packs.

**Recommendation.** Option 1, settling open question 11 before `jstream-sources` lands
in M1b.

**Tier** A. **Urgency:** now.

---

<a id="b-filters-and-the-render-pipeline"></a>
## B. Filters and the render pipeline
<!-- @section
-->

### D9. Filter model: separate video and audio chains on sources, scenes, outputs and buses

**Prototype.** "Separate **video** and **audio** chains, on sources, scenes and outputs.
Only the chains that fit are shown (a scene has no audio chain; a mic has no video
chain)" (README; `chainsOf`, `D:2173-2179`). Each filter is `{ id, type, on, p }`
(`FILTERS0`, `D:1156-1173`), reorderable within its chain only (`P:1062-1085`), with an
enable toggle (`P:1055`). The program output and each output have chains (`out:program`,
`out:youtube`).

**Today.** ARCH: `Source { …, filters[] }` (one list), `Bus { filters[] }`, and item
`effects[]` (line 173-177). Nothing on scenes or outputs. Not built: graph README "Not
yet: filters and effects".

**Status.** Gap. ARCH's single `filters[]` doesn't say which chain a filter is on, and
scenes and outputs have none.

**Consequences.**
- Model: `FilterChains { video: Vec<Filter>, audio: Vec<Filter> }` on `Source`, `Scene`
  (video only), `Output` (video and audio, D14) and `Bus` (audio). `Filter { id, kind,
  enabled, settings }` (D8).
- Edits: `filter.add { owner: FilterOwner, chain, filter_id, kind, settings, index? }`,
  `filter.remove`, `filter.set_enabled`, `filter.set_settings`, `filter.reorder` (within
  its chain). `FilterOwner = Source | Scene | Output | Bus`.
- Filter IDs are globally unique and client-chosen, like items (protocol OQ 3 stays
  consistent).
- A source kind declares whether it has video, audio or both (`audio` and `hasAudio` in
  `SOURCE_TYPES`, `D:969-988`). That is part of the kind descriptor (D8).

**Recommendation.** As above. It is a clean generalisation of ARCH. Write it into the
data model with D2's outcome on item effects.

**Tier** A. **Urgency:** now (M1b "filters and effects").

---

### D10. Filters that use another scene: graph edges, cycle checks, delete rejection

**Prototype.** Alpha mask, luma matte, clip-to and displacement take a `scene` param
(`D:1137-1140`). The referenced scene shows nested under the object in the tree
(`P:1385-1410`). "Scenes that would create a loop are not offered". The prototype's check
only excludes the current scene and the scenes containing the object (`sceneChoices`,
`D:2268-2285`; `P:996-1010`). That misses indirect loops: mask scene M whose own items
use a filter that references the current scene.

**Today.** The graph rejects nesting cycles, checked on `item.add` only, because "Only
`item.add` adds nesting" (graph README). Deleting a scene is rejected while a `scene`
source shows it (`apply.rs`; protocol `SceneDelete` doc).

**Status.** Gap. A new kind of dependency edge.

**Consequences.**
- The **scene dependency graph** gains edges from four places: items placing scene
  sources (exists), scene-typed filter params on sources and scenes (new), keyframe →
  parent (D5), and waveform/spectrum → audio source (D28, a source → source edge with no
  cycle risk). The cycle check must run on every edit that can add an edge: `item.add`,
  `item.set_source` (D3), `item.move` (D4), `filter.add`, `filter.set_settings`, and
  `scene.create_variant`. One shared `scene_deps(state)` function, plus a test that
  every edge-adding edit is checked.
- A filter on a **source** that references scene S creates edges from *every scene that
  places that source* to S. Placing a masked source into S itself must be rejected, so
  `item.add` must consider filters too.
- Delete rejection: `scene.delete` is also rejected while a filter references the scene,
  and the error names the filter.
- Render: a referenced scene is rendered (once per tick, D11) even when nothing shows it
  directly. That makes it "active" for source lifecycle (D29).
- Prototype detail to fix in the UI: offer only scenes the **engine** says are legal
  (e.g. a `scene.can_reference` query, or a rejection with the cycle path, like the
  existing cycle error).

**Recommendation.** Generalise the graph's cycle check now, while it is small. Typed
scene references in settings (D8) make this generic.

**Tier** A. **Urgency:** now.

---

### D11. Render graph: per-tick DAG, caching, render plan in the snapshot

**Prototype.** Implies: source filters run once and apply "everywhere" (`P:1100`).
Scenes are reused as nested layers, masks, mattes and displacement maps. Scene filters
apply to the whole scene. There is a program master chain, then per-output chains (D14).

**Today.** ARCH › Threads: per-subsystem immutable snapshots through `arc-swap`, GPU
resources keyed by ID (line 252-256). R-RT-002: bounded work per tick, no per-item
allocation in per-frame loops. GPU pipeline steps 3-5 describe one composite.

**Status.** Gap (the pipeline is described for one composite, not a graph).

**Consequences.**
- On the control thread, the render snapshot builder turns the collection into a
  **topologically sorted render plan**:
  - source nodes;
  - source filter chain nodes (one texture per source per tick, however many placements);
  - scene nodes (composite items, then the scene filter chain);
  - mix nodes (Program, with any in-progress transition, D25; Preview; any `VideoMix`);
  - the master chain;
  - per-output chain nodes;
  - NV12 conversion and encoder hand-off.

  The render thread walks the plan with preallocated textures, so there is no
  allocation per frame (R-RT-002).
- Each node renders at most once per tick (memoised by node ID). Unreferenced nodes are
  culled. That culling is also D29's "active" set.
- Texture budget: every scene used as a layer or mask costs one canvas-sized FP16
  texture (1920×1080×8 B ≈ 16.6 MB). This is a measured budget per R-RT-005, not an
  estimate, so it should be measured in M1b.
- Nested scenes at a size other than the canvas need a resolution policy (D1/D4: all
  scenes render at canvas size, and the consumer scales).

**Recommendation.** Record this as the render-graph section of ARCH › GPU pipeline
during M1b. It is technical and follows from ADR-0004.

**Tier** A. **Urgency:** now (design), built in M1b.

---

### D12. 3D per item: perspective per item, no depth buffer

**Prototype.** Per item: tilt (X), turn (Y), depth (Z) and **perspective per item**
(`P:1761-1766`; demo values `persp: 900` at `D:931`). Move interpolates rx, ry, z and
persp (`D:2770-2774`). Items stay in stacking order (painter's): `drawOrder` is the
objects order (`D:1780-1784`).

**Today.** ARCH: `transform: Transform3D` (line 173) and `jstream-types` "math
(Vec/Mat/Transform3D)" (crate map). ROADMAP M3: "Full 3D transforms". `jstream-types`
has only `rational.rs` today. The protocol has a 2D transform "replaced in a later minor
version, as added fields with defaults".

**Status.** Gap in semantics. "Transform3D" says nothing about the projection model.

**Consequences.**
- Decide: CSS-like per-item perspective with no shared camera and **no depth buffer**,
  where stacking order always wins, so Z only affects projection size. That is the
  cheapest to render, and it is what the prototype draws with CSS.
- The alternative is a scene camera with depth testing, which makes stacking order and
  depth interact and complicates blend and masks.
- Hit-testing and selection handles on the canvas need the projected quad. The UI
  computes it (D37), so the formula must be specified once in protocol docs, and golden
  tests must lock it.
- Interpolation: rotations take the shortest path (`angle`, `D:2736`).

**Recommendation.** Per-item perspective, painter's order, no depth buffer. Define the
fields now (`tilt_deg`, `turn_deg`, `depth_px`, `perspective_px`, all defaulting to
neutral), when D1 changes the transform anyway. Build them in M3.

**Tier** A. **Urgency:** the shape now (together with D1); the build in M3.

---

### D13. Blend modes in linear FP16

**Prototype.** Normal, Additive, Multiply, Screen, Overlay, Darken, Lighten, Difference
and Exclusion (`BLEND_NAMES`, `P:879`; mapped to CSS `mix-blend-mode`, `D:1095`).

**Today.** ARCH: `blend` on SceneItem. Compositing is "in linear light in an FP16
working format" (line 199).

**Status.** Decided by [ADR-0082](../../docs/adr/0082-blend-modes-drawn.md), which
takes the recommendation below, but draws screen with a fixed-function state and darken
and lighten from the destination copy, since min and max are wrong where either side is
translucent.

**Consequences.**
- Normal, Additive, Darken/Lighten (min/max) and Multiply map to fixed-function blend
  states. Screen, Overlay, Difference and Exclusion need the destination in the shader,
  which means a copy of the target or a ping-pong pass per item using them. That is a
  real per-item cost under R-RT-002.
- In linear light these modes look different from the CSS (sRGB) preview. The UI's
  preview stream comes from the engine, so what you see is right, but users coming from
  OBS or Photoshop expect sRGB-space Overlay. Decide: blend in linear, or convert to
  perceptual space for the non-linear modes.

**Recommendation.** Support the list, with a separate pass only for the modes that need
it. Blend in linear light, except Overlay, Difference and Exclusion, which blend in
sRGB-encoded space to match user expectations. Mark it "measured on …" when built.

**Tier** A. **Urgency:** pre-M2. Only the enum's values need to exist at M1b.

---

### D14. Program master chain and per-output chains (video)

**Prototype.** "**Program output** has a master chain for everything leaving JStream,
and each output (stream, recording, virtual camera) can add its own chain after it"
(README; `OUTPUTS`, `D:1101-1107`; outputs panel note, `P:653-662`: "Program filters
apply to every output; each output can add its own after them"). Demo: a LUT on program,
sharpen only on the YouTube output (`D:1167-1168`). The virtual camera has video only.

**Today.** Principle 3: "Outputs consume *video mixes* and *audio buses*" (line 71).
`Output { id, kind, video_mix, audio_buses, encoder_profile }`. **Encoder sharing:**
outputs with the same `(video_mix, encoder_profile)` share one session (line 196). ADR-0007
lets an output take any mix (e.g. a scene on the vcam).

**Status.** Gap, and a tension with encoder sharing.

**Consequences.**
- "Everything leaving JStream" is ambiguous when an output consumes a non-program mix.
  Recommended reading: the master chain belongs to the **Program mix** (video) and the
  **master bus** (audio, D17). An output consuming another mix doesn't get it.
- A per-output video chain makes that output's picture differ, so it can't share an
  encoder session with an output that lacks the chain. The engine forms a **derived
  mix** (`mix + chain`) per distinct chain, and sharing is keyed by `(derived mix,
  profile)`. That is one extra GPU pass and one extra encoder session per chained output,
  against vendor session caps (ARCH: "the engine reports the count"). The UI should warn
  when adding a video filter to an output costs a session.
- UI preview and Preview mix: does the canvas show the picture before or after the
  master chain? Recommended: the editing canvas shows the scene **pre-master** (what
  you're editing), and a "program" projector or preview shows post-master.
- Data model: `Output.filters: FilterChains` (D9); "Program" gets
  `studio.master_filters`, or rather `VideoMix.filters` on the Program mix.

**Options.**
1. Chains on mixes and outputs as above, with derived mixes and sharing on the derived
   key.
2. Master chain only, no per-output video chains (audio-only per output). Keeps sharing
   simple, but drops a prototype feature.
3. Per-output chains applied after encode? Impossible for video filters.

**Recommendation.** Option 1, with the session cost surfaced. Product-visible, so the
maintainer confirms it ("each output can add its own chain").

**Tier** M (feature scope); the engine shape is A. **Urgency:** pre-M2 for the feature;
**now** for `Output`'s model shape, since M1b builds encoder sharing.

---

### D15. Latency compensation and audio/video delay filters

**Prototype.** Video delay (0–5000 ms, "Hold the picture back to match audio") and audio
delay (0–5000 ms) filters (`D:1136`, `D:1150`). Mastering filters with look-ahead behaviour:
loudness target, true-peak limiter (`D:1151-1153`). README follow-up: "program and
per-output chains (with latency compensation)".

**Today.** ADR-0014: one media clock, all timestamps mapped into it. Render: "a missing
source frame repeats the last one", and the compositor takes the latest frame per tick
(capture mailbox, `capture.rs`). The platform-windows open questions say
`AudioRenderBuffer::time` is `None` "until the engine needs output sync, for example for
monitoring with delay compensation".

**Status.** Gap.

**Consequences.**
- **Audio latency** (look-ahead limiters, loudness riding, plugins in D23) should never
  shift A/V sync. Each audio filter reports its latency. The mixer stamps output blocks
  with `pts = media time of the input`, not the time of output, and muxers interleave by
  pts with a bounded wait. Nothing then needs to delay video, and encoder sharing is
  unaffected. This is a mixer design decision for M1b.
- **Video delay** means buffering frames on the GPU. At 1080p and 60 fps, 5000 ms is 300
  frames. In FP16 working format that is about 5 GB, and even NV12 is about 0.9 GB. Cap
  it (e.g. 1000 ms), and store delayed frames compressed or in 8-bit. Allocation happens
  when the filter is added, on the control thread, never on the render thread (R-RT-003).
- **Audio delay** means a ring buffer preallocated to the maximum when the filter is
  created (R-RT-001: no allocation on the mixer thread). 5000 ms at 48 kHz stereo f32 is
  about 1.9 MB. Fine.
- Per-output audio chains with different latencies are then just different pts
  bookkeeping per output.

**Recommendation.** pts-preserving latency compensation in the mixer, written into ARCH
› Threads. Cap video delay and measure its memory.

**Tier** A. **Urgency:** now (the mixer's timestamp contract is built in M1b).

---

### D16. Privacy screen

**Prototype.** Header button and the Pause key: "blur the whole output and mute the
mic" (`D:81`, `D:380`, `D:1572`). The mixer's mic channel shows "muted (privacy)"
(`P:1792`).

**Today.** Nothing.

**Status.** Dropped. ADR-0027 built it, and the maintainer then removed it after the alpha:
JStream has no privacy screen ([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md)
§4). What follows is the original analysis, kept for the record.

**Consequences.**
- It must act in the engine, on the program master (or on every output), so that
  outputs, the vcam and recordings are all covered. It is a runtime action
  (`privacy.set { on }`), not an undoable edit: an agent's or another client's undo must
  never un-hide the output. It emits an event, and all UIs show it.
- Which sources mute: "the mic" needs a definition. Recommended: a collection setting
  listing the sources or buses that privacy mutes (default: every audio input device
  source), plus the blur strength.
- Should privacy persist across restarts? Recommended: no, but it starts **off** with a
  visible indicator. The maintainer decides.
- MCP: agents can toggle it (ADR-0006 grants full access). It goes in the audit log.

**Recommendation.** An engine action on the program master with configurable mute
targets.

**Tier** M (product feature with safety semantics). **Urgency:** pre-M2.

---

<a id="c-audio"></a>
## C. Audio
<!-- @section
-->

### D17. Mixer UI (master + source faders) vs the ADR-0007 bus matrix

**Prototype.** The mixer shows a **master** strip ("Master · program output", with
loudness, true peak, monitor pre/post and FX), and channels fold under it (`P:580-650`;
README › Audio). Channels are mic, desktop and scene sources whose audio is on
(`mixerList`, `D:2505-2517`). Each channel has **one gain fader**, from -60 to +12 dB
(`P:1786-1795`). No buses, no sends, no tracks. Per-output audio chains live on outputs
(D14).

**Today.** ADR-0007: sources feed buses through a send matrix with a gain per send.
Buses have filter chains and feed outputs ("stream and recording tracks, and monitor
devices"). Consequence: "The UI must present the matrix simply (sends per source, a strip
per bus)". ARCH: `sends: (source_id, bus_id) → gain_db`, `Bus { id, name, filters[],
monitor_device? }`. ROADMAP M1b: "Bus-matrix mixer with filters and meters".

**Status.** Gap. The prototype only shows the default case. It doesn't contradict
ADR-0007, but it doesn't present the matrix either.

**Consequences.**
- A per-source **fader** and **mute** (pre-send) are needed beyond the send gains.
  Otherwise the single fader is ambiguous (which send does it move?). Model:
  `Source.audio { gain_db, muted, … }` plus `sends`.
- A **master bus** is the default. Every new audio source sends to it at 0 dB. It is
  what the prototype calls "Program output", and its filter chain is the "master chain"
  (audio).
- Per-output audio chains: either each output's audio chain is `Output.filters.audio`
  applied to its buses' mix (D9), or each output implicitly gets its own bus fed from
  master. Recommended: the former. Then buses stay user-visible routing, and output
  chains stay output-local.
- Recording tracks (several buses into one recording) stay possible: `Output.audio_buses`
  is a list.
- UI: an advanced "Routing" view for sends and buses; the prototype's master view is the
  default. M2 work, but the model must have master plus faders now.

**Options.**
1. **Bus matrix plus per-source fader/mute, with a default master bus.** The prototype's
   view is the simple presentation.
2. Replace the matrix with "master plus per-output chains". Simpler, but it loses
   ADR-0007's "music on the stream but not in the VOD" and needs a superseding ADR.

**Recommendation.** Option 1, which keeps ADR-0007. The maintainer should still confirm
that the advanced routing UI is wanted, since the prototype omits it.

**Tier** M to confirm the UI scope; the model is A. **Urgency:** now (M1b mixer).

---

### D18. Does audio follow scenes?

**Prototype.** The mixer lists "global devices, then scene sources whose audio is on",
walking the **current scene's** object tree (`mixerList`, `D:2505-2517`). A nested scene
has "Include its audio when nested … When off, its sources don't reach the Mixer through
this scene" (`D:1036`). Scenes count as audio-capable (`hasAudio`, `D:2490`). This is
OBS's model: a source's audio plays when its scene is live.

**Today.** ADR-0007: "Audio sources exist independently of scenes, and feed buses through
a send matrix". ARCH: `sends: (source_id, bus_id)`. Nothing in the model makes audio
depend on which scene is on program.

**Status.** **Conflict** with ADR-0007's scene-independent audio, at least for
non-device sources (browser, media, nested scenes, webcam audio).

**Consequences.**
- If audio follows scenes: the mixer thread needs a per-source **activity gain** driven
  by program, preview and transition state, with crossfades during transitions (Move and
  Fade). The render-side "active" set (D29) feeds the audio snapshot. Nested-scene audio
  include flags become edges in an audio activity graph. The mixer channel list changes
  with the program scene, so UIs and agents see channels appear and disappear.
- If audio stays scene-independent: browser alerts and media files are heard even when
  their scene is off-air. That surprises OBS users, and the prototype's mixer list makes
  no sense.

**Options.**
1. **Hybrid, OBS-like:** each audio-capable source has `audio_follows: always |
   when_shown`. Device sources (mic, desktop, app audio) default to `always`. Scene-bound
   sources (browser, media, webcam audio, nested scenes) default to `when_shown`, where
   "shown" means on program (D29), with a fade over the transition. Sends, buses and
   faders are unchanged (ADR-0007 stays, extended).
2. Pure ADR-0007: always on, and the user mutes by hand.
3. Pure OBS: everything follows scenes except "global" sources.

**Recommendation.** Option 1. It needs a maintainer ADR that **extends or supersedes
ADR-0007** (ADR-0017: superseding any ADR is maintainer tier). Even an extension changes
ADR-0007's decision text ("exist independently of scenes"), so supersede it with the
whole text restated (R-DOC-009 forbids partial amendment).

**Tier** M. **Urgency:** now. It changes the M1b mixer snapshot and the source model.

---

### D19. Per-source audio switch

**Prototype.** "Audio-capable sources (and nested scenes) can switch their audio off,
which removes them from the Mixer" (README; `AUDIO_FIELD`, `D:992`; `setAudio`,
`D:2501-2504`; right-click "Audio" check, `D:3017`). It is a source property, so
references share it.

**Today.** Nothing. The platform opens audio streams per source.

**Status.** Gap.

**Consequences.** `Source.audio.enabled: bool`. When it is off, the engine **doesn't
open** the source's audio stream (for example, it doesn't start CEF's audio handler or
the webcam microphone), the source has no sends, and it has no mixer channel. Turning it
back on restores the previous sends, so the sends are kept but inactive (which simplifies
the edit's inverse).

**Recommendation.** As above. It is distinct from mute (mute keeps the stream and the
meter).

**Tier** A. **Urgency:** now (M1b source model).

---

### D20. Sidechain and ducking edges in the audio graph

**Prototype.** Compressor with "Duck under: Nothing | Mic | Desktop audio" (`D:1144`).
A "Ducking" mastering filter: "Dip everything else while the mic talks", "Triggered by"
a source (`D:1153`). The demo puts mastering filters on the program master.

**Today.** Nothing. ARCH's mixer: sends, buses, filters and meters on the mixer thread.

**Status.** Gap.

**Consequences.**
- A filter can read **another source's signal** (the key input). The mixer must process
  in dependency order (key sources first) and give the filter a read-only view of the
  key's post-fader (or pre-fader) buffer for the same block. That is a DAG over sources
  and buses, topologically sorted when the audio snapshot is built (control thread),
  with no allocation on the mixer thread (R-RT-001).
- Cycles are possible (A ducks under B, B ducks under A, or a bus ducking under a source
  that sends into it). Reject them with a cycle error, like scenes (D10), or allow them
  with a one-block delay on the back edge. Rejecting is simpler and explicit.
- A ducking filter **on the master** that "dips everything else" can't work on a summed
  master that already contains the mic. It needs to apply to every input of the bus
  except the key, so it is a per-send gain stage, or it is defined as "ducking on a bus
  that the trigger doesn't feed". Decide the semantics: recommended, a ducking filter
  lives on a **bus** (or source), and its key must not feed that bus. So the UI offers
  it on a "Music" bus, not on the master, or the engine splits the master implicitly
  into "key" and "rest". The prototype's placement on the master is a UX shortcut that
  needs an engine definition.
- The key reference is a `ParamValue::Source` (D8), so it is indexed for delete
  rejection (D44).

**Recommendation.** Sidechain keys as typed source references. Audio-graph DAG with
cycle rejection. Ducking defined per bus or source, never on a sum containing the key.

**Tier** A (the master-placement UX is worth noting to M). **Urgency:** now. The audio
graph's shape is built in M1b.

---

### D21. Loudness metering (LUFS, true peak) and the meter stream

**Prototype.** Master strip: a loudness meter against a LUFS target, and true peak in
dBTP with "over 0 dBTP: add a true-peak limiter" (`P:580-650`; `masterVals`,
`D:2244-2259`). A loudness-target filter rides gain toward a LUFS target. A true-peak
limiter has a dBTP ceiling (`D:1151-1152`).

**Today.** ARCH: binary streams for "meters. Not designed yet" (line 283). Mixer
"metering" (crate map, `jstream-audio`).

**Status.** Gap in what meters carry.

**Consequences.**
- On the mixer thread, BS.1770 K-weighting and gating per metered bus: momentary
  (400 ms), short-term (3 s) and integrated loudness, plus true peak (4× oversampling).
  All of it is allocation-free with fixed-size state (R-RT-001). That costs CPU per
  metered bus, so meter only the buses and sources a client subscribes to, plus the
  master.
- Meter frame layout: per channel peak, RMS and true peak; per bus also momentary,
  short-term and integrated LUFS. Decide this before the binary stream PR (protocol
  README › Binary streams).
- Integrated loudness needs a reset action (`audio.reset_loudness`), and a definition of
  when it starts (at go-live?).

**Recommendation.** Design the meter frame with LUFS and true-peak fields from the
start. Subscriptions per bus or source (protocol OQ 7).

**Tier** A. **Urgency:** now (meter stream shape; M1b "mixer with filters and meters").

---

### D22. Monitoring pre or post FX

**Prototype.** Master strip toggle "Hear: before FX / after FX" (`P:1900`; `monitorPost`
in `masterVals`).

**Today.** `Bus { …, monitor_device? }` (ARCH line 177). Platform-windows open
questions: output render time for "monitoring with delay compensation".

**Status.** Gap (tap point).

**Consequences.** The monitor is a tap on a bus at a chosen point: `monitor: { device,
tap: pre_filters | post_filters }`. A per-source monitor (OBS's "monitor only" / "monitor
and output") isn't in the prototype. Decide whether it is in scope.

**Recommendation.** A tap point on bus monitoring. Per-source monitoring is deferred, and
noted as out of scope until asked for.

**Tier** A. **Urgency:** pre-M2.

---

### D23. CLAP plugin hosting

**Prototype.** A "CLAP plugin" audio filter (`D:1154`). The README itself says it is
**not settled**, because ADR-0005 rules out native dynamic plugins in v1.

**Today.** ADR-0005: "No native dynamic plugins in v1". Consequence: "Third parties can't
add capture devices or encoders until a later ADR allows it". R-SEC-003: plugins are
sandboxed by default.

**Status.** **Conflict** with ADR-0005.

**Consequences.**
- In-process CLAP means native code from third parties on the mixer thread. A crash
  takes down the engine (exactly what ADR-0005 avoids), and real-time safety can't be
  enforced (R-RT-001). Licensing: CLAP itself is MIT, and the plugins vary.
- Out-of-process hosting: a `jstream-plugin-host.exe` per plugin (or per chain), with
  audio through shared-memory rings. That adds latency of one block or more, which is
  reported through D15, and needs a bounded wait with bypass on a miss, to keep R-RT-001.
  Crashes are isolated. It is a new `unsafe` crate and a new binary in the portable zip
  (ADR-0020).

**Options.**
1. Keep ADR-0005 (no CLAP). Offer the built-in filters and possibly WASM DSP later.
2. **Out-of-process CLAP host**, superseding ADR-0005 for audio effects only.
3. In-process CLAP. Fastest, but contradicts ADR-0005's core rationale.

**Recommendation.** Option 2 if the maintainer wants it, not before M3. Until then the
UI hides the CLAP filter.

**Tier** M (supersedes ADR-0005). **Urgency:** later. It does not affect M1 shapes, as
long as D15's filter-latency reporting exists.

---

<a id="d-transitions-and-studio"></a>
## D. Transitions and studio
<!-- @section
-->

### D24. Transition presets, a default, and per-scene in/out overrides

**Prototype.** Presets `{ id, name, kind: Cut | Fade | Move, duration, easing: Ease
in-out | Ease out | Linear | Spring, unmatched: Fade | Scale | Slide, stagger }`, with a
`defaultId` (`D:1216-1229`). Per-scene overrides: `into[scene]` and `outOf[scene]`, where
"the destination's 'into' wins" (`transFor`, `D:2608-2617`). They are set from
right-click, Properties and the Inspector (`P:1601-1613`). Deleting a preset clears the
overrides that use it (`D:2818-2823`), and the default can't be deleted.

**Today.** ARCH: `studio { …, transition }`, a single transition. ROADMAP M2: "Studio
mode (Preview/Program) and transitions". Protocol: `studio.transition` is listed as an
action (ARCH › Control protocol).

**Status.** Gap.

**Consequences.**
- Collection: `transitions { presets: [Preset], default: PresetId }`, plus
  `Scene.transition_in` / `transition_out: Option<PresetId>`.
- Edits: `transition.create/update/delete/set_default` and `scene.set_transitions`.
  Deleting a preset that overrides use either cascades (an inverse that restores them)
  or is rejected like other references (D44). The prototype cascades.
- Actions: `scene.switch { scene_id, preset?: PresetId }` (D26). The resolution rule
  (`into(to) ?? outOf(from) ?? default`) lives in the engine, so agents and UIs agree.
- Spring easing overshoots (`ease`, `D:2668`), so interpolated values can leave their
  range (crop below 0, opacity above 1). The compositor must clamp.

**Recommendation.** As above, specified in the transitions ADR together with D25.

**Tier** M (product feature set; README asks for the ADR). **Urgency:** pre-M2. Reserve
`Scene.transition_in/out` in the model now.

---

### D25. Move transition: pairing, retargeting, per-item settings

**Prototype.**
- **Pairing:** Move pairs items **by source** between the outgoing and incoming scenes
  (`srcId(f.id) === srcId(it.id)`, `D:2700-2711`).
- **Interpolation:** x, y, w, h, crop, z and perspective linearly; rotation, tilt and
  turn the shortest way (`sampleAnim`, `D:2732-2778`).
- **Unmatched items** fade, scale or slide toward the nearest edge.
- **Retargeting:** "Switching mid-transition retargets from what is on screen": the
  `from` state is `frameNow()` sampled from the running animation (`D:2672-2682`).
- **Per item and per scene** (`itemTrans['Scene|item']`): delay, easing (or inherit),
  "Glide to other scenes" (Auto | Never), and appear/disappear effects (`itemSet`,
  `D:2780-2783`; Inspector "SCENE SWITCHES", `P:1615-1643`).
- **Stagger** per item index.
- **Group children** are paired individually, because the tree is flattened
  (`sceneSources`, `D:2653-2656`).

**Today.** Nothing. The README asks for an ADR.

**Status.** Gap.

**Consequences.**
- **Render:** the Program mix during a Move is **not** "blend texture A with texture B".
  The compositor draws a synthetic item list whose per-item transforms and alpha come
  from the transition state. The render plan (D11) must support a transition node that
  references both scenes' items and their source textures, with per-item uniform updates
  per tick. That fits R-RT-002 (bounded work), provided pairing is computed **on the
  control thread** when the switch starts, and the render thread only interpolates.
- **Retargeting:** the render thread owns the live interpolated state. A new switch
  mid-transition must start from the current sampled values. The control thread can't
  sample render-thread state without a channel, so either the render thread does the
  pairing itself from a preallocated plan, or it publishes the current per-item state
  (lock-free) for the control thread to read. Decide: the render thread resolves
  `from = current sample`, and the control thread sends `(target scene, pairing by
  source, per-item settings)`.
- **Nested scenes:** does Move look inside groups (pair children) or treat a group as
  one item? The prototype flattens. Flattening breaks the "a group renders to a texture"
  model (D4, D11) during the transition, so group filters and blend would vanish for the
  duration. Recommended: pair at the top level, and look inside a group only when the
  same group scene is on both sides **and** has no filters or non-normal blend
  (flattenable).
- **Duplicate placements** of one source in a scene: pairing by source is ambiguous.
  The prototype takes the first unused match. Define a deterministic rule (stacking
  order, then nearest position).
- **Audio** during the transition: crossfade "when shown" sources (D18) with the Move
  duration.
- **Data:** `SceneItem.transition { delay_ms, easing: Inherit | …, glide: Auto | Never,
  appear: Inherit | Fade | Scale | Slide | None, disappear: … }`, which is per placement,
  and so keyed by item ID (another reason for D3/D4's identity-preserving edits).

**Recommendation.** Write the transitions ADR (D24 + D25) before M2. The model fields
can be reserved now.

**Tier** M (feature scope; README asks for an ADR). **Urgency:** pre-M2.

---

### D26. Scene switching is an action; program and preview are runtime state

**Prototype.** Clicking a scene, pressing F1–F5 or choosing "Switch to it" switches the
**program** with the resolved transition (`switchScene`, `D:2678-2714`; hotkeys at
`D:1570-1579`). The current scene gets a "program" badge in Scenes (`P:1470`). Studio
mode is a **stub button** (`D:82`), and "Show in preview" is a context item.

**Today.** ARCH puts `studio { program_scene, preview_scene, transition }` **in the
collection** (line 174). Protocol open question 5: "`studio.transition` changes the
program scene, which is saved in the collection. Is it an action that also bumps `rev`,
or an edit plus a separate 'run transition' action?"

**Status.** Gap (OQ 5 open). The prototype makes the stakes clear: switches happen on air
by hotkey, constantly.

**Consequences.**
- If `program_scene` is an edit, Ctrl+Z switches scenes on air, and per-client undo
  interleaves scene switches with layout edits. If it is an action that bumps `rev`, it
  muddies "rev counts edits".
- Recommended: program and preview scenes are **runtime state**, like output state.
  - Actions: `scene.switch { scene_id, preset? }` and `studio.set_preview`.
  - Events: `studio.changed { program, preview, transition_running }`.
  - Not undoable, no `rev`.
  - The engine persists "last program scene" in app settings for restart convenience,
    not in the collection's undoable state.
  - Remove `studio` from the ARCH collection tree, and keep the default preset there
    (D24).
- An agent's `expected_rev` doesn't protect against a concurrent switch. That is fine,
  since switching isn't an edit.

**Recommendation.** As above. Resolve protocol OQ 5 this way in the studio ADR.

**Tier** A (technical, within ADR-0013's edit/action split). **Urgency:** now. ARCH's
data model still lists `studio` in the collection, and the M1b demo has two scenes.

---

### D27. Each client's editing scene is local; no studio-mode coupling

**Prototype.** The canvas edits the *current* scene, which is also program. Selecting a
scene in Scenes shows it on the canvas and switches program (`switchScene`).

**Today.** Nothing. The preview is "a protocol stream, not an Output" (ARCH line 195).

**Status.** Gap.

**Consequences.**
- With two UIs (desktop plus a browser) or an agent working on a non-program scene,
  "the scene being edited" must be **client-local** UI state. Each client subscribes to
  a preview stream of the scene it edits (D36). In non-studio mode, a UI may choose to
  switch program on scene click (prototype behaviour) by sending `scene.switch`.
- Studio mode: the shared Preview is engine state (it feeds a `VideoMix` that outputs may
  consume), while the editing scene remains per client. Clicking a scene in studio mode
  sets the preview (`studio.set_preview`).

**Recommendation.** Client-local editing scene. A UI preference decides "click = switch
program" (non-studio) or "click = set preview" (studio).

**Tier** A. **Urgency:** pre-M2.

---

<a id="e-sources"></a>
## E. Sources
<!-- @section
-->

### D28. Source type list

**Prototype.** `SOURCE_TYPES` (`D:969-988`):
- **Capture:** display; window; video capture device (webcam or capture card, with
  audio); **Spout2 receiver**.
- **Audio:** audio input; desktop audio; **application audio**.
- **Media and web:** **media file** (video or audio, looping); image (PNG, JPG, WebP,
  **animated GIF**); browser.
- **Generated:** text; colour; **audio waveform**; **audio spectrum**.
- **Scenes:** scene.

Property schemas are at `D:993-1086`.

**Today.** ROADMAP M1b: window, webcam (incl. MJPEG decode), image, colour, text, browser
(ADR-0021), and scenes. M1a: display plus WASAPI mic and loopback. "Later": **media-file
source**. ADR-0008: process loopback (app audio) is the chosen API. ARCH crate map:
`jstream-sources` built-ins "display, window, webcam, image, color, text, browser, media
file, scene". The README asks for "an ADR for the source type list (product scope:
maintainer tier)".

**Status.** Gap (scope). Pieces are covered (browser by ADR-0021, app audio by ADR-0008);
Spout2, waveform, spectrum and media-file timing are new.

**Consequences, per new type.**
- **Spout2:** Windows-only. It receives a D3D11 shared texture whose handle comes from
  Spout's shared-memory sender directory. It can be written in Rust in
  `jstream-platform-windows` (unsafe, under R-UNSAFE rules) without the C++ SDK. It
  imports through the same interop path as capture (ADR-0004, S1). macOS's equivalent is
  Syphon, so there is a parity question for M3. Product scope, because it adds a
  Windows-only type.
- **Media file:** MF demux and decode (ADR-0003 consequence: "Media-file sources depend on
  OS demux and decode APIs"). The `VideoDecoder` trait exists in `jstream-platform`. It
  needs looping, restart on show (D29), and A/V sync on the media clock. Moving it from
  "Later" into M1b or M2 is a roadmap decision.
- **Waveform and spectrum:** video sources that **read audio**. The mixer thread
  publishes per-source analysis (peaks, FFT magnitudes) into a lock-free triple buffer
  that the render thread reads. That is a new audio → render data path (R-RT-003), and
  the FFT must be allocation-free on the mixer thread or run on a separate analysis
  thread. A setting references a source (`ParamValue::Source`, D8, D44).
- **Video capture device:** see D30. Its audio is a second, paired audio stream.
- **Text and image:** see D32 (decoding and fonts).

**Recommendation.** One maintainer ADR, "Source types for v1", listing every kind with
its milestone. Proposed: M1b gets the ROADMAP list plus app audio. M2 gets waveform,
spectrum and media file. Spout2 comes when wanted, Windows-only, with a Syphon follow-up
for M3.

**Tier** M. **Urgency:** now. M1b is building the list, and the descriptors (D8) depend
on it.

---

### D29. Source lifecycle: what "shown", "active" and "visible" mean

**Prototype.** Settings that depend on visibility:
- browser: "Shut down when not visible", "Refresh when the scene becomes active"
  (`D:1016-1017`);
- webcam: "Turn the camera off when not shown" (`D:1006`);
- media: "Restart when it becomes visible", "Close the file when not visible"
  (`D:1065-1067`);
- image: "Unload when not showing" (`D:1029`).

Masks render scenes that are never shown directly (D10).

**Today.** ADR-0021 lists "shut down when not visible" and "refresh when shown" for the
browser source, without defining "visible". Nothing else.

**Status.** Gap. It must be defined once, engine-wide.

**Consequences.**
- Two engine-computed sets, refreshed when the render plan is rebuilt (D11):
  - **in use:** reachable from any rendered node, meaning program, preview, any output's
    mix, any preview stream or projector (D36), a mask reference, or a transition in
    progress;
  - **on program:** reachable from the Program mix.
- "Visible" settings key off *in use* (so an editor preview keeps the webcam on).
  "Becomes active" settings key off *on program* (OBS semantics). Audio-follows-scene
  (D18) keys off *on program*.
- Starting and stopping sources is control-thread work, triggered by snapshot changes,
  never on the render thread. Events `source.activity_changed` let UIs and agents see it.

**Recommendation.** Define both sets in ARCH › Data model, and use them in
`jstream-sources`.

**Tier** A. **Urgency:** now (M1b sources and the browser source).

---

### D30. Stable capture targets and device modes

**Prototype.**
- Window capture: "If the window closes, match a new one by: Title, then executable /
  Executable only / Title only" (`D:1040`).
- App audio: pick by application name ("Spotify, Discord…"), "Include its child
  processes" (`D:1057-1061`).
- Webcam: resolution, frame rate, format (MJPEG, NV12, YUY2), "MJPEG is decoded on the
  GPU", "Buffer frames" (`D:1000-1008`).
- Display: "Hide the yellow capture border (needs Windows 11)", cursor, HDR tone-map
  (`D:994-999`).
- Audio input: "Downmix to mono" (`D:1050`).

**Today.**
- `CaptureTarget::{Display(DeviceId), Window(DeviceId)}`: no video devices, and no
  re-matching (`capture.rs`).
- `AudioInputTarget::ProcessLoopback { pid }` (`audio.rs`): a PID changes every launch,
  and there is no include/exclude-tree mode.
- ROADMAP M1b: "webcam (incl. MJPEG decode)".

**Status.** Gap in the platform traits.

**Consequences.**
- Persisted settings must store **stable identities** (executable path or name, window
  class and title pattern, device symbolic link) rather than runtime IDs. The source
  re-resolves them to a `DeviceId` or PID when the target appears or disappears. That is
  control-thread logic in `jstream-sources`, with OS window and process watching from the
  platform layer.
- `ProcessLoopback` gains `mode: IncludeTree | ExcludeTree`, which maps to WASAPI's
  `PROCESS_LOOPBACK_MODE`.
- A video-device trait (`VideoDevices::list() -> modes`, `open(mode)`), separate from
  `Capture` or as a new `CaptureTarget` variant, with paired audio-device discovery.
- MJPEG GPU decode through MF/DXVA uses the `VideoDecoder` trait.

**Recommendation.** Extend the traits in M1b as above. This answers part of the
platform-windows open question on how capture reaches the GPU context.

**Tier** A. **Urgency:** now.

---

### D31. Asset file references and portability

**Prototype.** File paths in settings: images (`C:\Users\jeffm\Pictures\…`), LUTs
(`C:\LUTs\…`), shaders (`effects\crt.wgsl`, relative), media files (`D:1028`, `D:1063`,
`D:1125`, `D:1141`).

**Today.** ADR-0020: portable mode keeps state beside the exe on a USB stick. Collections
are JSON with migrations. Nothing on assets.

**Status.** Gap.

**Consequences.** Typed `ParamValue::Asset { path }` (D8), resolved against, in order:
the collection's folder, an app asset folder, then absolute paths. Relative paths are
stored when the file is under the collection or portable folder, so a portable setup
survives a moved drive letter. Missing assets are reported as source errors, not load
failures. Agents need `asset.list` or a path check query. A remote browser UI picking a
file means the server must list files (it can't use a local file dialog). That is a
protocol capability (`fs.list` under the token, scoped to allowed roots), with a security
review under ADR-0021's "no file access outside an explicit local file or folder" spirit.

**Recommendation.** Typed asset params with relative resolution, plus a scoped `fs.list`
query for remote UIs.

**Tier** A. **Urgency:** now (persistence lands in M1b; image source needs it).

---

### D32. Image, GIF and font decoding vs ADR-0003's "OS codecs only"

**Prototype.** Image source: "PNG, JPG, WebP or animated GIF" (`D:981`). Text source with
fonts (IBM Plex Sans/Mono, Segoe UI, Consolas) (`D:1021`). LUTs as `.cube` or `.png`
(`D:1125`).

**Today.** ADR-0003: "Encoding and decoding use **OS and GPU-vendor APIs only**. No
FFmpeg, GStreamer, or bundled C/C++ codec libraries" (enforced by R-DEP-002). The
context is audio and video codecs, but the decision text is unqualified.

**Status.** Ambiguity in ADR-0003. Pure-Rust image decoders (`png`, `jpeg-decoder`,
`image-webp`, `gif`) aren't C, but they aren't OS APIs either. The alternative is WIC
(the Windows Imaging Component), an OS API, plus ImageIO on macOS and nothing equivalent
on Linux. Text needs font discovery and shaping: DirectWrite (OS), or pure-Rust
`cosmic-text`/`swash`/`fontdb`.

**Options.**
1. Read ADR-0003 as **media (audio and video) codecs only**. Pure-Rust image and font
   libraries are allowed under R-DEP-001 and R-DEP-004. Record this as an explicit
   clarification ADR. ADR-0003 can't be amended, so it is a superseding ADR restating
   the whole decision.
2. OS APIs for images and text (WIC, DirectWrite). Matches the letter of ADR-0003, but
   every platform crate grows, and Linux has no image codec API.

**Recommendation.** Option 1, which is cross-platform and consistent with "All JStream
code is Rust". Under ADR-0017 this is maintainer tier: it supersedes ADR-0003 in wording,
even if only to clarify.

**Tier** M. **Urgency:** now. The image and text sources are M1b.

---

<a id="f-protocol-and-clients"></a>
## F. Protocol and clients
<!-- @section
-->

### D33. Continuous edits: scrubbing, drags, live dialogs with Cancel; undo granularity

**Prototype.**
- Every numeric field scrubs by drag, wheel or arrows, writing on every pointer move
  (`field()`, `P:1313-1377`).
- Canvas drags call `setXfMany` on every move (`D:1395-1541`).
- Mixer faders scrub.
- The filter dialog "edits preview live; Cancel restores the original" (`D:2181`,
  `feVals` restore at `D:2207`).
- The properties dialog "edits a draft and applies it on OK" (`D:2003-2004`).

**Today.**
- ADR-0013: every applied batch bumps `rev` and is one undo step.
- ARCH › Threads: "Continuous parameters (fader moves, transform drags) bypass snapshots
  and go through lock-free parameter channels", which covers the engine side but not the
  protocol or undo side.
- Protocol OQ 4: undo and redo on the wire are undecided. The core README says there is
  no undo history yet.

**Status.** Gap. At 60 Hz a drag would be 60 batches a second, 60 `rev` bumps and 60
undo steps, with a `state.changed` broadcast to every client each time.

**Options.**
1. **Live values plus a commit:**
   - While dragging, the client sends `live.set { target, field, value }`, a
     non-undoable, rev-less, rate-limited message. The engine applies it through
     parameter channels and broadcasts a coalesced `live.changed` event.
   - On release, the client sends the real edit (from the start value to the end value)
     as one batch: one `rev` bump, one undo step.
   - Cancel sends `live.clear`, which reverts to collection state.
   - Live values are dropped when the client disconnects or a timeout passes.
2. **Coalescing edits:** batches carry a `coalesce_key`. Consecutive batches from one
   client with the same key merge into one undo step, but each still bumps `rev`, and
   the event traffic stays high.
3. Client-side throttling only (send a batch every 100 ms). Simple, but it still yields
   many undo steps and a choppy engine preview.

**Recommendation.** Option 1. It fits ARCH's parameter channels, keeps `rev` meaningful
for agents, and makes the dialog's Cancel and the fader drag cheap. Settle it together
with OQ 4 (`history.undo` / `history.redo`). The live-value layer sits in front of
collection state, so ADR-0013's edits are unchanged.

**Tier** A (within ADR-0013; it adds a message kind, so it is an ADR under R-ARCH-004).
**Urgency:** now. Per-client undo is built in M1b, and the protocol call kinds are being
fixed.

---

### D34. Multi-client: selection, presence, conflicting edits

**Prototype.** Selection is a workspace-local `sel` and `selIds` (`D:1233-1234`). The
Agent activity panel shows MCP actions next to the user's (`P:715-720`). Nothing handles
another client deleting what you have selected.

**Today.** ADR-0013: per-client undo, `expected_rev`, and `undo_conflict`. ADR-0006:
every agent action goes to the audit log. Events carry `client_id` (`event.rs`).

**Status.** Partly covered.

**Consequences.**
- Selection, the editing scene (D27), folds, searches and tool modes stay client-local.
- Optional `client.presence { selection, editing_scene }` (ephemeral, no `rev`) lets two
  UIs, or a UI and an agent, see each other's focus, and lets an agent act on "what the
  user selected". Useful for MCP ("move the selected item"), but not required.
- UIs must handle `state.changed` from others: drop selected IDs that vanished, and keep
  in-flight drags working via live values (D33). On commit after a concurrent change,
  sending without `expected_rev` is the right default for drags (last writer wins per
  field), and with `expected_rev` for structural batches.

**Recommendation.** Client-local selection. Presence as an optional later addition. Write
the UI's concurrency rules into the `jstream-ui` design when M2 starts.

**Tier** A. **Urgency:** pre-M2.

---

### D35. Workspace layout persistence: client side or server side?

**Prototype.** Eight dock zones, tab groups, sizes and collapsed state. Named layouts
("Editing", "Live", "My layout N") are saved and reset (`EDITING`/`LIVE`, `D:1181-1188`;
layout menu, `D:3491-3500`). Meter orientation is a preference (`P:1781-1784`).

**Today.** Nothing. R-ARCH-001 forbids UI-only *features*, not UI-only presentation.

**Status.** Gap.

**Options.**
1. Browser storage per origin. The desktop app and each remote browser then have
   different layouts, and portable mode (ADR-0020) loses them when the WebView2 profile
   is elsewhere.
2. **A server-side opaque per-UI settings store:** `ui_settings.get` / `ui_settings.set`
   (an action, not undoable, not in the collection), keyed by UI kind, stored in the
   config directory (portable-aware). Same layout on desktop and browser.
3. In the collection. Wrong: switching collections would switch layouts, and edits would
   pollute undo.

**Recommendation.** Option 2. The UI owns the schema, and the engine stores the blob.

**Tier** A. **Urgency:** pre-M2.

---

### D36. Preview streams for any target, pop-outs, projectors and multiview

**Prototype.**
- The editing canvas shows the current scene.
- "Pop out": "Source in a window" or "Scene in a window", or "Full screen on a monitor"
  (a projector) (`D:1913-2000`).
- Scene property "Show in multiview" (`D:1035`).
- "Show in preview" (studio).
- Filter dialogs preview live (D33).

**Today.** ADR-0002: preview is low-res H.264 over binary WebSocket frames, decoded with
WebCodecs, with a JPEG fallback. "Preview needs its own small encoder session". ARCH:
"The preview shown in the UI is a protocol stream, not an Output". The binary streams are
deferred, and their intended shape uses subscriptions.

**Status.** Partly covered. One preview is planned; the prototype needs several, with
arbitrary targets.

**Consequences.**
- `preview.open { target: Mix(id) | Scene(id) | Source(id) | Output(id), max_size, fps,
  stage: pre_master | post_master }` returns a stream ID, plus `preview.close`. Each open
  stream adds a render node (D11) and marks its target *in use* (D29).
- **Encoder sessions:** every H.264 preview is a hardware session, and vendors cap them.
  Recommended: the engine composes all of a client's previews into **one atlas texture**
  and encodes it once (one session per client, or per engine). The client crops regions
  out. Multiview is then the same mechanism.
- **Fullscreen projectors** for confidence monitors: preview-stream latency (ADR-0002
  accepts "a few frames") may be too high. The option is a native projector window
  rendered by the engine (wgpu surface) in the desktop host. That is a host-and-platform
  addition (`jstream-desktop` plus render surfaces), which breaks "the UI talks to the
  engine only over WebSocket" for this one case. Maintainer call, because ADR-0002
  frames the UI as web-only.
- Pre or post master chain: see D14.

**Recommendation.** Atlas-based multi-target previews in the binary-stream design.
Native projector windows proposed as a maintainer decision (an ADR-0002 extension) for
M2.

**Tier** A for the stream shape, M for native projectors. **Urgency:** pre-M2. The
binary-stream PR should already allow multiple targets.

---

### D37. Item geometry for the canvas overlay: source sizes as runtime state

**Prototype.** The canvas draws handles, snapping guides, marquee hits, alignment and
"% of the source's own size" from item boxes and native sizes (`NATIVE`, `P:878`;
`snapCands`, `D:1361-1371`; `bounds`, `D:1341-1349`). Window captures change size at
runtime.

**Today.** `CaptureStream::latest` notes that "Its size can change between frames"
(`capture.rs`). No protocol query or event carries a source's current size. `ItemInfo`
has only the stored transform and crop.

**Status.** Gap.

**Consequences.** Add `SourceInfo.size: Option<{w, h}>` (runtime, not `rev`-bound) and an
event `source.size_changed`. Alternatively an engine query `item.geometry { scene_id } →
[projected quad per item, nested items included]`, which keeps the D1/D12 projection math
in one place. The UI needs geometry every frame while dragging, so it should compute
locally from sizes plus the transform formula. That requires the formula to be specified
exactly, and shared as a Rust function in `jstream-protocol` (or `jstream-types`) used by
both the engine and the WASM UI. Recommended: the shared function plus size events.

**Tier** A. **Urgency:** now. It goes with D1, because the formula is the transform's
definition.

---

### D38. Audit log over the protocol

**Prototype.** The "Agent activity" panel lists entries like "MCP · 2 min ago: Switched
to 'Desktop + Cam'" and "Mic gain −1.5 → −3.0 dB" (`P:715-720`). It is in the default
Live layout (`D:1186`).

**Today.** ADR-0006 and ARCH › MCP: every agent action goes to the audit log, with the
client's name. The core README says the audit log is an M1b item. There is no protocol
access to it.

**Status.** Gap. R-ARCH-001 requires it to be in the protocol for the UI to show it.

**Consequences.** Query `audit.list { since, limit, client? }` and event
`audit.appended`, with human-readable summaries computed by the engine. "Gain −1.5 →
−3.0" needs before and after values, which the inverse batch already has. Clients need
names (from `hello`), and live values (D33) are **not** audited per tick, only commits.

**Tier** A. **Urgency:** pre-M2. Design the audit record format in M1b with the log.

---

### D39. Header and stream stats; event subscriptions

**Prototype.** Header: "60 fps · 0 dropped · CPU 3%" (`D:79`). Stream stats panel: uptime,
watching now and peak, likes, chat per minute, dropped frames, a two-minute bitrate
chart, health ("Excellent/OK/Waiting"), and video/audio/latency/ingest/key details
(`ytVals`, `P:1155-1219`).

**Today.** ROADMAP M1d: `stats.tick`. R-RT-004: drop counts in `stats.tick`. ADR-0009:
stream health from the YouTube API. Protocol OQ 7: high-rate events need opt-in
subscriptions.

**Status.** Partly covered.

**Consequences.**
- `stats.tick` should carry per-output bitrate, dropped and encoded frames, render and
  encode times, and CPU and GPU load, with the bitrate history kept client-side.
- Viewer counts, likes and chat rate are YouTube API reads (D40), polled under quota.
  They need their own low-rate event (`youtube.stats`).
- All of these make OQ 7's subscription mechanism necessary before M2.

**Tier** A. **Urgency:** M1d / pre-M2.

---

### D40. YouTube chat: send as channel, feature on stream, audience stats

**Prototype.** The chat panel reads chat with badges and Super Chats, has a pinned
message, **sends as the channel** ("Chat as JStream…"), and offers **"Show this message on
stream"** (`P:548-578`; `sendChat`, `P:1882-1887`; `featured`, `P:1797-1807`). Stats show
viewers and likes (`P:1182-1184`).

**Today.** ADR-0009: OAuth plus the Live API for "creating and scheduling broadcasts,
binding streams, setting metadata, and reading stream health and live chat". ROADMAP M3:
`jstream-youtube` with chat.

**Status.** Gap beyond ADR-0009. **Sending** chat, **audience stats**, and a **feature on
stream** overlay are not in its scope.

**Consequences.**
- Sending needs the `youtube.force-ssl` write scope and quota for `liveChatMessages.insert`.
- "Show on stream" needs a **source** that renders the featured message. Either a native
  "chat message" source kind (text plus styling, fed by the engine), or a local page
  served to a browser source. The latter needs ADR-0021's "allow page to control
  JStream" or a read-only data feed. That is a new source kind (D28).
- Polling viewers and likes costs quota alongside chat polling. Rate policy needed.

**Recommendation.** A maintainer ADR, "YouTube interaction scope v2", for M3.

**Tier** M. **Urgency:** later (M3).

---

### D41. Go Live and broadcast creation

**Prototype.** The broadcast picker and "Create broadcast": title, description,
visibility, latency (Normal, Low, Ultra-low), start now or schedule, category, DVR, made
for kids. "Go Live" / "End stream" (`stVals`, `D:1849-1911`).

**Today.** ADR-0009 covers creating and scheduling broadcasts, binding, metadata, and
`youtube.go_live` (ARCH › Control protocol).

**Status.** Covered. A write-up only: list the broadcast fields in the `jstream-youtube`
design when M3 starts.

**Tier** —. **Urgency:** M3.

---

### D42. Hotkeys

**Prototype.** Scene hotkeys F1–F5 switch scenes "from anywhere". Pause toggles privacy.
Hotkeys are stored as a scene property (`D:1570-1579`; `PROP_DEFAULTS`, `D:1092-1093`).

**Today.** Nothing.

**Status.** Gap.

**Consequences.** Hotkeys from "anywhere" means OS-global shortcuts, which only the
desktop host can register (Tauri's global-shortcut plugin), and which a remote browser
can't. Bindings `{ chord → action }` belong in the collection (D7) or in app settings.
Dispatch belongs in the desktop host, calling protocol actions, so every hotkey action is
a protocol action (R-ARCH-001). Headless daemon: none.

**Recommendation.** Bindings in the collection (they are show-specific). Global dispatch
in `jstream-desktop`. In-page dispatch in `jstream-ui` when focused.

**Tier** A. **Urgency:** pre-M2.

---

### D43. Copy, paste and duplicate scene

**Prototype.** Context menu: Paste, Paste scene, Duplicate scene, and filter
copy-through-clone (`D:2886-3060`).

**Today.** Client-chosen IDs (protocol OQ 3) make a duplicate a client-built batch:
`scene.create`, then `item.add` for each item with new IDs and the same `source_id`, then
`filter.add`, and so on.

**Status.** Covered by batches. The clipboard is client-local.

**Consequences.** A `scene.duplicate` edit is optional, but helps agents. Item-level
settings (D25 transition settings, D2 opacity and blend) must be readable through
`state.get` for a faithful client-side copy. So keep `ItemInfo` complete ("it carries
every property an edit can set", `query.rs`), which is already the rule.

**Tier** A. **Urgency:** pre-M2.

---

### D44. Deleting referenced objects (protocol OQ 12)

**Prototype.** Deletes cascade freely: deleting a preset clears overrides
(`D:2818-2823`), "Delete scene" and "Delete keyframe" (`D:2596-2606`). Scenes are
referenced by nesting, masks and keyframes; sources by sidechain keys and waveform
settings.

**Today.** Rejection: `source.delete` and `scene.delete` fail while referenced, and the
client removes the blockers first, in the same batch if it likes (graph README).
OQ 12 asks whether to add a cascading delete.

**Status.** Gap. The prototype multiplies the reference kinds (D5, D10, D20, D24), so
"remove the blockers first" becomes a long, kind-specific client algorithm in every
client, MCP agents included.

**Recommendation.** Keep rejection as the primitive, with errors listing every blocker.
Add one engine-side **planning query**, `delete.plan { target } → Batch`, that returns the
batch which removes the blockers and the target. The client shows it ("This also removes
the mask on Webcam"), then sends it as an ordinary atomic batch with `expected_rev`. Undo
then stays a plain inverse batch, and the edits stay narrow.

**Tier** A. **Urgency:** now. The reference kinds grow in M1b.

---

<a id="proposed-adr-grouping"></a>
## Proposed ADR grouping
<!-- @section
-->

Numbers are provisional (the next free one is 0022).

| ADR | Title | Decides | Tier | When |
|---|---|---|---|---|
| ADR-0022 | Item transform and canvas coordinates | D1, D12 (shape), D37 (shared geometry function, size events) | agent (protocol meaning change per R-ARCH-004 and R-API-001) | **now** |
| ADR-0023 | Placements, references and clones | D2, D3; amends ARCH data model and the M1b "item effects" bullet | **maintainer** | **now** |
| ADR-0024 | Scene structure: groups, keyframe scenes, folders, organisation metadata | D4, D5, D6, D7 | **maintainer** (new product concepts); D6 and D7 shapes could be agent but ride along | now (shapes), build M1b/M2 |
| ADR-0025 | Typed settings through kind descriptors | D8, D31 (asset params), D44 (`delete.plan`); settles protocol OQ 11 and 12 | agent | **now** |
| ADR-0026 | Filter chains and the render graph | D9, D10, D11, D13, D15 (video-delay part) | agent | **now** |
| ADR-0027 | Program master, per-output chains and the privacy screen | D14, D16 | **maintainer** (feature scope, safety semantics) | pre-M2 (Output shape now) |
| ADR-0028 | Audio model v2: faders, master bus, audio that follows scenes, sidechains, loudness, monitoring | D17, D18, D19, D20, D21, D22, D15 (audio latency); **supersedes ADR-0007** (restated in full) | **maintainer** | **now** |
| ADR-0029 | Transitions: presets, overrides and Move | D24, D25 | **maintainer** | pre-M2 (reserve fields now) |
| ADR-0030 | Studio state and scene switching | D26, D27; settles protocol OQ 5 | agent | now |
| ADR-0031 | Source types for v1, and decoding scope | D28, D32 (supersedes ADR-0003 to clarify "codecs" as audio and video); D30 platform-trait changes can be a separate agent ADR or a `jstream-platform` design PR | **maintainer** | **now** |
| ADR-0032 | Live parameter edits and undo on the wire | D33; settles protocol OQ 4 | agent | **now** |
| ADR-0033 | Client-side state, previews and presence | D34, D35, D36 (stream shape), D38, D39, D42 | agent; **native projector windows** split out as a maintainer ADR extending ADR-0002 | pre-M2 |
| ADR-0034 | YouTube interaction scope v2 | D40 | **maintainer** | M3 |
| ADR-0035 | Native audio plugins (CLAP), out of process | D23; **supersedes ADR-0005** for audio effects | **maintainer** | later (only if wanted) |

The minimum set before more M1b code lands: **0022, 0023, 0025, 0026, 0028, 0031, 0032**.
Of these, 0023, 0028 and 0031 need the maintainer.

<a id="architecture-md-and-roadmap-edits"></a>
## ARCHITECTURE.md and ROADMAP edits
<!-- @section
-->

### Agent tier: write-ups of direction that is already accepted

These describe what the existing ADRs and code already decide, or what an agent-tier ADR
above decides.

1. **ARCH › Data model, references:** reword the shared-source and duplicate bullet in
   the prototype's vocabulary: "add existing" = reference, "duplicate" = clone. Ownership
   is *not* part of the model. Covered by ADR-0007 and the current graph.
2. **ARCH › Data model:** add `Collection.canvas` and the transform definition, after
   ADR-0022.
3. **ARCH › Data model:** split `filters[]` into video and audio chains, and add scene
   and output chains, after ADR-0026. Replace "Filters and effects" accordingly, once
   ADR-0023 settles item effects.
4. **ARCH › GPU pipeline:** add a "Render graph" subsection (D11), with the plan built
   on the control thread, nodes memoised once per tick, and a measured texture budget.
5. **ARCH › Threads:** add the mixer's pts-preserving latency contract (D15) and the
   audio → render analysis path for waveform and spectrum sources (D28, once in scope).
6. **ARCH › Control protocol:** a "Live values" bullet (D33), `preview.open` with
   multiple targets (D36), `audit.list` (D38), and subscriptions (OQ 7), after
   ADR-0032 and ADR-0033.
7. **ARCH › Data model:** remove `studio` from the collection tree and describe program
   and preview as runtime state (D26), after ADR-0030.
8. **ARCH › Data model:** define the *in use* and *on program* source sets (D29).
9. **ARCH › Persistence:** asset path resolution (D31), and the server-side UI settings
   store (D35).
10. **ROADMAP › M2:** name the dockable workspace (direction D, chosen per
    `design/ui-prototype/README.md`) and link the prototype as the design reference.
    Add the binary multi-target previews and the audit panel. This records an accepted
    choice; it doesn't make one.
11. **`design/ui-prototype/README.md` › Follow-ups:** link each follow-up to its ADR
    once opened.
12. **`jstream-protocol` README › Open questions:** mark OQ 4, 5, 11 and 12 as answered
    by their ADRs, once accepted.

### Need a maintainer decision first

1. **ARCH › Data model, SceneItem:** blend, opacity and effects placement (D2, ADR-0023).
   It also changes the **ROADMAP M1b** bullet "item effects apply to one placement".
2. **ARCH › Data model, audio** and **ADR-0007's** "independent of scenes":
   audio-follows-scene, faders and the master bus (D17, D18, ADR-0028).
3. **ARCH › Data model:** groups as scenes, keyframe scenes, folders (D4, D5, ADR-0024).
4. **ARCH › Data model, outputs:** master and per-output chains, encoder-sharing on
   derived mixes, privacy (D14, D16, ADR-0027).
5. **ARCH › Crate map, `jstream-sources` row**, and **ROADMAP M1b/M2/Later:** the source
   list (Spout2, media file moving from Later, waveform, spectrum), and the image and
   text decoding policy (D28, D32, ADR-0031).
6. **ROADMAP › M2:** transitions as presets, overrides and Move (D24, D25, ADR-0029);
   "Studio mode and transitions" expands.
7. **ROADMAP › M3:** YouTube chat send, feature-on-stream and audience stats (D40);
   native projector windows (D36); CLAP hosting (D23).

<a id="things-not-decided-here-but-noticed"></a>
## Things not decided here, but noticed
<!-- @section
-->

- The prototype's loop check for scene-referencing filters is incomplete
  (`sceneChoices`, `D:2268-2285`, only excludes containing scenes). The engine check in
  D10 must be authoritative, and the UI should rely on it.
- Spring easing overshoot (`D:2668`) needs clamping in the compositor (D24).
- "Custom shader, time-limited" (`D:1141`): wgpu has no GPU preemption, so "time-limited"
  can only be a measured per-effect budget with automatic disable on overrun (R-RT-002,
  R-RT-005). Belongs in the effect-pack design under ADR-0005.
- Noise suppression is labelled "RNNoise" (`D:1147`). RNNoise is C. A pure-Rust port
  (`nnnoiseless`) fits R-DEP-001 and R-DEP-002. Note it in the dependency justification.
- Video delay up to 5000 ms (`D:1136`) is a GPU-memory hazard (D15). Cap it.
