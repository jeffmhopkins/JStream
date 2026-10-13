<!-- @doc
id = "CRATE-jstream-protocol"
title = "jstream-protocol"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-protocol

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-protocol/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map"]
verified = { hash = "b3:ee3212f8d21ec9a6", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Re-read: depends_on ARCH#crate-map changed only in the jstream-sources built-ins list; jstream-protocol purpose unaffected" }
-->

**The API.** Every capability of JStream is a command, query, event or stream defined
here first ([R-ARCH-001](../../docs/RULES.md#R-ARCH-001)), and the engine, the web UI, the
CLI and MCP agents all speak it ([ADR-0001](../../docs/adr/0001-library-first-engine.md)).
Layer 1: plain data types with serde and JSON Schema derives, no I/O, no `unsafe`, and
WASM-safe, because `jstream-ui` and `jstream-control` compile it to `wasm32`. It depends
on no other JStream crate yet: `jstream-types` has no IDs or transforms so far, so the
first cut defines its own wire types for them.

Contents (a deliberately small first cut; [Design](#design) describes the whole model):
- **Envelope** (`envelope.rs`): `ClientMessage` (`hello`, `request`) and `ServerMessage`
  (`welcome`, `response`, `error`, `event`), `Request`/`Call` and `Response`/`CallResult`.
  `ClientMessage::parse` maps every parse failure to `malformed`.
- **Commands** (`command.rs`): `Edit` (`scene.create`, `scene.rename`, `scene.delete`,
  `item.add`, `item.remove`, `item.set_transform`), `Action` (`output.start`,
  `output.stop`), `Batch`, and the `Command`/`CommandClass` classification view.
- **Queries** (`query.rs`): `scene.list` and `output.status`, with `QueryResult`.
- **Events** (`event.rs`): `state.changed` and `output.state_changed`.
- **Errors** (`error.rs`): `ProtocolError`, a `thiserror` enum whose variants have stable
  codes, and `WireError`, its wire form with a message.
- **IDs and versions** (`id.rs`, `version.rs`): opaque string IDs, `Rev`, and
  `PROTOCOL_VERSION`.
- **Numbers** (`finite.rs`): `Finite32`, the only float type, which rejects NaN,
  infinity and out-of-range numbers at construction and deserialization.
- **Schema** (`schema.rs`): `json_schema()`, snapshot-tested in
  [`schema/protocol.snap`](schema/protocol.snap), with every fixture message's wire form in
  [`schema/examples.snap`](schema/examples.snap).

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-protocol/src/**", "crates/jstream-protocol/schema/**"]
depends_on = ["docs/adr/0013-command-model.md", "docs/RULES.md#R-API-001", "docs/RULES.md#R-API-002", "docs/RULES.md#R-API-004", "docs/RULES.md#R-ARCH-006"]
verified = { hash = "b3:69575b64d32ec805", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Review round 2 B2: every float is now Finite32 (Vec2.x/y, ItemTransform.rotation_deg); its Deserialize rejects NaN/inf and out-of-f32-range numbers like 1e39 (serde's as-narrowing), ClientMessage::parse maps that to malformed, Finite32::new/Vec2::new return None so in-process code can't build one; tests cover +-1e39, f32::MAX round trip, no null output. Removed the false 'JSON can't carry NaN' claim; compatibility now notes string enums lack Unknown (OQ8); can_serve rule unchanged" }
-->

This is the protocol's design. It is a proposal, so the maintainer can steer it: the open
points are listed under [Open questions](#open-questions).

### Envelope and versioning

A connection carries JSON text messages, each tagged by `type`. It starts with a
handshake: the client sends `hello` with its `ProtocolVersion` and a `client_name`, and
the server answers `welcome` with its own version, the `client_id` this connection acts
as, and the current `rev`. If the server can't serve that version it answers an
`unsupported_version` error instead. A request sent before the handshake gets
`handshake_required`.

After the handshake, the client sends `request`s. Each carries an `id` the client
chooses, and the server answers each request with exactly one `response` or `error`
carrying the same `id`. Answers may come out of order. `event`s arrive at any time.

```json
{"type":"hello","protocol":{"major":0,"minor":1},"client_name":"jstream-cli"}
{"type":"welcome","protocol":{"major":0,"minor":1},"client_id":"cli_7","rev":12}
{"type":"request","id":"r1","call":{"kind":"batch","edits":[{"type":"scene.create","scene_id":"scn_1","name":"Main"}],"expected_rev":12}}
{"type":"response","id":"r1","result":{"kind":"batch","rev":13}}
{"type":"event","event":{"type":"state.changed","rev":13,"client_id":"cli_7","request_id":"r1"}}
```

The version is `major.minor` and follows semver independently of the app
([R-API-001](../../docs/RULES.md#R-API-001)). A server serves a client with the same
major and a minor no newer than its own (`can_serve`): a 0.2 server serves a 0.1
client, and a 0.1 server refuses a 0.2 client. The rule is the same at 0.x as after 1.0,
because additions only ever bump the minor (see *Schema and compatibility* below). The
in-process `Control` implementation has
no handshake; it is compiled against the same crate.

### Tagging

Every enum uses serde's internally tagged form, so a message is one flat JSON object
whose tag says what it is:

- `type` names the message (`request`), the command, query or result (`scene.create`), or
  the event (`state.changed`). Command, query and event names are `noun.verb` in
  snake_case ([R-API-002](../../docs/RULES.md#R-API-002)), so the tag *is* the name that
  MCP tools, the CLI and logs use.
- `kind` names the class of a call and its result: `batch`, `action` or `query`. It is a
  different key from `type`, so a call can nest its command without the two tags
  colliding, and the class of every request is explicit on the wire.
- `code` names an error.

Why internally tagged: a flat object is easiest to write by hand and for agents, it
gives the schema one `oneOf` with a `const` per alternative, and an unknown tag fails
with an error that names it. Externally tagged enums nest every value one level deeper
under its name, adjacently tagged ones need a second `content` key, and untagged ones
give useless parse errors and ambiguous schemas.

### Edits, actions and queries

Following [ADR-0013](../../docs/adr/0013-command-model.md), every command is exactly one
of two classes ([R-ARCH-006](../../docs/RULES.md#R-ARCH-006)):

- **Edit** (`Edit`): changes the collection state, the data that is saved in the
  collection file and that `rev` counts. Undoable, batchable, atomic.
- **Action** (`Action`): has effects outside the collection state, such as network,
  devices, files or time (starting an output, going live, OAuth). Never undoable or
  batchable. The response only says the action was accepted; progress arrives as
  events (`output.state_changed`).

The classification rule: **if the command's whole effect is a change to the collection
state, and its inverse can be computed from the state before it, it is an edit.
Otherwise it is an action.** A command that does both is split into an edit and an
action. A **query** (`Query`) changes nothing and is neither.

The types enforce the split: a `Batch` holds only `Edit`s, an action travels alone, and
`Call` (tagged by `kind`) has exactly three forms: `batch`, `action` and `query`.
`Command` is a classification view over both enums, with `class()`, `is_undoable()` and
`is_batchable()`. The tests `names_are_valid` and `every_command_is_classified` check
every name in the generated schema.

Creating edits carry a client-chosen ID (`scene_id`, `item_id`). A batch can then refer
to an object it creates earlier in the same batch, a retry can't create a duplicate,
and an undo can re-create a deleted object under its old ID. IDs are opaque strings.

Every float in the protocol (`Vec2`, `ItemTransform`) is a `Finite32`: an `f32` that is
never NaN or infinite. serde would otherwise narrow an out-of-range JSON number such as
`1e39` to infinity, which then serializes as `null` and no longer parses. So a
non-finite or out-of-`f32`-range number fails deserialization, and
`ClientMessage::parse` rejects the whole message as `malformed`. In-process clients
can't build a non-finite value either: `Finite32::new` and `Vec2::new` return `None`
for one.

### Batches, revisions and `expected_rev`

Edits are only sent inside a `Batch`; a single edit is a batch of one.

- `rev` is the collection state revision. Every applied batch increments it by exactly
  one, however many edits it holds. Actions and queries never change it.
- A batch is atomic: the engine applies its edits in order, each against the state the
  previous one left. If any edit fails, nothing is applied, `rev` is unchanged, and the
  error names the failing edit's field (`edits.2.name`).
- `expected_rev` is optional ([R-API-004](../../docs/RULES.md#R-API-004)). If it is set
  and the state is no longer at that revision, the batch fails with `rev_conflict`
  (carrying `expected` and `current`) and nothing is applied. The client re-reads the
  state and retries: optimistic concurrency for read-modify-write. Without it, the batch
  applies to whatever is current.
- `idempotency_key` is optional. The engine remembers, per client, the result of each
  batch applied with a key. A retry with the same key is not applied again and gets the
  original result, even if `rev` has moved on since.
- Results: a successful batch answers `{"kind":"batch","rev":N}`, the revision it
  produced. Queries that read collection state report the `rev` they read.

### Undo

Undo is per client ([ADR-0013](../../docs/adr/0013-command-model.md)): each applied batch
becomes one undo step on the history of the `client_id` that sent it. The protocol
doesn't carry inverses. `jstream-graph` computes each edit's inverse from the state
before it, and the per-client undo stacks live in `jstream-core`
([ARCH › Data model](../../docs/ARCHITECTURE.md#data-model)). What the protocol must
guarantee is that the edit set is **closed under inversion**: every edit's inverse is
itself a sequence of edits. Each `Edit` variant documents its inverse (for
`scene.delete`: `scene.create` plus one `item.add` per deleted item).

**The first cut is not yet closed under inversion.** Undoing `item.remove` can't restore
the item's stacking position, and undoing `scene.delete` can't restore the scene's
display order, because there are no order edits yet. Order edits (`item.reorder`,
`scene.reorder`) must land before undo is implemented in `jstream-graph` and
`jstream-core`. A new edit that can't state its inverse in edits needs new edits first,
in the same way. Undo and redo requests, and the
`undo_conflict` error for an undo whose inverse conflicts with later edits by another
client, come with the engine's history ([Open questions](#open-questions)).

### Events

After `welcome`, the server pushes every event to every client.

- `state.changed {rev, client_id, request_id?}` follows every applied batch. A client
  matches it to its own response by `request_id`; there is no ordering promise between
  the two. Every other client re-reads what it shows. Since each batch adds exactly one
  to `rev`, a gap shows that an event was missed.
- `output.state_changed {output_id, state}` reports runtime state, which is not part of
  the collection, so it carries no `rev`.

### Errors

`ProtocolError` is a `thiserror` enum ([R-RS-003](../../docs/RULES.md#R-RS-003)). On the
wire (`WireError`) it is its stable `code`, its detail fields, and a `message` for
people and agents. Clients match on `code`, never on `message`.

| Code | Meaning | Details |
|---|---|---|
| `unsupported_version` | The server can't speak the client's version | `client`, `server` |
| `handshake_required` | A request came before `hello` | — |
| `malformed` | Bad JSON, unknown `type`, missing or mistyped field | `reason` |
| `empty_batch` | A batch had no edits | — |
| `rev_conflict` | `expected_rev` is stale; nothing applied | `expected`, `current` |
| `not_found` | A command named a missing object | `entity`, `id` |
| `already_exists` | A creating edit reused an ID | `entity`, `id` |
| `invalid_argument` | A field's value was rejected | `field`, `reason` |
| `action_failed` | An accepted action could not be carried out | `reason` |
| `internal` | An engine bug or unexpected condition; may be retried | `reason` |

An error that isn't tied to a request (a failed handshake, or a message too broken to
read an `id` from) has no `id`.

### Schema and compatibility

`json_schema()` generates the JSON Schema (draft 2020-12) of every message from the
Rust types with `schemars`. Each type is a named definition, so the MCP server can hand
out per-group schemas. The test `schema_snapshot` compares it with
[`schema/protocol.snap`](schema/protocol.snap), and `wire_examples_snapshot` compares one
message of every variant with [`schema/examples.snap`](schema/examples.snap). Any API
change shows up as a diff of those files, and that diff is the API review
([R-TEST-002](../../docs/RULES.md#R-TEST-002)). After an intended change, regenerate them
with `INSTA_UPDATE=always cargo test -p jstream-protocol` (or `cargo insta review`), and
review the diff.

Compatibility follows [R-API-001](../../docs/RULES.md#R-API-001), at 0.x exactly as
after 1.0:

- **Minor (additive):** a new command, query, event, error code or `Call` kind; a new
  optional field. Old clients keep working, and a newer server still serves them
  (`can_serve`), because unknown fields are ignored (no type denies them), and because
  unknown server messages, events and error codes parse as an `Unknown` variant that
  clients skip. An unknown error code still has its `message`. Not yet covered: a new
  value of a string enum (`OutputState`, `EntityKind`) inside a known message breaks an
  older client's parse, so adding one needs open question 8 settled first.
- **Major (breaking):** removing or renaming anything, changing a field's type or
  meaning, or making an optional field required. It needs an ADR and one minor release
  that deprecates the old form first.
- Every public enum is `#[non_exhaustive]`, so Rust clients must handle new variants too.

### Binary streams (deferred)

Snapshots (PNG/JPEG), preview video and audio meters
([ARCH › Control protocol](../../docs/ARCHITECTURE.md#protocol)) are deferred to a later
PR. The intended shape: they travel as WebSocket binary frames with a small fixed header
(stream ID, stream kind, the `rev` or media timestamp they show), so they never pass
through JSON. They are opened and closed with JSON requests: `video.snapshot` is a
query whose result names the binary frame that carries the image, and preview and
meters are subscriptions. The header layout and the subscription commands are part of
that PR.

<a id="open-questions"></a>
## Open questions
<!-- @section
-->

Decisions for the maintainer. Until one is settled, the first cut does what is
described above.

1. **Framing.** A custom envelope (as here), or JSON-RPC 2.0 framing (`method`,
   `params`, `id`)? JSON-RPC is what MCP speaks and has client libraries, but it has no
   handshake or typed error details of its own, and its errors are numeric.
2. **Single edits.** Every edit travels in a batch. Should there also be a shorthand
   that sends one edit without the `edits` array?
3. **IDs.** Clients choose the IDs of what they create (this design). Alternatives:
   server-assigned IDs, with batches referring to created objects by local names. And
   should the ID types move to `jstream-types`, once it has IDs and an optional serde
   feature?
4. **Undo and redo on the wire.** Proposed: `history.undo` and `history.redo` as a fourth
   kind of call, `history`. They change state and bump `rev`, but are neither edits
   (they aren't themselves undo steps) nor actions (no outside effects). R-ARCH-006
   only knows edits and actions, so this needs a decision, maybe a rule change.
5. **Actions that change collection state.** `studio.transition` changes the program
   scene, which is saved in the collection. Is it an action that also bumps `rev`, or an
   edit plus a separate "run transition" action?
6. **Idempotency.** How long the engine remembers keys (count or time), and whether a
   reused key with a different batch is an error (`idempotency_key_reused`).
7. **Event subscriptions.** Every client gets every event in this cut. High-rate events
   (`stats.tick`, meters) will need opt-in subscriptions.
8. **Unknown values in string enums.** `OutputState` and `EntityKind` have no `Unknown`
   fallback, so adding a value breaks older clients that parse it. Add fallbacks, or treat
   a new value as a major change?
9. **Schema delivery.** The schema lives in an `insta` snapshot. Should `xtask gen-schema`
   also publish a plain `.json` file for non-Rust clients?
10. **ADR.** Should the envelope, tagging and compatibility policy above be recorded as an
    ADR ([R-ARCH-004](../../docs/RULES.md#R-ARCH-004)) once they are settled?
