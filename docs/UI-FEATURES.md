<!-- @doc
id = "UI-FEATURES"
title = "UI features and implementation status"
kind = "reference"
status = "active"
review_every = "30d"
-->

# UI features and implementation status

The [UI prototype](../design/ui-prototype/README.md) (direction D, the dockable workspace)
is the reference for how JStream's UI looks and behaves. This page breaks it into
features, says what each one requires, and tracks how far the real UI
([`jstream-ui`](../crates/jstream-ui/README.md)) has got. The exact requirements (labels,
defaults, ranges, colours, sizes and modifier keys, with line references into the
prototype) are in two specs:

- [Workspace spec](../design/ui-prototype/spec-workspace.md): theme, header, docking,
  canvas, context menus, dialogs, keyboard, transitions.
- [Panels spec](../design/ui-prototype/spec-panels.md): Scenes, Scene objects, Inspector,
  Transform, Filters, Mixer, Outputs, Transitions, Stream stats, Live chat, Agent
  activity.

The prototype is a design reference, not a decision: where it conflicts with an accepted
ADR, the ADR wins, and the row says so. Behaviour the prototype only simulates (made-up
meter levels, a CSS blur for privacy, the demo source pictures) is not a requirement; the
real UI shows the engine's real state instead.

<a id="how-to-update"></a>
## Keeping this page current
<!-- @section
-->

