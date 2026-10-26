<!-- @doc
id = "CRATE-jstream-ui-e2e"
title = "jstream-ui end-to-end tests"
kind = "normative"
status = "active"
review_every = "90d"
-->

# jstream-ui end-to-end tests

The web UI tested in a real browser: [Playwright](https://playwright.dev/) drives a trunk
build of `jstream-ui` against a mock daemon. The UI's logic is also tested natively
against `MockEngine` ([jstream-ui › Design](../../README.md#design)); these tests cover
what only a browser shows: the page loading under the served Content-Security-Policy,
the WebSocket client, the components wired to the engine, and pointer and keyboard
input on the canvas.

<a id="harness"></a>
## Harness
<!-- @section
covers = ["crates/jstream-ui/tests/e2e/mock-daemon/**", "crates/jstream-ui/tests/e2e/*.js", "crates/jstream-ui/tests/e2e/package.json"]
-->

- **The mock daemon** (`mock-daemon/`, the test-only crate `jstream-ui-e2e`) puts
  `jstream-control`'s `MockEngine` behind `jstream-server` on `127.0.0.1` with an
  ephemeral port, serves the UI's `dist/` from the same port, and prints the page's URL
  as its first line. It seeds a small studio: scenes `Intro` and `Main` (on program),
  one colour source `Backdrop` placed in `Main` as a 640×360 box at the centre of the
  1920×1080 canvas, with two video filters (`sharpen`, then `blur` turned off), two
  audio sources no scene shows (`Mic · ES-9 Line`, an `audio_input`, at −3 dB and
  `Desktop audio`, a `desktop_audio`, at −8.5 dB; the mock is told both kinds produce
  audio, so they get mixer channels, and only the mic is one the privacy screen
  mutes), and the encoder profile `High quality`. It has no outputs. It stops
  when its standard input closes. It is a crate of its own because `jstream-ui` (layer
  3) may not depend on `jstream-server` (layer 4), not even for tests
  ([R-ARCH-002](../../../../docs/RULES.md#R-ARCH-002)). Nothing ships it.
- **One daemon per test** (`fixtures.js`): each test starts its own, with a random
  token, so tests share no state and run in parallel. The browser signs in with HTTP
  Basic, the token as the password, as a person's browser does when the daemon serves
  the UI; the connect form's token field can then stay empty. `connect` opens the page
  and connects; `disconnect` disconnects through the header's File menu;
  `openAddSource` opens the Add source dialog from the Scene objects panel's menu, by a
  right-click just under its last row, and
  `nestScene` nests a scene through it; `rev` reads the revision the header last heard
  of; `showPanel` makes sure a docked panel's tab is the shown one; `describeKinds`
  routes the page's WebSocket so that `kind.list` and `kind.describe` answer for the
  descriptors a test gives, which the mock engine can't, and passes every other message
  on; given `refuse`, it also holds back the requests that function names and refuses
  them, as `invalid_argument`, when the test calls the `release()` it returns.
  `meterTicks` routes it too, passing everything on, and returns a function that
  sends the page a `meters.tick`, which the mock engine never sends.
  `sourceSizes` routes it to answer `source.sizes` with the sizes a test gives, since
  the mock makes no frames and reports none, and returns a function that sends the
  page a `source.size_changed`. `recordCalls` routes it too, passing everything on, and
  its `sent(type)` lists every edit or action of that type the page has sent, for the
  transition edits the mock engine refuses.
- **Console errors fail a test.** Every console error and uncaught exception on a
  test's pages fails it, unless the test allows that message. A Content-Security-Policy
  violation is a console error, so a style or script the policy refuses fails too.
  Favicon requests are ignored: the UI has none.
- **Setup** (`global-setup.js`): builds the mock daemon with `cargo build --locked`
  (or uses the executable `JSTREAM_E2E_DAEMON` names) and checks that `dist/` exists
  (or the directory `JSTREAM_E2E_DIST` names). It does not run trunk.
- **The browser** is Playwright's Chromium, or the executable `JSTREAM_E2E_CHROMIUM`
  names. `@playwright/test` is pinned exactly in `package.json` and `package-lock.json`.

<a id="tests"></a>
## What the tests cover
<!-- @section
covers = ["crates/jstream-ui/tests/e2e/specs/**"]
-->

- **Connecting** (`connect.spec.js`): the form offers the serving engine's URL and
  connects with the browser's credentials; the header's File menu shows the URL and
  protocol and closes on Escape, and the header carries the revision; disconnecting returns to the form with the token field empty; a refused
  upgrade shows its reason; the token never appears in the page, its storage or its
  cookies. A URL the page's policy refuses shows a reason and re-enables Connect,
  although Chromium sends that socket an `error` event and no `close` (#305). The
  bundled IBM Plex Sans and Mono load from the page's own origin, and `fonts/OFL.txt`
  is served beside them.
- **Scenes and sources** (`scenes.spec.js`): the seed's scenes in order with the
  program scene's "program" badge, selected, and its item listed under the scene's
  caption; a click putting a scene on program, which a second client follows, and in
  studio mode on preview, with the hover button putting it on program and the mode
  kept over a reload; F2 renaming a
  scene in place (Del inside the field deletes a character, not the scene) and a drag reordering the scenes, which a second client follows and
  which doesn't select the dragged row; the search boxes and the favourites filter
  with its count, Favorites only and Clear; hiding and showing an item with its eye,
  placing an existing source again through the Add source dialog, a drag restacking the
  two items, F2 renaming one of the two placements of a source (Del there edits the
  name, not the scene) and Del removing one; selecting an item opens its properties. The mock engine lists no kinds and makes
  no delete plans, so new-kind sources, the properties form's fields and deletes are
  tested natively.
- **Scene folders** (`folders.spec.js`): New folder from the panel's and a folder's menu,
  named in place; drags into a folder's middle, out beside a top-level row and before a
  folder, which a second client follows; folding by the caret at two depths; a search
  keeping the open, unfoldable folders of a match; a folder refusing to go inside its
  own child; F2 renaming a folder, and Del deleting an empty one without asking. Deleting
  a folder that holds anything needs `delete.plan`, so it is tested natively.
- **Outputs** (`outputs.spec.js`, on the Outputs panel and the header): adding a stream and a recording; a stream can't start
  or go live until its key is set; the key field empties on save and the key never
  appears in the page; go live and end stream; the header's Record starts and stops a
  recording; clearing the key; deleting; editing the
  server URL and choosing an encoder profile, both read back by a fresh page; a second
  client sees outputs created, started and deleted.
- **Docking** (`docking.spec.js`): the Editing layout's zones (Filters and Transform as
  tabs under the Inspector, Transitions as a tab beside Outputs), with empty zones hidden; the View
  menu hiding and adding a panel at the end of its home zone, and the layout marked
  "(modified)"; dragging a tab onto a tab strip groups it, and onto an empty zone,
  which shows while dragging, docks it there; a lone panel collapses by its title,
  closes on a second click of its ×, and the Inspector doesn't collapse once it is alone
  in its zone; a splitter resizes a zone, and a saved layout and its sizes survive a reload;
  the Layout menu switches to Live and resets to Editing.
- **Canvas** (`canvas.spec.js`): the selected scene's box is drawn where the seed put
  it, under the scene's caption; a click on the box selects its row in the sources list
  and shows the size readout, a click on empty canvas clears both, and selecting another
  scene in the list switches the canvas to it; a Ctrl drag sends live values that change
  no revision and shows the top-left readout meanwhile, then one edit ending the gesture
  that Ctrl+Z and Ctrl+Shift+Z reverse; Escape during a drag sends `live.cancel` and
  puts the box back; Shift drags at half speed; the Snapping tool toggles; arrow keys nudge
  (Shift by 10) and Ctrl+Z undoes; a second client sees a move; a marquee started on
  the margin selects the box it touches and Escape clears it; with a second item
  placed, Ctrl+A selects both, one nudge moves both as one edit, Shift-click on the
  canvas takes one out and Shift-click on its row in the sources list adds it back; a
  double click opens the item's Properties dialog with it selected, and a double click
  on the canvas tools over an item leaves the selection alone.
- **Crop** (`crop.spec.js`): the Crop tool, C, V, Escape, the item menu's Crop and a
  switch of scene enter and leave crop mode, whose readout says there is no source size
  while the engine reports none, and then a handle drag resizes as usual; with `sourceSizes` giving the Backdrop's source a size,
  a crop-mode drag on the right handle crops that side as one edit, with the Inspector's
  Right field, the ` · cropped` readout and the crop ghost, which Ctrl+Z undoes; Alt on
  the left handle crops outside crop mode, and Clear crop gives the whole picture back;
  W % and H % read the size against the source's, set it, follow `source.size_changed`
  and go disabled without a size.
- **Menus and Add source** (`menus.spec.js`): the canvas's right-click menu on the
  frame (its items, Paste disabled, turning snapping off and Select all) and on the
  margin, which Escape closes; the arrow keys move through a menu, skipping a disabled
  item and wrapping, Enter chooses, and a press outside closes it; the Scene objects
  panel's menu opens the Add source dialog, whose types start with the seed's audio
  inputs, and its Color type places the seed's Backdrop again as a reference 40 px
  down and right, selected (the mock lists no kinds, so Color can't create); a scene nests once and isn't offered again, and the backdrop
  cancels the dialog. An item row's menu lists the item menu's lines and Delete source everywhere…, Send to back
  restacks in one edit and Stretch to canvas fills the canvas; on the canvas the menu
  ends with Select all, Duplicate as clone adds "Backdrop copy" selected, two selected
  items get a counted head with the duplicates disabled, and Remove from scene removes
  both; a placement of a shared source switches to an independent clone and back to a
  reference in one edit each, its menu's head and checks following, and the original,
  placed once, has no switch; Ctrl+D and Ctrl+Shift+D duplicate on the canvas and in a
  row, and Del removes there; a nested scene's menu opens it; a scene row's Rename
  renames, and the Scenes panel's New scene adds one with its name field focused.
- **Groups** (`groups.spec.js`): Group selection on two items makes "Group 1", left
  out of the Scenes panel, whose caret shows the two items indented; a click on one
  opens the group's scene with it selected; Ungroup puts them back, which a second
  client follows; the panel's New group groups the selected item, and a row dragged
  onto the group's row goes into it.
- **Properties dialog** (`properties.spec.js`): opened from an item's menu, it shows
  the type's title, the source's name, its two filters and an enabled Add filter
  button; Cancel and Escape drop
  a changed name, OK renames in one edit, and Enter with a blank name sends nothing.
  The mock describes no kinds, so the settings form and the draft patch are covered
  natively.
- **Inspector** (`inspector.spec.js`): the selected Backdrop's X, Y, W, H and Rotation;
  a new width with the W/H link on scales the height and the canvas readout follows,
  and with it off the other side stays; a stepper is one edit Ctrl+Z on the canvas
  undoes; arrow keys step (Ctrl by 10), Escape drops typed text, a drag across the
  field is sent once on release, and text that isn't a number sends nothing while `−`
  and `,` are read; crop (with the folded "cropped" hint), opacity and the favourite
  star are sent; with no item selected the scene's object count, uses and Transition
  in and out show, and a second selected item shows "+1". The mock engine doesn't
  implement `scene.set_transitions`, so choosing one isn't run.
- **Filters** (`filters.spec.js`): with no item selected the panel shows the scene's
  chain; with the Backdrop selected, its title, note, one video chain with its
  count and the two rows, Blur struck through; the eye turning Blur on as one edit, a
  drag putting it above Sharpen as one more, and × removing Sharpen; Edit filters… in
  an item's menu selecting the item and showing the panel behind the Transform tab; the
  Add filter dialog from the panel and over the Properties dialog, which Escape leaves
  open; and the Filter settings dialog on a double-click, with the prototype's hint for
  Blur, whose switch applies at once, Cancel puts back, OK keeps and Remove deletes.
  With three kinds described (`describeKinds`): the rows' summaries and carets, a click
  on a filter with four settings opening the dialog, Cancel pressed straight after a
  change there still putting the filter back (the change and its restore as two
  edits), the Add filter dialog offering
  only the video kinds under their group, its search, the picked kind's details, and
  Add as one edit, the new filter opening in place, where a changed setting is one more
  edit and updates the summary. With one setting refused late: a refusal that arrives
  after its row was closed, and one after the Filter settings dialog was cancelled,
  each in the problem bar.
- **Scene filters** (`scene-filters.spec.js`): with no item selected the panel showing
  Main's empty video chain with the scene note; with Sharpen and Gain described, Add
  filter offering only Sharpen, with the scene's foot, as one edit on the scene, while
  the Backdrop keeps its own two; another scene selected showing its own chain; a scene
  row's Edit filters… showing that scene's chain from behind the Transform tab while
  the item stays selected, selecting a scene going back to its chain, and the canvas
  margin's Edit scene filters… showing the shown scene's; and the scene Properties
  dialog listing the scene's chain, adding to it at once, which Cancel keeps.
- **Program and output chains** (`output-chains.spec.js`): the Outputs panel's Program
  output row ("master", "FX 0"), a recording's row under it with its state and FX
  button, and the footnote; the program's FX showing the master chain in the Filters
  panel from behind the Transform tab, with its note, its one audio chain and the note
  on video, the output's FX its own chain, and selecting an item going back to its
  source's. With Gain and Sharpen described: Add filter on the master offering only
  Gain, with its foot, as one edit that the program's FX and the Mixer's count; the new
  filter opening in place, a setting there as one more edit; the output's chain adding
  its own Gain, counted on its FX button alone, whose eye and × edit the output's chain,
  not the master's; and the master's filter in the Filter settings dialog, whose Remove
  deletes it.
- **Mixer** (`mixer.spec.js`, on the Audio mixer tab): the master card counting the
  seed's two channels, its Hear button enabled, its "FX 0" showing the program
  output's chain in the Filters panel, and its button (with its tooltip) unfolding the
  channel rows by name and folding them again; a typed gain in the Mic's dB field and
  its + stepper each as one edit, with the fader following; with `meterTicks`, the
  short-term loudness and its tooltip, the bar's fill against the target, and the true
  peak "safe" with nothing announced, then the warning over 0 dBTP, announced; and the Buses section folded under its
  count, the master first, adding a bus. The Hear button reading "Hear: off", then
  turning the master's monitor on after FX and switching it before FX, one edit each;
  the master's strip under More showing that monitor, naming its device (the tooltip
  follows), switching the tap with the device kept and turning it off, the field going;
  a new bus's own monitor leaving the master's off; and the vertical strip's "Hear
  off" turning it on as "Hear post". A tab click elsewhere in the dock leaving a
  channel's open Routing open (the rows aren't rebuilt). The title bar's flip button turning the meters
  vertical: the master strip, its "FX 0" enabled, and its channels folded beside it by short name, the
  Mic's stacked field scrubbed upwards and stepped down as one edit each, and its mute;
  docked in the Right sidebar the strips start vertical, 64px wide, and the button
  flips them back; and, with `meterTicks`, the vertical meters filling upwards and the
  master strip's loudness and true-peak warning, announced only once over 0 dBTP.
- **Labels** (`labels.spec.js`): the label manager, opened from the Scene objects
  panel's filter, adding a label (its icon picker open on the tag), renaming it,
  choosing an icon and a colour, a second label taking the next swatch, and Done; a
  row's tag popover putting a label on the Backdrop and staying open, the row's and the
  Inspector's chips and the Inspector's tag button, the filter's labels with their
  count, and the item menu's Labels submenu, checked with the label's colour as its
  swatch, taking it off; deleting a label taking it off the Backdrop in one edit; and
  on a scene ([ADR-0078](../../../../docs/adr/0078-scenes-carry-labels.md)): the manager
  opened from the Scenes panel's filter, in that panel, the scene row's tag popover
  opening under the row and filling its box with the label's colour, the row's and the
  Inspector's chips and tag button, the Scenes panel's labels filter, and a delete
  taking the label off the scene in one edit.
- **Polish** (`polish.spec.js`): Duplicate as clone's toast, which hides by itself; a
  folded Crop section staying folded after another scene is selected and after a
  reload; the Inspector's type icon and its 3D section, folded at first, with
  Perspective at 1200 and a Tilt sent as one edit and then "in use"; and the View
  menu's eleven panels, adding Live chat, which says it waits on YouTube sign-in.
- **Transform** (`transform.spec.js`, on the Transform tab): the summary and the disabled buttons with
  nothing selected; with one item, Selection off and Canvas on, Stretch as one edit
  Ctrl+Z undoes, and Reset rotation and Clear crop sending nothing when nothing
  changes; with two items, Selection on, Distribute off, and Match width and height
  giving both the primary item's size.
- **Keyframes and hotkeys** (`keyframes.spec.js`): New keyframe on Main's row nests
  "Keyframe 1" under it with the diamond, selected, under the canvas banner, and Main's
  caret folds it away and back; in the keyframe, the eye and the X field write its
  overrides while Main's own item stays as it was, the blend mode is locked, the Scene switches section is gone, and the
  item menu's Remove from scene and layer order are disabled; Reset to “Main” clears
  the overrides, after which the canvas's Ctrl+D and Del change nothing there; Switch
  to it puts it on program, and Delete keyframe removes it and selects Main. The Inspector's Hotkey row binds F1 to Intro (which a second client
  sees), F1 on the canvas then puts Intro on program on both clients, binding F1 to
  Main takes it from Intro, Escape cancels a capture and × unbinds.
- **Studio mode** (`studio.spec.js`, on the Transitions tab): in studio mode a scene
  picked for preview goes to program with Transition, and the old program scene comes
  back on preview; Transition and Cut are off while preview is the program scene; the
  studio bar shows only in studio mode; the preset picker lists the default with its
  kind and "+ New preset…", the default's editor has no duration, Delete or Make
  default and counts its overrides, and choosing Fade and "+ New preset…" send their
  `transition.update` and `transition.create`. The mock engine refuses preset edits,
  so the panel after an edit is tested natively.
- **Scene transitions** (`transitions.spec.js`): a scene row's Transition in submenu
  checks "Default (Cut)", lists the Cut preset with its kind, and sends
  `scene.set_transitions`, and Manage presets… shows the Transitions tab; a double click
  opens the scene's Properties dialog, whose hotkey field takes a chord, clears on
  Backspace, and with a new name is saved by OK and read back by the dialog; the canvas margin's Scene properties… opens it for the shown scene; the
  Inspector's Scene switches send `item.set_transition` with the whole settings. The
  mock engine refuses the transition edits after the page has sent them.
- **Privacy screen** (`privacy.spec.js`, on the header, the canvas and the Audio
  mixer tab): the Privacy button turning it on, with "Privacy on" pressed, the canvas
  frame blurred under its notice and the Mic's row reading "muted (privacy)" while
  Desktop audio's doesn't, then off again; Pause toggling it with focus in a gain
  field, and a second client following each change and turning it off with Pause, and
  then on again from a scene's rename field, which keeps its other keys; Pause
  toggling it while the Inspector's Set key waits, which leaves Pause unbound and
  binds the next key; Pause toggling it from the scene Properties dialog's Hotkey
  field, which stays empty and takes the next key; a
  client that connects while it is on showing it at once; and the vertical mixer's
  Mic strip reading "muted (privacy)" under its short name while Desktop audio's
  doesn't.

<a id="running"></a>
## Running them
<!-- @section
covers = ["crates/jstream-ui/tests/e2e/playwright.config.js", "crates/jstream-ui/tests/e2e/package.json"]
-->

```sh
cd crates/jstream-ui && trunk build     # the page under test, into dist/
cd tests/e2e
npm ci --ignore-scripts                 # Playwright, from the lockfile
npx playwright test                     # builds the mock daemon, then runs every spec
```

Playwright needs a Chromium: point `JSTREAM_E2E_CHROMIUM` at one, or set
`PLAYWRIGHT_BROWSERS_PATH` to a directory holding the build this Playwright version
expects. A failed test keeps a trace under `test-results/`
(`npx playwright show-trace <trace.zip>`).

CI's `ui-e2e` job builds the UI as the `wasm` job does, installs Playwright with
`npm ci --ignore-scripts` and runs the tests in the runner's Google Chrome, so it
downloads no browser.
