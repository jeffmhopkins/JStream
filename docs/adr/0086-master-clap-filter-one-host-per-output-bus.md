<!-- @doc
id = "ADR-0086"
title = "A master-chain CLAP filter runs one plugin host per bus that feeds an output"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0086: A master-chain CLAP filter runs one plugin host per bus that feeds an output

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0080](0080-program-master-and-output-chains-v2.md) §1 says the master stage applies
to every output, "whichever mix and buses it takes", and that the master audio chain runs
on each bus that feeds an output. [ADR-0028](0028-audio-model-v2.md) lets an output take
a list of buses. [ADR-0035](0035-clap-plugins-out-of-process.md) §1 starts one plugin
host process per CLAP filter instance, and one host takes one chain's blocks.

So the mixer runs a CLAP filter on the master chain on the master bus's copy of that
chain only. Every other bus's copy holds a stand-in that delays its dry signal by the
filter's latency (#913). Today every output hears the plugin, because the engine taps
only the master bus (`add_output_tap` in `crates/jstream-core/src/audio.rs`). Once
outputs read other buses, a built-in master filter will reach them but a master CLAP
filter won't, which breaks ADR-0080 §1 (#922).

Two ways out were put to the maintainer:
1. start one host per bus that feeds an output, so each bus's copy of the master chain
   gets its own `ClapFilter` and block; or
2. refuse or report a master CLAP filter while a non-master bus feeds an output.

<a id="decision"></a>
## Decision
**This is the maintainer's decision.** Asked in the project chat to pick one of the two,
he answered "1", and his answer is relayed on #922. This ADR records it. It amends how
ADR-0035 §1's "one host process per CLAP filter instance" applies to the master chain,
so that ADR-0080 §1 holds for CLAP filters too. The rest of both ADRs stands.

1. **One host per bus that feeds an output.** A CLAP filter on the master audio chain
   counts as one filter instance on each bus that feeds an output. Each such bus's copy of
   the master chain gets its own `ClapFilter`, its own shared block and its own
   `jstream-plugin-host` process, so every output hears the plugin, whichever buses it
   takes.
2. **Same settings, separate state.** Every copy loads the same plugin with the filter's
   settings. Each host keeps its own processing state, so a stateful plugin, such as a
   compressor, acts on each bus on its own.
3. **No stand-in once built.** The delayed-dry stand-in on non-master buses goes away
   with this: no bus that feeds an output runs a master chain without the plugin.
4. **Built with multi-bus outputs.** This is built in the same work that lets an output
   take buses other than the master. Until then nothing changes: the engine taps only
   the master bus, the plugin runs on the master bus's copy, and the other copies keep the
   stand-in.

<a id="consequences"></a>
## Consequences
- ADR-0080 §1's promise holds for CLAP filters: a master CLAP filter reaches every
  output.
- One more plugin process for each extra bus that feeds an output, with its own
  supervisor and restarts. The cost grows with buses, not outputs.
- One filter now has several hosts, so the multi-bus output work also settles which
  host's editor opens and how a parameter change reaches every host.
- Option 2, refusing a master CLAP filter, is not taken: no outputs or buses are
  restricted by it.
- **Not built yet.** #922 stays open until the multi-bus output work builds this. The
  stand-in described in `jstream-audio`'s and `jstream-core`'s READMEs and in
  [ARCH](../ARCHITECTURE.md) is the behaviour until then.
