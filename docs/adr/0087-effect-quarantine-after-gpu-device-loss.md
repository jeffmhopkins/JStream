<!-- @doc
id = "ADR-0087"
title = "A GPU device loss quarantines the effect-pack kinds drawn before it, until a person re-enables them"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0087: A GPU device loss quarantines the effect-pack kinds drawn before it, until a person re-enables them

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0035](0035-clap-plugins-out-of-process.md) loads third-party WGSL effect packs and
draws them on the program stage. naga validates a shader but can't bound how long it
runs, so a pack that loops too long can hang the GPU until the OS resets the device
(Windows' TDR; #349). The engine already survives a lost device: the program stage
switches to a new one, and the third loss within 60 s fails the outputs
([jstream-core › Runtime](../../crates/jstream-core/README.md#runtime)). Without
anything more, the same pack hangs the new device on its next frame.

#349 asks for three things on top of surviving the loss: an engine event that reports
it, blaming the loss on the pack that was drawing, and quarantining that pack. The
maintainer set one rule for this work in the project chat: a blamed pack stays off until
the user turns it back on.

<a id="decision"></a>
## Decision
1. **Blame.** A device loss on the program stage is blamed on the effect-pack kinds the
   shot being drawn draws. With no pack drawn, nothing is blamed and the loss is only
   reported.
2. **Quarantine.** Each blamed kind is quarantined: it stays in `kind.list`, marked
   `quarantined`, and its filters are bypassed, as a filter whose pack isn't loaded is.
   A pack one of whose kinds is blamed a second time in one run is quarantined whole.
3. **Kept until a person lifts it.** The quarantine is kept in `quarantine.json` in the
   effect-packs folder, outside the collection. So undo doesn't lift it, it lasts across
   restarts, and editing the pack doesn't lift it either. Only the protocol action
   `effect.enable` lifts it, for the kind and its pack.
4. **People only.** `effect.enable` is for a person, as `plugin.enable` is under
   [ADR-0069](0069-wasm-plugin-limits-and-capabilities.md) G1: the `effect` noun is in
   `jstream-mcp`'s `PEOPLE_ONLY`, no plugin grant reaches it, and no hotkey binds it. An
   agent can't put a shader that hung the GPU back on air.
5. **Reported to everyone.** The event `gpu.device_lost {cause, blamed, quarantined,
   recovering}` reports each loss. It is runtime state with no `rev`, and agents receive
   it, since it lets them explain why an effect stopped drawing.

<a id="consequences"></a>
## Consequences
- A pack that hangs the GPU costs one device reset, not a reset on every frame until
  the third loss fails the outputs.
- Blame is coarse. Every pack kind in the drawn shot is quarantined, including kinds
  that didn't cause the hang, and a shot that changes in the frame or two a loss takes
  to surface is blamed for the newer shot's kinds. A person re-enables an innocent kind
  with one action.
- The quarantine file sits beside the packs, inside the data folder, with the same
  trust as the rest of it. A file that can't be read quarantines nothing.
- **Not covered here.** The GPU-time guard of #349, which turns an effect off before the
  OS resets the device, is not built. A forced TDR on Windows hardware hasn't been run;
  the tests lose the device on the software adapter.
