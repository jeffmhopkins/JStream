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
  session is open, then the shell (the header over the workspace), or on a projector
  window's page the projector, and keeps the shared state current from the session's
  events. `SessionContext` gives components the open
  session.
- **Header** (`header.rs`): `Header`, the bar along the top: the brand, the File menu
  (the connection's details and Disconnect), the Edit menu (Undo, Redo), the View menu
  (show or hide each panel, and open the program or multiview projector) and the Layout menu (saved layouts), the stats
  readout (the program's frame rate, the frames dropped since the page connected and the
  engine's CPU use), Studio mode (kept in the browser's storage), Record, and Go
  Live / End stream. `RecordButton`, `Dropped` and `Load` are its logic, tested natively.
- **Dock** (`dock.rs`): `Dock`, the model of which panel sits where: eight `Zone`s
  around the canvas, each holding `Group`s of tabbed `PanelId`s, the zones' sizes, and
  the saved layouts (`Layout`, with Editing and Live built in). Every change goes
  through it (`place`, `close`, `toggle`, `click_tab`, `resize_zone`, `resize_between`,
  `save_as_new`, `apply`, `reset`), and it reads and writes itself as JSON. Tested
  natively.
- **Workspace** (`workspace.rs`): `Workspace`, which draws the dock around the canvas,
  with the splitters, dividers, tabs and close buttons (and the mixer's meter flip in
  its title bar), and drags a tab to dock it;
  `DockState`, the dock as context, which the header's menus change; and `restore`,
  the dock the browser kept.
- **Projectors** (`projector.rs`): `Projector`, the page a projector window shows (the
  program, one scene, one source alone or a multiview, filling the window), `Request`
  (what a projector page's query asks for, and its window name), `open`, `pop_out` (a
  scene's Pop out submenu) and `pop_out_source` (an object's), which open one, and `subscribe` and `multiview_scenes`, its logic, tested
  natively.
- **Host detection** (`host.rs`): `desktop()`, the engine URL and token the desktop
  app hands the page through the `__JSTREAM_DESKTOP__` global, if the page runs there,
  and `desktop_vcam()`, what that global says about the virtual camera.
- **Session** (`session.rs`): `Session` (the connection and the engine's `welcome`),
  `Session::connect` and `Session::open` (`hello` with the client name `jstream-ui`),
  and `engine_url_for_page`, the engine URL the connect form offers.
- **Store** (`store.rs`): `Studio`, the state the scene, sources and properties panels
  share: the collection as `state.get` last reported it (`Collection`, with its buses
  for the bus picker, its transition presets, its hotkey bindings, its scene folders and
  the master stage's chains, and `chains` to read any owner's; a keyframe scene's items
  are its parent's with its overrides applied, `shown_items`, and the children of the
  groups in its tree likewise, `keyframe_children`, which `item_in` and the selection
  count as its own), the program and preview
  scenes, whether studio mode is on (and `scene_click`, what a click on a scene sends
  in either mode), the `Selection`,
  each source's size (`Sizes`, from `source.sizes` and `source.size_changed`;
  `Collection::source_size` adds a nested scene's, the canvas), whether the canvas is in
  crop mode (which a switch of scene leaves), the source kinds
  and the kind descriptors read so far, the last failed call, the toast's confirmation
  (`notify`, `apply_noting`), and `keys_changed`, which the outputs panel bumps when a stream key
  is set or cleared. Also the call helpers (`query`, `apply`, `list_kinds`, `plan_delete`),
  `plan_summary`, `reset_edits` (a keyframe's Reset) and `fresh_id`.
- **Hotkeys** (`hotkeys.rs`): `Hotkeys`, which runs the collection's hotkey bindings
  while the page has the focus and no dialog or menu is open, and the chord of a key
  press, its label and the edits that bind a chord to a scene, tested natively.
- **Menus, dialogs and icons**: `menu.rs` (`ContextMenu`, the right-click menu, and
  `Menus`, the context that opens it with a list of `Entry`s, each with an optional
  check, colour swatch, hint or submenu), `item_menu.rs` (the
  selected items' entries, which the canvas and the sources list share, with the
  layer moves, canvas arrangements, reference ↔ clone switches, grouping and
  ungrouping they send, tested natively), `clipboard.rs` (copy and paste, KB-05: the
  page's own clipboard of items or a scene, and the batches Paste, Paste scene and
  Duplicate scene send, tested natively), `dialog.rs` (`Modal`,
  the modal dialog frame) and `icons.rs` (`Icon`, the prototype's stroke icons).
- **Panels** (`panels/`): the scene list (`scenes.rs`, with its folders: the rows each
  level shows and the edits a drop sends are tested natively), the sources list
  (`sources.rs`), what those two share (`tree.rs`: the search box, the favourites and
  labels filter, the star and drag reordering, with the matching and the drop's index
  tested natively), labels (`labels.rs`: the chips, the tag button and its popover, the label manager
  and the `LabelManager` context that opens it in the Scenes or Scene objects panel,
  with the edits they send to a scene or a source tested natively), the Inspector (`inspector.rs`: the selected item's box, 3D, crop,
  blending, scene switches and settings, or the selected scene's objects, uses and transitions, in
  sections whose folds `Folds` remembers by title), the
  properties form inside it (`properties.rs`), the Add source dialog (`add_source.rs`:
  `AddSource` and the `AddSourceDialog` context that opens it, with its type list and
  the edits it sends, tested natively), the Properties dialog (`source_properties.rs`:
  `SourceProperties` and the `PropertiesDialog` context that opens it, with its live
  picture of a capture source in `device_preview.rs`, whose request and what it follows
  are tested natively), the scene
  Properties dialog (`scene_properties.rs`: `SceneProperties` and the `SceneDialog`
  context that opens it, with the edits OK sends and the hotkey chords tested
  natively), the Filters
  panel and its dialogs (`filters.rs`: `Filters`, `Chains`, which the Properties dialog
  shows too, `AddFilter`, `FilterSettings` and the `FilterDialogs` context that opens
  them and holds the chain an FX button picked, with the catalogue (a scanned CLAP plugin's `clap/` kind under Plugins), the row summaries,
  the choice of in place or dialog, each owner's chains, name and note (an item's
  effects too), and Cancel's restore patch tested natively), the number
  field the panels share (`number.rs`) and the delete confirmation they and the mixer share (`confirm.rs`); `outputs.rs`: `Outputs`, the outputs panel
  (the program output and each output with its FX button, creating, editing and
  deleting streams, recordings and the virtual camera, their stream keys, start and
  stop, and the pipeline's health), and the calls and
  logic behind it and the header's Go Live, tested natively against `MockEngine`; and `mixer.rs`: `Mixer`, the
  audio mixer (the master card with its loudness and true peak, the channels folded
  under it with their meters, faders, dB fields, mutes and routing, the same as
  vertical strips, and the buses),
  also tested natively against `MockEngine`; and `studio.rs`:
  `StudioMode`, the Transitions panel (preview, program, Transition and Cut in studio
  mode, the preset picker and editor, and each scene's overrides; [Design](#design)),
  and the scene's Transition in and out submenus the menus share
  (`override_submenu`); and
  `transform.rs`: `Transform`, the Transform panel (align, distribute and arrange the
  selected items).
- **Pairing** (`pairing.rs`): `Pairing`, the pairing panel under the properties in the
  Inspector, shown only when the server pairs devices (start pairing and show the code, list and
  revoke devices), and the calls behind it, tested natively against `MockEngine` and a
  stub that answers them as the server does.
- **Canvas** (`canvas/`): the canvas editor. It draws the selected scene's item boxes
  over that scene's live picture (the program mix while it is on program), edits their geometry with the pointer and the keyboard, and undoes and
  redoes ([Canvas](#canvas)).
- **Entry point** (`main.rs`): `start()`, which mounts `App` on the page's `<body>`;
  `index.html`, `style.css` and `Trunk.toml` are the page trunk builds around it
  ([Build](#build)), and `fonts/` holds the bundled IBM Plex files with their licence,
  `OFL.txt` ([ADR-0077](../../docs/adr/0077-bundle-ibm-plex-fonts.md)).

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
  `events.dropped`), and follows `studio.changed` for the program and preview scenes.
  JStream has no privacy screen
  ([ADR-0080](../../docs/adr/0080-program-master-and-output-chains-v2.md) §4), so the
  UI sends no `privacy.*` call. It reads the
  sources' sizes with `source.sizes` after its first read and after `events.dropped`,
  and follows `source.size_changed` between them. Reads coalesce:
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
- **Scenes and sources.** The Scenes and Scene objects panels are the prototype's
  trees (spec-panels.md 1 and 2). The scene list shows each
  folder level's folders in folder order, then its scenes in display order (ADR-0064),
  indented by depth, with a caret that folds a folder, and leaves out groups (ADR-0024). It marks the program scene
  "program". A click selects a scene and puts it on program (`scene.switch`, naming no
  preset, so the engine resolves the transition); in studio mode it goes on preview
  instead (`studio.set_preview`), and a hover button puts it on program. The click is
  sent even for a scene already there, which the engine ignores, so a stale copy of
  program or preview never drops one; and outside studio mode the double click that
  opens a scene's Properties puts it on program first, as in OBS. Studio mode is the
  page's own, since the engine has no such switch: the header's button turns it on and
  off, and the browser keeps it (`jstream.studio-mode`). A
  scene's keyframe scenes (ADR-0024 §2) nest under it, one level in, with the diamond,
  folded away by its caret, and stay there: they aren't dragged. A keyframe scene has no items of its
  own; it shows its parent's at its overrides, so selecting one lists and draws those,
  and its eye, the canvas, the Inspector's placement fields and the item menu's
  placement choices send placement edits addressed to it, which write its overrides
  (ADR-0049). The children of the groups in its tree (through groups only) list at its
  overrides too: their eye writes its override, a click selects one in the keyframe,
  where the Inspector's placement fields write its override, and its row's item menu
  is the keyframe's, so its placement choices (Center, Fit, Stretch, Reset rotation and
  Clear crop, which `crop::clear` finds through `keyframe_child`) write the override and
  what is off below is off for it too, with Crop, since the canvas draws no box for it
  (#846, protocol 0.55); a plain nested scene's items still open their own scene. Every
  Transform panel button but Clear crop works on the canvas's boxes, so it leaves such
  a child as it is. What would change the parent's items (Del, dragging rows, Add source,
  New group, layer order, the duplicates, Remove, the blend mode, the reference or clone
  switch) is off there. The
  sources list shows the selected scene's items with the top of the stack first, each
  with its source's type icon, in italics when the source is placed in another scene
  too, the chips of its source's labels, an eye that shows or hides it, and a tag
  button whose popover, under the row, puts labels on the source or takes them off
  (`source.set_meta`). A scene's row has the chips of its own labels and the same tag
  button (`scene.set_meta`; scenes carry labels,
  [ADR-0078](../../docs/adr/0078-scenes-carry-labels.md)). A nested scene's row has a caret that shows
  its items under it, nested ones too; a click on one of them selects it in its own
  scene, which the list then shows, since the engine draws a nested scene as one item.
  A scene that one of the object's filters uses (a scene mask, or another kind's scene
  setting, on its source's chain or its own effects) is a child row under it with a role
  badge (OB-05); under a plain object they start open, under a nested scene they open
  with its row, the object's caret folds them, and a click selects the
  scene. Each panel has a search box and a filter for
  favourites (the star, `scene.set_meta` or `source.set_meta`), applied in the page; each
  filter also keeps the rows carrying any checked label, and its "Edit labels…" opens
  the label manager over that panel's rows, which renames, recolours, gives icons to,
  adds (`label.create`) and deletes labels; a delete takes the label off every source
  and scene that carries it in the same batch;
  rows drag before or after each other (`scene.reorder`, `folder.reorder`,
  `item.reorder`, counted in the whole list so a hidden row doesn't shift the index) with
  pointer events, but not while a filter is on, and a drop on a folder row's middle half
  moves the row into it (`scene.set_folder`, `folder.set_parent`); one on a nested
  scene's row, or beside one of its items, moves the item into that scene (`item.move`).
  A search keeps the folders and nested scenes that hold a match, open. F2 renames a row in place and Del deletes a scene or a folder (through its
  plan, or at once for an empty folder) or removes an item; a double click on a scene or
  an item opens its Properties dialog (on a keyframe it renames it). New scenes, sources and items get IDs from `fresh_id`, which includes
  the session's client ID so two UIs on one engine don't pick the same one. Sources
  are added through the Add source dialog (below). Deleting a scene, or a source
  everywhere, reads `delete.plan` first,
  shows what else it takes, and sends the plan's edits expecting the plan's revision,
  so a change in between makes the engine refuse it rather than delete more than the
  user saw. Removing an item from its scene is a plain `item.remove`.
- **Right-click menus and dialogs.** `menu.rs` draws the one right-click menu, which
  the shell provides as `Menus` context: a component opens a list of entries (items
  with a check, a colour swatch, a shortcut hint or a submenu, heads and separators)
  at the pointer, and
  the menu keeps itself on screen, opens submenus on hover or the right arrow, moves
  the focus with the arrows, Home and End, and closes on a choice, a press outside,
  Escape or Tab. The canvas opens the empty canvas or margin menu, the scene and
  sources lists their panels' menus, and a scene row its scene's (Show in preview,
  Rename, Transition in and out, Properties…, New scene, New keyframe, New folder and
  Delete scene work, as do a folder's own menu's, and Edit filters… shows the scene's
  chain in the Filters panel; Pop out opens the scene in a projector window, and
  Duplicate scene sends `clipboard.rs`'s copy of it). The
  Transition in and out submenus,
  on a scene row and on the canvas margin for the scene it shows
  (`studio::override_submenu`), start with "Default (<the default preset>)", checked
  with no override, then list each preset with its kind and duration, checked on the
  scene's override, and end with "Manage presets…", which shows the Transitions panel;
  a choice sends `scene.set_transitions` with the other direction kept. A keyframe row's menu switches to it,
  renames it, resets it to its parent (`item.clear_override` for each field it
  overrides), makes another keyframe of the parent (`scene.create_variant`) and deletes
  it: a plain `scene.delete`, since it holds no items, or the delete plan after a
  confirmation when anything refers to it. A right-click on an item, on the canvas or its row, selects it
  unless it was selected and opens `item_menu.rs`'s menu for the selection: Properties
  (the Properties dialog), for one item a Labels submenu (each label checked while the
  source carries it, with its colour as the swatch, then Edit labels…), the Audio check
  (`source.set_audio`), layer order
  (`item.reorder`s that keep the selected items' order), Center, Fit and Stretch to
  canvas and Reset rotation (`item.set_geometry`), Clear crop (`item.set_crop`, with
  each item's uncropped extent as its box while its source's size is known), Crop
  (crop mode, for one item), Duplicate as reference
  (`item.add` of the same source, 40 pixels down and right, just above the original)
  or as clone (`source.duplicate` first), also Ctrl+D and Ctrl+Shift+D, each confirmed by a
  toast, and Remove from scene, each one batch. Ctrl+C copies the selection (on a scene
  row, the scene) into the page's own clipboard (`Clipboard` context), and Ctrl+V,
  Paste or Paste scene sends one batch that places the copied items' sources again,
  with their placement settings and effects, or duplicates the copied scene. For one item of a source placed more than once, or a
  copy whose original exists, it switches between a reference and an independent clone
  (decision register D3): `source.duplicate` and `item.set_source` to the copy, or
  `item.set_source` back to the original and `source.delete` of a copy nothing else
  places, so the item keeps its ID. Group selection, and New group in the sources list,
  groups the selection (decision register D4): a `group` scene named "Group N", placed
  over the whole canvas where the topmost item was, and the items moved into it with
  `item.move`, keeping their IDs and boxes since the group shares the canvas. In the
  sources list, one item's menu ends with deleting its source everywhere. A nested
  scene and an audio-only source get the prototype's shorter menus; the first can be
  opened, and a group ungrouped: its items move back where it was, each given the box
  it draws at when the group isn't at its default placement (ADR-0024 §1), and the group
  scene and its sources are deleted; a hidden group's items are hidden with it. Each
  keyframe scene whose tree holds the parent (its own, or one of a scene that places
  the parent as a group, at any depth) gets the overrides it needs to keep drawing the items
  where it drew them through the group: its override of an item, or its own value of
  the group, carried onto the item's box, and hidden where it hid the group (#846).
  Ungroup shows disabled, with the reason, while another item places the group, the
  group scene has filters, or the group's look, or a keyframe scene's, can't become its
  items' own (`Kept` in `item_menu.rs`). The scene and object lists fill their dock group, so the space under
  their rows opens the panel's menu. `dialog.rs` is the modal frame: Escape and the
  backdrop cancel, and Enter presses the button marked `data-primary`. The Add source
  dialog (`panels/add_source.rs`, opened through `AddSourceDialog` context) lists the
  source kinds `kind.list` reports, under the prototype's groups, names, icons and
  descriptions, plus the kinds of placed sources it didn't report, which can be
  placed again but not created, and Scene. "Create new" sends `source.create` and
  `item.add` as one batch, with a box of the prototype's native size for the type,
  centred and scaled down to fit the canvas (ADR-0022's `fit` keeps the picture's
  aspect within it). An existing source is placed again 40 pixels right of and below
  its first placement, or, placed nowhere, at its own size when the engine reports one
  (`source.sizes`), scaled down to fit. A scene is nested through a `scene` source, reusing one that
  already shows it. The new item is selected; a new source's Properties dialog opens
  once the engine has accepted the batch, waiting for the collection to list the
  source, and a placement of an existing one shows in the Inspector. A `kind.list`
  that fails leaves only the placed kinds and Scene, and its error shows as any failed
  call's does.
- **Projectors** (`projector.rs`, [UI features › DL-07](../../docs/UI-FEATURES.md#dialogs)).
  A scene's Pop out submenu (on its row, on the canvas margin for the scene it shows,
  and on a nested scene's item), any other single object's (Source in a window or full
  screen, its source alone with its own filters) and the View menu's Projectors group
  open a projector:
  this same page, opened with `window.open` under one window name per target so asking
  again brings that window back, with a query (`?projector=program`, `multiview`,
  `scene:<percent-encoded ID>` or `source:<percent-encoded ID>`, then `&full=1` for full screen) that `App` reads before
  anything else. Such a page connects as the desktop app's page does, or in a browser
  at once with the credentials the browser already sends (the connect form if they
  aren't enough), keeps `Studio` current like the workspace, and shows `Projector`
  instead of the shell. Every picture on it is a preview stream (`preview.subscribe`,
  with `scene_id` for a scene and `source_id` for a source alone, 0.56) on the session's one binary channel, which `Channels`
  reads and hands out by channel number, each picture taking a number never used
  before on the page; a picture falls back to JPEG as the canvas does, says why when its
  stream ends (a deleted scene), and unsubscribes when it goes. One picture asks for the
  screen's size in device pixels, at most 1920×1080, at 30 fps. The multiview shows
  program (with preview beside it while the page's studio mode, read from the browser's
  storage and followed as the workspace changes it, is on) over up to 8 scenes that are not groups and are marked "Show in
  multiview", red-framed on program and green on preview. A double click fills the
  screen (the Fullscreen API) or leaves it, Escape closes a projector that isn't full
  screen, and the pointer and the hint at the bottom hide after 2 s without moving. A
  window the browser doesn't open is reported in the problem bar; a full-screen one in
  a browser opens as large as the screen, and the toast says to double-click it.
- **The Inspector** ([UI features › inspector](../../docs/UI-FEATURES.md#inspector))
  shows the primary selected item, or with none the selected scene, under a header with
  its type icon (the scene's icon for a scene), name, kind, "+N" for the other selected
  items, the item's source's or the scene's label chips and tag button, whose popover
  opens under the header, and a favourite star (`source.set_meta` or `scene.set_meta`). An item's name is in italics when its source
  is placed in another scene too. Its sections fold, and each stays as it was last left,
  by title, across items and reloads (`Folds`, kept in the browser's storage as
  `jstream.folds`): Parameters (X and Y of the
  unrotated box's top-left corner, W, H, W % and H % of the source's own size (blank
  while the engine knows none) and Rotation, sent as `item.set_geometry`; the W/H link,
  on by default, keeps the aspect ratio when either side changes), 3D (folded
  at first: Tilt, Turn, Depth and Perspective, also `item.set_geometry`; Perspective
  shows the prototype's 1200 px while the item has none, and the first change that puts
  the item in 3D sets it, since without one the engine draws 3D orthographically, where
  Depth has no effect), Crop
  (`item.set_crop`, in source pixels), Blending (the nine modes, `item.set_blend`, each drawn by
  the engine since 0.47, and Opacity, `item.set_opacity`) and
  Scene switches (folded: the item's delay, easing, glide and appear and disappear
  effects, each change one `item.set_transition` with the whole settings, "customised"
  while they differ from the defaults; not shown in a keyframe, whose items switch as
  the parent's) and Properties, the source's settings form. A scene shows its object
  count, the scenes that use it as a source, its Hotkey row (the chords bound to its
  `scene.switch`, an × that unbinds one, and Set key, which binds the next chord pressed
  and takes it from whatever it ran), and its Transition in and out (`scene.set_transitions`, with the
  presets from the shared collection). The panel is built
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
- **The Transform panel** ([UI features › transform](../../docs/UI-FEATURES.md#transform))
  aligns, distributes and arranges the selected items with the prototype's rules,
  on their unrotated boxes: alignment is to the selection's bounds with two or more
  selected and Align to on Selection, else to the canvas; distributing (three or more)
  evens the gaps in centre order, keeping the first and last; Center moves the
  selection as a block; Fit and Stretch also stand the item upright; Match width and
  height take the primary item's. `plan` works out the new boxes, and each press sends
  the changed ones as one batch of `item.set_geometry` (Clear crop: `item.set_crop`, and the
  uncropped box while the source's size is known, `canvas::crop::clear`),
  so it is one undo step. Items still in the legacy form are left alone, and Match does
  nothing while the primary item is one.
- **The properties form** is generated from the kind's descriptor (`kind.describe`,
  read once per kind and kept), with no code per kind (ADR-0025): a checkbox, number
  (and slider when bounded), select, colour, text, asset (root and path) or reference
  select (of the scenes, the other sources, or the buses, with None for `null`, which
  for a `waveform`'s or `spectrum`'s `audio` is the program mix; a source reference
  restricted to audio, such as their `audio_source`, offers only the sources that
  report audio settings, less `scene` sources, as the engine accepts no other), or capture
  target list per parameter type. A `capture_target` (a `display` or `window` source's
  `target`) lists the targets of its kind that `capture.targets` reports, each by name
  and size, after "Primary display" or "None" for the empty value, and a target the
  source names that isn't listed now (a closed window) as "Window not open (<ID>)". It
  asks again each time the field gets focus, so a window opened since shows up; until
  the first list arrives, or while the engine can't list targets, it is the ID as text,
  and a failed refresh keeps the last list. Fields
  come in the descriptor's order, grouped by `group`, advanced
  ones collapsed, and hidden while their `visible_if` doesn't hold. A type this UI
  doesn't know is shown read-only. A change is checked with the descriptor's own check,
  then sent as a one-parameter `source.set_settings` patch, and Reset sends `null` for
  the default. The descriptor's or the engine's reason shows under the field. A
  `scene` source has no settings.
- **The Properties dialog** (`source_properties.rs`) is the prototype's: the name and the
  same form on the left, on a draft instead of the stored settings, and the source's
  filter chains on the right. OK sends only what the user changed since the dialog
  opened, so another client's edit meanwhile stays: a rename (unless the name is blank
  or as it opened) and one `source.set_settings` with each changed value and `null` for
  each one the draft dropped (`draft_patch`), as one batch, and closes once the engine
  accepts it;
  Cancel, Escape and the backdrop drop the draft; Defaults empties it. Enter in a
  field blurs it before pressing OK, so the field's `change` reaches the draft first.
  The filter chains are the Filters panel's rows (below), which apply at once. A double click on
  an item and Properties… in the item menus open it for the item's source; a nested
  scene's item opens the scene's Properties dialog instead. A display, window or webcam
  source shows a live picture above the name (`device_preview.rs`, DL-02): a
  `preview.subscribe` stream of the draft source (`source`, 0.49, small at 15 fps) on a
  channel from the canvas's taps (`canvas/taps.rs`, numbered from 2³¹ so none is the
  canvas's), drawn by the canvas's `Screen`. It follows the settings the capture reads
  (a webcam's `device`, a display's or window's `target` and `show_cursor`): a change
  closes the stream and opens the next on a new channel, and closing the dialog closes
  it. A window source with no window picked says to pick one; an engine that refuses or
  ends the stream shows why.
- **The scene Properties dialog** (`scene_properties.rs`) opens from a double click on a
  scene row, the row menu's Properties…, the canvas margin's Scene properties… and a
  nested scene's item. It edits a draft of the name, Transition in and out and the
  hotkey: the hotkey field takes the next key press as a chord (the modifiers in the
  protocol's order, then the key's `code`; Backspace or Delete clears it, Tab,
  Escape and Enter keep their dialog meaning). OK sends what changed as one batch:
  `scene.rename`, `scene.set_transitions`, and `hotkey.clear` of the old chord before
  `hotkey.set` of the new one, bound to a `scene.switch` with the preset the old binding
  named (none for a first hotkey). A chord bound already is refused before sending, with
  what it runs. The scene's hotkey is the first binding
  whose action switches to it. Defaults clears the overrides and the hotkey. The engine
  has no scene background colour, multiview or scene-wide "include its audio", so the
  dialog leaves them out. On the right, in the source Properties dialog's shell, are the
  scene's filters: the Filters panel's rows on its video chain, which apply at once (a
  keyframe shows its parent's).
- **Filters** ([UI features › filters](../../docs/UI-FEATURES.md#filters)). The
  Filters panel shows the primary selected item's source's chains (ADR-0026): video
  unless the source is audio-only, audio when it has a mixer channel and isn't a
  nested scene. Each row's eye sends `filter.set_enabled`, its × `filter.remove`, and a
  drag within its chain `filter.reorder` (the tree rows' pointer drag). Its summary and
  its settings come from the kind's descriptor, read once per kind and kept; a kind the
  engine doesn't describe shows its name alone. A filter with three settings or fewer
  opens the properties form under its row, any other opens the Filter settings dialog,
  and a double-click always does; each field's change is a one-parameter
  `filter.set_settings`. The dialog applies every change at once and, on Cancel, Escape
  or the backdrop, sends one batch putting back the settings and the switch it opened
  with, including each one it sent whose change the engine hasn't reported yet; an
  edit that fails after the dialog, or the row edited in place, has closed shows in the
  problem bar. Defaults resets each setting but a scene input's scene. The Add filter
  dialog offers `kind.list`'s filter kinds for the source's chains under the prototype's
  groups, names and hints for the kinds it knows, and sends `filter.add` with an ID from
  `fresh_id`; the new filter opens in place, or in the settings dialog when it has more
  than three settings or isn't described yet. Edit filters… in the item menus selects the item and shows the
  panel. Names come from the prototype's catalogue, then `kind.list`, then the kind's ID.
  An FX button (the Outputs panel's rows, the Mixer's master card or strip) picks the program
  output's chain (owner `master`) or an output's (ADR-0080) instead, and shows the
  panel; the same rows and dialogs edit it with that owner. Both chains show, and the
  virtual camera, which has no sound, has a video chain only. The Add filter dialog
  won't add a kind whose settings name a scene, a source, a bus or a file there
  (`refers`: the engine refuses those references yet), and, for an output whose video
  chain is empty, reads `output.status` and states what its first video filter costs
  (`first_video_cost`, ADR-0080 §3): its own GPU pass and encoder session, with the
  encoder sessions in use now, or for the virtual camera, which opens no encoder
  session, a GPU pass only, from its next start if it runs. A scene's chain (owner `scene`, ADR-0065) shows while no
  item is selected, for the selected scene, or a keyframe's parent, since a keyframe has
  none of its own; a scene row's Edit filters… and the canvas margin's Edit scene
  filters… pick it as an FX button does (`show_filters`). It is a video chain, and the
  Add filter dialog's Uses the scene picker on it offers only the scenes whose picture
  doesn't hold the scene by nesting or as a keyframe; the engine refuses other cycles.
  Selecting another item or scene, or an item's Edit filters…, goes back to the
  selection's chains. Under the selected item's source's chains, the panel shows the
  item's own effects (owner `item`, ADR-0065), a video chain for an item whose source has
  a picture, with the same rows and dialogs; `Collection::chains` reads them from
  `ItemInfo.effects`, and their Uses the scene picker leaves out the scenes whose
  picture holds the item's scene. The Add filter dialog lists the vignette under Style.
- **The shell** is the header bar over the workspace. A failed call shows in a
  dismissable bar. A confirmation, such as "Added a reference…" after a placement of
  an existing source or a duplicate, shows in the toast (`Studio::notify`, or
  `apply_noting` once a batch is applied): one at a time, bottom centre, gone after
  4.5 s, a new one replacing it. The look follows the UI prototype ([UI features](../../docs/UI-FEATURES.md)): dark only, with the
  prototype's palette as CSS variables in `style.css`. The fonts are IBM Plex Sans and
  Mono, bundled in `fonts/` under the SIL OFL 1.1 and served from the page's own origin,
  since its policy loads fonts from nowhere else
  ([ADR-0077](../../docs/adr/0077-bundle-ibm-plex-fonts.md)). They show with
  `font-display: swap`, so the system font stands in until they load.
- **The workspace** follows the prototype's dock
  ([UI features › docking](../../docs/UI-FEATURES.md#docking)): eight zones nest around
  the canvas, outer columns, full-width top and bottom, sidebars, and above and below
  the canvas. A zone stacks groups (columns top to bottom, rows side by side); a group
  shows one panel with its title, or tabs. Panels are the prototype's eleven: Scenes,
  Scene objects (`Sources`), the Inspector (`Inspector`, with `Pairing` under it), the
  Transform, Filters, Live chat, Stream stats, Audio mixer, Outputs, Agent activity and
  Transitions (`StudioMode`); `panel_view` maps each to its component, and a new panel
  adds a `PanelId` and a line there. Live chat and Stream stats wait on the engine
  (YouTube sign-in), so they say what they wait on and neither is in a built-in layout.
  Agent activity (`panels/agent.rs`) lists what agents did from the engine's
  `activity.list` ([ADR-0083](../../docs/adr/0083-activity-log-over-the-protocol.md)),
  leaving out JStream's own UIs, and asks again after each `state.changed`,
  `studio.changed`, `output.state_changed`, `live.ended` and `events.dropped`, and every
  5 seconds; the
  Live layout shows it as a second tab beside Outputs. The
  panels' own `h2` headings are hidden, since the tab is the title. Narrow windows
  stack the zones, without splitters.
  - Each group keeps every tab's panel mounted and hides the inactive ones, so a tab
    switch keeps a panel's state; moving a panel to another group mounts it afresh.
  - A tab drags with pointer events, not HTML drag and drop, which desktop webviews
    can intercept for file drops. Once a press moves 4 pixels it is a drag: empty zones
    show their "Drop here" placeholders, and the element under the pointer, through its
    `data-drop` attributes, names the target: a tab strip (join that group), half of a
    group's body (a new group before or after it) or a zone (a new group at its end).
    Escape cancels. A tab keeps the arrow pointer, as a title does; the grabbing hand
    shows only while it drags.
  - Splitters and dividers capture the pointer while they drag. Zone sizes and group
    weights are set through the CSS object model, which the page's policy allows,
    unlike `style` attributes. After every dock change the workspace fires a window
    `resize`, which the canvas measures itself on.
  - The dock is kept in the browser's storage (`jstream.dock`) after every change and
    read back when the workspace opens; a stored dock that doesn't parse is ignored.
  - It is also kept in the engine's UI settings under `workspace.dock`
    (`ui_settings.rs`, [ADR-0084](../../docs/adr/0084-ui-settings-kept-by-the-server.md)),
    so the desktop app and every browser of one engine show the same layout. When the
    workspace opens it shows the browser's copy, then reads the server's
    (`ui_settings.get`), and `ui_settings::first_step` decides what happens next. The
    browser also keeps the dock it last read from or sent to the server
    (`jstream.dock.shared`), so it knows when its own copy holds a change the server
    never got (the page closed within the send delay, or the server couldn't be
    reached): that copy is sent. Otherwise the server's dock is shown, or the browser's
    is sent when the server keeps none. A dock this UI can't read is left alone until
    the person changes theirs, so an older UI never overwrites a newer one's just by
    opening. Nothing is sent before that answer. After that each change is sent
    (`ui_settings.set`) once the dock has stayed the same for 500 ms, since a splitter
    drag changes it on every move, and a dock just read from the server isn't sent back.
    A session with no server in front answers `unsupported`, and the browser alone keeps
    the dock. Another open client picks up a change the next time its workspace opens.
- **The header** keeps its own copy of the outputs, as the outputs panel does (both
  re-read on `state.changed` and `events.dropped` and follow `output.state_changed`),
  plus `Studio::keys_changed`, which the panel bumps after a stream key is set or
  cleared, since no event reports that. Go Live and End stream are described under
  Outputs. Record starts every stopped or failed recording output and, while any runs,
  becomes "Stop recording", which stops them; with no recording output it is disabled.
  The stats readout reads "60 fps · 0 dropped · CPU 3%" from `stats.tick`. The dropped
  count is captured, rendered and encoded frames dropped since the first tick this page
  saw, starting over when the engine restarts. The frame rate and CPU (`Load`) come from
  the last two ticks' `load` (protocol 0.48): program frames over the elapsed time,
  shown only while the program drew some, and CPU time over the elapsed time and the
  logical CPUs, "<1%" below half a percent, left out when the engine reads none. Undo and
  Redo in the Edit menu call `history.undo` and `history.redo`, like the canvas's
  buttons. A failed call shows in the shell's problem bar. The menus close on Escape
  or on a press anywhere outside them.
- **Outputs.** The panel reads the outputs and encoder profiles with `state.get`, then
  each output's `output.status`, and re-reads them on `state.changed` and
  `events.dropped`; `output.state_changed` updates one output's state in place. It
  creates RTMP(S), recording and virtual camera outputs (IDs `output-N`, the first free one), changes
  an RTMP server URL (`output.set_target`) and the encoder profile, deletes a stopped
  output, and starts and stops each. Above the cards, the program output's row
  ("master") and a row per output with its state each have an "FX N" button counting
  that chain's filters, which shows the chain in the Filters panel (below the rows,
  the prototype's footnote). The header's "Go Live" starts every stopped or
  failed RTMP output whose stream key is set, and "End stream", which replaces it while
  any runs, stops the running ones; recordings start from their own buttons or the
  header's Record. Both try every output, even after one fails, and report
  each failure. An RTMP output without a key can't be started from the
  panel.
  A virtual camera output (`virtual_cam`) has no target, key or encoder profile, so its
  card shows none of them; neither "Go live" nor "End stream" touches it. The card says
  what the page knows of the camera's registration. Only the desktop app knows (its global's `vcam`,
  read in `host.rs`): there an unregistered camera, or one turned off with `--no-vcam`,
  can't be started, and the card gives the `jstream-daemon --register-vcam` hint, which
  names the step as optional; an `outdated` one (registered, but not for a current copy
  of this version's DLL) can still be started, and its hint offers registering again,
  also optional. In a
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
  `state.get` and the studio state with `studio.get` into a copy of its own,
  re-reads them on `state.changed` and
  `events.dropped`, and takes `studio.changed` in place (a re-read that a
  `studio.changed` overtook keeps the event's program and preview). The studio bar (preview,
  program, Transition and Cut) shows only in studio mode; with it off a line says that
  a click on a scene puts it on program. Picking a scene sends
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
  The presets section is the prototype's (spec-panels.md 10): a picker listing each
  preset as "<name> — <Kind> · N ms" (a cut shows only its kind), the default marked,
  then "+ New preset…", which adds "New preset", a 500 ms ease-in-out fade (ID from its
  name, `-2`, `-3`… when taken), and picks it. Until a preset is picked the editor shows
  the one the switch names, else the selected scene's transition in, else the default.
  The editor sends each change at once, as a `transition.update` naming only what
  changed (after a kind change, the patch is against the new kind's defaults, as the
  engine applies it): Name, the kind as a Cut / Fade / Move segmented control, and for
  every kind but a cut the duration, the kind's parameters and the easing. Under them a
  note counts the scenes that override with it ("Overrides: N in, M out."), and Make
  default and Delete show for every preset but the default. The engine's preset wins:
  each change it reports resets the fields to it. The kind's parameters come from
  `kind.list` and `kind.describe` (category `transition`), or the built-in `cut`,
  `fade` and `move` when the engine can't list them; enum, boolean, number and text
  parameters are editable, others read-only. A value the editor refuses is not sent.
  Each scene's `transition_in` and `transition_out` can also be set in a folded table
  under the editor. The engine runs the transitions, `move` pairing and gliding items,
  and the program preview shows them, so the page draws none itself.
- **The stream key is write-only** ([R-SEC-001](../../docs/RULES.md#R-SEC-001)). Its
  password field is not bound to a signal: the panel reads it only on save, wraps it in
  a `SecretString` at once (trimming a pasted newline), empties the field and sends
  `output.set_key`. It never stores, logs or displays the key, and the engine never
  sends it back: the panel shows only `output.status`'s `key_set`, re-read after it
  sets or clears a key, since no event reports that. Another client's key change shows
  on the next re-read.
- **Plain `rtmp://` gets a warning.** A stream output whose server URL is `rtmp://` to
  a host other than `localhost` or a loopback address shows, under its target, that
  the key is sent unencrypted and to use `rtmps://` if the service offers it
  (`cleartext_warning`, #353). The panel shows it when `output.status` reports
  `cleartext` (from 0.37, #562), so the rule is the engine's alone.
- **Mixer** ([ADR-0028](../../docs/adr/0028-audio-model-v2.md)). The panel reads
  the audio sources (those `state.get` reports with `audio`), the buses and the sends
  into a copy of its own, not `Studio`'s, and re-reads them on `state.changed` and
  `events.dropped`. It follows the prototype's layout
  ([UI features › mixer](../../docs/UI-FEATURES.md#mixer)): the master card on top, the
  channels folded under it (folded at first), then the buses under a folded Buses
  header. The master card's button shows and hides the channels and counts them; it
  has the master's short-term loudness on a bar from −60 to +6 LUFS with a marker at
  the −14 LUFS target (70% along, where the prototype draws it), the three loudness
  readings in its tooltip, its true peak with "safe" or a warning over 0 dBTP, and a
  button that resets the integrated loudness (`audio.reset_loudness`). Its Hear
  button is the master's monitor (`bus.set_monitor`, D22): it reads "Hear: off" while
  the master has none, a click turns one on at the default output after FX, and later
  clicks switch it between after and before FX on the same device. Its "FX N"
  counts the master chain's filters, from `Studio`'s collection, and shows that chain
  in the Filters panel. Each source gets a row, ordered by name: its meter, fader, a dB field
  (the shared `NumberField`), mute and, folded under "Routing", whether its audio is
  on, when it is heard (`always` or `when_shown`) and its send into each bus (on or
  off, and its gain). A nested scene's row has only the routing toggles, since it has
  no fader or sends of its own. Docked in a side column the mixer shows vertical strips
  instead (`MixerOrientation`, until the flip button the dock draws in the mixer's
  title bar, `MixerFlip`, sets an orientation for the session; a memo, so a dock change
  that keeps the orientation doesn't rebuild the rows): a master strip with the
  loudness filling up to the target line, the loudness and true peak (its warning
  heard by a screen reader only, for want of room) and the same "FX N", then, folded
  beside it, a strip per source with its short name, a meter filling upwards, the dB
  field stacked + over − and scrubbed up and down, and mute. Each bus gets a strip, the master first, with, under More, its
  monitor (off, after FX or before FX, and the output device it plays on, empty for the
  default output, trimmed and checked as the engine checks it; the menu shows the
  bus's monitor, so a refused pick goes back, and a tap from a newer engine shows as
  "Another tap"), renaming and (except
  the master) deleting. A delete reads `delete.plan`
  first, as a scene's or source's does: the strip asks, saying how many `waveform` or
  `spectrum` settings drawing the bus the plan clears, and sends the plan's edits
  expecting the plan's revision. New buses take the first
  free `bus-N` ID; the button is off at the protocol's bus limit (`MAX_BUSES`, 16).
  - **Faders** run from −100 to +24 dB on a console taper: unity at three quarters
    of the travel, the quiet end compressed, and snapping to 0 dB within a quarter of
    a dB. While the pointer holds a fader the strip shows the value under it; letting
    go (or each key press) sends one `source.set_audio` or `bus.set_fader`, so a drag
    is one undo step. Hearing the fader during the drag needs live values
    ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)), which `live.set` doesn't
    take for faders yet. A source's dB field shows the fader's value during the drag
    and sends its own value the same way: typed, stepped or scrubbed, one edit each.
    Clicking a bus's readout sets 0 dB. An accepted edit is applied to the shown
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
  the trees' search and drop index,
  the Add source dialog's types and edits, the menu's placement and keys, the item
  menu's layer moves and arrangements, the Properties dialog's draft patch, the scene
  dialog's edits and chords, the Transitions panel's picker, submenus and counts, the
  Inspector's scene switches, the form's
  sections, parsing and checks); only the components
  themselves need a browser.
- **Debug bundle size.** A leptos view's type spells out every element and closure
  inside it, and an unstripped build names each function with those types in the
  `_bg.wasm`'s name section. The server refuses a file over its size limit
  ([jstream-server › Design](../jstream-server/README.md#design)), so the debug build
  uses the workspace's `ui-dev` Cargo profile (`data-cargo-profile-dev` in
  `index.html`), which strips the symbols and leaves no name section
  ([Build](#build)). The mixer's strips, rows, meters, faders and mute buttons also
  return `AnyView` rather than `impl IntoView`, which keeps a nested view's type out of
  its parents' types, and so out of their names in an unstripped build.

<a id="canvas"></a>
## Canvas
<!-- @section
covers = ["crates/jstream-ui/src/canvas/**"]
depends_on = ["docs/adr/0022-item-transform-and-canvas.md", "docs/adr/0032-live-edits-and-undo.md"]
-->

The canvas edits the boxes of [ADR-0022](../../docs/adr/0022-item-transform-and-canvas.md)
in canvas pixels. `transform.rs` maps the page to the canvas, hit-tests boxes and
handles, and turns a drag into a new geometry; `crop.rs` turns a crop handle's drag
into a new box and crop, from the source's size; `group.rs` does the same for several
selected items and the box select; `snap.rs` snaps; `model.rs` lays out the scene and
sends the edits; `preview.rs` chooses the preview stream and decides what to do with
each H.264 frame and when each JPEG decodes. These are pure and tested natively,
`model.rs` against `MockEngine`. `screen.rs` decodes and draws the preview in the
browser, and `mod.rs` is the component.

- **What it shows.** The selected scene ([Design › Selection](#design)), laid out with
  `Layout::sized` from the shared collection and the sources' sizes, so it follows every state change the app
  reads. The frame is the largest 16:9 box in the canvas area, with the scene's name
  and the canvas size captioned bottom left. Boxes are outlined with
  `jstream-protocol`'s placement formula, so the overlay matches what the compositor
  draws. An item still stored as a 0.3 legacy placement is not drawn: its box depends
  on the source's runtime size, which the canvas doesn't evaluate a legacy transform
  against yet (ADR-0022 decision 5), and a note top left counts such items. Over the live
  picture only the selected items, the box under the pointer and hidden items (which
  aren't in the picture but still take clicks) are outlined, so an unselected source
  shows no box; without the picture every box gets a faint outline and
  its name. Hidden items are drawn dashed, and the box under the pointer gets a light
  outline. A keyframe scene shows its
  parent's items at its overrides under a banner, "◆ Keyframe of “<parent>”: only
  placement and visibility can change", and its edits are addressed to it, so they
  write its overrides (ADR-0049). Its drags send the keyframe's live values, addressed
  to it as the commit is, which the engine takes from protocol 0.53 (#846).
- **The preview.** Under the boxes is the shown scene's picture, live: the program
  mix while the canvas shows the scene on program, so its transitions show, else that
  scene's own stream (`preview.subscribe`'s `scene_id`, CV-03). The canvas opens the
  session's binary channel (`Control::binary`; a session has one, so the canvas is
  its only reader, and hands each frame of another channel to that channel's tap,
  `taps.rs`, the Properties dialog's picture) and one `preview.subscribe` stream on it, at 30 fps and as
  sharp as the stage (its size in device pixels, kept between 320×180 and 1280×720).
  It asks for H.264 first when the browser has WebCodecs' `VideoDecoder`, then JPEG.
  H.264 frames go to a `VideoDecoder` configured from the SPS in each keyframe
  (`avc1.PPCCLL`), starting at a keyframe and skipping to the next one when the
  decoder's queue passes 3 chunks; JPEG frames go through `createImageBitmap` one at a
  time, and while one decodes only the newest that arrives waits, so the last picture
  is never lost. Each decoded picture is drawn into a `<canvas>` placed with the
  canvas's own fit, so the picture lies exactly under the boxes. If the decoder fails, the canvas closes the stream and subscribes
  again for JPEG only. If the engine refuses or ends the stream, the background says
  why, and the channel is still read for the taps. Showing another scene closes the stream and opens that scene's on the next
  channel number, so a late frame of the old stream is never drawn; until the new
  stream's first frame, the background says the preview is starting. WebCodecs is reached through `js_sys::Reflect`, because `web-sys` has it only
  behind the `web_sys_unstable_apis` build flag. A drag's live values (below) reach
  the output as it moves, so the preview follows the drag.
- **Selecting and dragging.** A press selects the topmost box under the pointer and
  drags it, or, on a box already selected, drags every selected box. Shift-click adds
  a box to the selection (and drags them all) or takes a selected one out. A press on
  empty canvas, or on the dark margin around the frame, draws a marquee that selects
  every box whose bounds it touches (Shift keeps what was selected); a click there
  clears the selection. Ctrl+A selects every box. The shared selection follows each of
  these. A double click selects a box alone and opens its source's Properties dialog
  ([Design](#design)). The pointer is
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
  drag, or clears the selection. Del removes the selected items, and Ctrl+D and
  Ctrl+Shift+D duplicate the selected one as the item menu does, except in a keyframe
  scene, whose items are its parent's.
- **Several selected.** Each selected box keeps its 2 px outline without handles, and
  a dashed box 4 screen pixels outside their bounds gets the handles, a knob 42 pixels
  above it, and a readout 12 pixels below (`N items · W × H`, the size while scaling,
  the angle turned while rotating). Its handles scale every box from the far side of
  the bounds (an edge handle scales the other axis about the centre), keeping the
  aspect unless Ctrl is held, down to 20 pixels; the knob turns every box about the
  bounds' centre, in 45° steps with Ctrl. A move of several boxes snaps their bounds
  the way one box snaps, unless one of them is rotated.
- **Crop.** In crop mode (the Crop tool or C; V, the Move tool, Escape or a switch of
  scene leaves it) every resize handle of the one selected box crops, and outside it
  Alt on a handle does. The crop is in source pixels, so it needs the source's size
  (`source.sizes`, ADR-0022 decision 4). The handles sit on the item's picture: the
  part of the box the source fills, which `jstream-protocol`'s `content` gives for its
  fit mode, clipped to the box. A drag moves that edge of the picture, which keeps its
  scale, and stops at the uncropped extent, leaving at least 10 pixels; the box becomes
  the picture's rectangle and the crop is rounded to whole source pixels (a `fill`
  box's clipped margins count as crop). A flipped box crops the source's side it
  shows. In crop mode the outline, the handles (12 px, filled) and the readout are
  amber, and the uncropped extent is drawn as a dashed amber ghost, as it is whenever
  the selected item is cropped. While a crop drag goes on the readout says
  `crop l / r / t / b`; at rest it adds ` · cropped` for a cropped item, or, in crop
  mode with no size known, ` · no source size yet`, and then there are no crop handles:
  the box's handles resize as usual, with Alt too.
- **Snapping.** A moved upright box's edges and centre snap, within 6 screen pixels,
  to the canvas's edges and centre lines and to the other boxes' edges and centres,
  using each box's axis-aligned bounds; the lines it lands on are drawn as guides. An
  upright box's resized edges snap the same way. Ctrl moves without snapping, a
  rotated box never snaps, and the Snapping button in the floating tools (top right)
  turns it off. So does the canvas's right-click menu: on the frame it offers Select
  all, the snapping toggle and Add source, on the margin around the frame the scene's
  menu, and on an item the selected items' menu ([Design](#design)).
- **Undoable edits.** A drag changes no stored state until it ends: the overlay draws
  the gesture locally, and on release one batch with an `item.set_geometry` per item it
  changed (and an `item.set_crop` for a crop) commits it, so each gesture (and each nudge) is one undo step. Meanwhile the
  drag opens a gesture (`live.rs`) whose `live.set` carries the same edits as live
  values ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)), so the output and
  the preview follow the drag without changing `rev`. The calls go one at a time, the
  values that arrive while one is in flight collapsing into the newest; the committing
  batch follows them with `ends_gesture` (or none, if no `live.set` got through), on
  the gesture's own scene, and Escape sends `live.cancel` instead, as does a commit
  that fails or can't be sent, so the output doesn't keep the dragged geometry.
  Gesture IDs come from `fresh_id`, so they are unique for the session. The
  local geometries stay drawn until no commit is in flight and the collection has
  caught up with the last one's revision; a later drag replaces only its own items',
  so a nudge still in flight isn't lost. A commit that fails shows its error and stops
  drawing its geometries, unless a later gesture has replaced them. It carries no
  `expected_rev`: a drag is last writer wins per field
  ([ADR-0032](../../docs/adr/0032-live-edits-and-undo.md)). Undo and redo (Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y while the canvas has focus, and
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
trunk build            # debug (the `ui-dev` profile), into dist/
trunk build --release  # what the daemon and the desktop app ship
trunk serve            # rebuilds on change and serves dist/ locally
```

trunk needs the `wasm-bindgen` CLI at the version `Cargo.lock` resolves for
`wasm-bindgen`. It uses one found on `PATH`, and otherwise downloads it; a release build
also downloads `wasm-opt`. `dist/` holds `index.html`, the JavaScript glue, the `.wasm`
and the stylesheet, with content hashes in their names, and `fonts/`, copied as it is
(`copy-dir` in `index.html`, so the fonts and `OFL.txt` keep their names). It is not
committed. trunk's
inline loader script carries a `{{__TRUNK NONCE__}}` placeholder (`create_nonce` in
`Trunk.toml`), which the serving engine replaces with each response's
Content-Security-Policy nonce.

A debug build uses the workspace's `ui-dev` Cargo profile, which `index.html` picks with
`data-cargo-profile-dev`: `dev` without debug info and with the symbols stripped. With
`dev` itself, wasm-bindgen's `_bg.wasm` is many times larger, nearly all of it the name
section, and over the server's size limit for a UI file
([jstream-server › Design](../jstream-server/README.md#design)), so every load would
get a 500. A release build keeps its own profile. The UI installs no panic hook, so a
panic shows in the browser console only as `RuntimeError: unreachable`, with no message
or location; in a debug build its stack frames are unnamed (`wasm-function[N]`) too.

CI's `wasm` job builds the UI this way on every PR, with trunk and the `wasm-bindgen`
CLI downloaded at pinned versions and checked against pinned SHA-256 sums, and runs
clippy for `wasm32` over `jstream-control` (with `ws`) and `jstream-ui`.

A page served by trunk is on a different port from the engine, so the engine refuses
its `Origin` unless it is configured as an extra origin.

The built page is tested in a browser by CI's `ui-e2e` job: Playwright against a mock
daemon ([end-to-end tests](tests/e2e/README.md)).
