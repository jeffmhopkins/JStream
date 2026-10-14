<!-- @doc
id = "ADR-0028"
title = "Audio model v2: bus matrix with faders, scene-following audio, keys, meters"
kind = "adr"
status = "accepted"
tier = "maintainer"
-->

# ADR-0028: Audio model v2: bus matrix with faders, scene-following audio, keys, meters

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

Supersedes [ADR-0007](0007-bus-matrix-audio.md).

<a id="context"></a>
## Context
[ADR-0007](0007-bus-matrix-audio.md) decided that audio sources exist independently of
scenes and feed buses through a send matrix with a gain per send, that buses carry filter
chains and feed outputs, and that video outputs consume a video mix rather than "the
current scene". Its reason still holds: video and audio routing must be independent (music
on the stream but not in the VOD, or a different scene on the virtual camera than on the
stream).

The UI prototype (direction D) and its decision register
(`design/ui-prototype/decision-register.md`, entries D15, D17 to D22 and D29) show what
ADR-0007 leaves open or contradicts:

- **The mixer the prototype shows** (D17) is a master strip ("Program output", with
  loudness, true peak, a monitor pre/post switch and FX) and one gain fader per channel.
  It has no visible buses or sends. ADR-0007's matrix has gains per send only, so a single
  fader per source has no defined meaning.
- **Audio that follows scenes** (D18). The prototype, like OBS, lists the current scene's
  sources and lets a nested scene include or exclude its audio. ADR-0007 says audio is
  independent of scenes, so a browser alert or a media file would be heard while its
  scene is off-air. That is a conflict with ADR-0007's decision text, so it needs a
  superseding ADR, and [R-DOC-009](../RULES.md#R-DOC-009) forbids amending part of an
  accepted ADR. This ADR therefore restates ADR-0007's whole decision with the changes.
- **A per-source audio switch** (D19): a source's audio can be turned off, which removes it
  from the mixer. It is not the same as mute.
- **Sidechain and ducking** (D20): a compressor "ducks under" another source, and a
  "Ducking" filter "dips everything else while the mic talks". The prototype puts it on
  the master, which already contains the mic.
- **Loudness metering** (D21): LUFS against a target and true peak in dBTP. The binary
  meter stream is "not designed yet" in ARCH.
- **Monitoring** (D22): "Hear: before FX / after FX" on the master strip.
- **Latency and delay** (D15): look-ahead limiters and loudness filters add latency, and
  an audio delay filter (up to 5000 ms) shifts audio on purpose. Neither may break A/V sync
  by accident.

The maintainer decided these in the builder session:
- **D17:** "Matrix + simple view". Keep ADR-0007's bus matrix in the engine, add a
  per-source fader and mute and a default master bus. The prototype's mixer is the
  everyday view; an advanced routing view comes later in the UI.
- **D18:** "Per-source setting". Each audio source is either `always` or `when_shown` (on
  program). Mic, desktop and app audio default to `always`; browser, media file, webcam
  audio and nested scenes default to `when_shown`, fading over the transition.
- **D19 to D22, and the audio part of D15:** follow the register's recommendations.

The time line is [ADR-0014](0014-media-clock.md)'s: one media clock, every input
resampled onto it. The first cut of `jstream-audio` (one stereo bus, a gain and mute per
input, peak and RMS meters, blocks stamped on the media clock) already exists.

<a id="decision"></a>
## Decision
**Routing: a bus matrix, unchanged in principle.**
- Audio sources feed **buses** through a **send matrix** with a gain per send. Buses carry
  audio filter chains and feed outputs: stream and recording tracks (an output takes a list
  of buses), and monitor devices. Buses do not feed other buses.
- Video outputs consume a *video mix* (Program, Preview, or any scene), not "the current
  scene". Audio routing stays independent of video routing.
- A **virtual microphone** is out of scope. Windows has no user-mode API for creating an
  audio endpoint, so it would need a signed kernel driver, which requires its own ADR.

**Master bus, faders and mute.**
- Every collection has one **master bus**, created with the collection and never deleted.
  It is what the UI calls "Program output", and its filter chain is the audio master
  chain. Every new audio source gets a send to it at 0 dB, and new outputs take it as their
  bus. Other buses are added by the user.
- Every audio source has a **fader** (`gain_db`) and **mute**, applied before its sends.
  Every bus has a fader and mute too, applied after its filter chain.
- Signal order per source: stream → the source's audio filter chain → fader and mute →
  activity gain (below) → each send's gain → bus sum. Per bus: sum → filter chain → fader
  and mute → outputs and monitor. Sends are post-fader; pre-fader sends are not offered.
- **The UI:** the prototype's master view (a strip per audio source folded under the
  master) is the everyday mixer. An advanced routing view for buses and sends comes later
  in the UI. The engine and the protocol expose the full matrix from the start.

