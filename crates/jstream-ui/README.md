<!-- @doc
id = "CRATE-jstream-ui"
title = "jstream-ui"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-ui

<a id="purpose"></a>
## Purpose
<!-- @section
covers = ["crates/jstream-ui/src/**"]
depends_on = ["docs/ARCHITECTURE.md#crate-map/jstream-ui"]
-->

The web UI: one Rust app, compiled to WASM, for both the desktop app and the browser
([ADR-0045](../../docs/adr/0045-leptos-web-ui.md)). It is built with Leptos and rendered
client-side. The daemon serves it over HTTP and the desktop app loads it in a Tauri
webview. It is a client like the CLI and the MCP server: it holds a `dyn Control` from
`jstream-control` and never links the engine
([R-ARCH-003](../../docs/RULES.md#R-ARCH-003)). Layer 3: it builds on `jstream-control`
(layer 2) and `jstream-protocol`.

Contents ([Design](#design) explains the choices):
- **App** (`app.rs`): `App`, the root component. It shows the connect form until a
  session is open, then the shell (the header over the workspace), and keeps the shared
  state current from the session's events. `SessionContext` gives components the open
  session.
- **Header** (`header.rs`): `Header`, the bar along the top: the brand, the File menu
  (the connection's details and Disconnect), the Edit menu (Undo, Redo), the View menu
  (show or hide each panel) and the Layout menu (saved layouts), the frames dropped
  since the page connected, Record, and Go Live / End stream. `RecordButton` and
  `Dropped` are its logic, tested natively.
- **Dock** (`dock.rs`): `Dock`, the model of which panel sits where: eight `Zone`s
  around the canvas, each holding `Group`s of tabbed `PanelId`s, the zones' sizes, and
  the saved layouts (`Layout`, with Editing and Live built in). Every change goes
  through it (`place`, `close`, `toggle`, `click_tab`, `resize_zone`, `resize_between`,
  `save_as_new`, `apply`, `reset`), and it reads and writes itself as JSON. Tested
  natively.
- **Workspace** (`workspace.rs`): `Workspace`, which draws the dock around the canvas,
  with the splitters, dividers, tabs and close buttons, and drags a tab to dock it;
  `DockState`, the dock as context, which the header's menus change; and `restore`,
  the dock the browser kept.
- **Host detection** (`host.rs`): `desktop()`, the engine URL and token the desktop
  app hands the page through the `__JSTREAM_DESKTOP__` global, if the page runs there,
  and `desktop_vcam()`, what that global says about the virtual camera.
- **Session** (`session.rs`): `Session` (the connection and the engine's `welcome`),
  `Session::connect` and `Session::open` (`hello` with the client name `jstream-ui`),
  and `engine_url_for_page`, the engine URL the connect form offers.
- **Store** (`store.rs`): `Studio`, the state the scene, sources and properties panels
  share: the collection as `state.get` last reported it (`Collection`, with its buses
  for the bus picker), the program
  scene, the `Selection`, the source kinds and the kind descriptors read so far, and the
  last failed call, and `keys_changed`, which the outputs panel bumps when a stream key
  is set or cleared. Also the call helpers (`query`, `apply`, `list_kinds`, `plan_delete`),
  `plan_summary` and `fresh_id`.
- **Panels** (`panels/`): the scene list (`scenes.rs`), the sources list
  (`sources.rs`), the Inspector (`inspector.rs`: the selected item's box, crop,
  blending and settings, or the selected scene's objects, uses and transitions), the
  properties form inside it (`properties.rs`), the number field the panels share
  (`number.rs`) and the delete confirmation they and the mixer share (`confirm.rs`); `outputs.rs`: `Outputs`, the outputs panel
  (creating, editing and deleting streams, recordings and the virtual camera, their
  stream keys, start and stop, and the pipeline's health), and the calls and
  logic behind it and the header's Go Live, tested natively against `MockEngine`; and `mixer.rs`: `Mixer`, the
  audio mixer (faders, mutes, routing and buses, with meters and the
  master's loudness), also tested natively against `MockEngine`; and `studio.rs`:
  `StudioMode`, the Transitions panel (preview, program, Transition and Cut, the
  transition presets and each scene's overrides; [Design](#design)).
- **Pairing** (`pairing.rs`): `Pairing`, the pairing panel under the properties in the
  Inspector, shown only when the server pairs devices (start pairing and show the code, list and
  revoke devices), and the calls behind it, tested natively against `MockEngine` and a
  stub that answers them as the server does.
- **Canvas** (`canvas/`): the canvas editor. It draws the selected scene's item boxes
  over a placeholder background, edits their geometry with the pointer and the
  keyboard, and undoes and redoes ([Canvas](#canvas)). The preview stream is not
  hooked up yet.
- **Entry point** (`main.rs`): `start()`, which mounts `App` on the page's `<body>`;
  `index.html`, `style.css` and `Trunk.toml` are the page trunk builds around it
  ([Build](#build)).

<a id="design"></a>
## Design
<!-- @section
covers = ["crates/jstream-ui/src/**", "crates/jstream-ui/style.css"]
depends_on = ["docs/adr/0045-leptos-web-ui.md", "crates/jstream-control/README.md#design"]
-->

- **Connecting.** The connect form asks for the engine URL and the control token. The
  URL defaults to the page's own host with the API path (`ws://` for an `http:` page,
  `wss://` for an `https:` one), because the daemon serves the UI from the port the API
  is on. The token stays in memory: the page never stores it or writes it to the
  console, and the field is a password field. It may be left empty when the daemon
  served the page behind HTTP Basic with the token as the password: the browser's
  cached Basic credentials then carry the token on the upgrade, and no token
  subprotocol is sent. It may also be left empty in a browser that paired on the
  server's `/pair` page, which holds its device token in a cookie the browser sends on
  the upgrade ([ADR-0047](../../docs/adr/0047-browser-pairing-page-and-device-cookie.md),
  [jstream-server › Design](../jstream-server/README.md#design)); that page is the
  server's, not the UI's. In the desktop app the form is skipped: the
  app defines `window.__JSTREAM_DESKTOP__` (`{ url, token }`) before the page loads,
  and `App` connects to that engine at once (`host.rs`). That connection carries the
  token in the subprotocol, which `jstream-server` accepts
  ([jstream-server › Design](../jstream-server/README.md#design)). After a disconnect the form
  shows, without the token; reloading the page connects again. A failed connect shows the transport's reason;
  a browser doesn't say why an upgrade was refused, so a wrong token, a refused origin,
  an unreachable engine and a URL the page's policy blocks read the same.
- **The session.** `Session::connect` opens `jstream-control`'s `WsControl` and says
  `hello`. The open session is provided as Leptos context (`SessionContext`), so a
  panel calls the engine with `session.control.call(…)` and never sees the socket. The
  session is `Rc`-shared: everything in the page runs on one thread, and on `wasm32`
  `Control` needs no `Send`.
- **Events and the shared state.** One task reads the session's event stream. It keeps
  the header's revision current and hands each event to `Studio`, which re-reads
  the state with `state.get` after a `state.changed` newer than its copy (or after
  `events.dropped`), and follows `studio.changed` for the program scene. Reads coalesce:
  while one `state.get` is in flight, any number of further changes cost one more read.
  Panels never edit the copy: they send edits and the copy follows from the
  `state.changed` that comes back, so every client's change shows the same way. When
  the stream ends (the engine closed the connection) the UI goes back to the connect
  form with the reason. The loop reads the session from the context per event and
  holds no handle of its own, so Disconnecting drops the last one, which closes the
  socket.
- **Selection.** `Selection` is the scene being edited (shown by the sources list) and
  the items selected in it: a primary item (shown in the Inspector) and any
  others the canvas selected with it. A new session reads the program scene
  before the state, so it starts on the scene on program. It is fixed up after every
  change: a
  deleted scene falls back to the program scene, then the first listed scene, and an
  item that left the scene is dropped, the next selected one becoming primary. The
  canvas edits the same scene and selects the same items, so a click on a box selects
  its row and opens its properties, and every selected item's row is highlighted.
- **Scenes and sources.** The scene list shows scenes in display order and leaves out
  groups (ADR-0024), marks the program scene, and puts a scene on program with
  `scene.switch`. The sources list shows the selected scene's items with the top of the
  stack first. New scenes, sources and items get IDs from `fresh_id`, which includes
  the session's client ID so two UIs on one engine don't pick the same one. "Add"
  creates a source of a kind `kind.list` reports, or places an existing source or
  another scene (a `scene` source), as one batch, with a geometry covering the canvas
  (ADR-0022). An engine that lists no kinds (the mock) offers no new kinds, only
  existing sources and scenes. A `kind.list` that fails does the same, and its
  error shows as any failed call's does. Deleting a scene, or a source everywhere, reads `delete.plan` first,
  shows what else it takes, and sends the plan's edits expecting the plan's revision,
  so a change in between makes the engine refuse it rather than delete more than the
  user saw. Removing an item from its scene is a plain `item.remove`.
- **The Inspector** ([UI features › inspector](../../docs/UI-FEATURES.md#inspector))
  shows the primary selected item, or with none the selected scene, under a header with
  its name, kind, "+N" for the other selected items and a favourite star
  (`source.set_meta` or `scene.set_meta`). An item's name is in italics when its source
  is placed in another scene too. Its sections fold: Parameters (X and Y of the
  unrotated box's top-left corner, W, H and Rotation, sent as `item.set_geometry`; the
  W/H link, on by default, keeps the aspect ratio when either side changes), Crop
  (`item.set_crop`, in source pixels), Blending (the nine modes, of which the engine
  draws only Normal so the others are disabled, and Opacity, `item.set_opacity`) and
  Properties, the source's settings form. A scene shows its object count, the scenes
  that use it as a source, and its Transition in and out (`scene.set_transitions`, with
  the presets read by `state.get` whenever the collection changes). The panel is built
  once per selected item or scene and each field follows the collection, so an edit
  doesn't rebuild it or take the focus. The fields show committed values: a canvas drag
  shows in them once it is dropped.
- **Number fields** (`NumberField`, spec-panels.md G10) have − and + steppers and a
  unit. Typed text is sent on Enter or blur, Escape puts the value back, and `−`
  (U+2212) and `,` are read as a minus and a decimal point; ArrowUp and ArrowDown step
  (Shift a tenth for steps under 1, Ctrl, Meta or Alt ten times). A drag across an
  unfocused field steps once per 3 px (Shift a tenth, Ctrl four times, per move so a
  modifier never makes the value jump) and is sent once on release, so it is one edit
  and one undo step; a drag whose release the field never saw ends unsent at the next
  move with no button held. Values are kept to the field's range and rounded, and
  negatives are written with `−`. A value its panel doesn't send, because the edit
  would change nothing, puts the committed value back in the field.
- **The properties form** is generated from the kind's descriptor (`kind.describe`,
  read once per kind and kept), with no code per kind (ADR-0025): a checkbox, number
  (and slider when bounded), select, colour, text, asset (root and path) or reference
  select (of the scenes, the other sources, or the buses, with None for `null`, which
  for a `waveform`'s or `spectrum`'s `audio` is the program mix; a source reference
  restricted to audio, such as their `audio_source`, offers only the sources that
  report audio settings, less `scene` sources, as the engine accepts no other) per
  parameter type, in the descriptor's order, grouped by `group`, advanced
  ones collapsed, and hidden while their `visible_if` doesn't hold. A type this UI
  doesn't know is shown read-only. A change is checked with the descriptor's own check,
  then sent as a one-parameter `source.set_settings` patch, and Reset sends `null` for
  the default. The descriptor's or the engine's reason shows under the field. A
  `scene` source has no settings.
- **The shell** is the header bar over the workspace. A failed call shows in a
  dismissable bar. The look follows the UI prototype ([UI features](../../docs/UI-FEATURES.md)): dark only, with the
  prototype's palette as CSS variables in `style.css`. The font stack names IBM Plex
  first and falls back to the system's, since the page's policy loads fonts only from
  its own origin and Plex isn't bundled.
- **The workspace** follows the prototype's dock
  ([UI features › docking](../../docs/UI-FEATURES.md#docking)): eight zones nest around
  the canvas, outer columns, full-width top and bottom, sidebars, and above and below
  the canvas. A zone stacks groups (columns top to bottom, rows side by side); a group
  shows one panel with its title, or tabs. Panels are the six the UI has: Scenes,
  Scene objects (`Sources`), the Inspector (`Inspector`, with `Pairing` under it), the
  Audio mixer, Outputs and Transitions (`StudioMode`); `panel_view` maps each to its component, and a new panel
  adds a `PanelId` and a line there. The panels' own `h2` headings are hidden, since
  the tab is the title. Narrow windows stack the zones, without splitters.
  - Each group keeps every tab's panel mounted and hides the inactive ones, so a tab
    switch keeps a panel's state; moving a panel to another group mounts it afresh.
  - A tab drags with pointer events, not HTML drag and drop, which desktop webviews
    can intercept for file drops. Once a press moves 4 pixels it is a drag: empty zones
    show their "Drop here" placeholders, and the element under the pointer, through its
    `data-drop` attributes, names the target: a tab strip (join that group), half of a
    group's body (a new group before or after it) or a zone (a new group at its end).
    Escape cancels.
  - Splitters and dividers capture the pointer while they drag. Zone sizes and group
    weights are set through the CSS object model, which the page's policy allows,
    unlike `style` attributes. After every dock change the workspace fires a window
    `resize`, which the canvas measures itself on.
  - The dock is kept in the browser's storage (`jstream.dock`) after every change and
    read back when the workspace opens; a stored dock that doesn't parse is ignored. So
    a layout lasts per browser, not per engine, until the engine stores UI settings
    (decision register D35).
- **The header** keeps its own copy of the outputs, as the outputs panel does (both
  re-read on `state.changed` and `events.dropped` and follow `output.state_changed`),
  plus `Studio::keys_changed`, which the panel bumps after a stream key is set or
  cleared, since no event reports that. Go Live and End stream are described under
  Outputs. Record starts every stopped or failed recording output and, while any runs,
  becomes "Stop recording", which stops them; with no recording output it is disabled.
  The dropped readout counts captured, rendered and encoded frames dropped since the
  first `stats.tick` this page saw, starting over when the engine restarts. Undo and
  Redo in the Edit menu call `history.undo` and `history.redo`, like the canvas's
  buttons. A failed call shows in the shell's problem bar. The menus close on Escape
  or on a press anywhere outside them.
- **Outputs.** The panel reads the outputs and encoder profiles with `state.get`, then
  each output's `output.status`, and re-reads them on `state.changed` and
  `events.dropped`; `output.state_changed` updates one output's state in place. It
  creates RTMP(S), recording and virtual camera outputs (IDs `output-N`, the first free one), changes
  an RTMP server URL (`output.set_target`) and the encoder profile, deletes a stopped
  output, and starts and stops each. The header's "Go Live" starts every stopped or
  failed RTMP output whose stream key is set, and "End stream", which replaces it while
  any runs, stops the running ones; recordings start from their own buttons or the
  header's Record. Both try every output, even after one fails, and report
  each failure. An RTMP output without a key can't be started from the
  panel.
  A virtual camera output (`virtual_cam`) has no target, key or encoder profile, so its
  card shows none of them; neither "Go live" nor "End stream" touches it. The card says
  what the page knows of the camera's registration. Only the desktop app knows (its global's `vcam`,
  read in `host.rs`): there an unregistered camera, or one turned off with `--no-vcam`,
  can't be started, and the card gives the `jstream-daemon --register-vcam` hint. In a
  browser the page can't tell, so it lets the engine decide and explains its refusal:
  an unregistered camera ("is not registered") gets the full registration command, and
  an engine with no camera at all ("no virtual camera": not running on Windows, or the
  desktop app's `--no-vcam`) gets that, with no registration advice; on Windows 10 the
  sink's own reason ("it needs Windows 11") is shown as it is. The health line sums `stats.tick`'s drop totals over the last five ticks; a
  `seq` that doesn't increase or a total that shrinks (an engine restart) starts it
  over.
- **Pairing.** The pairing panel ([ADR-0044](../../docs/adr/0044-pairing-remote-browsers.md))
  calls `device.list` when it opens and shows only if that is served: a session with no
  server in front answers `unsupported` and the panel stays hidden. "Pair a device"
  calls `pairing.start` and shows the code, large, with a countdown from its
  `expires_in_secs` and how a browser uses it (`/pair` on the host's HTTPS address). The
  code shows until the countdown ends or a device pairs: every 3 seconds the panel
  re-reads `device.list`, and a device ID that wasn't listed when the code was drawn
  means the code was used. A newer code replaces the shown one. Each device shows its
  name and its paired and last-used dates (UTC), with a button that calls
  `device.revoke` and re-reads the list. The code is never logged or stored by the page,
  and `OpenCode`'s `Debug` form hides it.
- **Studio mode** ([ADR-0029](../../docs/adr/0029-transitions.md)).
  The Transitions panel (a tab beside Outputs in the Editing layout, under the canvas
  in Live) reads the scenes (groups left out) and presets with
  `state.get` and the studio state with `studio.get` into a copy of its own, since
  `Studio` keeps neither the presets nor preview, re-reads them on `state.changed` and
  `events.dropped`, and takes `studio.changed` in place. The engine has no
  studio-mode switch, so the panel always shows preview and program. Picking a scene sends
  `studio.set_preview`. "Transition" sends `scene.switch` for the preview scene, naming
  the preset picked beside it, or none so the engine resolves it; a line under the
  button names the preset the engine will use and why, in the engine's order (the
  switch's, the new scene's `transition_in`, the old scene's `transition_out`, the
  default). "Cut" names a `cut` preset (the default if it is one), and is off when the
  collection has none. With "Swap" on (the default), the scene that was on program
  then goes on preview with a second `studio.set_preview`, as in other studio tools;
  the engine itself never moves preview on a switch. While a transition runs, the bar
  shows an indeterminate progress bar for the `duration_ms` `studio.changed` reported;
  a cut, or program moving without a transition, clears it at once. A preview scene
  the list doesn't show (a group) reads "Choose a scene".
  The presets section lists each preset, adds one (ID from its name, `-2`, `-3`… when
  taken; a 300 ms ease-in-out fade by default), edits one with a `transition.update`
  naming only what changed (after a kind change, the patch is against the new kind's
  defaults, as the engine applies it), makes one the default and deletes any other.
  The kind's parameters come from `kind.list` and `kind.describe` (category
  `transition`), or the built-in `cut`, `fade` and `move` when the engine can't list
  them; enum, boolean, number and text parameters are editable, others read-only. A
  value the form refuses keeps the preset from being saved until it is fixed.
  Each scene's `transition_in` and `transition_out` are set with
  `scene.set_transitions`. Items' own transition settings (`item.set_transition`) are
  not edited here; they belong to the properties panel, which doesn't edit them yet.
- **The stream key is write-only** ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). Its
  password field is not bound to a signal: the panel reads it only on save, wraps it in
  a `SecretString` at once (trimming a pasted newline), empties the field and sends
  `output.set_key`. It never stores, logs or displays the key, and the engine never
  sends it back: the panel shows only `output.status`'s `key_set`, re-read after it
  sets or clears a key, since no event reports that. Another client's key change shows
  on the next re-read.
- **Mixer** ([ADR-0028](../../docs/adr/0028-audio-model-v2.md)). The panel reads
  the audio sources (those `state.get` reports with `audio`), the buses and the sends
  into a copy of its own, not `Studio`'s, and re-reads them on `state.changed` and
  `events.dropped`. Each source gets a strip,
  ordered by name: its meter, fader, mute and, folded under "Routing", whether its
  audio is on, when it is heard (`always` or `when_shown`) and its send into each bus
  (on or off, and its gain). A nested scene's strip has only the routing toggles,
  since it has no fader or sends of its own. Each bus gets a strip too, the master
  first, with renaming and (except the master) deleting. A delete reads `delete.plan`
  first, as a scene's or source's does: the strip asks, saying how many `waveform` or
  `spectrum` settings drawing the bus the plan clears, and sends the plan's edits
  expecting the plan's revision. The master's adds its
  momentary, short-term and integrated loudness, its true peak, and a button that
  resets the integrated loudness (`audio.reset_loudness`). New buses take the first
  free `bus-N` ID; the button is off at the protocol's bus limit (`MAX_BUSES`, 16).
  - **Faders** run from −100 to +24 dB on a console taper: unity at three quarters
    of the travel, the quiet end compressed, and snapping to 0 dB within a quarter of
    a dB. While the pointer holds a fader the strip shows the value under it; letting
    go (or each key press) sends one `source.set_audio` or `bus.set_fader`, so a drag
    is one undo step. Hearing the fader during the drag needs live values
    ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)), which `live.set` doesn't
    take for faders yet. Clicking the readout sets 0 dB. An accepted edit is applied to the shown
    state at once, so the strip doesn't jump back before the re-read.
  - **Meters** take `meters.tick`: an RMS bar over a fainter peak bar per channel,
    from −60 dBFS to full scale, and a peak hold that waits about 1.5 s, then falls
    about 3 dB a tick, and turns red at full scale. A source the tick leaves out loses
    its reading, and half a second without a tick (no output runs, so the mixer is
    stopped) blanks every meter.
- **Browser password managers are out of scope.** The stream key and the control
  token are secrets for the OS credential store, not for the browser's
  ([R-SEC-001](../../docs/RULES.md#R-SEC-001)), so their fields opt out: each is
  `autocomplete="off"` with a name that doesn't look like a credential, and neither
  sits in a `<form>`: a button click or Enter saves the key or connects, so there is
  no form submission for the browser to offer saving. A test checks both fields.
- **Native builds.** The crate is a workspace member, so `cargo clippy` and
  `cargo test` build it natively. Its logic is tested there against `MockEngine`
  (`session.rs`, `store.rs`, `panels/outputs.rs`, `panels/mixer.rs`) and in plain functions the panels are built on (the rows,
  what "Add" sends, the form's sections, parsing and checks); only the components
  themselves need a browser.
- **Debug bundle size.** A leptos view's type spells out every element and closure
  inside it, and a debug build names each function with those types: most of its
  `_bg.wasm` is that name section. The server refuses a file over its size limit
  ([jstream-server › Design](../jstream-server/README.md#design)), so the mixer's strips, rows, meters, faders and mute buttons return `AnyView` rather than
  `impl IntoView`, which keeps a nested view's type out of its parents' names. A
  component that grows as large does the same.

<a id="canvas"></a>
## Canvas
<!-- @section
covers = ["crates/jstream-ui/src/canvas/**"]
depends_on = ["docs/adr/0022-item-transform-and-canvas.md", "docs/adr/0032-live-edits-and-undo.md"]
-->

The canvas edits the boxes of [ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)
in canvas pixels. `transform.rs` maps the page to the canvas, hit-tests boxes and
handles, and turns a drag into a new geometry; `group.rs` does the same for several
selected items and the box select; `snap.rs` snaps; `model.rs` lays out the scene and
sends the edits. These are pure and tested natively, `model.rs` against `MockEngine`;
`mod.rs` is the component.

- **What it shows.** The selected scene ([Design › Selection](#design)), laid out with
  `Layout::of` from the shared collection, so it follows every state change the app
  reads. The frame is the largest 16:9 box in the canvas area, with the scene's name
  and the canvas size captioned bottom left. Boxes are outlined with
  `jstream-protocol`'s placement formula, so the overlay matches what the compositor
  draws. An item still stored as a 0.3 legacy placement is not drawn: its box depends
  on the source's runtime size, which the protocol doesn't report yet (ADR-0022's
  `source.sizes`), and a note top left counts such items. Hidden items are drawn
  dashed, and the box under the pointer gets a light outline.
- **Selecting and dragging.** A press selects the topmost box under the pointer and
  drags it, or, on a box already selected, drags every selected box. Shift-click adds
  a box to the selection (and drags them all) or takes a selected one out. A press on
  empty canvas, or on the dark margin around the frame, draws a marquee that selects
  every box whose bounds it touches (Shift keeps what was selected); a click there
  clears the selection. Ctrl+A selects every box. The shared selection follows each of
  these. A double click selects a box alone and shows the Inspector, in place of the
  prototype's Properties dialog, which the UI doesn't have yet. The pointer is
  followed over the whole canvas area, so handles reaching past the frame can be
  grabbed. The cursor shows what a press would do. The selected box has
  a 2 px outline, eight resize handles, a rotate knob on a stem above its top edge, and
  a readout under it: its size (and rotation, if turned), or during a drag its
  top-left, size or angle. A resize keeps the box's aspect ratio (Ctrl stretches): a
  corner follows the axis that snapped, else the one that changed more, and an edge
  handle scales the other axis about the centre. The opposite corner or edge stays
  put, in the box's own rotated frame, and a box never gets smaller than 10 pixels or
  flips. Rotation turns about the centre and snaps to the nearest multiple of 90°
  within 3°, else to 0.1°; Ctrl moves it in 45° steps. Shift drags at half speed for
  every gesture, following the pointer's movement so pressing it mid-drag doesn't
  jump. Arrow keys nudge the selected boxes by 1 pixel (Shift: 10). Escape cancels a
  drag, or clears the selection.
- **Several selected.** Each selected box keeps its 2 px outline without handles, and
  a dashed box 4 screen pixels outside their bounds gets the handles, a knob 42 pixels
  above it, and a readout 12 pixels below (`N items · W × H`, the size while scaling,
  the angle turned while rotating). Its handles scale every box from the far side of
  the bounds (an edge handle scales the other axis about the centre), keeping the
  aspect unless Ctrl is held, down to 20 pixels; the knob turns every box about the
  bounds' centre, in 45° steps with Ctrl. A move of several boxes snaps their bounds
  the way one box snaps, unless one of them is rotated.
- **Crop** on the canvas (Alt on a handle, the crop tool) needs each source's size to
  turn a dragged edge into source pixels, which the protocol doesn't report yet
  (ADR-0022's `source.sizes`), so the canvas doesn't crop.
- **Snapping.** A moved upright box's edges and centre snap, within 6 screen pixels,
  to the canvas's edges and centre lines and to the other boxes' edges and centres,
  using each box's axis-aligned bounds; the lines it lands on are drawn as guides. An
  upright box's resized edges snap the same way. Ctrl moves without snapping, a
  rotated box never snaps, and the Snapping button in the floating tools (top right)
  turns it off.
- **Undoable edits.** A drag changes nothing in the engine until it ends: the overlay
  draws the gesture locally, and on release one batch with an `item.set_geometry` per
  item it changed commits it, so each gesture (and each nudge) is one undo step. The
  local geometries stay drawn until no commit is in flight and the collection has
  caught up with the last one's revision; a later drag replaces only its own items',
  so a nudge still in flight isn't lost. A commit that fails shows its error and stops
  drawing its geometries, unless a later gesture has replaced them. It carries no
  `expected_rev`: a drag is last writer wins per field
  ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)). Live values
  (`live.set` with `ends_gesture` on the committing batch), which would show the drag
  on the output while it happens, are in the protocol but the canvas doesn't send them
  yet. Undo and redo (Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y while the canvas has focus, and
  the Edit menu) are `history.undo` and `history.redo`; nothing left to undo is
  silent, and an `undo_conflict` is explained in the problem bar.
- **Styles** are classes in `style.css`. The hosts' Content Security Policy has no
  `'unsafe-inline'` for styles, so a `style` attribute would be refused, static or set at
  run time (`style=move || …`, `attr:style`); a style computed at run time uses a Leptos
  `style:` binding, which sets it through the CSSOM. A test fails on any `style`
  attribute in the crate's sources outside test modules, or in `index.html`.

<a id="build"></a>
## Build
<!-- @section
covers = ["crates/jstream-ui/index.html", "crates/jstream-ui/Trunk.toml", "crates/jstream-ui/Cargo.toml", "crates/jstream-ui/src/main.rs"]
depends_on = ["docs/adr/0045-leptos-web-ui.md"]
-->

The UI is built with [trunk](https://github.com/trunk-rs/trunk) for
`wasm32-unknown-unknown` (a target `rust-toolchain.toml` already installs):

```sh
cd crates/jstream-ui
trunk build            # debug, into dist/
trunk build --release  # what the daemon and the desktop app ship
trunk serve            # rebuilds on change and serves dist/ locally
```

trunk needs the `wasm-bindgen` CLI at the version `Cargo.lock` resolves for
`wasm-bindgen`. It uses one found on `PATH`, and otherwise downloads it; a release build
also downloads `wasm-opt`. `dist/` holds `index.html`, the JavaScript glue, the `.wasm`
and the stylesheet, with content hashes in their names. It is not committed. trunk's
inline loader script carries a `{{__TRUNK NONCE__}}` placeholder (`create_nonce` in
`Trunk.toml`), which the serving engine replaces with each response's
Content-Security-Policy nonce.

CI's `wasm` job builds the UI this way on every PR, with trunk and the `wasm-bindgen`
CLI downloaded at pinned versions and checked against pinned SHA-256 sums, and runs
clippy for `wasm32` over `jstream-control` (with `ws`) and `jstream-ui`.

A page served by trunk is on a different port from the engine, so the engine refuses
its `Origin` unless it is configured as an extra origin.

The built page is tested in a browser by CI's `ui-e2e` job: Playwright against a mock
daemon ([end-to-end tests](tests/e2e/README.md)).
