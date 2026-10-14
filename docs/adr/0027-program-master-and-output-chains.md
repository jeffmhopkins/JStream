<!-- @doc
id = "ADR-0027"
title = "Program master, per-output chains and the privacy screen"
kind = "adr"
status = "proposed"
tier = "maintainer"
-->

# ADR-0027: Program master, per-output chains and the privacy screen

<a id="context"></a>
## Context
The chosen UI prototype (`design/ui-prototype/`, direction D) gives the program output
a **master chain** "for everything leaving JStream", and lets each output (stream,
recording, virtual camera) "add its own chain after it". Its demo puts a LUT on the
program and a sharpen filter on the YouTube output only. It also has a **privacy screen**:
a header button and the Pause key "blur the whole output and mute the mic". The UI
decision register
([B. Filters and the render pipeline](../../design/ui-prototype/decision-register.md#b-filters-and-the-render-pipeline),
entries D14 and D16) marks both as gaps, and as maintainer decisions.

**Today** ([ARCH › Data model](../ARCHITECTURE.md#data-model)):
- Outputs consume a *video mix* and a list of *audio buses*
  (`Output { id, kind, video_mix, audio_buses, encoder_profile }`). A mix can be Program,
  Preview or any scene ([ADR-0007](0007-bus-matrix-audio.md)).
- **Encoder sharing:** outputs with the same `(video_mix, encoder_profile)` share one
  encoder session. Hardware session caps differ by GPU vendor, so the engine reports the
  count. M1b builds this sharing.
- Sources and buses have filter chains. Scenes, mixes and outputs have none.
- Nothing hides the output on demand.

**Forces:**
- A per-output video chain changes that output's picture, so that output can no longer
  share an encoder session with one that lacks the chain. Hardware encoders cap their
  concurrent sessions, and the cap is per vendor and often per driver.
- A privacy screen is safety-critical. It must hide what really leaves the machine
  (stream, recording, virtual camera), not only the editing canvas. Another client's or
  an agent's undo must never un-hide it ([ADR-0013](0013-command-model.md)).
- Agents have full access ([ADR-0006](0006-mcp-full-access.md)), so they can use both.

The maintainer decided both questions in the builder session:
- D14, per-output chains: the maintainer's choice was **"Allow, show the cost"**.
- D16, privacy screen: the maintainer's choice was **"Engine action"**.

<a id="decision"></a>
## Decision

**1. The program master chain.**
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

**2. Per-output chains.**
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
- **Order on the output path:** mix → master video chain → output video chain → privacy
  stage (below) → colour conversion → encoder.

**3. Show the cost.**
- The engine reports, for each output, whether it shares an encoder session (and with
  which outputs) or has its own, plus the total count of hardware sessions in use.
- When an edit would give an output its first video filter, and so a new session, the
  edit succeeds, but the UI warns first and states the cost: one more GPU pass and one more
  encoder session, with the sessions in use.
- The engine does not guess vendor caps. If the encoder refuses a session, `output.start`
  fails with a stable error code that says the hardware session limit was reached and
  names the outputs that hold sessions. There is no silent fallback to a software encoder.

**4. The privacy screen is an engine action.**
- `privacy.set { on }` is an **action** on the program master stage. It is never undoable
  or batchable, does not change `rev`, and is not stored in the collection's undoable state.
  Every change emits a `privacy.changed { on }` event, which every client shows, and goes to
  the audit log with the client's name.
- **While on, every output is covered:** stream, recording, virtual camera, and every
  preview stream that shows a post-master stage. This includes outputs that consume a
  non-program mix. The privacy stage is the last step before colour conversion, so no
  per-output chain can undo it.
- **What it shows:** a heavy blur of the outgoing picture, or a slate (a solid colour or a
  chosen image). This is a collection setting, and so an ordinary undoable edit.
- **What it mutes:** a configurable set of sources, by default every audio input-device
  source (the microphones). Muting is independent of each source's own mute, so turning
  privacy off restores exactly the state from before.
- **Agents and hotkeys:** the action is in the protocol, and so over MCP. Hotkeys are client
  bindings that send it.
- **Takes effect on the next tick and block.** The render and mixer threads read the
  privacy flag through a lock-free parameter channel, not a snapshot rebuild. The blur or
  slate pass is allocated whenever an output exists, so switching it on allocates nothing
  on a real-time thread ([R-RT-001](../RULES.md#R-RT-001),
  [R-RT-002](../RULES.md#R-RT-002)).
- **It fails closed.** If the privacy pass can't run, the output gets a black frame, not
  the unblurred picture.
- **Not persisted:** the engine starts with privacy off. Its state is runtime state, like
  output state.

<a id="consequences"></a>
## Consequences
- **The cost is visible, not hidden.** One sharpen filter on one output costs a GPU pass
  and an encoder session, and the user is told so. Machines near their vendor's session cap
  can run out of sessions sooner than in OBS-style "one encoder per mix" setups.
- **Encoder sharing stays simple.** The key gains one case (chained outputs are keyed by
  output ID). It needs no chain comparison.
- **The privacy screen covers what leaves the machine**, including recordings and the
  virtual camera. Agents and other clients can't undo it away. A flag that is not persisted
  means a restart always starts with privacy off, so the indicator must be obvious.
- **Render graph:** the render plan gains a master node per consumed mix, a node per
  chained output, and a privacy node per output. The per-output nodes need a measured
  budget when built ([R-RT-005](../RULES.md#R-RT-005)).
- **Follow-on edits when this ADR is accepted** (not made while it is proposed):
  - [ARCH › Data model](../ARCHITECTURE.md#data-model): a `master` stage with video and
    audio chains, `Output` gains its video and audio chains, a `privacy` setting (mode,
    blur strength or slate, muted sources), and the new encoder sharing key.
  - [ARCH › GPU pipeline](../ARCHITECTURE.md#gpu-pipeline) and
    [ARCH › Threads](../ARCHITECTURE.md#threading): the order of the output path, and the
    privacy flag on the parameter channels.
  - [ARCH › Control protocol](../ARCHITECTURE.md#protocol): `privacy.set` (action),
    `privacy.changed` (event), per-output session reporting in `output.status`, and the
    session-limit error code.
  - [ROADMAP › M2](../../ROADMAP.md#m2): the master chain, per-output chains and the
    privacy screen in the UI.
  - The M1b `Output` model reserves its chains now, with an empty default, because M1b
    builds encoder sharing.
- **Built:** the model shape in M1b; the chains, cost reporting and privacy screen before
  or during M2.