**Audio on or off per source** (D19).
- Each audio-capable source has `audio.enabled`. When it is off, the engine does not open
  the source's audio stream (for example, no CEF audio handler, no webcam microphone), and
  the source has no mixer channel. Its sends are kept but inactive, so turning it back on
  restores them and the edit's inverse stays simple.
- It is distinct from **mute**, which keeps the stream open and metered and only silences
  its contribution.

**Audio that follows the program** (D18).
- Each audio-capable source has `audio.follow = always | when_shown`.
  - `always`: the source's activity gain is always 1.
  - `when_shown`: the activity gain follows whether the source is **shown**, defined below.
- Defaults by kind: microphone and other audio inputs, desktop audio and application audio
  are `always`; browser, media file, webcam audio and nested scenes are `when_shown`. A
  source kind added later declares its default with it.
- The activity gain is runtime state computed by the engine, not collection state: it is
  never an edit and never undoable. A change of shown state that is not part of a
  transition (a cut, an item made visible) ramps over one mixer block, so it never clicks.

**What "shown" means.** "Shown" is relative to the **Program** mix only.
1. A **root** is a scene the Program mix is currently showing: `program_scene` when no
   transition runs, or both the outgoing and the incoming scene while one runs. Each root
   `r` has a weight `w(r)`: 1 without a transition; during a transition, the transition's
   audio curve gives the two weights, which always sum to 1 (a linear crossfade unless the
   transition defines another curve; a cut or a stinger's cut point switches at once).
2. A source `s` is **reachable** from a scene if the scene has an item placing `s` with
   `visible = true`, or an item with `visible = true` placing a nested scene source `n`
   whose `audio.enabled` is on and whose scene reaches `s`. Nesting is followed to any
   depth (the scene graph has no cycles). A hidden item, or a nested scene with its audio
   off, stops reachability through that path; other paths still count.
3. A nested scene source set to `follow = always` is an extra root of weight 1: what it
   reaches counts as shown whether or not it is on program.
4. The **activity gain** of a `when_shown` source is `min(1, Σ w(r))` over the roots `r`
   that reach it. So a source in both scenes of a transition stays at full level, a source
   in one of them fades with it, and a source in neither is silent.

A nested scene source has `audio.enabled` and `audio.follow` but no stream, fader, mute or
sends of its own: its contents keep their own channels and sends. A submix is a bus.

The engine-wide definition of "on program" for sources (D29) is written into ARCH ›
Data model with this ADR's acceptance, and must match this one. Preview, outputs on other
video mixes, projectors and editor previews never make a source shown.

**Sidechain keys and ducking** (D20).
- A filter may take a **key input**: a typed reference to another source. The key signal
  is that source after its fader, mute and activity gain, so a muted or off-air mic never
  ducks anything. A key reference is indexed like any other reference, so deleting a source
  that is still a key is rejected.
- Sources and buses, with sends and keys as edges, form an **audio graph**. It is checked
  when edited and topologically sorted when the audio snapshot is built, on the control
  thread. The mixer processes in that order (keys first) and gives a keyed filter a
  read-only view of the key's buffer for the same block, with no allocation
  ([R-RT-001](../RULES.md#R-RT-001)).
- An edit that would close a cycle (A keyed on B and B on A, or longer) is **rejected**
  with a cycle error, as for nested scenes. There is no one-block back edge.
- **Never duck a sum that contains the key.** A keyed filter on a bus is rejected if its key
  source sends to that bus, and adding such a send is rejected while the filter exists. A
  keyed filter on a source is rejected if the key is that source. The prototype's "dip
  everything else" on the master is a UI shortcut that must be expressed within these
  rules (for example, as ducking on the other sources or on a bus the key does not feed);
  the engine never implements it on the master sum itself.

**Meters and loudness** (D21).
- The meter stream carries, per channel: sample peak, RMS and true peak (4× oversampled,
  ITU-R BS.1770). Per bus it also carries momentary (400 ms), short-term (3 s) and
  integrated loudness (LUFS, BS.1770 K-weighting and gating). The frame layout has these
  fields from the first version of the binary meter stream.
- Metering runs on the mixer thread with fixed-size state, allocation-free. Loudness is
  computed only for the master bus and for the buses and sources a client subscribes to.
- Integrated loudness is reset by an action (`audio.reset_loudness`), never by an edit
  ([ADR-0013](0013-command-model.md)).

**Monitoring** (D22).
- A bus may have a monitor: a device and a **tap point**, `pre_filters` or `post_filters`.
  It replaces ADR-0007's bare "monitor device".
- Per-source monitoring (OBS's "monitor only" and "monitor and output") is out of scope
  until asked for.

**Latency and timestamps** (D15, audio).
- Every audio filter reports its latency in frames. The mixer compensates: before a sum,
  it delays the shorter paths so all inputs line up, and it stamps every output block with
  `pts` = the media time of the input it was computed from, not the time it was produced.
  Muxers interleave by `pts` with a bounded wait. Filter latency therefore never shifts A/V
  sync, video is never delayed to match it, and encoder sharing is unaffected.
- An **audio delay filter** is a deliberate shift, not latency: its output is stamped
  later by its delay. Its ring buffer is allocated at its maximum when the filter is
  created, on the control thread, never on the mixer thread.
- A latency change (a filter added, removed or reconfigured) rebuilds the audio snapshot
  on the control thread, like any other graph change.

**Commands** ([ADR-0013](0013-command-model.md)). `audio.enabled`, `audio.follow`, faders,
mute, sends, buses, keys, filters and monitors are collection state, changed by undoable
edits. Continuous fader moves go through the lock-free parameter channels, as in ARCH ›
Threads. The activity
gain, meters and loudness are runtime state, read through queries, events and the meter
stream.

<a id="consequences"></a>
## Consequences
- OBS's track model is still a special case of the matrix (buses as tracks), and OBS's
  scene-following audio is now one too (`when_shown`). Users coming from OBS get what they
  expect, and ADR-0007's "music on the stream but not in the VOD" still works.
- Audio follows **Program** only. An output whose video mix is another scene (say the
  virtual camera) still carries buses whose `when_shown` sources follow program, not its
  own video. Per-output audio that follows a different mix would need a new ADR.
- The mixer gains work: an activity gain per source fed from the studio state and
  transition progress, an audio graph with topological order and two rejection rules
  (cycles, a key in its own sum), latency compensation, BS.1770 loudness and true peak,
  and monitor taps. All of it stays allocation-free on the mixer thread.
- The audio snapshot depends on the scene graph and the studio state, not only on audio
  state: a scene edit or a program change rebuilds the reachable set on the control
  thread. Transition progress itself is continuous, so it reaches the mixer like a fader
  move, not as a snapshot per block.
- Not decided here: per-output audio filter chains and the split of source filters into
  video and audio chains (register D9, D14), transition curves (D24, D25), and plugin
  hosting (D23). They must fit this model.

**Edits that follow acceptance**, in the PR that sets this ADR to `accepted` (links from
normative docs and code to a superseded ADR fail `docs check`, so they move together):
- **ADR-0007:** `status = "superseded"` and
  `superseded_by = "docs/adr/0028-audio-model-v2.md"`. Nothing else in it changes
  ([R-DOC-009](../RULES.md#R-DOC-009)).
- **ARCHITECTURE › Data model:** `depends_on` points at this ADR instead of ADR-0007. The
  sources line loses "independent of scenes" and gains the source's audio settings
  (`enabled`, `follow`, `gain_db`, `muted`, audio filters). The audio block gains the
  master bus, bus fader and mute, keys, and `monitor { device, tap }` in place of
  `monitor_device?`. A bullet defines "shown" and "on program" as above (D29).
- **ARCHITECTURE › Threads, clock and sync:** the mixer row adds activity gains, keys and
  latency compensation, and the `pts` contract is written there (D15). The "In the
  engine" paragraph is unaffected.
- **ARCHITECTURE › Control protocol:** the meter stream's fields and `audio.reset_loudness`.
- **`crates/jstream-audio`:** the README (Purpose and Not yet) and the `Docs:` line in
  `src/lib.rs` cite this ADR instead of ADR-0007.
- **ROADMAP M1b:** "Bus-matrix mixer with filters and meters" gains the master bus, faders,
  scene-following audio and loudness meters, for the maintainer to confirm.

**How the first cut of `jstream-audio` maps onto this model.** Nothing in it contradicts
the model; it is a subset.
- Each `Mixer` input is a source's audio stream, and `InputControl`'s gain and mute are the
  source's fader and mute. Its ramp over one block is the ramp this ADR requires.
- The one stereo bus and `BusControl` are the master bus and its fader and mute. Every
  input implicitly sends to it at 0 dB, which is the default this ADR sets.
- `Block::pts` is already the media time of the frames mixed, which is the `pts` contract;
  latency compensation joins it once filters exist.
- Peak and RMS meters are the first fields of the meter frame; true peak and loudness are
  added.
- `audio.enabled = false` maps to not calling `add_input` for that source (and removing
  the input once adding and removing while running exists).
- Still to build: the activity gain, the send matrix and further buses, filters with keys
  and latency reporting, loudness, and monitor taps.
