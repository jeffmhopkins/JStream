<!-- @doc
id = "DESIGN-UI-PROTOTYPE"
title = "UI prototype (workspace D)"
kind = "reference"
status = "active"
review_every = "90d"
-->

# UI prototype (workspace D)

An interactive mock-up of the JStream editor UI, built to work out layout and interaction
before the real UI is written. It is a **design reference**, not shipping code and not a
normative doc: what it proposes still needs to be decided and written into
`docs/ARCHITECTURE.md` and ADRs (see [Follow-ups](#follow-ups)).

The live version is a [claude.ai Design canvas artifact](https://claude.ai/artifact/JtfdfPAbhubDN5tEcspTvN)
(private to the maintainer). The files here are its source, committed so the work
survives outside that artifact.

<a id="files"></a>
## Files
<!-- @section
-->

| File | What it is |
|---|---|
| `Docking.dc.html` | **Direction D, the chosen one:** the dockable workspace (header, docks, canvas, menus, dialogs, transition engine). |
| `Panel.dc.html` | Every panel's contents (Scenes, Scene objects, Inspector, Filters, Transitions, Mixer, Outputs, chat, stats…). Imported by `Docking.dc.html`. |
| `Main.dc.html`, `CanvasFirst.dc.html`, `ShowControl.dc.html` | Directions A, B and C from the first round, kept for comparison. C was rejected. |
| `canvas.json` | Board layout for the Design canvas. |

Each `.dc.html` is a Design Component: HTML markup with `{{…}}` bindings and
`<sc-for>` / `<sc-if>` blocks, plus a `class Component extends DCLogic` script whose
`renderVals()` supplies the bindings. They render only inside the Design canvas runtime.
To keep working on them, publish them back to the artifact (or a copy of it).

<a id="settles"></a>
## What the prototype proposes
<!-- @section
-->

### Workspace
- Dockable panels in eight zones: outer left/right full-height columns, top and bottom
  full-width docks, left/right sidebars, and docks directly above and below the canvas.
  Drag tabs between zones, onto a tab strip to group, or onto a panel's top/bottom half to
  split. Dividers resize panels; splitters resize docks. Layouts can be saved and reset.
- Clicking a single panel's title collapses it (not in a tabbed group, not when it is the
  only panel in its zone). In side-by-side zones a collapsed panel becomes a narrow strip
  with its title turned (reads bottom to top).
- Closing takes two clicks: the first turns the × red, the second closes. Single panels have
  the × at the right of the title bar; tabbed groups have one per tab.
- Menus close on an outside click; the right-click menu has Windows-style submenus.

### Canvas
- Click to select, Shift-click to add, box-select on empty canvas; drag to move, handles to
  resize (keeps aspect; Ctrl stretches; Alt crops), rotate handle (Ctrl snaps to 45°),
  Shift halves pointer speed. Snapping to edges, centres and other items.
- Floating tool toggles (Move/resize, Crop, Snapping) at the top right.
- Per item: position, size in px and %, rotation, **3D** (tilt, turn, depth,
  perspective), crop, blend mode, opacity, visibility.

### Scenes and objects
- Scenes tree with folders (organisation only). Scene objects tree with groups, and
  **a group is a nested scene**.
- **Keyframe scenes:** a variant of a scene, nested under it, that may change only
  placement and visibility; everything else follows the parent. It stores only what differs.
- **References vs clones:** a reference shares everything with its source except its
  placement (position, size, rotation, crop), opacity and blend; a clone is independent.
  Each placement of a shared source shows how often the source is placed ("shared ×3");
  right-click toggles between reference and clone
  ([ADR-0023](../../docs/adr/0023-placements-references-and-clones.md)).
- Labels and favourites with custom icons and colours; filtering by them.
- Properties dialog per source type and per scene (double-click).

### Sources
Display, window and video-device capture; Spout2; audio input, desktop audio, application
audio; media file, image, browser; text, colour, audio waveform, audio spectrum; scene.
Audio-capable sources (and nested scenes) can switch their audio off, which removes them
from the Mixer.

### Filters
- Separate **video** and **audio** chains, on sources, scenes and outputs. Only the chains
  that fit are shown (a scene has no audio chain; a mic has no video chain).
- Video: colour correction, LUT, chroma/colour/luma key, sharpen, blur, rounded corners,
  drop shadow, outline, glow, scroll, video delay, custom shader, and filters that **use
  another scene**: alpha mask, luma matte, clip to, displacement. The referenced scene shows
  nested under the object; scenes that would create a loop are not offered.
- Audio: gain, compressor (with ducking input), limiter, noise gate, noise suppression,
  invert polarity, EQ, audio delay, and mastering filters (loudness target, true-peak
  limiter, ducking). The prototype also shows a **CLAP plugin** filter: that is *not
  settled*, because hosting CLAP plugins means loading native dynamic libraries, which
  [ADR-0005](../../docs/adr/0005-plugins-wgsl-and-wasm.md) rules out for v1. It needs an
  ADR superseding ADR-0005 first (see [Follow-ups](#follow-ups)).
- Filters panel: a reorderable list; filters with three settings or fewer expand in place,
  busier ones open a settings dialog (double-click always does). Adding is a dialog.
- **Program output** has a master chain for everything leaving JStream, and each output
  (stream, recording, virtual camera) can add its own chain after it. Privacy screen (Pause)
  blurs the output and mutes the mic.

### Transitions
- Transition **presets** (Cut, Fade, Move) with duration and easing; one is the default.
- Each scene can override its **transition in** and **transition out** (right-click
  submenu, Properties, Inspector). The next scene's "in" beats the current scene's "out".
- **Move** pairs items by source and interpolates position, size, rotation (shortest way),
  3D and crop; unmatched items fade, scale or slide. Switching mid-transition retargets from
  what is on screen. Per item: delay, easing, glide on/off, appear and disappear effects.

### Audio
Mixer with a master (program output) strip: loudness meter against a LUFS target, true peak,
pre/post-FX monitoring, FX. Channels fold under the master (folded by default). Meters can be
horizontal or vertical (toggle in the Mixer's title bar; vertical by default in side columns).

<a id="simulated"></a>
## What is simulated
<!-- @section
-->

Rendering of filters, masks and 3D on the canvas (masks are only approximated), audio levels
and loudness readings, YouTube data, capture devices and outputs. The prototype has no
engine behind it.

<a id="follow-ups"></a>
## Follow-ups
<!-- @section
-->

These decisions belong in the docs, through PRs. The
[decision register](decision-register.md) works through all of them (and more the
prototype implies), with options, a recommendation, a tier and an urgency for each:
- `docs/ARCHITECTURE.md` data model: groups as scenes, keyframe scenes, reference/clone
  semantics, per-source audio switch, video/audio filter chains with scene inputs, program
  and per-output chains (with latency compensation), transition presets and overrides.
- An ADR for the source type list (product scope: maintainer tier).
- An ADR for the Move transition and per-item transition settings.
- If CLAP plugin hosting is wanted: an ADR superseding
  [ADR-0005](../../docs/adr/0005-plugins-wgsl-and-wasm.md) (maintainer tier), since it
  loads native dynamic libraries. Until then the CLAP filter stays out of scope.
