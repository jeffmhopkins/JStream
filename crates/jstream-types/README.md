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
verified = { hash = "b3:28b366a59970bdf9", by = "agent:claude@session_01QBbMRwRuZe7FL4ak2fU793", note = "ARCH#crate-map changed only in the jstream-sources row (ADR-0031 kinds by milestone; pure-Rust image and font crates) and the ADR-0003 to ADR-0031 citation; the jstream-types row and this Purpose are unchanged and still match the crate" }
-->

Layer-0 value types shared by every JStream crate. No dependencies on other JStream crates,
and no `unsafe`.

Contents so far:
- `Rational`: an exact, reduced rational number for frame rates and time bases
  (e.g. `30000/1001`).

Still to come in M0: IDs, math (Vec/Mat/Transform3D), color, units.
