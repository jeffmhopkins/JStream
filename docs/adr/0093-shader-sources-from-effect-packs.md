<!-- @doc
id = "ADR-0093"
title = "Shader sources from effect packs"
kind = "adr"
status = "accepted"
tier = "agent"
-->

# ADR-0093: Shader sources from effect packs

<!-- @generated begin adr-status -->
> **Status:** accepted
<!-- @generated end adr-status -->

<a id="context"></a>
## Context
[ADR-0035](0035-clap-plugins-out-of-process.md) keeps WGSL effect packs as JStream's
data-only extension point, and [ADR-0025](0025-typed-settings-kind-descriptors.md) makes
each pack effect a `video_filter` kind (`<pack>/<effect>`). An effect is one fullscreen
pass over its input. It can't make a picture of its own, read the frame it drew last,
read a second image beside its input, or hear audio.

The scenes agents build over MCP need exactly those. The cockpit scenes (a warp
starfield, a hyperspace tunnel, a fireball, reflections of a sky plate, a CRT look,
audio-driven glow) are drawn today as `browser` sources: a canvas animation costs a CPU
core per layer at 1080p60, steps its motion at the page's frame rate, and a reflection
needs a full-resolution copy and a blur per layer. A shader computes the same streaks
analytically from time, on the engine's GPU, in the same frame as the compositor.

Agents can't write a pack either: no command reaches the packs folder. Whether they may
is a security-posture question, so it is decided apart, in
[ADR-0095](0095-agents-write-effect-packs.md); this ADR doesn't depend on its answer.

<a id="decision"></a>
## Decision
1. **A shader source is a pack kind.** `effect.toml` gains `[[source]]` tables beside
   its `[[effect]]` tables. Each becomes a `source` kind with the ID `<pack>/<source>`,
   listed by `kind.list` and described by `kind.describe` like a pack effect, with the
   same ID rules, `version` and `renames`, and its own `[[source.param]]` list. A pack's
   effects and sources share one ID space and one limit of 64.
2. **Its picture is a program of passes.** A source lists up to 8 `[[source.pass]]`
   tables, each an `id`, a WGSL file and the names it reads (`inputs`). The last pass is
   the source's picture. Every pass draws at the source's size: the `width` and `height`
   settings every shader source has (live, 16 to 4096, defaults from the manifest), or
   at a `scale` of it from 1/16 to 1, so a blur can run at half size. A pass reads:
   - **another pass.** One listed earlier reads this frame's picture of it. The pass
     itself, or one listed later, reads the picture it drew on the frame before: that is
     feedback, for trails, smoke and simulations. Feedback starts transparent, and starts
     again when the size changes and when the source is shown again.
   - **an image of the pack,** named in the source's `images` table (PNG or JPEG, at
     most 2048 pixels each way), for masks, noise and sprites.
   - **a scene,** through a parameter of type `scene`: the scene's composite, so a
     setting picks what the shader reads (a sky plate, a webcam's scene). An empty
     setting reads transparent. A scene that reads the source back is a cycle, refused
     like any other reference cycle.
   - **audio,** when the source sets `audio = true`. It then gets the `audio` (a bus) and
     `audio_source` (a source) settings the `spectrum` kind has, with the same meaning,
     and the `audio` image: 64 spectrum bands on its first row, the latest waveform on
     its second.
   At most 12 named images per source. The generated header declares each as a function
   of its name returning premultiplied linear colour, so a pass writes `sky(uv)`.
3. **What a pass sees.** The host's prelude, then a generated header, then the source's
   optional shared `common` file, then the pass's file, which defines
   `@fragment fn fs(in: VsOut) -> @location(0) vec4<f32>`. The header holds the
   parameters as `p` (one uniform struct, as for effects) and `sys`: the pass's and the
   source's sizes in pixels, `time` (seconds since the source was last shown, so a
   one-shot warp plays from its start each time), `clock` (media-clock seconds, the same
   for every source on a frame, so separate layers stay in step; wrapped to an hour),
   the frame count since shown, and for an audio source its `level`, `peak` and `onset`
   (0 to 1). The effect contract holds otherwise: no bindings, immediates, overrides or
   entry points of its own, and the same memory caps. Every pack parameter stays live,
   so `live.set` ([ADR-0091](0091-source-settings-as-live-values.md)) and keyframe
   scenes move it.
4. **It draws like a source with a chain.** The engine runs the program on the render
   thread first in the source's video chain, every frame the source is drawn, before
   the source's own filters ([ADR-0026](0026-source-filter-chains.md)). A device loss
   while a shader source draws is blamed on its kind and quarantines it, as
   [ADR-0087](0087-effect-quarantine-after-gpu-device-loss.md) does for effects. A
   quarantined or unloaded shader kind draws transparent at its size. A reloaded pack
   keeps its sources: each draws the new program from its next frame.
5. **A source's layers are bounded.** The layers a program draws into at its size (one
   per pass, two for a pass read back as feedback, `Rgba16Float`) may take at most
   512 MiB. A program over it draws nothing and the failure is logged, like a filter
   that fails, so a `width` and `height` set live can't take gigabytes of GPU memory.

<a id="consequences"></a>
## Consequences
- Packs can make native animated graphics, and the program stage draws them at full
  frame rate with no browser.
- Who can do what doesn't change: a pack is still a folder a person puts in the packs
  folder, the WGSL is validated by naga before anything draws it, and a shader that
  hangs the GPU is quarantined under ADR-0087. So this is an agent-tier decision.
- `effect.toml` grows a second table kind. A pack written for an older JStream still
  loads, and a pack with `[[source]]` tables fails to load on an older JStream (its
  manifest refuses unknown fields), which is what a pack author would want.
- No protocol change: shader sources are `source` kinds, created and set like any other.
- A shader source that draws every frame costs GPU time every frame. Nothing caps a
  pass's cost: naga can't bound a loop, and quarantine is the backstop.
- Reading a source (not a scene) as an image is left out: a scene with the source in it
  does the same, through edges the engine already orders and cycle-checks.
