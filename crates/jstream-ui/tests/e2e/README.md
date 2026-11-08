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
  audio, so they get mixer channels), and the encoder profile `High quality`. It has no outputs. It stops
  when its standard input closes. It is a crate of its own because `jstream-ui` (layer
  3) may not depend on `jstream-server` (layer 4), not even for tests
  ([R-ARCH-002](../../../../docs/RULES.md#R-ARCH-002)). Nothing ships it.
- **One daemon per test** (`fixtures.js`): each test starts its own, with a random
  token, so tests share no state and run in parallel. The browser signs in with HTTP
  Basic, the token as the password, as a person's browser does when the daemon serves
  the UI; the connect form's token field can then stay empty. `connect` opens the page
  and connects; `disconnect` disconnects through the header's File menu;
  `openAddSource` opens the Add source dialog from the Sources panel's menu, by a
  right-click just under its last row (or under the empty scene's hint, since the tool
  bar at the panel's foot opens no menu), and
  `nestScene` nests a scene through it; `outputRow` finds an output's row in the
  Outputs panel, `addOutput` adds one through its "+ Add output" dialog,
  `openOutputSettings` opens an output's settings dialog from its row's ⋯ menu, and
  `openEncoders` opens the Encoders & mixes dialog; `rev` reads the revision the header last heard
  of; `showPanel` makes sure a docked panel's tab is the shown one; `describeKinds`
  routes the page's WebSocket so that `kind.list` and `kind.describe` answer for the
  descriptors a test gives, which the mock engine can't, and passes every other message
  on; given `refuse`, it also holds back the requests that function names and refuses
  them, as `invalid_argument`, when the test calls the `release()` it returns.
  `meterTicks` routes it too, passing everything on, and returns a function that
  sends the page a `meters.tick`, which the mock engine never sends, and re-sends it
  every 100 ms until the next one, as the engine's stream would, so the panel doesn't
  blank the meters while a slow run is still asserting on them. `statsTicks` sends
  `stats.tick`s the same way, since the mock runs no pipeline.
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
  Headless Chromium can hold a page that is not in front to about one animation frame a
  second, so a test with a second client calls `bringToFront()` on the page it drives
  with drags or clicks that wait on frames.

<a id="tests"></a>
## What the tests cover
<!-- @section
covers = ["crates/jstream-ui/tests/e2e/specs/**"]
-->

- **Browse** (`browse.spec.js`): with the desktop global stubbed, an image file's
  Browse… stores a path under the collection folder as a collection path and a path
  outside it as absolute, hides the button for app assets, and leaves the field when
  the stub cancels; a recording's add-output dialog and its settings dialog ask for a save dialog
  and a stream has no button; Install plugin asks for a folder dialog and fills the path and
  the suggested id. A page with no desktop global has no Browse button. The native
  dialog itself is not opened: the stub returns the path.
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
- **Tool bars** (`tool-bars.spec.js`): the Scenes bar's arrows moving the selected
  scene up and back down, off at either end; Add scene making a scene with its name
  open, whose empty Sources list says how to add one; Scene properties and Scene
  filters on the selected scene, and both off, with the arrows, for a keyframe, as Add source is. The Sources bar with nothing selected (only Add source
  on); Add source opening the dialog, placing the Backdrop again; the arrows restacking
  the selected item, each off at its end of the stack; Properties and Filters on it; Remove from scene taking it out. A
  right-click on a bar opens no menu, and the panel menu still opens on an empty scene.
  Delete scene needs `delete.plan`, so it is tested natively.
- **Scene folders** (`folders.spec.js`): New folder from the panel's and a folder's menu,
  named in place; drags into a folder's middle, out beside a top-level row and before a
  folder, which a second client follows; folding by the caret at two depths; a search
  keeping the open, unfoldable folders of a match; a folder refusing to go inside its
  own child; F2 renaming a folder, and Del deleting an empty one without asking. Deleting
  a folder that holds anything needs `delete.plan`, so it is tested natively.
- **Outputs** (`outputs.spec.js`, on the Outputs panel and the header): the panel is a
  compact list (health in its header, no form or library under the rows; a stream's row
  shows its host and encoder, the plain-rtmp ⚠ with the warning on hover, and why it
  can't start; a double-click opens its settings); adding a stream and a recording
  through the add-output dialog, a stream added with its key starting at once; a stream can't start or go live until its key is set
  in its settings; the key never appears in the page, and a set key reads masked;
  go live and end stream, with the target and Delete off while live; Remove taking the
  key on OK; Delete asking first, and Cancel keeping it; the header's Record starts and
  stops a recording; a settings edit dropped on Cancel; the server URL and an encoder
  profile sent on Apply, both read back by a fresh page; a recording's Audio select
  moving it to another bus on OK, read back by a fresh page; in the Encoders & mixes
  dialog, a new profile named "Default H.264", making an AV1 VBR encoder profile (a peak
  under the average is named in the form, which stays open) and a vertical video mix, a
  recording using both (neither can then be deleted), both read back by a fresh page,
  the mix resized from its Edit form opened over the settings dialog and deleted once
  the recording is back on the program; a second client sees outputs created, started
  and deleted.
- **Codec downloads** (`codec-downloads.spec.js`, in the Outputs panel's Encoders & mixes
  dialog): the mock
  engine has no codec backend, so the spec routes the page's WebSocket and answers
  `codec.downloads`, `codec.download` and `codec.delete` for one OpenH264 download. With
  no hardware H.264 encoder, Encoders offers it with Cisco's notice, the licence link and
  the licence's full text, sends nothing until Download is pressed, then shows it
  installed with the notice, link and Delete. The About dialog then lists it with the
  notice, link and terms; Delete sends `codec.delete`, brings the offer back, and takes
  it out of About. With hardware, nothing is offered until the encoder form asks for software
  H.264 (and not for HEVC), and a saved software profile brings the section's offer.
- **Plugins** (`plugins.spec.js`, the Settings dialog's Plugins page, opened by File ›
  Plugins…): the mock engine runs no plugins,
  so the spec routes the page's WebSocket itself and answers `plugin.list`,
  `plugin.install`, `plugin.grant` and `plugin.enable` from a list it keeps. An install
  sends the folder and the ID suggested from its name with no grant, and, since the
  manifest asks for more than `read`, opens the grant dialog, which offers `read` ticked
  and fixed and only the classes and host asked for, and sends just what was ticked; the
  row then names what is asked for and not granted. A plugin off as `needs_grant` is
  granted a class and a folder and turned on in one step. The Inspector holds neither
  plugins nor pairing; File › Settings… opens on General, a click or Up and Down move
  between General, Plugins and Devices, and the install dialog opens centred over a
  moved Settings dialog and closes alone on Escape.
- **Header stats** (`header-stats.spec.js`): the readout shows the dropped count from
  the first `stats.tick`, the CPU share ("<1%") from the second while the program draws
  nothing, and "60 fps · 2 dropped · CPU 3%" once the load says it drew 60 frames in a
  second.
- **Stream stats** (`stream-stats.spec.js`): added from the View menu, the panel says
  there are no outputs; with a recording added it is READY with no uptime and shows
  the default profile's H.264 at 8000 kbps; once the recording starts it is LIVE with
  the no-data box, and the ticks the test sends give the uptime, the bitrate against
  the target, the dropped share, Excellent then Poor, the chart and what was sent;
  hovering the chart's right edge reads the newest sample "now" and further left the
  one before "1 s ago", gone when the pointer leaves; a stop puts it back to READY and
  Waiting.
- **Docking** (`docking.spec.js`): the Editing layout's zones (the Inspector, Filters and
  Transform as tabs of one group on the right, Transitions as a tab beside Outputs), with empty zones hidden; the View
  menu hiding and adding a panel at the end of its home zone, and the layout marked
  "(modified)"; dragging a tab onto a tab strip groups it, and onto an empty zone,
  which shows while dragging, docks it there; a lone panel collapses by its title,
  closes on a second click of its ×, and the Inspector doesn't collapse once it is alone
  in its zone; a splitter resizes a zone, and a saved layout and its sizes survive a reload;
  the Layout menu switches to Live, with Stream stats, Outputs and Agent activity as
  tabs on the right, and resets to Editing.
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
  on the canvas tools over an item leaves the selection alone; a row's lock button
  locks the Backdrop, after which a drag selects it but leaves it in place, the readout
  reads “640 × 360 · locked”, an arrow key sends nothing, and the item menu's checked
  Lock unlocks it.
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
  item and wrapping, Enter chooses, and a press outside closes it; the Sources
  panel's menu opens the Add source dialog, whose types start with the seed's audio
  inputs, and its Color type places the seed's Backdrop again as a reference 40 px
  down and right, selected (the mock lists no kinds, so Color can't create); a scene nests once and isn't offered again, and the backdrop
  cancels the dialog. Its search box narrows the types by name ("COLOR" leaves Color,
  picked) or description ("nest" leaves Scene), shows "No source type matches." with
  Add disabled when none match, and an empty search lists them all again. An item row's menu lists the item menu's lines and Delete source everywhere…, Send to back
  restacks in one edit and Stretch to canvas, from the Transform submenu, fills the
  canvas; the item menu's Rename opens the row's name, Visible hides and shows the item
  with its check following, Screenshot (source) shows a toast (the mock engine streams
  no pictures, so it can't take one), Blending's Multiply replaces the checked Normal, and Paste
  stays off until Copy, then places the source again; a scene row's Show in multiview
  toggles its check, and Copy then Paste scene adds a copy of the scene; Copy filters
  from the Backdrop pastes its two video filters onto a nested Intro, a second paste
  appends two more and Paste filters, replacing leaves two, each one edit, and Paste
  transform gives a stretched second placement the Backdrop's 640 × 360 box; on the canvas the menu
  ends with Select all, Duplicate as clone adds "Backdrop copy" selected, two selected
  items get a counted head with the duplicates disabled, and Remove from scene removes
  both; a placement of a shared source switches to an independent clone and back to a
  reference in one edit each, its menu's head and checks following, and the original,
  placed once, has no switch; Ctrl+D and Ctrl+Shift+D duplicate on the canvas and in a
  row, and Del removes there; a nested scene's menu opens it; a scene row's Rename
  renames, and the Scenes panel's New scene adds one with its name field focused. The
  header's menus act as a menu bar: a click opens File, pointing at Edit and View opens
  each in its place, a click on the open menu's button closes it, after which pointing
  opens nothing, and a press outside closes one; from the keyboard Down opens a menu at
  its first item and Up on a button at its last, Up and Down wrap through it, Right and
  Left move along the bar (wrapping from File to Help), Escape closes it back to its
  button, and Ctrl+Right on a button is left alone; with animation frames delayed, a
  second Left pressed before the next menu is drawn still moves along the bar. The
  header's mark is the bundle's `icon.png`, loaded; the Help menu lists Documentation,
  Getting started, Release notes and Report an issue, each a link to the repository's
  page in a new tab with `noopener noreferrer`, then Check for updates and About
  JStream. About shows
  the icon, the version, the build, the engine's protocol and the licence with its
  links, and closes on Escape and on Close. Check for updates, on this page, says the
  check runs in the desktop app. File's first item is Settings…, and its "Check for
  updates at launch" checkbox starts off and is kept in the page's storage. Arriving at
  File from the keyboard focuses Settings….
- **Copy and paste** (`clipboard.spec.js`): Ctrl+C and Ctrl+V on the canvas paste the
  seed's Backdrop again as one batch, on top and selected; the empty canvas's Paste puts
  it in Intro, and a row's Ctrl+C and Ctrl+V do the same; Paste scene is disabled until
  a scene is copied, then a scene row's Ctrl+C and Ctrl+V, the panel's Paste scene and
  a row's Duplicate scene each add "\<name\> copy" just below, with the scene's item.
- **Scene-input rows** (`scene-refs.spec.js`, with a scene mask from `describeKinds`):
  a mask added to Backdrop with Intro as its scene shows Intro as a child row with the
  purple badge and the tooltip "Alpha mask: uses the scene “Intro”"; the row's caret
  folds it and brings it back, and a click selects Intro.
- **Groups** (`groups.spec.js`): Group selection on two items makes "Group 1", left
  out of the Scenes panel, whose caret shows the two items indented; a click on one
  opens the group's scene with it selected; Ungroup puts them back, which a second
  client follows; the panel's New group groups the selected item, and a row dragged
  onto the group's row goes into it.
- **Properties dialog** (`properties.spec.js`): opened from an item's menu, it shows
  the type's title, the source's name, its two filters and an enabled Add filter
  button; Cancel and Escape drop
  a changed name, OK renames in one edit, and Enter with a blank name sends nothing.
  Dragging the title moves the dialog, which stays inside the window and opens again
  where it was left; a double-click on the title centres it, and the header's × still
  closes it.
  The mock describes no kinds, so the settings form and the draft patch are covered
  natively. A video capture device created through Add source, with its descriptor
  from `describeKinds`, opens the dialog with its live picture: a `preview.subscribe`
  of the draft source on a channel from 2³¹ up, another on a new channel once the
  Device field changes, closing the first with nothing saved, and an unsubscribe when
  Cancel closes the dialog; a colour source shows no picture. The mock streams no
  pictures, so what the picture draws is checked by hand. With the `color` kind
  described (its `color` live, ADR-0091), a changed colour goes out as a `live.set`
  of `source.set_settings` with `rev` unchanged; Cancel sends `live.cancel` and saves
  nothing; OK's batch carries `ends_gesture` with the final colour as one edit, and the
  dialog opened again shows it. That the output draws the value is the engine's
  test. With the `text` kind described, a new Text source's Font list offers "Bundled
  font", the mock's two fonts (`MOCK_FONTS`) and "Other file…" (ADR-0092); a picked
  font is saved on OK as one edit and selected when the dialog opens again, and "Other
  file…" shows the root and path, which stay shown for a path the list doesn't name.
- **Capture targets** (`capture-targets.spec.js`): with the `window` kind described, a
  new Window capture's Properties dialog lists the mock's one window by name and size
  after None, and not its display; picking it and pressing OK is one edit, and the
  dialog opened again shows that window.
- **Inspector** (`inspector.spec.js`): the selected Backdrop's X, Y, W, H and Rotation;
  a new width with the W/H link on scales the height and the canvas readout follows,
  and with it off the other side stays; a stepper is one edit Ctrl+Z on the canvas
  undoes; arrow keys step (Ctrl by 10), Escape drops typed text, a drag across the
  field is sent once on release, and text that isn't a number sends nothing while `−`
  and `,` are read; crop (with the folded "cropped" hint), opacity, the Fit select and
  the favourite star are sent; with no item selected the scene's object count, uses and Transition
  in and out show, and a second selected item shows "+1". The mock engine doesn't
  implement `scene.set_transitions`, so choosing one isn't run.
- **Filters** (`filters.spec.js`): with no item selected the panel shows the scene's
  chain; with the Backdrop selected, its title, note, one video chain of its source
  (above the item's effects) with its count and the two rows, Blur struck through; the eye turning Blur on as one edit, a
  drag putting it above Sharpen as one more, and × removing Sharpen; Edit filters… in
  an item's menu selecting the item and showing the panel behind the Transform tab; the
  Add filter dialog from the panel and over the Properties dialog, which Escape leaves
  open; and the Filters window on a double-click, with the prototype's hint for
  Blur, whose switch applies at once, Cancel puts back and OK keeps; from the panel's
  head it opens on the first filter with Source, This item and Scene tabs, a row's click
  shows that filter, the Scene tab shows its empty chain, and Remove deletes the filter
  shown and shows the next, the window staying open.
  With three kinds described, Cancel in the window putting back what its own list did
  (an eye on a filter never shown, × on Sharpen) and dropping a Blur added from the
  window, which the Add filter dialog opened over and which the window then showed,
  keeping those earlier changes; then a Blur added from the panel, which opens the
  window on it, whose changed size and switch Cancel puts back, keeping the filter.
  With three kinds described (`describeKinds`): the rows' summaries and carets, a click
  on a filter with four settings opening the dialog, Cancel pressed straight after a
  change there still putting the filter back (the change and its restore as two
  edits), the Add filter dialog offering
  only the video kinds under their group, its search, the picked kind's details, and
  Add as one edit, the new filter opening in place, where a changed setting is one more
  edit and updates the summary. With one setting refused late: a refusal that arrives
  after its row was closed, and one after the Filters window was cancelled,
  each in the problem bar.
- **Scene filters** (`scene-filters.spec.js`): with no item selected the panel showing
  Main's empty video chain with the scene note; with Sharpen and Gain described, Add
  filter offering only Sharpen, with the scene's foot, as one edit on the scene, while
  the Backdrop keeps its own two; another scene selected showing its own chain; a scene
  row's Edit filters… showing that scene's chain from behind the Transform tab while
  the item stays selected, selecting a scene going back to its chain, and the canvas
  margin's Edit scene filters… showing the shown scene's in place of the selected item's; and the scene Properties
  dialog listing the scene's chain, adding to it at once, which Cancel keeps.
- **Custom shader** (`custom-shader.spec.js`): with `kind.list`, `kind.describe`,
  `pack.list`, `pack.read` and `pack.write` answered by the page's socket from the packs
  the test writes (the mock engine loads no packs), Add filter › Custom shader writes
  the template as `custom-1` and opens the editor on it; the coloured copy holds exactly
  the text, with the annotations, an attribute, a keyword, a call and a type coloured,
  under a transparent textarea in the same box; Tab indents at the caret and the change
  compiles ("Compiled."); a line the fake engine refuses is marked in the gutter and
  named under the text while the colours follow the new text; and scrolling the text
  scrolls the copy, to the very end both ways. Then, on a second shader whose `amount`
  the Filters window set to 0.25, an `@renamed amount mix_amount` line sends a manifest
  at version 2 with its `renames` and is coloured as an annotation; Save as pack…
  refuses a taken ID and a bad one, then writes `warm-look` with the same shader, and
  the filter moves to its kind in the old one's place (the chain reads Sharpen, Blur,
  Warm look) with the editor open on `warm-look`, its add carrying exactly
  `mix_amount: 0.25`. The fake kinds declare the shader's own `@param` lines, and a
  `filter.add` of a written pack with a setting its kind doesn't declare is refused, as
  the engine refuses it.
- **Item effects** (`effects.spec.js`): no effects section with no item selected; with
  the Backdrop selected, "This item’s effects" with its note and an empty Effects chain
  under the source's; with Vignette (the core kind's four settings), Sharpen and Gain
  described, "+ Add effect…" offering the two video kinds, Vignette under Video · Style
  with its hint, for this item, with the item's foot; Add as one edit on the item, which
  opens the Filters window on it, "Effect on “Backdrop”", once the collection
  holds it, while the source keeps its own two; Sharpen added as a second effect, and
  dragged above the vignette as one reorder of the item's chain; the row's eye and × as
  one edit each; and selecting the scene again leaving the effects section out. In a
  new keyframe of Main, the Backdrop's effects "in this keyframe", with the keyframe's
  note and the note that they follow the item's; Sharpen added there as one edit, "For
  this item of “Backdrop” in this keyframe", after which the keyframe has its own and
  Main's item still has none; and Follow the item clearing them as one edit.
- **Program and output chains** (`output-chains.spec.js`): the Outputs panel's Program
  output row ("master", "FX 0", the program filters' note as its tooltip), a recording's
  row under it with its state and FX button, its ID listed only once; the program's FX showing the master chain in the Filters
  panel from behind the Transform tab, with its note and its video and audio chains,
  the output's FX its own chain, and selecting an item going back to its source's. With
  Gain and Sharpen described: Add filter on the master offering both, with its foot,
  Gain added as one edit that the program's FX and the Mixer's count; the new filter
  opening in place, a setting there as one more edit; on the output, Sharpen showing
  the first video filter's cost note, then its own Gain, counted on its FX button
  alone, whose eye and × edit the output's chain, not the master's; and the master's
  filter in the Filters window on a real double-click, which opens it even
  though the first click's toggle scrolls the taller panel (#984), with no scope tabs,
  and whose Remove deletes it.
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
- **Labels** (`labels.spec.js`): the label manager, opened from the Sources
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
  to it puts it on program, and Delete keyframe removes it and selects Main. A drag on
  a keyframe's canvas sends `live.set` values that name the keyframe (the
  `scene.create_variant`'s scene, not its parent), as its commit's `item.set_geometry` does, with no revision until the drop, and leaves Main's box as it
  was. With the Backdrop and a second colour grouped in Main, the keyframe lists the
  group's children: a child's eye hides it there as one edit, a click selects it in
  the keyframe, whose X field writes its override, a Left crop is written there, its row's
  menu has Remove from scene, Bring to front and Crop disabled and its Clear crop clears
  that crop as one edit, leaving the keyframe selected, Main still shows it as it was (where
  a click opens the group's scene at its own box), and Reset to “Main” brings it back.
  The Inspector's Hotkey row binds F1 to Intro (which a second client
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
  Backspace, and with a new name, a background colour with alpha, and Show in multiview and Include its audio when nested unchecked is saved by OK and read
  back by the dialog (the swatch showing the colour without its alpha); the canvas margin's Scene properties… opens it for the shown scene; the
  Inspector's Scene switches send `item.set_transition` with the whole settings. The
  mock engine refuses the transition edits after the page has sent them.
- **3D scenes** (`scene-cameras.spec.js`): the shown scene's Properties turns on its 3D
  camera, which starts from the framing camera (Position X 960, Target Z 0, Nearest on
  top), and OK sends `scene.set_camera` with a moved Position Z, which the dialog reads
  back and which draws the box smaller; Alt-drag on the canvas shows the camera's
  readout, sends live values, and commits one `scene.set_camera` with a moved camera as
  one revision, which one undo puts back. With a window capture in the scene, the
  camera and Depth of field on, Follow pointer offering Add Follow pointer… (a filter on
  the capture, ADR-0120), and a camera-motion row added, the settings column's
  `scrollWidth` stays within its `clientWidth` at the dialog's width and when a
  720-wide viewport clamps the dialog; the Nearest on top check sits in front of its
  text, and the Roll field keeps room for its number.
- **Projectors** (`projectors.spec.js`): the View menu's Program in a window opens a
  page of its own at `?projector=program`, titled for it, with one picture and the
  toast on the workspace, and Escape closes it; Intro's row's Pop out submenu offers the
  scene in a window or full screen and opens `?projector=scene:intro`; Backdrop's
  object row's Pop out offers Source in a window or full screen and opens a
  `?projector=source:` page titled "Source: Backdrop"; Program full
  screen opens `&full=1` and, in a browser, says to double-click it; the multiview shows
  program captioned with Main, no preview outside studio mode, and Intro and Main below
  it, Main framed as on program, and turns to studio mode when the workspace does. The mock engine sends no preview streams, so no test
  checks a picture: the engine's tests check the frames.
- **Video settings** (`video-settings.spec.js`): File › Video settings… opens on
  1920×1080 at 30 fps; a preset fills both sides, a typed side of 0 turns the preset to
  Custom and is refused in the dialog; 1280×720 at 60 fps applies as one revision and
  reopens with those values; Apply with nothing changed sends nothing; and one Undo
  takes back both.

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
