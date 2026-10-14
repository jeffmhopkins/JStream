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
verified = { hash = "b3:52c85f8186b054b9", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "Dependency ARCH#crate-map only reworded jstream-cli's apps row (test-only dev-deps outside it); types' layer 0, no deps, Rational contents unaffected" }
-->

Layer-0 value types shared by every JStream crate. No dependencies on other JStream crates,
and no `unsafe`.

Contents so far:
- `Rational`: an exact, reduced rational number for frame rates and time bases
  (e.g. `30000/1001`).

Still to come in M0: IDs, math (Vec/Mat/Transform3D), color, units.
