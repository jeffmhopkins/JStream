<!-- @doc
id = "ADR-0260"
title = "A replace sets a model's glow strength"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0260: A replace sets a model's glow strength

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Amends [ADR-0203](0203-mappings-add-or-replace.md) decision 6 for one material
setting, `emissive_strength`. Every other material setting stays add-only, and the rest
of ADR-0203 is unchanged.

<a id="context"></a>
## Context
ADR-0203 decision 6 keeps a model's material drives add-only: a material's values live
in the model file, which the frame doesn't read, so a `replace` motion row on a material
drives nothing. Such a row is still accepted and stored, so a scene that drives a
material's glow strength from a channel with `mode: "replace"` saw nothing happen, and
had to add from a rest of 1 instead (issue #1835).

That reason doesn't hold for `emissive_strength`. It is not a value from the file but a
multiplier of the file's glow, and its own value is always 1
([ADR-0171](0171-live-data-from-a-daw-drives-scenes.md) decision 4). The frame knows it
without reading the file.

<a id="decision"></a>
## Decision
A `replace` row on a model's `emissive_strength`, on every material or on the ones its
`material` names, sets that multiplier to its value, as ADR-0203 decision 4 has a replace
row set a source setting. Its own value is 1, so its `amount` channel pulls it toward 1.
On each material the last enabled such row in the list sets it, then the `add` rows add
to it, and the sum is kept within 0 and 64 as before. So a row mapping a channel from 0
to 6 turns the glow off at 0 and makes it six times the file's at 1.

`color`, `metallic`, `roughness` and `emissive` on a material keep ADR-0203 decision 6:
a `replace` row on them drives nothing.

<a id="consequences"></a>
## Consequences
- **Easier:** a glow follows a fader or a channel exactly, with no offset from 1.
- **Harder:** a material drive carries one more field, the strength it sets, which the
  renderer applies before the drives' sum.
- **Not decided here:** replacing a material's colour, metalness or roughness, which
  needs the file's value in the frame.

<!-- Once accepted, only `status` and `superseded_by` may change (R-DOC-009), and the
     generated `adr-status` banner, which only `cargo xtask docs generate` writes (ADR-0038).
     A changed decision is a new ADR that supersedes this one. -->
