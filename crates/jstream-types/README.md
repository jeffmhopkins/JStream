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
depends_on = ["docs/ARCHITECTURE.md#crate-map"]
verified = { hash = "b3:eb9c19c77b9e5016", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Merged main (docs(adr): ADR-0035 CLAP plugins out of process, superseding ADR-0005 (accepted)): sections changed only by main's text and verified lines; re-read against this PR's change and still hold" }
-->

Layer-0 value types shared by every JStream crate. No dependencies on other JStream crates,
and no `unsafe`.

Contents so far:
- `Rational`: an exact, reduced rational number for frame rates and time bases
  (e.g. `30000/1001`).

Still to come in M0: IDs, math (Vec/Mat/Transform3D), color, units.