- Each area's section covers the UI files that implement it, so a PR that changes them
  makes the section stale in `cargo xtask docs check`. Re-read the rows against the
  change, update each row's status and notes, then verify the section. The
  [Summary](#summary) depends on every area, so it goes stale with them: update its counts.
- A new UI file joins the `covers` of the area it implements.
- Statuses:
  - ✅ **built**: the real UI does what the spec says.
  - 🟡 **partial**: some of it is built; the note says what is missing.
  - ⬜ **to build**: the engine already supports it; only UI work is left.
  - 🔒 **needs the engine**: the protocol or engine can't do it yet; the note names the
    roadmap item or PR that adds it.
  - ⏸ **decision**: waits on a decision the note names.
- A row's ID (`CV-04`) is permanent. PR titles name the rows they deliver, e.g.
  "feat(ui): dark theme and header bar (WS-01, HD-01)".

<a id="summary"></a>
## Summary
<!-- @section
depends_on = ["docs/UI-FEATURES.md#theme", "docs/UI-FEATURES.md#header", "docs/UI-FEATURES.md#docking", "docs/UI-FEATURES.md#canvas", "docs/UI-FEATURES.md#menus", "docs/UI-FEATURES.md#dialogs", "docs/UI-FEATURES.md#scenes", "docs/UI-FEATURES.md#objects", "docs/UI-FEATURES.md#inspector", "docs/UI-FEATURES.md#transform", "docs/UI-FEATURES.md#filters", "docs/UI-FEATURES.md#mixer", "docs/UI-FEATURES.md#outputs", "docs/UI-FEATURES.md#transitions", "docs/UI-FEATURES.md#live-panels", "docs/UI-FEATURES.md#keyboard"]
-->

| Area | Rows | ✅ | 🟡 | ⬜ | 🔒 | ⏸ |
|---|---|---|---|---|---|---|
| [Theme and shared controls](#theme) | 8 | 3 | 2 | 2 | 0 | 1 |
| [Header bar](#header) | 9 | 4 | 2 | 1 | 2 | 0 |
| [Docking and layouts](#docking) | 10 | 8 | 2 | 0 | 0 | 0 |
| [Canvas](#canvas) | 18 | 11 | 3 | 0 | 4 | 0 |
| [Context menus](#menus) | 8 | 0 | 5 | 1 | 2 | 0 |
| [Dialogs and windows](#dialogs) | 8 | 2 | 1 | 2 | 3 | 0 |
| [Scenes panel](#scenes) | 10 | 1 | 3 | 4 | 2 | 0 |
| [Scene objects panel](#objects) | 9 | 1 | 3 | 4 | 1 | 0 |
| [Inspector](#inspector) | 10 | 5 | 2 | 0 | 3 | 0 |
| [Transform panel](#transform) | 4 | 4 | 0 | 0 | 0 | 0 |
| [Filters](#filters) | 8 | 0 | 0 | 5 | 3 | 0 |
| [Mixer](#mixer) | 7 | 0 | 3 | 2 | 2 | 0 |
| [Outputs](#outputs) | 4 | 0 | 1 | 0 | 3 | 0 |
| [Transitions](#transitions) | 6 | 3 | 2 | 0 | 1 | 0 |
| [Stream stats, live chat, agent activity](#live-panels) | 4 | 0 | 0 | 0 | 4 | 0 |
| [Keyboard](#keyboard) | 7 | 1 | 2 | 2 | 2 | 0 |
| **Total** | **130** | **43** | **31** | **23** | **32** | **1** |

[Beyond the prototype](#beyond) lists what the real UI has that the prototype doesn't, and
must keep. [Delivery plan](#plan) gives the order the ⬜ rows land in.

<a id="theme"></a>
## Theme and shared controls
<!-- @section
covers = ["crates/jstream-ui/style.css", "crates/jstream-ui/index.html", "crates/jstream-ui/src/panels/number.rs", "crates/jstream-ui/src/icons.rs"]
-->

Spec: [workspace › theme](../design/ui-prototype/spec-workspace.md#theme-fonts-and-global-styling),
[panels › global look](../design/ui-prototype/spec-panels.md#global-look-and-shared-building-blocks).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| WS-01 | Dark palette | Dark only: app `#0F1115`, panels `#16191E`, dock zones `#121418`, canvas area `#0A0B0E`, menus `#1F232A`, borders `#262A31`/`#2E333B`, text `#E6E8EB`/`#9AA1AC`/`#7D8590`, accent `#4C8DFF`, selected `#22324A`, danger `#C62F36`. | ✅ | The palette's CSS variables in `style.css`; panels restyle further in their own rows. |
| WS-02 | Fonts | IBM Plex Sans 13px for the UI, IBM Plex Mono for numbers and readouts. | ⏸ | The UI's CSP allows fonts from `'self'` only, so the prototype's Google Fonts link can't load. Bundling the OFL-licensed Plex files is a licensing question for the maintainer ([R-GOV-005](RULES.md#R-GOV-005)). Until then, the font stack names Plex first and falls back to system fonts. |
| WS-03 | Scrollbars, focus and selection | Thin 10px scrollbars (`#3A3F48` thumb), 2px accent focus ring, `#2B4A7A` text selection. | ✅ | |
| WS-04 | Icons | 24×24 stroke icons (1.8 stroke, round caps) for source types, scene kinds, labels and tools, in place of text glyphs. | 🟡 | The prototype's icons are in `icons.rs`; the Add source and Properties dialogs and the canvas tools use them. Panel rows still use text glyphs (▲ ▼ ✕ ● ○ ↺). |
| WS-05 | Number field | Stepper with − and +, typed entry (Enter commits, Escape cancels, `−` and `,` accepted), arrow keys (Shift fine, Ctrl coarse), and pointer scrubbing (3px per step, Shift ×0.1, Ctrl ×4). | ✅ | `NumberField` (`panels/number.rs`), used by the Inspector; the filters, mixer and dialogs take it as they are rebuilt. A drag shows its value as it goes and is sent once on release, so it is one undo step, where the prototype writes every step. |
| WS-06 | Foldable section headers | Caret plus uppercase title; folded sections show a hint ("cropped", "in use"); fold state remembered per title. | 🟡 | `Fold` in `inspector.rs`: caret, uppercase title and folded hint ("cropped", the transitions hint); fold state isn't remembered per title yet, and the Inspector's other users take it as they are built |
| WS-07 | Hover-revealed row buttons | Row tools fade in on hover or focus, and stay shown while their state is on. | ⬜ | Rows already show tools on hover; the prototype's look and "stays on" rule are missing. |
| WS-08 | Toasts | One toast at a time, bottom centre, hides after 4.5 s. | ⬜ | Today: a fixed bottom-right error box with Dismiss, in the prototype's toast colours. Errors may stay sticky; toasts are for confirmations ("Added a reference…"). |

<a id="header"></a>
## Header bar
<!-- @section
covers = ["crates/jstream-ui/src/app.rs", "crates/jstream-ui/src/header.rs", "crates/jstream-ui/src/panels/outputs.rs"]
-->

Spec: [workspace › header bar](../design/ui-prototype/spec-workspace.md#header-bar).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| HD-01 | Brand and menu row | Accent square and "JStream", then File, Edit, View and Layout menus; one top menu open at a time, closed by any outside press. | ✅ | The prototype leaves File and Edit empty; the real ones hold the connection details and Disconnect, and Undo and Redo. |
| HD-02 | View menu | Lists all 11 panels with "shown" or "add"; clicking hides a shown panel or adds a hidden one to its home zone. | 🟡 | Built for the six panels the UI has; the rest join as they are built. |
| HD-03 | Layout menu | "Layout: <name>", saved layouts with "current", "Save current layout as new", "Reset to default". | ✅ | |
| HD-04 | Stats readout | Mono "60 fps · 0 dropped · CPU 3%". | 🟡 | "N dropped" built from `stats.tick` (frames dropped since the page connected). fps and CPU need engine stats (decision register D39). |
| HD-05 | Privacy button | "Privacy" / "Privacy on" (red); blurs every output and mutes the mic; Pause toggles it. | 🔒 | Privacy screen is E2b (#317). |
| HD-06 | Studio mode button | Turns studio mode (preview and program) on and off. | ⬜ | UI only: the engine has no studio-mode switch, so the Transitions panel (#304) always shows preview and program; the button would hide them. |
| HD-07 | Record button | Starts and stops recording. | ✅ | Starts every stopped or failed recording output; "Stop recording" while any runs; disabled with none. |
| HD-08 | Broadcast picker | Shows the YouTube broadcast Go Live uses; menu lists broadcasts and "+ Create new stream…". | 🔒 | YouTube sign-in needs the OAuth client (A6b, Y4, [ADR-0058](adr/0058-youtube-sign-in-with-a-jstream-client.md)). |
| HD-09 | Go Live / End stream | Red "Go Live", dark red "End stream" while live, at the right end of the header. | ✅ | Starts every stopped or failed stream output whose key is set. |

<a id="docking"></a>
## Docking and layouts
<!-- @section
covers = ["crates/jstream-ui/src/app.rs", "crates/jstream-ui/src/dock.rs", "crates/jstream-ui/src/workspace.rs"]
-->

Spec: [workspace › dock zones and docking](../design/ui-prototype/spec-workspace.md#dock-zones-and-docking).
The dock is built (`dock.rs`, `workspace.rs`) with the seven panels the UI has: Scenes,
Scene objects, Inspector (with the pairing panel under the properties), Transform, Audio
mixer, Outputs and Transitions (studio mode and the presets). The other prototype panels join
the dock, its View menu and its layouts as their rows are built.

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| DK-01 | Eight dock zones | Outer left and right columns, full-width top and bottom, left and right sidebars, above and below the canvas, nested as the spec says; empty zones hidden. | ✅ | |
| DK-02 | Panel groups and tabs | A zone stacks groups; a group holds tabs (uppercase, accent underline on the active tab of a multi-tab group). | ✅ | |
| DK-03 | Resize | 5px splitters resize zones within their limits; 6px dividers trade space between groups. | ✅ | |
| DK-04 | Collapse | Clicking a lone panel's title collapses it (not in a tab group, not the only group in its zone); in a side-by-side zone it becomes a narrow strip with its title turned. | ✅ | |
| DK-05 | Close twice | × arms red on the first click, closes on the second; disarms after 3 s or when the pointer leaves. | ✅ | |
| DK-06 | Drag to dock | Drag a tab onto a tab strip (group), a panel's half (split before or after), or a zone (append); empty zones show "Drop here" placeholders while dragging. | ✅ | Pointer drag rather than HTML drag and drop, which desktop webviews can intercept. |
| DK-07 | Home zones | A panel added from the View menu goes to its home zone (Scenes and Scene objects left; Inspector, Transform, Filters, chat, stats and agent right; Mixer, Outputs and Transitions bottom). | ✅ | |
| DK-08 | Editing and Live layouts | Editing is the default: left Scenes, Scene objects; right Inspector (weight 2), Filters+Transform; bottom Mixer, Outputs+Transitions. Live: Scenes; Mixer and Transitions under the canvas; chat, then stats+Outputs+Agent on the right. | 🟡 | Both built with the panels the UI has: Editing puts Transform under the Inspector, where it shares a group with Filters once that is built; Filters, chat, stats and agent join them as they are built. |
| DK-09 | "(modified)" and saving | Any move, close, resize or View toggle marks the layout "(modified)"; layouts can be saved as "My layout N" and reset. | ✅ | |
| DK-10 | Layouts persist | Layouts survive a reload, the same on the desktop app and in a browser. | 🟡 | Kept in the browser's storage, so per browser rather than the same everywhere. Decision register D35 recommends an engine-side UI settings store, which the protocol doesn't have. |

<a id="canvas"></a>
## Canvas
<!-- @section
covers = ["crates/jstream-ui/src/canvas/**", "crates/jstream-ui/src/store.rs"]
-->

Spec: [workspace › canvas structure](../design/ui-prototype/spec-workspace.md#canvas-structure-and-rendering),
[canvas interactions](../design/ui-prototype/spec-workspace.md#canvas-interactions).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| CV-01 | Frame | 16:9 frame (`#141A22`, 1px outline) centred in a `#0A0B0E` area; caption "<scene> · 1920 × 1080" bottom left. | ✅ | The frame is the largest 16:9 box inside 28/28/20px margins; the caption is mono 11px. |
| CV-02 | Edits the selected scene | The canvas shows and edits the scene selected in Scenes; selecting on the canvas and in Scene objects is one selection. | ✅ | Laid out from the shared collection and `Selection`, so a box click selects its row and opens its properties. |
| CV-03 | Live picture | The scene's real picture under the item boxes. | 🟡 | The program scene's live picture is under the boxes while it is the selected scene (U4p, #302); another scene shows a placeholder. Other scenes need previews for any target (D36). |
| CV-04 | Selection chrome | 2px accent outline, 8 white 10px handles, rotation knob on a stem, size pill under the item ("460 × 410 · cropped"); hover outline on other items. | 🟡 | Outline, handles, knob and stem, pill and hover outline are built; " · cropped" waits on crop (CV-08). Until the picture is drawn (CV-03), every box gets a faint outline and its name. |
| CV-05 | Move with snapping | Drag moves; snaps edges and centres to the canvas and other items within 6px, with magenta guides; Ctrl turns snapping off for the drag. | ✅ | A rotated item doesn't snap, as in the prototype. The live picture follows the drag through live values ([ADR-0032](adr/0032-live-edits-and-undo.md), #747), and the drop is one undoable edit. |
| CV-06 | Resize | Handles keep the aspect ratio; Ctrl stretches; Shift halves pointer speed; minimum 10px. | ✅ | A corner follows the axis that snapped, else the larger change; an edge scales the other axis about the centre. |
| CV-07 | Rotate | Knob rotates; Ctrl steps 45°; snaps to 0/90/180 within 3°. | ✅ | Off the 90° snap, the angle rounds to 0.1°. |
| CV-08 | Crop | Alt-drag a handle, or any handle in crop mode, crops; amber handles and a dashed amber ghost of the uncropped extent. | 🔒 | `item.set_crop` is in source pixels, and turning a dragged edge into source pixels needs the source's size: runtime source sizes (`source.sizes`, [ADR-0022](adr/0022-item-transform-and-canvas.md)) aren't in the protocol. |
| CV-09 | Multi-select | Shift-click adds or removes; Ctrl+A selects all; the group gets a dashed box with its own handles to scale and rotate. | ✅ | The group box can reach past the frame; the pointer is followed over the whole canvas area so its handles stay reachable. A corner scales by the larger change (as a single item's resize does), where the prototype takes the larger factor; they differ only when shrinking. |
| CV-10 | Box select | Drag on empty canvas draws a marquee; Shift adds; a click on empty canvas selects the scene. | ✅ | A marquee can start on the dark margin too. Hits use each item's axis-aligned bounds. |
| CV-11 | Floating toolbar | Top right: Move/resize (V), Crop (C), then Snapping. | 🟡 | Move/resize and Snapping are built; Crop waits on CV-08. |
| CV-12 | Nudge | Arrows move 1px, Shift 10px. | ✅ | |
| CV-13 | Double-click | Opens the item's Properties (a reference opens its source's). | ✅ | Selects the item alone and opens its source's Properties dialog ([DL-02](#dialogs)), which is the same for every placement of the source. A nested scene's properties are the scene's ([DL-03](#dialogs)), so it shows in the Inspector until that dialog exists. |
| CV-14 | Right-click | Opens the canvas context menu ([menus](#menus)). | ✅ | On an item, the selected items' menu, after selecting the item alone unless it was selected; on the frame and the margin, theirs ([CM-02](#menus)). |
| CV-15 | Opacity, blend and 3D drawn | Items draw with their opacity, blend mode and 3D tilt. | 🔒 | The picture comes from the engine; it accepts only the `normal` blend and no 3D yet (X1). |
| CV-16 | Keyframe banner | "◆ Keyframe of “<parent>”: only placement and visibility can change" while editing a keyframe. | 🔒 | Keyframe scenes in the protocol are K1. |
| CV-17 | Privacy overlay | Blurred frame with "Privacy screen on · output blurred · mic muted". | 🔒 | E2b (#317). |
| CV-18 | Half speed | Shift halves pointer speed for any drag but the marquee. | ✅ | The slow pointer follows the real one's movement, so pressing Shift mid-drag doesn't jump. |

<a id="menus"></a>
## Context menus
<!-- @section
covers = ["crates/jstream-ui/src/menu.rs", "crates/jstream-ui/src/item_menu.rs"]
-->

Spec: [workspace › context menus](../design/ui-prototype/spec-workspace.md#context-menus-and-submenus).
One right-click menu at a time (`menu.rs`). The canvas, the Scenes and Scene objects
panels and their rows open theirs; the selected items' menu (`item_menu.rs`) is the same
on the canvas and in Scene objects but for its last lines.

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| CM-01 | Menu mechanics | One menu at the pointer, kept on screen; check column, colour swatch, label, shortcut hint, submenu arrow; Windows-style submenus that open on hover; full keyboard navigation; any outside press closes. | 🟡 | Built but for the colour swatch column, which comes with the label menus that use it. Items the UI or the engine can't do yet show disabled, as the prototype's "(dis)" items do. |
| CM-02 | Canvas menus | Item: Properties, Edit filters, Pop out, reference/clone, layer order, Center/Fit/Stretch, Reset rotation, Clear crop, Crop, Duplicate as reference/clone, Remove, Select all. Empty canvas and margin: Select all, snapping, Add source, scene transitions and Properties. | 🟡 | Built, with an Audio check for a source with sound and the prototype's shorter menus for a nested scene and an audio-only source. Properties opens the Properties dialog ([DL-02](#dialogs)). Reference and clone wait on [CM-06](#menus), Crop on [CV-08](#canvas), Group selection on [OB-04](#objects), and Pop out, the transitions, Edit filters and the scene's Properties on [DL-07](#dialogs), [CM-07](#menus), [FL-06](#filters) and [DL-03](#dialogs), so they show disabled. Remove from scene works, though the prototype shows it disabled. |
| CM-03 | Scene row menu | Rename, Transition in ▸, Transition out ▸, Properties, Edit filters, Pop out ▸, New scene, New keyframe, New folder, Duplicate scene, Delete scene. | 🟡 | Built. Rename, New scene and Delete scene work, Rename and Delete though the prototype shows them disabled. Transitions and keyframes wait on T1 and K1, folders on [SC-06](#scenes), Properties on [DL-03](#dialogs), and Duplicate scene on an engine edit that copies a scene, so they show disabled. |
| CM-04 | Scene object row menu | Audio on/off, Properties, Edit filters, Pop out, reference/clone, layer order, Add source, New group, Remove from scene. | 🟡 | The selected items' menu of [CM-02](#menus), ending with Add source and New group instead of Select all; New group waits on [OB-04](#objects). |
| CM-05 | Empty panel menus | Scenes: New scene, New folder, Paste scene. Scene objects: Add source, Add scene as source, New group, Select all, Paste. | 🟡 | Both are built. New folder ([SC-06](#scenes)), New group ([OB-04](#objects)) and the two Pastes (decision register D43) show disabled. |
| CM-06 | Reference ↔ clone | A placement of a shared source toggles between "Reference to original" and "Independent clone". | ⬜ | `item.set_source` and `source.duplicate` exist ([ADR-0023](adr/0023-placements-references-and-clones.md)). |
| CM-07 | Transition in/out submenus | Default (preset), then each preset, then "Manage presets…". | 🔒 | T1 (#260). |
| CM-08 | Keyframe and folder menus | Switch to it, Reset to parent, New keyframe, Delete keyframe; New folder, Rename, Delete folder / Ungroup. | 🔒 | K1 (#319). |

<a id="dialogs"></a>
## Dialogs and windows
<!-- @section
covers = ["crates/jstream-ui/src/panels/confirm.rs", "crates/jstream-ui/src/dialog.rs", "crates/jstream-ui/src/panels/add_source.rs", "crates/jstream-ui/src/panels/source_properties.rs"]
-->

Spec: [workspace › dialogs and floating windows](../design/ui-prototype/spec-workspace.md#dialogs-and-floating-windows).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| DL-01 | Modal dialog frame | Centred dialog over a dim backdrop; Esc and the backdrop cancel, Enter presses the primary button. | ✅ | `dialog.rs`. Enter isn't taken from a text area, a button or a select; from a field, it blurs the field first so its value is committed. The inline delete confirmation stays. |
| DL-02 | Properties dialog | 860 × 640: Name and the source's settings on the left, its video and audio filters on the right; Defaults, Cancel, OK; settings change only on OK. Opens on double-click and after Add source. | ✅ | Built (`source_properties.rs`): the Inspector's settings form (from `kind.describe`) editing a draft, sent with the name as one batch on OK, holding only what the user changed since it opened; Defaults empties the draft, so OK sends `null` for each stored value. Also opened by Properties… in the item menus. The subtitle is the source's name, as the engine names no capture backend. The filter column lists each chain with the eye and remove at once; summaries, reordering, Add filter and the settings dialog come with [FL-02](#filters) to [FL-05](#filters) and [DL-04](#dialogs) and [DL-05](#dialogs). |
| DL-03 | Scene properties | Transition in and out, hotkey, background colour, multiview, "Include its audio when nested". | 🔒 | Transitions are T1; hotkeys K1. |
| DL-04 | Filter settings dialog | Live-preview settings with an on/off switch; Cancel restores; Defaults; Remove. | ⬜ | Needs [filters](#filters). |
| DL-05 | Add filter dialog | 720 × 520: searchable catalogue grouped by chain and group, details pane, "Uses the scene" picker for scene-input filters. | ⬜ | The catalogue comes from `kind.list`, not the prototype's list. |
| DL-06 | Add source dialog | 720 wide: types grouped Capture, Audio, Media and web, Generated, Scenes; "Create new" with a name, or add an existing source as a reference; new sources placed at native size and their Properties opened. | 🟡 | Built and opened from the right-click menus, with the prototype's names, icons and descriptions for the engine's kinds. A kind the prototype doesn't name goes under Other, and a placed source whose kind the engine doesn't list can be added again but not created. Native sizes are the prototype's per-type table, as boxes with the `fit` mode, because the protocol has no source sizes (see [CV-08](#canvas)). A new source's item is selected and its Properties dialog (DL-02) opens once the engine has accepted it; a placement of an existing source is selected with the Inspector shown. A blank name becomes "<type> N", counting that kind's sources. |
| DL-07 | Pop-out and projectors | A scene or source in a floating window, or full screen on a monitor. | 🔒 | Previews of any scene or source (decision register D36). |
| DL-08 | Create YouTube stream | Title, description, visibility, latency, start now or schedule, category, DVR, made for kids. | 🔒 | Y4 and the OAuth client ([ADR-0058](adr/0058-youtube-sign-in-with-a-jstream-client.md)). |

<a id="scenes"></a>
## Scenes panel
<!-- @section
covers = ["crates/jstream-ui/src/panels/scenes.rs"]
-->

Spec: [panels › scenes](../design/ui-prototype/spec-panels.md#scenes-panel).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| SC-01 | Scene list | Scenes in display order, with type icons; click selects; groups hidden. | ✅ | Icons come with WS-04. |
| SC-02 | "program" badge | The scene on program shows "program" in red after its name. | 🟡 | Built as a "Program" pill; restyle. |
| SC-03 | Search | "Search scenes" filters by name, keeping parents of matches. | ⬜ | |
| SC-04 | Drag to reorder | Drag rows before or after others, or into a folder. | 🟡 | Reordering exists as ▲/▼ buttons. Into a folder is `scene.set_folder` (SC-06). |
| SC-05 | Rename, delete, new | Rename (F2 or menu), delete with the plan's summary, New scene. | 🟡 | Rename, New scene and Delete scene are in the right-click menus ([CM-03](#menus), [CM-05](#menus)), and still in the inline controls (double-click rename, ✕, "New scene name" field) until the restyle; F2 and Del have no key handlers, as in the prototype. |
| SC-06 | Folders | Folders organise scenes; collapsible. | ⬜ | `folder.*` and `scene.set_folder`; folders nest ([ADR-0024](adr/0024-scene-structure.md) §3, [ADR-0064](adr/0064-nested-scene-folders.md), #323). |
| SC-07 | Keyframe rows | Keyframes nest under their scene with a purple diamond icon. | 🔒 | K1. |
| SC-08 | Labels and favourites | Label chips and a favourite star per row; label manager (name, colour, icon). | ⬜ | `label.*`, `source.set_meta` and `scene.set_meta` (ADR-0024 §4, #323). |
| SC-09 | Filter by favourites and labels | Funnel button with a count badge; "Favorites only", labels (any of), Clear, Edit labels. | ⬜ | Filtering is client-side over `state.get`'s labels and favourites (#323). |
| SC-10 | Put on program | Clicking a scene switches to it (or, in studio mode, puts it on preview). | 🔒 | Today a hover "Program" button cuts; the click-to-switch rule follows T1's studio mode. |

<a id="objects"></a>
## Scene objects panel
<!-- @section
covers = ["crates/jstream-ui/src/panels/sources.rs", "crates/jstream-ui/src/store.rs"]
-->

Spec: [panels › scene objects](../design/ui-prototype/spec-panels.md#scene-objects-panel).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| OB-01 | Object list | "In “<scene>”, top to bottom" caption, then the scene's items top first, with type icons. | 🟡 | The list exists as "Sources" with kind text and no caption. |
| OB-02 | Visibility eye | Hover-revealed eye; stays shown while hidden; hidden rows dimmed. | 🟡 | Built as ●/○; restyle. |
| OB-03 | References in italics | A placement of a source shared with other scenes reads in italics. | ⬜ | |
| OB-04 | Nested scenes as groups | A nested scene (group) expands to show its items. | ⬜ | `scene.set_role` and `item.move` exist ([ADR-0024](adr/0024-scene-structure.md)). |
| OB-05 | Scene-input filter rows | An object with a mask, matte, clip or displacement filter shows the scene it uses as a child row with a role badge. | 🔒 | Scene-input video filters (V3, #313). |
| OB-06 | Drag to reorder and group | Drag before, after or into a group. | ⬜ | Reorder exists as ▲/▼. |
| OB-07 | Search | "Search objects". | ⬜ | |
| OB-08 | Multi-select | Shift-click adds; shares the canvas selection. | ✅ | Shift-click adds a row or takes it out; every selected item's row is highlighted. |
| OB-09 | Add, remove, delete | Add source (dialog), Remove from scene, delete everywhere with the plan's summary. | 🟡 | Add source is the dialog (DL-06), from the panel's right-click menu, and Remove from scene is also in the item menu ([CM-04](#menus)). Delete everywhere is still an inline control. |

<a id="inspector"></a>
## Inspector
<!-- @section
covers = ["crates/jstream-ui/src/panels/inspector.rs", "crates/jstream-ui/src/panels/properties.rs"]
-->

Spec: [panels › inspector](../design/ui-prototype/spec-panels.md#inspector). Today the
Inspector shows the item's placement, then the source's settings form in a Properties
section. The Properties dialog (DL-02) uses the same form on a draft; the section stays
in the Inspector until the Inspector's own rows say otherwise.

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| IN-01 | Header | Type icon, name (italic for a reference), kind, "+N" for more selected, labels and favourite. | 🟡 | Name (italic when the source is placed in another scene too), kind, "+N" and the favourite star (`source.set_meta`, `scene.set_meta`) are built. Type icons and labels wait on SC-08's icons and label manager. |
| IN-02 | Parameters | X, Y, W, H in px, and Rotation, as number fields bound to the canvas. | ✅ | Each change is one `item.set_geometry`. X and Y are the unrotated box's top-left corner. The fields show committed values, so a canvas drag shows in them once dropped. |
| IN-03 | Size in % | W % and H % of the source's own size. | 🔒 | Source sizes as runtime state (`source.sizes`, [ADR-0022](adr/0022-item-transform-and-canvas.md)) aren't in the protocol. |
| IN-04 | W/H link | Link toggle keeps the aspect ratio while editing W or H. | ✅ | On by default, kept while the panel is shown. |
| IN-05 | 3D | Tilt, Turn, Depth, Perspective, folded by default. | 🔒 | The engine refuses non-neutral 3D until X1 ([ADR-0050](adr/0050-3d-transforms-rendering.md)). |
| IN-06 | Crop | Left, Right, Top, Bottom in px. | ✅ | `item.set_crop`, in source pixels. The canvas doesn't draw crop yet (CV-08). |
| IN-07 | Blending | Mode (9 modes) and Opacity %. | ✅ | Opacity is `item.set_opacity`. The select lists all nine modes; the engine accepts only `normal`, so the others are disabled until it draws them. |
| IN-08 | Scene section | Objects count, "Used as a source in", hotkey. | 🟡 | Objects and "Used as a source in" are built. The hotkey row waits on K1. |
| IN-09 | Scene transitions | Transition in and out selects, with the "next scene's in wins" note. | ✅ | `scene.set_transitions`, with "Default (<name>)" for none and the folded hint. The presets are read with `state.get` when the collection changes. |
| IN-10 | Scene switches per item | Delay, easing, glide, appear and disappear effects. | 🔒 | T1 (`item.set_transition`). |

<a id="transform"></a>
## Transform panel
<!-- @section
covers = ["crates/jstream-ui/src/panels/transform.rs"]
-->

Spec: [panels › transform](../design/ui-prototype/spec-panels.md#transform-arrange-panel),
[workspace › transform actions](../design/ui-prototype/spec-workspace.md#sources-duplicates-and-groups) (9.11).
All of it is client-side geometry, sent as `item.set_geometry` edits (`item.set_crop` for Clear crop).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| TF-01 | Align to | Selection or Canvas. | ✅ | Selection is off with fewer than two selected, which align to the canvas. |
| TF-02 | Align | Left, centres, right, top, middles, bottom. | ✅ | On the unrotated boxes, as the prototype does. |
| TF-03 | Distribute | Horizontally and vertically, 3 or more selected. | ✅ | Equal gaps between edges in centre order; the first and last stay. |
| TF-04 | Arrange | Center on canvas, Fit, Stretch, Match width/height, Rotate 90° left/right, Reset rotation, Clear crop. | ✅ | Each press is one batch, so one undo step, and sends only the items it changes. Match takes the primary item's size. Clear crop resets `item.set_crop`; crop is in source pixels, so the box stays as it is. |

<a id="filters"></a>
## Filters
<!-- @section
-->

Spec: [panels › filters](../design/ui-prototype/spec-panels.md#filters-panel). The protocol
has source filter chains (`filter.add`, `remove`, `set_enabled`, `set_settings`,
`reorder`; [ADR-0026](adr/0026-source-filter-chains.md)); the UI shows none of them yet.

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| FL-01 | Filters panel | For the selected source: separate Video and Audio chains, count per chain, "No video filters." when empty. | ⬜ | |
| FL-02 | Filter rows | Eye toggle, name (struck through when off), summary, caret; drag to reorder within a chain; × removes. | ⬜ | |
| FL-03 | Edit in place or in a dialog | Filters with three settings or fewer expand in place; others open the settings dialog; double-click always does. | ⬜ | |
| FL-04 | Add filter | "+ Add filter…" opens DL-05. | ⬜ | |
| FL-05 | Settings from descriptors | Each filter's settings come from its kind descriptor ([ADR-0025](adr/0025-typed-settings-kind-descriptors.md)). | ⬜ | The prototype's catalogue is a wish list; the engine lists audio gain, noise gate, compressor, limiter and delay, and video filters from effect packs. |
| FL-06 | Scene filters | A scene's video chain. | 🔒 | Filter owners are sources only today. |
| FL-07 | Program and output chains | Master chain on the program output, one chain per output. | 🔒 | D8p (#303), [ADR-0027](adr/0027-program-master-and-output-chains.md). |
| FL-08 | CLAP plugin filter | Host a CLAP audio effect. | 🔒 | [ADR-0035](adr/0035-clap-plugins-out-of-process.md): hidden until CL1 lands in M3. |

<a id="mixer"></a>
## Mixer
<!-- @section
covers = ["crates/jstream-ui/src/panels/mixer.rs"]
-->

Spec: [panels › mixer](../design/ui-prototype/spec-panels.md#mixer-panel). The real mixer
does more than the prototype (buses, sends, routing); that stays ([Beyond](#beyond)).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| MX-01 | Master card | "Master · program output", N channels, Loudness bar against a target marker with "−14.2 LUFS", True peak with "safe" or a warning, "FX N". | 🟡 | Master loudness (M, S, I, dBTP) is shown as a text line on the master bus. |
| MX-02 | Channels fold under the master | Channels hidden under the master card by default. | ⬜ | |
| MX-03 | Channel strips | Name, level bar, dB number field. | 🟡 | Built with meters, a tapered fader, mute and routing; restyle around them. |
| MX-04 | Vertical meters | Vertical strips in side columns; a title-bar toggle flips orientation. | ⬜ | The dock's title bar is built; the toggle comes with the mixer's restyle. |
| MX-05 | Monitoring | "Hear: after FX" / "before FX". | 🔒 | Decision register D22; no monitor device in the protocol. |
| MX-06 | Privacy mute | The mic reads "muted (privacy)" while privacy is on. | 🔒 | E2b (#317). |
| MX-07 | Real levels | Meters show the engine's levels. | 🟡 | Built (`meters.tick`); the prototype's numbers are simulated. Counted 🟡 until MX-03's restyle. |

<a id="outputs"></a>
## Outputs
<!-- @section
covers = ["crates/jstream-ui/src/panels/outputs.rs"]
-->

Spec: [panels › outputs](../design/ui-prototype/spec-panels.md#outputs-panel). The real
panel manages outputs (create, keys, start and stop, encoders), which the prototype
doesn't show; that stays ([Beyond](#beyond)).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| OU-01 | Program output row | "Program output" first, "master", with an FX button for the master chain. | 🔒 | Master chain is D8p. |
| OU-02 | Output rows | Each output indented under it with its status and "FX N". | 🟡 | Output cards with status exist; the FX button needs FL-07. |
| OU-03 | Footnote | "Program filters apply to every output; each output can add its own after them." | 🔒 | With OU-01. |
| OU-04 | YouTube output | A YouTube stream row bound to the chosen broadcast. | 🔒 | A6b and the OAuth client. |

<a id="transitions"></a>
## Transitions
<!-- @section
-->

Spec: [panels › transitions](../design/ui-prototype/spec-panels.md#transitions-panel),
[workspace › transitions](../design/ui-prototype/spec-workspace.md#scenes-keyframes-transitions).
The engine side is T1 (#260); T1u (#304) builds the Transitions panel: the studio bar,
with the presets and each scene's overrides in a section under it.

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| TR-01 | Presets panel | Preset picker ("Name — Kind · N ms", default marked, "+ New preset…"), Name, Cut/Fade/Move. | 🟡 | A list of presets under the studio bar, each with its name, a line such as "Fade, 300 ms, ease in and out" (a cut shows only its kind) and a "Default" badge on the default, then an "Add a preset" form. It is not the spec's picker. |
| TR-02 | Preset fields | Duration; Move: stagger, items in one scene only (Fade, Scale, Slide); easing (Ease in-out, Ease out, Linear, Spring). | ✅ | The kind's own fields come from `kind.describe`. |
| TR-03 | Default and delete | Make default, Delete (not the default); override counts. | 🟡 | Make default and Delete are built (Delete is off for the default); no override counts yet. |
| TR-04 | Resolution order | Next scene's "in", then current scene's "out", then the default. | ✅ | The engine resolves it ([ADR-0029](adr/0029-transitions.md)); the studio bar names the preset it will use and why. |
| TR-05 | Move transition | Pairs items by source, interpolates placement, retargets mid-transition; unmatched items fade, scale or slide. | 🔒 | Engine side of T1. |
| TR-06 | Studio mode | Preview and program, Transition and Cut. | ✅ | With "Swap" putting the old program scene on preview. |

<a id="live-panels"></a>
## Stream stats, live chat and agent activity
<!-- @section
-->

Spec: [panels › stream stats](../design/ui-prototype/spec-panels.md#stream-stats-panel-simulated),
[live chat](../design/ui-prototype/spec-panels.md#chat-panel-simulated-static),
[agent activity](../design/ui-prototype/spec-panels.md#agent-panel-static).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| ST-01 | Stream stats | LIVE/READY pill and uptime, watching, likes, chat rate, dropped frames, stream health, 2-minute bitrate chart, details. | 🔒 | Y4 (YouTube health and audience stats); bitrate needs engine stats (D39). |
| CH-01 | Live chat | YouTube chat with badges and Super Chats, pinned message, composer. | 🔒 | Y4 (#363) and the OAuth client. |
| CH-02 | Show on stream | Feature one message on stream. | 🔒 | Y4 (#363 draws the featured message). |
| AG-01 | Agent activity | What MCP clients changed, newest first. | 🔒 | An audit log over the protocol (decision register D38). |

<a id="keyboard"></a>
## Keyboard
<!-- @section
covers = ["crates/jstream-ui/src/canvas/mod.rs", "crates/jstream-ui/src/canvas/transform.rs"]
-->

Spec: [workspace › keyboard shortcuts](../design/ui-prototype/spec-workspace.md#keyboard-shortcuts).
The prototype shows Ctrl+D, Ctrl+Shift+D, Del, F2 and Ctrl+V as menu hints without
handlers; the real UI makes them work.

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| KB-01 | Canvas keys | C crop mode, V move tool, Esc deselect, Ctrl+A select all, arrows nudge. | 🟡 | Esc, Ctrl+A and arrows built (arrows move every selected item, as one edit); C and V wait on crop (CV-08). |
| KB-02 | Modifiers | Shift half speed or add; Ctrl no snap, stretch, 45° steps; Alt crops. | 🟡 | Shift half speed and add, and all of Ctrl, built; Alt crop waits on CV-08. |
| KB-03 | Undo and redo | Ctrl+Z, Ctrl+Shift+Z, Ctrl+Y. | ✅ | The prototype has none; kept. |
| KB-04 | Edit keys | Ctrl+D duplicate as reference, Ctrl+Shift+D as clone, Del remove, F2 rename. | ⬜ | |
| KB-05 | Copy and paste | Ctrl+C, Ctrl+V. | ⬜ | Decision register D43. |
| KB-06 | Privacy key | Pause toggles privacy. | 🔒 | E2b (#317). |
| KB-07 | Scene hotkeys | F1–F5 switch to the scene bound to them. | 🔒 | K1 (#319). |

<a id="beyond"></a>
## Beyond the prototype
<!-- @section
covers = ["crates/jstream-ui/src/session.rs", "crates/jstream-ui/src/host.rs", "crates/jstream-ui/src/pairing.rs"]
-->

The real UI does these and the prototype doesn't show them. Matching the prototype must
not remove them; they move into the prototype's look.

- Connecting: the connect form, the desktop app's automatic connection, and the
  connection details (engine URL, protocol, client, revision) with Disconnect.
- Pairing ([ADR-0044](adr/0044-pairing-remote-browsers.md)): the pairing panel shows a
  one-time code for a remote browser and lists and revokes paired devices. In the
  workspace it sits under the properties in the Inspector panel, since it hides itself
  where the server pairs nothing.
- Undo and redo, with the engine's history.
- Delete plans: deleting a scene, source or bus shows what else it takes.
- Outputs: creating stream, recording and virtual camera outputs, the write-only stream
  key, per-output start and stop, encoder profiles, virtual camera registration hints,
  and the pipeline's health line.
- Mixer: buses, sends, "heard always or when on program", mute, the console-taper fader,
  peak hold and loudness reset.
- Settings forms generated from kind descriptors, with groups, advanced settings,
  conditional fields and reset to default.

<a id="plan"></a>
## Delivery plan
<!-- @section
-->

The ⬜ rows land in this order, a few PRs at a time, each into the earliest open wave ([runbook](runbooks/waves.md#which-wave)). A batch
starts when the one before it has passed review.

1. **Look and frame:** dark theme, shared controls, header bar, Go Live in the header,
   Record (WS-01, WS-03 to WS-08, HD-01, HD-07, HD-09).
2. **Workspace:** dock zones, tabs, resize, collapse, close twice, drag to dock, View and
   Layout menus, Editing and Live layouts (DK-01 to DK-09, HD-02, HD-03). The panels split
   into Scenes, Scene objects, Inspector, Mixer and Outputs; Filters and Transform join
   with their batches.
3. **Canvas:** edits the selected scene with one shared selection, frame and chrome,
   floating toolbar, multi-select, box select, the prototype's modifier keys
   (CV-01, CV-02, CV-04 to CV-07, CV-09 to CV-13, CV-18, KB-01, KB-02). The context
   menu (CV-14) comes with the other menus in batch 5, and crop (CV-08) once the
   protocol reports source sizes.
4. **Inspector and Transform:** the shared number field, placement fields, W/H link,
   crop, blending, scene section and transitions (WS-05, IN-01, IN-02, IN-04, IN-06 to
   IN-09), then align, distribute and arrange (TF-01 to TF-04).
5. **Menus and dialogs:** context menus, Properties dialog, Add source dialog,
   reference ↔ clone, groups, edit keys, copy and paste (CM-01 to CM-06, DL-01, DL-02,
   DL-06, OB-03, OB-04, KB-04, KB-05).
6. **Filters:** panel, rows, add and settings dialogs (FL-01 to FL-05, DL-04, DL-05).
7. **Trees and mixer:** search, drag to reorder, folders, labels and favourites, mixer
   master card and vertical meters (SC-03 to SC-06, SC-08, SC-09, OB-01, OB-02, OB-06 to
   OB-09, MX-01 to MX-04).

🔒 rows follow their engine items: each of those PRs, or a UI PR right after it, flips its
rows here.
