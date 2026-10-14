<!-- @doc
id = "CRATE-jstream-types"
title = "jstream-types"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-types

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-types/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-types"]
verified = { hash = "b3:ca662a5d03c38a2c", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "ARCH#crate-map changed only in the jstream-encode row (ADR-0027 sharing: no video chain shares per mix and encoder profile, a chained output gets its own session), the jstream-mcp row (stdio here, streamable HTTP served by jstream-server) and the jstream-server row (serves that transport, not built); the jstream-types row is unchanged and this Purpose still matches it" }
-->

Layer-0 value types shared by every JStream crate. No dependencies on other JStream crates,
and no `unsafe`.

Contents so far:
- `Rational`: an exact, reduced rational number for frame rates and time bases
  (e.g. `30000/1001`).

Still to come in M0: IDs, math (Vec/Mat/Transform3D), color, units.
