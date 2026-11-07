<!-- @doc
id = "ADR-0132"
title = "Release gates from the profile: the client's views report back, a quality probe, and the bench's gates"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0132: Release gates from the profile: the client's views report back, a quality probe, and the bench's gates

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
#1220 makes the engine's profiling into pass or fail gates that block a release. Two
parts have landed. `profile.get` reports what happens past the drawers: each output's
and each H.264 preview stream's frames from its encoder to where it sends them, and
the mixer's blocks (protocol 0.92, #1271). `jstream bench` runs a standard load on the
engine it is connected to and fails on dropped frames and an off-rate recording (#1260).
Three gaps remain before the bench can hold the alpha.5 bar:

- **The engine can't see what a person sees.** A preview stream's frames are counted
  until they reach the client's socket. Whether the canvas or a projector decoded and
  drew them, skipped them behind a slow decoder, or showed "Starting…" in their place
  is only known in the client. Placeholders and restarts on a scene switch (#1205)
  reached the maintainer first because nothing reported them.
- **Nothing judges quality.** A soft preview (#1203) or a starved recording reads as
  healthy as long as it is fast. The engine encodes every stream itself, so it holds
  both the frame it encoded and the packet it made of it.
- **The bench only reads drops.** It ignores the streams' late frames, the mixer's late
  blocks and anything a client reports, and runs only where a CLI is, not from an
  agent's MCP session.

<a id="decision"></a>
## Decision
1. **A client reports its views: `preview.report` (protocol 0.103, a placeholder the
   wave's queue sets).** A client that shows preview streams (the canvas, a projector)
   sends, about once a second while anything changed, its running totals for one
   *view*: a label of its own (`canvas`, `projector:program`, at most 64 characters)
   and the counts since the view opened, over every stream it has shown:
   - `received`: preview frames that arrived for it;
   - `shown`: pictures it drew;
   - `skipped`: frames it didn't decode to catch up (a decoder backlog, a newer JPEG);
   - `decode_errors`: frames or decoders the browser refused;
   - `placeholders`: times it showed its waiting text ("Starting…") where a picture
     belongs, the view's first included;
   - `blanks`: times its stream ended or failed and it showed why instead;
   - `reopens`: streams it opened after the first (a scene switch, a JPEG fallback).

   Like `preview.subscribe` it serves only the session that sends it: it changes no
   collection state, it can't be bound to a hotkey or granted to a plugin, and it is not
   in the activity log. The MCP `preview` tool carries it like the other preview
   actions, as an action that changes nothing outside the session. Counts only grow; a report whose counts
   fall below the last one for the view starts the view again (a reloaded page).
2. **`profile.get` gains `views`.** One `ViewProfile` per view a session reported in
   the window: the session's client name, the label, and each count's growth over the
   window, with `received` and `shown` also as frames a second. A view whose session
   has ended stays in later windows that reach back to its last report, and no longer.
   The engine keeps the latest report per (session, view), at most 16 views a
   session; a 17th is refused with `invalid_argument`.
3. **A quality probe, on request only: `quality.probe`.** A query naming a stream (an
   output, or one of the session's preview channels) and a number of frames (1 to 10,
   default 3). The stream's encoder keeps a copy of the next frames it takes and the
   packets it makes of them; the engine decodes the packets with the platform's
   decoder and answers PSNR (luma and full) and SSIM (luma) between each frame and its
   decoded picture, at the encoded size, and their averages. Nothing is copied or
   decoded unless a probe is waiting, so the stream costs nothing otherwise. A platform
   without a decoder for the stream's codec answers `action_failed` saying so. A
   separate PR builds it; until then the query is not in the protocol.
4. **The bench grows into the gate.** Its gates read `streams` (no dropped or late
   frame on any stream), `mixer` (no late block, no input underrun), `views` when a
   client reported any (no placeholder or blank in the window, `skipped` and
   `decode_errors` zero; reopens are reported, not gated, since a scene switch reopens
   the canvas's stream by design) and, when the probe is available, the probes it takes after
   each switch (PSNR and SSIM above a floor, one floor for previews and one for
   outputs). Each limit is a field of the gates file it already reads, so a run on
   software rendering loosens them in one place. Its scenarios add a looping media
   file, a browser page, a display capture, a webcam and many small 2D items, each
   skipped with a reason where the machine can't build it. The bench is also an MCP
   tool, `bench`, that runs one scenario and answers its report.
5. **CI runs the bench and `cargo xtask perf` on Linux at software bars**, in a job of
   its own on lavapipe with OpenH264, with its own gates file. A hardware run on
   Windows uses the default gates and stays the release gate.

<a id="consequences"></a>
## Consequences
- A Windows run with the desktop app open shows its canvas and projectors in the same
  report as the engine's streams, so a placeholder on a scene switch fails the bench
  instead of reaching the maintainer.
- A client that never reports adds nothing: a CLI or MCP session has no views, and the
  view gates pass vacuously there. The bench says so in its summary.
- `preview.report` costs one small call a second per view; the engine keeps one entry
  per view.
- The probe's decode runs on the control side, never on a real-time thread, and only
  while one is asked for. Linux needs an H.264 decoder for it (OpenH264's own, through
  the library it already loads); until that lands there, the probe answers
  `action_failed` on Linux and CI skips the quality gates.
- Thresholds stay in files the bench and `cargo xtask perf` read, never in code, so
  tightening the bar is a reviewed one-line change.
