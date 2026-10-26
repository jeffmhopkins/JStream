<!-- @doc
id = "ADR-0080"
title = "Program master and per-output chains v2: no privacy screen"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0080: Program master and per-output chains v2: no privacy screen

<!-- @generated begin adr-status -->
> **Status:** proposed
<!-- @generated end adr-status -->

Supersedes [ADR-0027](0027-program-master-and-output-chains.md). It keeps ADR-0027's
program master chain, per-output chains and cost reporting (its decisions 1 to 3, restated
below unchanged), and drops its decision 4, the privacy screen.

<a id="context"></a>
## Context
ADR-0027 made the privacy screen an engine action (decision register D16): `privacy.set`
covers every output with a blur or black and mutes the microphones, `privacy.get` reads
it, `privacy.changed` reports it, and every client shows it. The engine built it (E2b,
#317 and #804) and the UI added its button, canvas blur, mixer mark and Pause key (#866).

After trying the alpha, the maintainer asked to get rid of privacy mode, and chose
complete removal, engine included, over keeping a read-only indicator in the UI. A
partial removal doesn't work: as long as the engine can cover the outputs, ADR-0027 has
every client show it, and a UI with no control and no indicator would leave a person
streaming a blurred picture with a muted mic and no way to tell.

The protocol follows semver ([R-API-001](../RULES.md#R-API-001)): removing a call needs
an ADR and one minor release that deprecates it first. `item.set_transform` set the
pattern: deprecated since 0.11 ([ADR-0022](0022-item-transform-and-canvas.md)), removed
in 1.0.

<a id="decision"></a>
## Decision

**1. The program master chain.** Unchanged from ADR-0027:
- The collection has one **master** stage with a video chain and an audio chain. It
  applies to **everything leaving JStream**: every output, whichever mix and buses it
  takes. It does not apply to the editing canvas or to audio monitoring.
- **Video:** the master video chain runs once per tick for each distinct mix that some
  output consumes, after the mix is composited. In the common case that is one run, on
  Program.
- **Audio:** the master audio chain runs once per block on each bus that feeds an output,
  after that bus's own chain. Monitor devices tap the bus before the master chain.
- Preview streams state whether they show a target before or after the master stage, so
  the UI can show both what is being edited and what viewers see.

**2. Per-output chains.** Unchanged from ADR-0027:
- Each output (stream, recording, virtual camera) may add its own **video chain** and its
  own **audio chain**, which run after the master stage. The virtual camera has no audio,
  so it has only a video chain.
- **An output with a non-empty video chain gets its own render pass and its own encoder
  session. It never shares.** This holds even if two outputs carry identical chains, so the
  cost of a chain is always exactly one pass and one session, and is predictable.
- **Encoder sharing key:** outputs whose video chain is empty share as before, keyed by
  `(video_mix, encoder_profile)`, with the master stage included in the mix. An output
  with a video chain is keyed by its own output ID. A disabled filter still counts as part
  of the chain, so toggling a filter never moves an output between sessions while live.
- **Audio chains don't affect video sharing.** An output with its own audio chain gets its
  own audio encode for its tracks.
- **Order on the output path:** mix → master video chain → output video chain → colour
  conversion → encoder.

**3. Show the cost.** Unchanged from ADR-0027:
- The engine reports, for each output, whether it shares an encoder session (and with
  which outputs) or has its own, plus the total count of hardware sessions in use.
- When an edit would give an output its first video filter, and so a new session, the
  edit succeeds, but the UI warns first and states the cost: one more GPU pass and one more
  encoder session, with the sessions in use.
- The engine does not guess vendor caps. If the encoder refuses a session, `output.start`
  fails with a stable error code that says the hardware session limit was reached and
  names the outputs that hold sessions. There is no silent fallback to a software encoder.

**4. No privacy screen.** This replaces ADR-0027's decision 4:
- JStream has no privacy screen. The engine has no privacy stage on the output path, no
  privacy flag on the render or mixer threads, and mutes nothing on its own. The UI has no
  privacy button, overlay, mixer mark or key; Pause is an ordinary key a hotkey can bind.
- **The protocol deprecates the calls, and removes them in 1.0** (R-API-001):
  `privacy.set` and `privacy.get` stay on the wire, marked deprecated, and
  `privacy.changed` stays in the event schema. From the release that carries this ADR,
  `privacy.set` is refused with `unsupported`, `privacy.get` always answers `on: false`,
  and the engine never sends `privacy.changed`. The deprecation changes no wire shape and
  adds nothing, so it takes no `minor` bump of its own.
- **MCP offers no privacy tool.** Like the people-only nouns, a deprecated noun gets no
  tool, so an agent is never offered a call that is always refused.
- A person who wants to hide the output uses what the app already has: switch to another
  scene, or mute a source.

<a id="consequences"></a>
## Consequences
- **Less to keep safe.** The privacy stage was safety-critical: it had to fail closed,
  survive every per-output chain, and reach the next frame and block without a lock. With
  it gone, the output path is mix, chains, conversion, encoder, and the mixer has no
  privacy mute to keep allocation-free.
- **No panic button.** Nothing covers every output at once. Switching to a safe scene
  covers what that scene's mix feeds; an output on another mix keeps showing its mix.
- **Old clients keep connecting.** A client built for 0.x before this ADR still speaks
  the protocol. Its `privacy.set` fails with `unsupported`, and its indicator stays off.
- **Follow-on edits when this ADR is accepted:**
  - ADR-0027's `status` becomes `superseded`, with `superseded_by` naming this ADR, and
    every link to ADR-0027 moves here.
  - [ARCH › Data model](../ARCHITECTURE.md#data-model),
    [ARCH › Control protocol](../ARCHITECTURE.md#protocol) and the GPU pipeline and
    threads sections drop the privacy stage and flag, and list the privacy calls as
    deprecated.
  - `docs/UI-FEATURES.md`'s HD-05, CV-17, MX-06 and KB-06 rows record the removal.
- **Built:** with this ADR, in the PR that removes the privacy screen.
