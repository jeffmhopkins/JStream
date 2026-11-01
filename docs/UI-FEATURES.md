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
meter levels, the demo source pictures) is not a requirement; the
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
| [Theme and shared controls](#theme) | 8 | 8 | 0 | 0 | 0 | 0 |
| [Header bar](#header) | 9 | 7 | 0 | 0 | 1 | 1 |
| [Docking and layouts](#docking) | 10 | 9 | 1 | 0 | 0 | 0 |
| [Canvas](#canvas) | 18 | 17 | 0 | 0 | 0 | 1 |
| [Context menus](#menus) | 8 | 6 | 2 | 0 | 0 | 0 |
| [Dialogs and windows](#dialogs) | 8 | 5 | 2 | 0 | 1 | 0 |
| [Scenes panel](#scenes) | 10 | 10 | 0 | 0 | 0 | 0 |
| [Scene objects panel](#objects) | 9 | 9 | 0 | 0 | 0 | 0 |
| [Inspector](#inspector) | 10 | 10 | 0 | 0 | 0 | 0 |
| [Transform panel](#transform) | 4 | 4 | 0 | 0 | 0 | 0 |
| [Filters](#filters) | 9 | 8 | 1 | 0 | 0 | 0 |
| [Mixer](#mixer) | 7 | 6 | 0 | 0 | 0 | 1 |
| [Outputs](#outputs) | 4 | 3 | 0 | 0 | 1 | 0 |
| [Transitions](#transitions) | 6 | 6 | 0 | 0 | 0 | 0 |
| [Stream stats, live chat, agent activity](#live-panels) | 4 | 1 | 0 | 0 | 3 | 0 |
| [Keyboard](#keyboard) | 7 | 6 | 0 | 0 | 0 | 1 |
| **Total** | **131** | **115** | **6** | **0** | **6** | **4** |

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
| WS-02 | Fonts | IBM Plex Sans 13px for the UI, IBM Plex Mono for numbers and readouts. | ✅ | Bundled in `crates/jstream-ui/fonts/` under the SIL OFL 1.1, with `OFL.txt` beside the files ([ADR-0077](adr/0077-bundle-ibm-plex-fonts.md)): IBM's Latin-1 woff2 subsets, Sans at 400, 600 and 700 and Mono at 400 and 600, served from the page's own origin as its CSP requires. `font-display: swap` shows the system font until they load. |
| WS-03 | Scrollbars, focus and selection | Thin 10px scrollbars (`#3A3F48` thumb), 2px accent focus ring, `#2B4A7A` text selection. | ✅ | |
| WS-04 | Icons | 24×24 stroke icons (1.8 stroke, round caps) for source types, scene kinds, labels and tools, in place of text glyphs. | ✅ | The prototype's icons are in `icons.rs`, its mask icon among them (the scene-reference badge, [OB-05](#objects)), with a close and a reset icon added; the Add source, Properties and filter dialogs, the canvas tools, the Scenes and Scene objects rows, the Filters panel, the labels (chips, tag buttons, the manager's icon picker), the Inspector's header, the output cards and the settings' reset use them. The dialogs' close buttons keep the prototype's ×. |
| WS-05 | Number field | Stepper with − and +, typed entry (Enter commits, Escape cancels, `−` and `,` accepted), arrow keys (Shift fine, Ctrl coarse), and pointer scrubbing (3px per step, Shift ×0.1, Ctrl ×4). | ✅ | `NumberField` (`panels/number.rs`), used by the Inspector and the mixer's channel rows (named by the row, with no label above) and strips (stacked, scrubbed up and down); the filters and dialogs take it as they are rebuilt. A drag shows its value as it goes and is sent once on release, so it is one undo step, where the prototype writes every step. |
| WS-06 | Foldable section headers | Caret plus uppercase title; folded sections show a hint ("cropped", "in use"); fold state remembered per title. | ✅ | `Fold` in `inspector.rs`: caret, uppercase title and folded hint ("cropped", "in use", the transitions hint). `Folds` remembers each title's state across items and reloads, in the browser's storage (`jstream.folds`); 3D and Scene switches (IN-10) start folded. The mixer's Buses section uses it too. |
| WS-07 | Hover-revealed row buttons | Row tools fade in on hover or focus, and stay shown while their state is on. | ✅ | The Scenes and Scene objects rows (`tree.rs`): the buttons fade in over 120 ms on hover or focus, and a hidden item's eye, a favourite's star and an open tag button stay shown; the filter rows' × (`filters.rs`). The Transitions panel's preset editor shows Make default and Delete without hover, on every preset but the default ([TR-03](#transitions)). An output card's Start, Stop, Edit and Delete are its main controls, not row tools, so they always show. |
| WS-08 | Toasts | One toast at a time, bottom centre, hides after 4.5 s. | ✅ | `Toast` in `app.rs` (`Studio::notify`, `apply_noting`): "Added a reference…" after placing an existing source again or Duplicate as reference, "Added a clone…" after Duplicate as clone. Failed calls stay in the sticky problem bar. |

<a id="header"></a>
## Header bar
<!-- @section
covers = ["crates/jstream-ui/src/app.rs", "crates/jstream-ui/src/header.rs", "crates/jstream-ui/src/panels/outputs.rs"]
-->

Spec: [workspace › header bar](../design/ui-prototype/spec-workspace.md#header-bar).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| HD-01 | Brand and menu row | Accent square and "JStream", then File, Edit, View and Layout menus; one top menu open at a time, closed by any outside press. | ✅ | The prototype leaves File and Edit empty; the real ones hold the connection details and Disconnect, and Undo and Redo. The menus behave as a desktop menu bar: a click opens or closes one; while one is open, pointing at another menu opens it instead; Escape closes it. From the keyboard, Down, Enter or Space opens the focused menu at its first item (Up at its last), Up, Down, Home and End move through it, Left and Right move along the bar (opening the next menu while one is open), and Tab closes it. Keys with Ctrl, Alt or Meta are left to the hotkeys. |
| HD-02 | View menu | Lists all 11 panels with "shown" or "add"; clicking hides a shown panel or adds a hidden one to its home zone. | ✅ | All eleven, in the prototype's order. Live chat and Stream stats wait on YouTube sign-in ([live panels](#live-panels)), so for now they say what they wait on. Beyond the prototype, a Projectors group under them opens the program or the multiview in a window or full screen ([DL-07](#dialogs)). |
| HD-03 | Layout menu | "Layout: <name>", saved layouts with "current", "Save current layout as new", "Reset to default". | ✅ | |
| HD-04 | Stats readout | Mono "60 fps · 0 dropped · CPU 3%". | ✅ | Built from `stats.tick`: the frame rate is the program's over the last tick's interval, and shows only while an output runs, since the program stage draws nothing otherwise; "N dropped" counts video frames dropped since the page connected; CPU is the engine process's share of every logical CPU, as Task Manager counts it, "<1%" below half a percent (protocol 0.48's `load`). The daemon and the desktop app tick about once a second while idle too. CPU shows on Windows and macOS; elsewhere the engine can't read it and the readout leaves it out. |
| HD-05 | Privacy button | "Privacy" / "Privacy on" (red); blurs every output and mutes the mic; Pause toggles it. | ⏸ | Removed at the maintainer's request after the alpha: JStream has no privacy screen ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md) §4). Bringing it back is the maintainer's call. |
| HD-06 | Studio mode button | Turns studio mode (preview and program) on and off. | ✅ | A page setting, since the engine has no studio-mode switch: off at first, kept by the browser, and pressed (blue) while on. On, a click on a scene puts it on preview ([SC-10](#scenes)) and the Transitions panel shows preview and program ([TR-06](#transitions)); off, a click puts it on program and the panel shows only the presets. |
| HD-07 | Record button | Starts and stops recording. | ✅ | Starts every stopped or failed recording output; "Stop recording" while any runs; disabled with none. |
| HD-08 | Broadcast picker | Shows the YouTube broadcast Go Live uses; menu lists broadcasts and "+ Create new stream…". | 🔒 | YouTube sign-in needs the OAuth client (A6b, Y4, [ADR-0058](adr/0058-youtube-sign-in-with-a-jstream-client.md)). |
| HD-09 | Go Live / End stream | Red "Go Live", dark red "End stream" while live, at the right end of the header. | ✅ | Starts every stopped or failed stream output whose key is set. |

<a id="docking"></a>
## Docking and layouts
<!-- @section
covers = ["crates/jstream-ui/src/app.rs", "crates/jstream-ui/src/dock.rs", "crates/jstream-ui/src/workspace.rs"]
-->

Spec: [workspace › dock zones and docking](../design/ui-prototype/spec-workspace.md#dock-zones-and-docking).
The dock is built (`dock.rs`, `workspace.rs`) with the prototype's eleven panels: Scenes,
Scene objects, Inspector (with the pairing panel under the properties), Transform,
Filters, Live chat, Stream stats, Audio mixer, Outputs, Agent activity and Transitions
(studio mode and the presets). Live chat and Stream stats say what they wait on until
their rows are built ([live panels](#live-panels)).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| DK-01 | Eight dock zones | Outer left and right columns, full-width top and bottom, left and right sidebars, above and below the canvas, nested as the spec says; empty zones hidden. | ✅ | |
| DK-02 | Panel groups and tabs | A zone stacks groups; a group holds tabs (uppercase, accent underline on the active tab of a multi-tab group). | ✅ | |
| DK-03 | Resize | 5px splitters resize zones within their limits; 6px dividers trade space between groups. | ✅ | |
| DK-04 | Collapse | Clicking a lone panel's title collapses it (not in a tab group, not the only group in its zone); in a side-by-side zone it becomes a narrow strip with its title turned. | ✅ | |
| DK-05 | Close twice | × arms red on the first click, closes on the second; disarms after 3 s or when the pointer leaves. | ✅ | |
| DK-06 | Drag to dock | Drag a tab onto a tab strip (group), a panel's half (split before or after), or a zone (append); empty zones show "Drop here" placeholders while dragging. | ✅ | Pointer drag rather than HTML drag and drop, which desktop webviews can intercept. Unlike the spec's `cursor: grab`, a tab keeps the arrow pointer, on the maintainer's alpha feedback; the grabbing hand shows only while it drags. |
| DK-07 | Home zones | A panel added from the View menu goes to its home zone (Scenes and Scene objects left; Inspector, Transform, Filters, chat, stats and agent right; Mixer, Outputs and Transitions bottom). | ✅ | |
| DK-08 | Editing and Live layouts | Editing is the default: left Scenes, Scene objects; right Inspector (weight 2), Filters+Transform; bottom Mixer, Outputs+Transitions. Live: Scenes; Mixer and Transitions under the canvas; chat, then stats+Outputs+Agent on the right. | 🟡 | Both built with the panels the UI has: Editing puts Filters and Transform in one group under the Inspector, Filters shown. Live puts Agent activity as a second tab beside Outputs; Live chat and Stream stats only say what they wait on, so Live leaves them out until the engine supports them. |
| DK-09 | "(modified)" and saving | Any move, close, resize or View toggle marks the layout "(modified)"; layouts can be saved as "My layout N" and reset. | ✅ | |
| DK-10 | Layouts persist | Layouts survive a reload, the same on the desktop app and in a browser. | ✅ | The engine's server keeps the dock, saved layouts included, in its UI settings (`ui_settings.get`/`set`, [ADR-0084](adr/0084-ui-settings-kept-by-the-server.md)), in the data folder the desktop app and the daemon share. The browser keeps a copy too, shown first and used alone with no server in front, and a change the server never got (the page closed within the send delay) is sent on the next load rather than lost. Another open client picks up a change when its workspace next opens. |

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
| CV-03 | Live picture | The scene's real picture under the item boxes. | ✅ | The program mix is under the boxes while the canvas shows the program scene (U4p, #302), so its transitions show. Any other scene, a keyframe scene too, shows its own picture: the canvas closes its stream and opens one of that scene (`preview.subscribe`'s `scene_id`, as the projectors do, [DL-07](#dialogs)), on a new channel so no late frame of the old one is drawn. Until the new stream's first frame, the canvas says it is starting. |
| CV-04 | Selection chrome | 2px accent outline, 8 white 10px handles, rotation knob on a stem, size pill under the item ("460 × 410 · cropped"); hover outline on other items. | ✅ | The pill adds " · cropped" for a cropped item. Over the live picture an unselected item has no box, except a hidden one, which keeps its dashed outline since it still takes clicks; while the picture isn't there (its stream starting, refused or ended, CV-03), every box gets a faint outline and its name. |
| CV-05 | Move with snapping | Drag moves; snaps edges and centres to the canvas and other items within 6px, with magenta guides; Ctrl turns snapping off for the drag. | ✅ | A rotated item doesn't snap, as in the prototype. The live picture follows the drag through live values ([ADR-0032](adr/0032-live-edits-and-undo.md), #747), and the drop is one undoable edit. |
| CV-06 | Resize | Handles keep the aspect ratio; Ctrl stretches; Shift halves pointer speed; minimum 10px. | ✅ | A corner follows the axis that snapped, else the larger change; an edge scales the other axis about the centre. |
| CV-07 | Rotate | Knob rotates; Ctrl steps 45°; snaps to 0/90/180 within 3°. | ✅ | Off the 90° snap, the angle rounds to 0.1°. |
| CV-08 | Crop | Alt-drag a handle, or any handle in crop mode, crops; amber handles and a dashed amber ghost of the uncropped extent. | ✅ | The drag moves one edge of the item's picture, which keeps its scale, and stops at the uncropped extent; the drop is one undoable batch of `item.set_geometry` and `item.set_crop` (crop is in source pixels), shown live as it goes. The box becomes the picture's rectangle, so a `fit` box with room around its picture closes onto it, and a `fill` box's clipped margins become crop. It needs the source's size (`source.sizes` and `source.size_changed`, [ADR-0022](adr/0022-item-transform-and-canvas.md)): a capture source has one while an output runs and its capture has a frame, and a nested scene's is the canvas. Without one, crop mode has no crop handles, the box's handles resize as usual (with Alt too) and the pill says "no source size yet"; the Inspector's crop fields still work. In crop mode the selection, its handles (12 px, filled) and its pill are amber, and the ghost shows whenever the item is cropped. |
| CV-09 | Multi-select | Shift-click adds or removes; Ctrl+A selects all; the group gets a dashed box with its own handles to scale and rotate. | ✅ | The group box can reach past the frame; the pointer is followed over the whole canvas area so its handles stay reachable. A corner scales by the larger change (as a single item's resize does), where the prototype takes the larger factor; they differ only when shrinking. |
| CV-10 | Box select | Drag on empty canvas draws a marquee; Shift adds; a click on empty canvas selects the scene. | ✅ | A marquee can start on the dark margin too. Hits use each item's axis-aligned bounds. |
| CV-11 | Floating toolbar | Top right: Move/resize (V), Crop (C), then Snapping. | ✅ | Move/resize leaves crop mode; Crop toggles it, in the prototype's amber when on. |
| CV-12 | Nudge | Arrows move 1px, Shift 10px. | ✅ | |
| CV-13 | Double-click | Opens the item's Properties (a reference opens its source's). | ✅ | Selects the item alone and opens its source's Properties dialog ([DL-02](#dialogs)), which is the same for every placement of the source. A nested scene opens the scene's Properties dialog ([DL-03](#dialogs)). |
| CV-14 | Right-click | Opens the canvas context menu ([menus](#menus)). | ✅ | On an item, the selected items' menu, after selecting the item alone unless it was selected; on the frame and the margin, theirs ([CM-02](#menus)). |
| CV-15 | Opacity, blend and 3D drawn | Items draw with their opacity, blend mode and 3D tilt. | ✅ | The picture comes from the engine. It draws every blend mode from protocol 0.47, in linear light except overlay, difference and exclusion, which blend in sRGB as other editors do ([ADR-0082](adr/0082-blend-modes-drawn.md)), and any 3D from 0.34 (X1). |
| CV-16 | Keyframe banner | "◆ Keyframe of “<parent>”: only placement and visibility can change" while editing a keyframe. | ✅ | A keyframe scene shows its parent's items at its overrides, under the banner, and its edits are placement edits addressed to it (K1). Its drags send live values addressed to it, as their commit is, which the engine takes on a keyframe scene from protocol 0.53 (#846). |
| CV-17 | Privacy overlay | Blurred frame with "Privacy screen on · output blurred · mic muted". | ⏸ | Removed at the maintainer's request after the alpha: JStream has no privacy screen ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md) §4). Bringing it back is the maintainer's call. |
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
| CM-01 | Menu mechanics | One menu at the pointer, kept on screen; check column, colour swatch, label, shortcut hint, submenu arrow; Windows-style submenus that open on hover; full keyboard navigation; any outside press closes. | ✅ | Built, with the colour swatch column (an 8 px dot, transparent unless set), which the item menu's Labels submenu uses for each label's colour. Items the UI or the engine can't do yet show disabled, as the prototype's "(dis)" items do. |
| CM-02 | Canvas menus | Item: Properties, Edit filters, Pop out, reference/clone, layer order, Center/Fit/Stretch, Reset rotation, Clear crop, Crop, Duplicate as reference/clone, Remove, Select all. Empty canvas and margin: Select all, snapping, Add source, scene transitions and Properties. | 🟡 | Built, with an Audio check for a source with sound, a Labels submenu for one item (beyond the prototype, since its labels are set from rows only), and the prototype's shorter menus for a nested scene and an audio-only source. Properties opens the Properties dialog ([DL-02](#dialogs)), and Edit filters selects the item and shows the Filters panel ([FL-01](#filters)). Crop enters crop mode ([CV-08](#canvas)), or reads "Leave crop mode" in it, for one item. Clear crop gives each item its uncropped extent back when its source's size is known, else clears the crop alone. The reference ↔ clone block ([CM-06](#menus)), the two duplicates and Group selection ([OB-04](#objects)) work. The margin's Transition in and out ([CM-07](#menus)) and Scene properties… ([DL-03](#dialogs)) work on the scene the canvas shows. Edit scene filters… shows the scene's chain in the Filters panel ([FL-06](#filters)). Pop out opens the scene the margin's menu is for, or a nested scene's item, as a projector ([DL-07](#dialogs)); on any other item it shows disabled. Remove from scene works, though the prototype shows it disabled. |
| CM-03 | Scene row menu | Rename, Transition in ▸, Transition out ▸, Properties, Edit filters, Pop out ▸, New scene, New keyframe, New folder, Duplicate scene, Delete scene. | ✅ | Built. Show in preview (`studio.set_preview`), Rename, Transition in and out ([CM-07](#menus)), Properties… ([DL-03](#dialogs)), New scene, New folder and Delete scene work, though the prototype shows Show in preview, Rename and Delete scene disabled; New scene and New folder land in the scene's folder. New keyframe works too (`scene.create_variant`, [SC-07](#scenes)), and Edit filters… shows the scene's chain in the Filters panel without switching to it ([FL-06](#filters)), and Pop out opens the scene in a window or full screen ([DL-07](#dialogs)). Duplicate scene copies the scene in one batch (decision register D43): "\<name\> copy" just below it, in its folder, with its labels, transitions, filters and items, each item placing the same source. Its keyframes and hotkey stay with the original. |
| CM-04 | Scene object row menu | Audio on/off, Properties, Edit filters, Pop out, reference/clone, layer order, Add source, New group, Remove from scene. | 🟡 | The selected items' menu of [CM-02](#menus), ending with Add source and New group instead of Select all. New group groups the selected items ([OB-04](#objects)). |
| CM-05 | Empty panel menus | Scenes: New scene, New folder, Paste scene. Scene objects: Add source, Add scene as source, New group, Select all, Paste. | ✅ | Both are built, with the Scenes panel's New folder and the Scene objects panel's New group, which groups the selection. Paste and Paste scene ([KB-05](#keyboard)) are enabled while the clipboard holds items or a scene; the empty canvas's Paste works the same. |
| CM-06 | Reference ↔ clone | A placement of a shared source toggles between "Reference to original" and "Independent clone". | ✅ | `item_menu.rs`, for one item (decision register D3, [ADR-0023](adr/0023-placements-references-and-clones.md)): a placement of a source placed more than once shows "Source" with "Reference to original" checked, and "Independent clone" gives it a copy of its own (`source.duplicate`, then `item.set_source`, keeping the item). A copy whose original still exists shows "Duplicate of <original>" with "Independent clone" checked, and "Reference to original" places the original again, deleting the copy if nothing else places it. Each switch shows the prototype's toast ([WS-08](#theme)): “<name> is now a clone. Changes stay with it.” or “<original> reference restored. Local changes were discarded.” |
| CM-07 | Transition in/out submenus | Default (preset), then each preset, then "Manage presets…". | ✅ | On a scene row and the canvas margin. The opener's hint names the override or "Default"; the submenu has "Default (<the default preset>)", checked with no override, an "Override" head, each preset with its kind and duration ("Fade 300 ms", "Cut"), checked on the override, and "Manage presets…", which shows the Transitions panel. A choice sends `scene.set_transitions`, keeping the other direction. Keyframe rows ([CM-08](#menus)) get them with those rows. |
| CM-08 | Keyframe and folder menus | Switch to it, Reset to parent, New keyframe, Delete keyframe; New folder, Rename, Delete folder / Ungroup. | ✅ | The keyframe menu: Switch to it (`scene.switch`), Rename, the transition submenus (empty: a keyframe follows its parent's), Reset to “<parent>” (a batch of `item.clear_override`s, disabled while it overrides nothing), New keyframe (of the parent) and Delete keyframe (`scene.delete`, at once since it holds no items, and undo brings it back; when anything refers to it, the delete plan after a confirmation, as for a scene). The folder menu: New scene and New folder inside the folder, Rename (F2) and Delete folder (Del), which deletes an empty folder at once and asks with `delete.plan`'s summary otherwise. A group's Ungroup is on its row's nested-scene menu ([OB-04](#objects)). |

<a id="dialogs"></a>
## Dialogs and windows
<!-- @section
covers = ["crates/jstream-ui/src/panels/confirm.rs", "crates/jstream-ui/src/dialog.rs", "crates/jstream-ui/src/panels/add_source.rs", "crates/jstream-ui/src/panels/source_properties.rs", "crates/jstream-ui/src/panels/scene_properties.rs"]
-->

Spec: [workspace › dialogs and floating windows](../design/ui-prototype/spec-workspace.md#dialogs-and-floating-windows).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| DL-01 | Modal dialog frame | Centred dialog over a dim backdrop; Esc and the backdrop cancel, Enter presses the primary button. | ✅ | `dialog.rs`. Enter isn't taken from a text area, a button or a select; from a field, it blurs the field first so its value is committed. The inline delete confirmation stays. |
| DL-02 | Properties dialog | 860 × 640: Name and the source's settings on the left, its video and audio filters on the right; Defaults, Cancel, OK; settings change only on OK. Opens on double-click and after Add source. | ✅ | Built (`source_properties.rs`): the Inspector's settings form (from `kind.describe`) editing a draft, sent with the name as one batch on OK, holding only what the user changed since it opened; Defaults empties the draft, so OK sends `null` for each value the dialog opened with. Also opened by Properties… in the item menus. The subtitle is the source's name, as the engine names no capture backend. The filter column is the Filters panel's chains (`filters.rs`), whose changes apply at once: a double-click on a row opens the Filter settings dialog ([DL-04](#dialogs)) over it, and "+ Add filter…" the Add filter dialog ([DL-05](#dialogs)). Beyond the prototype, as in OBS, a display, window or webcam source shows a live picture of what the settings being edited capture, above the name (`device_preview.rs`): a preview stream of the draft source (`preview.subscribe`'s `source`, 0.49), reopened when what is captured changes, so the device being picked shows before OK; a window source with no window picked says so. It shows the device without the source's filters. |
| DL-03 | Scene properties | Transition in and out, hotkey, background colour, multiview, "Include its audio when nested". | 🟡 | `scene_properties.rs`, the Properties dialog's 860 × 640 shell: Name, Transition in and out with the spec's notes, and a Hotkey field that takes the next key press as the chord (Backspace clears it), all a draft until OK, which sends what changed as one batch (`scene.rename`, `scene.set_transitions`, `hotkey.clear` and `hotkey.set` of a `scene.switch`); a chord bound already is refused, saying what it runs. Defaults clears the overrides and the hotkey. Opens on a double click on a scene row, the row menu's Properties…, the canvas margin's Scene properties… and a nested scene's Properties. The engine has no scene background colour, multiview or scene-wide "Include its audio when nested" (a nested placement's Audio check is [CM-02](#menus)'s), so those are left out. The filter column on the right is the scene's chain ([FL-06](#filters)), whose changes apply at once as in DL-02; a keyframe shows its parent's. |
| DL-04 | Filter settings dialog | Live-preview settings with an on/off switch; Cancel restores; Defaults; Remove. | ✅ | `filters.rs`, 460 wide, over the Properties dialog when opened from it: the kind's settings form, each change a `filter.set_settings` at once, the switch a `filter.set_enabled`. Cancel, Escape and the backdrop send one batch putting back the settings and the switch it opened with, including any the dialog sent that the engine hasn't reported back yet; Defaults resets every setting but a scene input's scene; Remove sends `filter.remove`. The hint line is the prototype's for the kinds it knows. |
| DL-05 | Add filter dialog | 720 × 520: searchable catalogue grouped by chain and group, details pane, "Uses the scene" picker for scene-input filters. | ✅ | `filters.rs`. The catalogue is `kind.list`'s filter kinds for the owner's chains (a source's, a scene's video chain, [FL-06](#filters), an item's effects, [FL-09](#filters), or the program output's or an output's audio chain, [FL-07](#filters)), not the prototype's wish list: the prototype's groups, names and hints for the kinds it knows, the engine's pixelate, aspect ratio and image mask in the groups they fit, effect-pack kinds under Effect packs and anything else under Other. A kind with a scene setting (the scene mask) needs its scene first, from the scenes that don't show the source, or for a scene's chain the scenes whose picture doesn't hold that scene. A filter with more than three settings, or whose kind isn't described yet, opens DL-04 once added; one with fewer opens in place in the panel. Add is off while the chain holds its eight filters. |
| DL-06 | Add source dialog | 720 wide: types grouped Capture, Audio, Media and web, Generated, Scenes; "Create new" with a name, or add an existing source as a reference; new sources placed at native size and their Properties opened. | ✅ | Built and opened from the right-click menus, with the prototype's names, icons and descriptions for the engine's kinds. A kind the prototype doesn't name goes under Other, and a placed source whose kind the engine doesn't list can be added again but not created. An existing source not placed anywhere goes at its own size (`source.sizes`, scaled down to fit the canvas), a nested scene covers the canvas, and an existing placement is copied 40 px right and down. A source created new has no frame yet, so it goes at the prototype's per-type native size, as a box with the `fit` mode, which draws it at its own aspect ratio. A new source's item is selected and its Properties dialog (DL-02) opens once the engine has accepted it; a placement of an existing source is selected with the Inspector shown. A blank name becomes "<type> N", counting that kind's sources. |
| DL-07 | Pop-out and projectors | A scene or source in a floating window, or full screen on a monitor. | 🟡 | A scene pops out from its Pop out submenu (its row, the canvas margin, a nested scene's item) into a window of its own, or full screen; View › Projectors does the same for the program and a multiview (program, preview beside it in studio mode, and up to 8 scenes marked "Show in multiview", program framed red and preview green). Each is the UI's own page (`projector.rs`) showing preview streams of the scene or the program (`preview.subscribe`'s `scene_id`, 0.46), filling its window; a double click fills the screen the window is on, and Escape closes it. The desktop app opens these as its own windows, a full-screen one full screen on the main window's monitor; a browser opens a screen-sized window, since a page may only fill the screen after a click on it. Not yet: a source alone (no preview stream draws one), and a list of monitors to pick from (a page can't list them without asking): a window is dragged to its monitor and double-clicked. |
| DL-08 | Create YouTube stream | Title, description, visibility, latency, start now or schedule, category, DVR, made for kids. | 🔒 | Y4 and the OAuth client ([ADR-0058](adr/0058-youtube-sign-in-with-a-jstream-client.md)). |

<a id="scenes"></a>
## Scenes panel
<!-- @section
covers = ["crates/jstream-ui/src/panels/scenes.rs", "crates/jstream-ui/src/panels/tree.rs"]
-->

Spec: [panels › scenes](../design/ui-prototype/spec-panels.md#scenes-panel).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| SC-01 | Scene list | Scenes in display order, with type icons; click selects; groups hidden. | ✅ | Each row has its scene's icon (`scene.set_meta`'s, else the scene icon) in the prototype's 30 px row. |
| SC-02 | "program" badge | The scene on program shows "program" in red after its name. | ✅ | |
| SC-03 | Search | "Search scenes" filters by name, keeping parents of matches. | ✅ | Case-insensitive, on the name. The folders that hold a match stay, open and dimmed, and can't be folded while the search is on. |
| SC-04 | Drag to reorder | Drag rows before or after others, or into a folder. | ✅ | Rows drag before or after others (`scene.reorder`, `folder.reorder`), with the prototype's drop line, and not while filtering. A drop on a folder row's middle half moves the row into it (`scene.set_folder`, `folder.set_parent`) and opens it; beside a row it joins that row's folder. A folder can't go into itself or a folder within it. A pointer drag rather than HTML drag and drop, as for the dock's tabs. |
| SC-05 | Rename, delete, new | Rename (F2 or menu), delete with the plan's summary, New scene. | ✅ | F2 or the menu's Rename renames in place, Del or Delete scene asks with the plan's summary, and the menus' New scene adds one with its name open. A double click opens the scene's Properties dialog ([DL-03](#dialogs)), which renames too (on a keyframe it renames in place); outside studio mode its first click puts the scene on program, as in OBS. |
| SC-06 | Folders | Folders organise scenes; collapsible. | ✅ | Each level lists its folders, then its scenes, each indented by its depth; a caret folds a folder, and the panel's, a scene's and a folder's menus add folders. `folder.*` and `scene.set_folder`; folders nest ([ADR-0024](adr/0024-scene-structure.md) §3, [ADR-0064](adr/0064-nested-scene-folders.md), #323). |
| SC-07 | Keyframe rows | Keyframes nest under their scene with a purple diamond icon. | ✅ | From `SceneInfo.variant_of` (K1). The scene's caret folds them away; they aren't dragged, and a drop on one does nothing. A new keyframe is selected, so it can be arranged. |
| SC-08 | Labels and favourites | Label chips and a favourite star per row; label manager (name, colour, icon). | ✅ | The favourite star (`scene.set_meta`, `source.set_meta`), label chips (each icon in its colour on a 16 % tint) and a hover tag button whose popover opens under the row, as the prototype's does, and puts labels on or takes them off, each box filled with its label's colour (`scene.set_meta` on a scene row, `source.set_meta` on a Scene objects row), and the label manager (`labels.rs`: name, colour, icon picker, + New label, delete), inline in the Scenes or Scene objects panel, whichever "Edit labels…" was chosen in. Scenes carry labels as the prototype's do ([ADR-0078](adr/0078-scenes-carry-labels.md)); the Inspector's header sets them too, and an item menu's Labels submenu sets a source's. A new collection file starts with the prototype's four sample labels (Intro / outro, Gameplay, Overlay, Sponsor, in its colours and icons). |
| SC-09 | Filter by favourites and labels | Funnel button with a count badge; "Favorites only", labels (any of), Clear, Edit labels. | ✅ | The funnel with its count, Favorites only, Labels · any of (each box filled with its label's colour), Clear and Edit labels…, client-side, in both panels: the Scenes panel's over its scenes' labels, the Scene objects panel's over its sources'. |
| SC-10 | Put on program | Clicking a scene switches to it (or, in studio mode, puts it on preview). | ✅ | A click selects the scene and sends `scene.switch` (the engine picks the transition), or, in studio mode ([HD-06](#header)), `studio.set_preview`, sent even for a scene already there (the engine ignores it), so a stale copy never drops a click. In studio mode a hover button (broadcast icon) puts the scene on program. The row menu's Show in preview works in either mode. |

<a id="objects"></a>
## Scene objects panel
<!-- @section
covers = ["crates/jstream-ui/src/panels/sources.rs", "crates/jstream-ui/src/store.rs", "crates/jstream-ui/src/panels/tree.rs", "crates/jstream-ui/src/panels/labels.rs"]
-->

Spec: [panels › scene objects](../design/ui-prototype/spec-panels.md#scene-objects-panel).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| OB-01 | Object list | "In “<scene>”, top to bottom" caption, then the scene's items top first, with type icons. | ✅ | The caption and each source's type icon (the scene icon for a nested scene) in the prototype's 30 px rows. A keyframe scene lists its parent's items as it shows them, under a note saying so: there the eye writes its override, and Del, dragging and Add source are off (ADR-0049). |
| OB-02 | Visibility eye | Hover-revealed eye; stays shown while hidden; hidden rows dimmed. | ✅ | |
| OB-03 | References in italics | A placement of a source shared with other scenes reads in italics. | ✅ | Italics when the source is placed in another scene too, as the Inspector's header does, since a source has no owner scene. |
| OB-04 | Nested scenes as groups | A nested scene (group) expands to show its items. | ✅ | `sources.rs`: a nested scene's row has a caret that shows its items under it, nested ones too. Group selection, or New group, makes a `group` scene ("Group N", left out of the Scenes panel) placed over the whole canvas where the topmost selected item was, and moves the items into it, keeping their IDs and boxes (`item.move`, decision register D4). Ungroup on a group's menu moves them back where it was, each keeping where it draws: a moved, scaled or turned group's placement is carried onto their boxes in the same batch (ADR-0024 §1), and the group goes; a hidden group's items are hidden with it. It shows disabled, with the reason, while another item places the group too, while the group scene has filters of its own, or while the group's look can't become their items' own: a crop, opacity, blend, item effects, 3D, a mirror, a fill of another shape or a 0.3 transform on the group's placement, or, once it is moved, an item with no box or an item turned in 3D, partly off the group's canvas (which clips it), turned against a stretch, or fitted inside one. Grouping: keyframe scenes' overrides of the selected items stay, as the items stay in the keyframe's tree (#846). Ungroup: a keyframe scene's override of the group's item isn't kept on that item, which goes (the engine drops it and reports it); Ungroup carries the keyframe's value of the group onto the items instead. It writes overrides in each keyframe scene whose tree holds the group's parent (a keyframe of the parent, or of a scene that places the parent as a group, at any depth), where that keyframe would otherwise draw an item differently: its override of an item, or its own value of the group, carried onto the item's box, and a group it hides hiding the items there; a keyframe value it can't carry keeps the group ("keeps its look"). In a keyframe scene ([SC-07](#scenes)) Group selection, New group and Ungroup show disabled, as they would change its parent's items. The prototype edits a group's items inline on the parent canvas; here a click on one opens the group's scene with it selected, as the engine draws a nested scene as one item. In a keyframe scene the children of the groups in its tree (through groups only, at any depth) list at its overrides: their eye writes its override, and a click selects the child in the keyframe, whose Inspector placement fields and item menu write its override, with what would change the parent's items off, as for the keyframe's own (#846, protocol 0.55); a plain nested scene's items still open their scene, and their eye does nothing there. The canvas draws no box for such a child, and the Transform panel's align, match and Clear crop, which work on the canvas's boxes, leave it as it is. |
| OB-05 | Scene-input filter rows | An object with a mask, matte, clip or displacement filter shows the scene it uses as a child row with a role badge. | ✅ | Every scene a filter on the object's source, or one of its effects, refers to in its settings is a child row one level in: the role badge (an Alpha mask, Luma matte or Clip to scene mask, by its mode, in purple; any other kind's scene as an Effect source, in amber; grey while the filter is off), the scene icon and the scene's name, with the tooltip "Alpha mask (off): uses the scene “Cam shape”". The object's caret folds them; under a plain object they start open, as in the prototype, and under a nested scene they open with its row, which starts folded. A click selects the scene. The rows have no tag, star or eye and aren't dragged. On a nested scene's row they come before its items, where the prototype puts them after. |
| OB-06 | Drag to reorder and group | Drag before, after or into a group. | ✅ | Rows drag before or after others (`item.reorder`), not while filtering. A drop on a nested scene's row (its middle half) or beside one of its items moves the dragged item into that scene (`item.move`); the engine refuses a move that would nest a scene in itself. A nested scene's items don't drag out; Ungroup does that. |
| OB-07 | Search | "Search objects". | ✅ | |
| OB-08 | Multi-select | Shift-click adds; shares the canvas selection. | ✅ | Shift-click adds a row or takes it out; every selected item's row is highlighted. |
| OB-09 | Add, remove, delete | Add source (dialog), Remove from scene, delete everywhere with the plan's summary. | ✅ | Add source is the dialog (DL-06), from the panel's right-click menu; Del or the item menu's Remove from scene removes; the row menu's Delete source everywhere… asks with the plan's summary. |

<a id="inspector"></a>
## Inspector
<!-- @section
covers = ["crates/jstream-ui/src/panels/inspector.rs", "crates/jstream-ui/src/panels/properties.rs"]
-->

Spec: [panels › inspector](../design/ui-prototype/spec-panels.md#inspector). Today the
Inspector shows the item's placement, then the source's settings form in a Properties
section. The Properties dialog (DL-02) uses the same form on a draft; the section stays
in the Inspector until the Inspector's own rows say otherwise. A display or window
capture's target is picked from a list of the displays or open windows the engine
reports (`capture.targets`), not typed as an ID.

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| IN-01 | Header | Type icon, name (italic for a reference), kind, "+N" for more selected, labels and favourite. | ✅ | Type icon (the scene's icon for a scene), name (italic when the source is placed in another scene too), kind, "+N", the source's or scene's label chips and its always-shown tag button, whose popover opens under the header, and the favourite star (`source.set_meta`, `scene.set_meta`). |
| IN-02 | Parameters | X, Y, W, H in px, and Rotation, as number fields bound to the canvas. | ✅ | Each change is one `item.set_geometry`. X and Y are the unrotated box's top-left corner. The fields show committed values, so a canvas drag shows in them once dropped. |
| IN-03 | Size in % | W % and H % of the source's own size. | ✅ | The box's width and height against the source's size (`source.sizes` and `source.size_changed`, [ADR-0022](adr/0022-item-transform-and-canvas.md)), 0.1 to 10000 %, following the W/H link. Blank and disabled while the engine knows no size for the source. |
| IN-04 | W/H link | Link toggle keeps the aspect ratio while editing W or H. | ✅ | On by default, kept while the panel is shown. |
| IN-05 | 3D | Tilt, Turn, Depth, Perspective, folded by default. | ✅ | Folded by default, with the "in use" hint; each field is one `item.set_geometry` ([ADR-0050](adr/0050-3d-transforms-rendering.md)). Perspective shows the prototype's 1200 px while the item has none, and the first change that puts the item in 3D sets it, since without one the engine draws 3D orthographically, where Depth has no effect. |
| IN-06 | Crop | Left, Right, Top, Bottom in px. | ✅ | `item.set_crop`, in source pixels. The canvas crops by dragging too (CV-08). |
| IN-07 | Blending | Mode (9 modes) and Opacity %. | ✅ | Opacity is `item.set_opacity`. The select lists all nine modes, each sent as `item.set_blend`, and the engine draws them all (0.47, [ADR-0082](adr/0082-blend-modes-drawn.md)). |
| IN-08 | Scene section | Objects count, "Used as a source in", hotkey. | ✅ | The Hotkey row lists the chords bound to the scene's `scene.switch`, each with × to unbind (`hotkey.clear`); Set key binds the next chord pressed (`hotkey.set`, after a `hotkey.clear` when it ran something else), and Escape cancels. A keyframe's header says “Keyframe of “<parent>””, and its item's blend mode is locked, with the prototype's note. |
| IN-09 | Scene transitions | Transition in and out selects, with the "next scene's in wins" note. | ✅ | `scene.set_transitions`, with "Default (<name>)" for none and the folded hint. The presets come from the shared collection. |
| IN-10 | Scene switches per item | Delay, easing, glide, appear and disappear effects. | ✅ | The "Scene switches" section, folded, with "customised" while any value differs from the default: Delay (0–3000 ms, step 10), Easing (Inherit, Ease in-out, Ease out, Linear, Spring), Glide to other scenes (Auto, Never), Appear and Disappear (Inherit, Fade, Scale, Slide, None). Each change sends `item.set_transition` with the whole settings. A keyframe ([SC-07](#scenes)) doesn't show it, since a keyframe changes only placement, visibility and opacity. |

<a id="transform"></a>
## Transform panel
<!-- @section
covers = ["crates/jstream-ui/src/panels/transform.rs"]
-->

Spec: [panels › transform](../design/ui-prototype/spec-panels.md#transform-arrange-panel),
[workspace › transform actions](../design/ui-prototype/spec-workspace.md#sources-duplicates-and-groups) (9.11).
All of it is client-side geometry, sent as `item.set_geometry` edits (Clear crop sends `item.set_crop`, with the uncropped box while the source's size is known, as the item menu does).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| TF-01 | Align to | Selection or Canvas. | ✅ | Selection is off with fewer than two selected, which align to the canvas. |
| TF-02 | Align | Left, centres, right, top, middles, bottom. | ✅ | On the unrotated boxes, as the prototype does. |
| TF-03 | Distribute | Horizontally and vertically, 3 or more selected. | ✅ | Equal gaps between edges in centre order; the first and last stay. |
| TF-04 | Arrange | Center on canvas, Fit, Stretch, Match width/height, Rotate 90° left/right, Reset rotation, Clear crop. | ✅ | Each press is one batch, so one undo step, and sends only the items it changes. Match takes the primary item's size. Clear crop resets `item.set_crop` and, while the source's size is known, gives the box its uncropped extent back, as the item menu's does; without a size the box stays as it is. |

<a id="filters"></a>
## Filters
<!-- @section
covers = ["crates/jstream-ui/src/panels/filters.rs"]
-->

Spec: [panels › filters](../design/ui-prototype/spec-panels.md#filters-panel). The protocol
has source filter chains (`filter.add`, `remove`, `set_enabled`, `set_settings`,
`reorder`; [ADR-0026](adr/0026-source-filter-chains.md)), and the Filters panel
(`filters.rs`) edits them. The master stage and each output own chains too
([ADR-0080](adr/0080-program-master-and-output-chains-v2.md)); an FX button in the Outputs
panel or on the Mixer's master card shows one in the panel (FL-07). A scene owns a video
chain (FL-06), and so does each item: its effects (FL-09,
[ADR-0065](adr/0065-scene-chains-and-item-effects.md)).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| FL-01 | Filters panel | For the selected source: separate Video and Audio chains, count per chain, "No video filters." when empty. | ✅ | The selected item's source, with its type icon, name and "Applies everywhere this source appears, top to bottom."; a reference shows its source's chains, which it shares. Video unless the source is audio-only, audio when it has a mixer channel and isn't a nested scene. Under them, the item's own effects ([FL-09](#filters)). With no item selected it shows the selected scene's chain ([FL-06](#filters)), and with no scene either "Nothing selected" and "Select a source or a scene." (F1). |
| FL-02 | Filter rows | Eye toggle, name (struck through when off), summary, caret; drag to reorder within a chain; × removes. | ✅ | `filter.set_enabled`, `filter.reorder` (a pointer drag, as for the tree rows; a drop on the other chain does nothing) and `filter.remove`. The summary follows F6 from the kind's descriptor: a scene input's scene (or "no scene"), a lone number with its unit, else up to two changed numbers or "defaults". The Properties dialog shows the same rows without the caret. |
| FL-03 | Edit in place or in a dialog | Filters with three settings or fewer expand in place; others open the settings dialog; double-click always does. | ✅ | In place, the kind's settings form in two columns under the row, each change a `filter.set_settings`; a refusal shows under its field, or in the problem bar once the row has closed. A filter just added opens in place. |
| FL-04 | Add filter | "+ Add filter…" opens DL-05. | ✅ | Under the chains, and in the Properties dialog. |
| FL-05 | Settings from descriptors | Each filter's settings come from its kind descriptor ([ADR-0025](adr/0025-typed-settings-kind-descriptors.md)). | ✅ | The Inspector's form (`properties.rs`) on `kind.describe`'s descriptor. The prototype's catalogue is a wish list; the engine lists gain, noise gate, compressor, limiter and delay, its built-in video filters and effect-pack kinds. A scene setting's select is the form's, listing every scene with None, not F8's "Choose a scene…" over the allowed scenes. |
| FL-06 | Scene filters | A scene's video chain. | ✅ | The `scene` owner ([ADR-0065](adr/0065-scene-chains-and-item-effects.md)), which the engine runs over the scene's composite. The Filters panel shows the selected scene's chain while no item is selected, with the scene's name and "Scene filters apply to the whole scene, top to bottom."; Edit filters… on a scene row ([CM-03](#menus)) and Edit scene filters… on the canvas margin ([CM-02](#menus)) show it, as an FX button does an output's, until another item or scene is selected. A keyframe scene shows its parent's chain, as it has none of its own. Rows, Add filter (video kinds only, foot "Applies to the whole scene.") and the settings dialog work as on a source's; the Add filter dialog's Uses the scene picker, on a scene's chain, offers the scenes whose picture doesn't hold that scene by nesting or as a keyframe; it doesn't follow other filters' scene references, and the settings form lists every scene, as for a source (FL-05), so the engine's cycle check refuses the rest. The scene Properties dialog shows the chain too ([DL-03](#dialogs)). |
| FL-07 | Program and output chains | Master chain on the program output, one chain per output. | 🟡 | The audio chains: an FX button ([OU-01](#outputs), [OU-02](#outputs), [MX-01](#mixer)) shows the program output's (`{"type":"master"}`) or an output's chain in the panel, titled "Program output" or the output's ID with the prototype's note ("Master chain: applies to everything leaving JStream.", "Only this output, after the program filters.", and "No sound." for the virtual camera, which has no sound and so no audio chain). Rows, Add filter and the settings dialog work as on a source's. Selecting another item or scene, or an item's Edit filters…, shows the selection's chains again: the item's source's, or with no item the scene's ([FL-06](#filters)). The video chains are 🔒: the engine refuses them so far ([ARCH › GPU pipeline](ARCHITECTURE.md#gpu-pipeline)), and the panel says "Video filters on outputs aren’t available yet." |
| FL-08 | CLAP plugin filter | Host a CLAP audio effect. | ✅ | [ADR-0035](adr/0035-clap-plugins-out-of-process.md): each effect the engine's scan finds is its own kind, `clap/<plugin id>`, so the Add filter dialog lists each plugin by name under Plugins with the hint "A CLAP audio effect", rather than the prototype's one "CLAP plugin" with a Plugin select. It goes on any audio chain: a source's, the program output's and an output's ([FL-07](#filters)); on the program output's the plugin runs on the master bus, which every output reads. Its settings are the plugin's parameters, from the kind's descriptor (FL-05). The live-audio run on Windows is #912. |
| FL-09 | Item effects | One placement's own video chain, after its crop. | ✅ | Beyond the prototype, which has no item effects ([ADR-0065](adr/0065-scene-chains-and-item-effects.md), the `item` owner, `ItemInfo.effects`). Under the selected item's source's chains, the Filters panel shows "This item’s effects" with "Only this item, after its crop, top to bottom.", an "Effects" chain ("No effects.", "N effects") and "+ Add effect…", for an item whose source has a picture, while no FX button's or scene's pick is shown. Rows, the Add filter dialog (video kinds only, "For this item of “Backdrop”", foot "Applies to this item only, after its crop.") and the settings dialog ("Effect on “Backdrop”") work as on a source's chain. In a keyframe scene the item's own effects show, as a keyframe doesn't override them. On an item's effects the Uses the scene picker offers the scenes whose picture doesn't hold the item's scene. The Properties dialog shows the source's chains only. |

<a id="mixer"></a>
## Mixer
<!-- @section
covers = ["crates/jstream-ui/src/panels/mixer.rs"]
-->

Spec: [panels › mixer](../design/ui-prototype/spec-panels.md#mixer-panel). The real mixer
does more than the prototype (buses, sends, routing); that stays ([Beyond](#beyond)).

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| MX-01 | Master card | "Master · program output", N channels, Loudness bar against a target marker with "−14.2 LUFS", True peak with "safe" or a warning, "FX N". | ✅ | `mixer.rs`: the short-term loudness on a bar from −60 to +6 LUFS, the marker at the −14 LUFS target (70%), momentary, short-term and integrated in the tooltip, the true peak's note in green or red, and a Reset button for the integrated loudness. "Hear" is [MX-05](#mixer)'s; "FX N", on the card and on the vertical master strip, counts the master chain's filters and shows it in the Filters panel ([FL-07](#filters)). |
| MX-02 | Channels fold under the master | Channels hidden under the master card by default. | ✅ | The master card's button folds them (not remembered across reloads). The buses, which the prototype doesn't have, fold under their own Buses header below. |
| MX-03 | Channel strips | Name, level bar, dB number field. | ✅ | Each row: name, the stereo meter as the level bar, the console-taper fader ([Beyond](#beyond)), the dB field (`NumberField`, −100 to +24, step 0.5), mute and Routing. |
| MX-04 | Vertical meters | Vertical strips in side columns; a title-bar toggle flips orientation. | ✅ | `mixer.rs`: docked in a side column the meters turn vertical until the button in the mixer's title bar flips them, for the session. The master strip folds the channel strips beside it; each strip has its short name (the full one as a tooltip), a stereo meter filling upwards, the dB field stacked + over −, scrubbed up and down, and mute. Routing stays in the horizontal rows. |
| MX-05 | Monitoring | "Hear: after FX" / "before FX". | ✅ | Decision register D22: a bus's monitor (`bus.set_monitor`, protocol 0.42), which the engine plays on its device while the mixer runs. `mixer.rs`: the master card's Hear button (the vertical strip's "Hear post" / "Hear pre") turns the master's monitor on at the default output after FX, then switches it between after and before FX on the same device; it reads "Hear: off" while the master has none, where the prototype always hears something. Each bus strip's More picks its monitor (off, after FX or before FX) and names its output device, empty for the default output ([Beyond](#beyond)). |
| MX-06 | Privacy mute | The mic reads "muted (privacy)" while privacy is on. | ⏸ | Removed at the maintainer's request after the alpha: JStream has no privacy screen ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md) §4). Bringing it back is the maintainer's call. |
| MX-07 | Real levels | Meters show the engine's levels. | ✅ | `meters.tick`, with RMS, peak and peak hold; the prototype's numbers are simulated. |

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
| OU-01 | Program output row | "Program output" first, "master", with an FX button for the master chain. | ✅ | `outputs.rs`: the row above the output cards, its "FX N" button always blue, titled "Filters on everything leaving JStream", shows the master chain in the Filters panel ([FL-07](#filters)). |
| OU-02 | Output rows | Each output indented under it with its status and "FX N". | ✅ | Each output's ID, indented, with its state ("Stopped", "Live"…) and "FX N", blue once its chain holds a filter, titled "Filters for <ID> only". The cards below keep the controls the prototype lacks ([Beyond](#beyond)). |
| OU-03 | Footnote | "Program filters apply to every output; each output can add its own after them." | ✅ | Under the rows. |
| OU-04 | YouTube output | A YouTube stream row bound to the chosen broadcast. | 🔒 | A6b and the OAuth client. |

<a id="transitions"></a>
## Transitions
<!-- @section
covers = ["crates/jstream-ui/src/panels/studio.rs"]
-->

Spec: [panels › transitions](../design/ui-prototype/spec-panels.md#transitions-panel),
[workspace › transitions](../design/ui-prototype/spec-workspace.md#scenes-keyframes-transitions).
The engine side is T1 (#260); T1u (#304) built the Transitions panel: the studio bar in
studio mode, then the preset picker and editor, with each scene's overrides folded
under them.

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| TR-01 | Presets panel | Preset picker ("Name — Kind · N ms", default marked, "+ New preset…"), Name, Cut/Fade/Move. | ✅ | "Presets" with the spec's help text, then the picker ("Name — Cut" or "Name — Fade · 300 ms", " (default)" on the default, then "+ New preset…", which adds "New preset", a 500 ms fade, and picks it), then the picked preset's Name and a Cut / Fade / Move segmented control. It starts on the studio bar's chosen preset, else the selected scene's transition in, else the default. Each change is sent at once (`transition.update`). Each scene's overrides are also in a folded table under it. |
| TR-02 | Preset fields | Duration; Move: stagger, items in one scene only (Fade, Scale, Slide); easing (Ease in-out, Ease out, Linear, Spring). | ✅ | The kind's own fields come from `kind.describe`. |
| TR-03 | Default and delete | Make default, Delete (not the default); override counts. | ✅ | Make default and Delete, hidden for the default, as in the spec; Delete goes back to the picker's starting preset. The note reads "[The default: used unless a scene overrides it. ]Overrides: N in, M out." |
| TR-04 | Resolution order | Next scene's "in", then current scene's "out", then the default. | ✅ | The engine resolves it ([ADR-0029](adr/0029-transitions.md)); the studio bar names the preset it will use and why. |
| TR-05 | Move transition | Pairs items by source, interpolates placement, retargets mid-transition; unmatched items fade, scale or slide. | ✅ | The engine pairs by source, interpolates, retargets from what is on screen and fades, scales or slides the unmatched ([ADR-0029](adr/0029-transitions.md)); the program preview shows it. The page edits it: Move in the preset editor with its stagger and unmatched effect ([TR-02](#transitions)), and each item's glide and effects ([IN-10](#inspector)). |
| TR-06 | Studio mode | Preview and program, Transition and Cut. | ✅ | With "Swap" putting the old program scene on preview. Shown only in studio mode ([HD-06](#header)). |

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
| AG-01 | Agent activity | What MCP clients changed, newest first. | ✅ | `activity.list` (protocol 0.51, [ADR-0083](adr/0083-activity-log-over-the-protocol.md)): the engine's sentence for each call under the client's name and how long ago; JStream's own UIs are left out, a failed call shows its error code. |

<a id="keyboard"></a>
## Keyboard
<!-- @section
covers = ["crates/jstream-ui/src/canvas/mod.rs", "crates/jstream-ui/src/canvas/transform.rs", "crates/jstream-ui/src/hotkeys.rs"]
-->

Spec: [workspace › keyboard shortcuts](../design/ui-prototype/spec-workspace.md#keyboard-shortcuts).
The prototype shows Ctrl+D, Ctrl+Shift+D, Del, F2 and Ctrl+V as menu hints without
handlers; the real UI makes them work.

| ID | Feature | Requirement | Status | Notes |
|---|---|---|---|---|
| KB-01 | Canvas keys | C crop mode, V move tool, Esc deselect, Ctrl+A select all, arrows nudge. | ✅ | Arrows move every selected item, as one edit. Esc also leaves crop mode, and so does switching scenes. |
| KB-02 | Modifiers | Shift half speed or add; Ctrl no snap, stretch, 45° steps; Alt crops. | ✅ | Alt on a handle crops while the source's size is known (CV-08); Shift halves a crop drag's speed too. |
| KB-03 | Undo and redo | Ctrl+Z, Ctrl+Shift+Z, Ctrl+Y. | ✅ | The prototype has none; kept. |
| KB-04 | Edit keys | Ctrl+D duplicate as reference, Ctrl+Shift+D as clone, Del remove, F2 rename. | ✅ | F2 renames and Del removes in the Scenes and Scene objects rows (Del deletes a scene or a folder there, through its plan). Ctrl+D and Ctrl+Shift+D duplicate the item as a reference or a clone on the canvas and in its row, and Del on the canvas removes the selection; in a keyframe scene ([SC-07](#scenes)) the duplicates and Del do nothing, as they would change its parent's items. |
| KB-05 | Copy and paste | Ctrl+C, Ctrl+V. | ✅ | The page's own clipboard, not the system's (decision register D43, `clipboard.rs`). Ctrl+C on the canvas or an object row copies the selected items; on a scene row it copies the scene. Ctrl+V on the canvas or in Scene objects places the copied items' sources again in the shown scene, as references with the items' boxes, crop, visibility, opacity, blend, transition settings and effects, 40 pixels right and down when pasted into the scene they came from; a nested scene that would contain itself, or a source deleted since, is left out. In the Scenes panel Ctrl+V duplicates the copied scene, as Duplicate scene does ([CM-03](#menus)). Each paste is one undo step, and in a keyframe scene Ctrl+V does nothing. |
| KB-06 | Privacy key | Pause toggles privacy. | ⏸ | Removed at the maintainer's request after the alpha: JStream has no privacy screen ([ADR-0080](adr/0080-program-master-and-output-chains-v2.md) §4). Bringing it back is the maintainer's call. |
| KB-07 | Scene hotkeys | F1–F5 switch to the scene bound to them. | ✅ | Every binding in the collection (K1) runs its action while the page has the focus and no dialog or menu is open (`hotkeys.rs`), unless a field or a focused row took the key: F2 on a scene row still renames, and Del on a keyframe's object row does nothing. A focused header menu button takes the plain arrows, Enter and Space ([HD-01](#header)). The prototype's F1–F5 are bindings like any other, set from the Inspector ([IN-08](#inspector)). |

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
  peak hold, loudness reset, and a monitor on any bus, with its output device.
- Settings forms generated from kind descriptors, with groups, advanced settings,
  conditional fields and reset to default.
- A live picture of the display, window or webcam being picked in the Properties
  dialog, before OK, as in OBS ([DL-02](#dialogs)).
- Item effects: one placement's own filters, after its crop, in the Filters panel
  ([FL-09](#filters)).

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
   menu (CV-14) comes with the other menus in batch 5, and crop (CV-08) with the size
   as a % (IN-03) once the UI reads source sizes (`source.sizes`, protocol 0.40).
4. **Inspector and Transform:** the shared number field, placement fields, W/H link,
   crop, blending, scene section and transitions (WS-05, IN-01, IN-02, IN-04, IN-06 to
   IN-09), then align, distribute and arrange (TF-01 to TF-04).
5. **Menus and dialogs:** context menus, Properties dialog, Add source dialog,
   reference ↔ clone, groups, edit keys, copy and paste (CM-01 to CM-06, DL-01, DL-02,
   DL-06, OB-03, OB-04, KB-04, KB-05).
6. **Filters:** panel, rows, add and settings dialogs (FL-01 to FL-05, DL-04, DL-05).
7. **Trees and mixer:** search, drag to reorder, folders, labels and favourites, mixer
   master card, vertical meters and monitoring (SC-03 to SC-06, SC-08, SC-09, OB-01,
   OB-02, OB-06 to OB-09, MX-01 to MX-05).
8. **Keyframes:** keyframe rows, their menu and the keyframe banner (SC-07, CM-08,
   CV-16), on K1's keyframe scenes.
9. **Studio mode, chains and hotkeys**, on T1, D8p and K1's hotkeys: the studio-mode button and a
   scene click putting it on program or preview, the transition submenus, per-item
   switches and Move (HD-06, SC-10, CM-07, IN-10, TR-05); the program and output chains
   (FL-07, OU-01 to OU-03); and the scene hotkeys (KB-07).

🔒 rows follow their engine items: each of those PRs, or a UI PR right after it, flips its
rows here.
