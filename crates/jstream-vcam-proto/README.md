<!-- @doc
id = "CRATE-jstream-vcam-proto"
title = "jstream-vcam-proto"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-vcam-proto

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-vcam-proto/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-vcam-proto"]
verified = { hash = "b3:e983ea67355b838a", by = "agent:claude@session_01653X3QeMRu5VETn2xTqUWA", note = "Review round 1: re-read against the code after B1/N2/N3: sddl_objects(user_sid) replaces the IU DACL, logging left out of the contract, choose_frame safety stated as a timing assumption, re-initialisation and format_matches listed" }
-->

Layer-0 contract between the engine, which produces the virtual camera's frames, and the
`jstream-vcam-windows` media source DLL, which Frame Server loads and which serves them.
No dependencies, no `unsafe` and no Windows bindings, so both sides share one definition
and its tests run on any host. Taken from spike S2, which proved the hand-off on
Windows 11 ([ARCHITECTURE › Packaging](../../docs/ARCHITECTURE.md#packaging)).

Contents:
- `Control`: the `repr(C)` control block at the start of the shared mapping. Every field
  is atomic, and a compile-time assertion pins each field's offset. The producer half
  carries the state, namespace, format, ring size, adapter, newest frame and heartbeat;
  the consumer half carries the DLL's identity and counters.
- `MAGIC` and `VERSION`: a consumer uses a block only when both match (`is_live`). Any
  change to the layout, the names or a field's meaning bumps `VERSION`.
- The frame format (1920x1080 at 30 fps, BGRA), the ring size and the liveness timeout.
- The frame hand-off rule: `slot_of` and `choose_frame`, which picks the newest frame
  whose fence already completed and whose slot the producer wasn't rewriting when
  `latest_frame` was read. The ring has no back-pressure, so the DLL's copy must finish
  within about one frame period.
- How a restarted producer re-initialises the block (`STATE_EMPTY` first) and what the
  DLL re-checks, including `format_matches` against the constants.
- Object names and security: `Scope` (`Global\` or the engine's session, opened from
  session 0 as `Session\<id>\`), the texture, fence and control-block names, the order
  the DLL probes them in, and `sddl_objects`, the DACL that admits only SYSTEM,
  Administrators, LOCAL SERVICE (Frame Server) and the producing user's SID.
- Not logging: spike S2's log folder, writable by every interactive user, is left out;
  the DLL's logging (ETW or a location a user can't redirect) is decided in
  `jstream-vcam-windows`.
- The media source's CLSID and the camera's friendly name.

Mapping the block, creating the objects and registering the DLL need Windows APIs and
`unsafe`, so they live in `jstream-platform-windows` (engine side) and
`jstream-vcam-windows` (DLL side).
