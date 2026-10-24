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
  1920×1080 canvas, and the encoder profile `High quality`. It has no outputs. It stops
  when its standard input closes. It is a crate of its own because `jstream-ui` (layer
  3) may not depend on `jstream-server` (layer 4), not even for tests
  ([R-ARCH-002](../../../../docs/RULES.md#R-ARCH-002)). Nothing ships it.
- **One daemon per test** (`fixtures.js`): each test starts its own, with a random
  token, so tests share no state and run in parallel. The browser signs in with HTTP
  Basic, the token as the password, as a person's browser does when the daemon serves
  the UI; the connect form's token field can then stay empty. `connect` opens the page
  and connects; `disconnect` disconnects through the header's File menu;
  `openAddSource` opens the Add source dialog from the Scene objects panel's menu and
  `nestScene` nests a scene through it; `rev` reads the revision the header last heard
  of; `showPanel` makes sure a docked panel's tab is the shown one.
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
  although Chromium sends that socket an `error` event and no `close` (#305).
- **Scenes and sources** (`scenes.spec.js`): the seed's scenes in order with the
  program scene marked and selected, and its item listed; putting a scene on program,
  which a second client follows; adding a scene, which is selected, and renaming it in
  place; hiding and showing an item, placing an existing source again through the Add source
  dialog and removing that item; selecting an item opens its properties. The mock engine lists no kinds and makes
  no delete plans, so new-kind sources, the properties form's fields and deletes are
  tested natively.
- **Outputs** (`outputs.spec.js`, on the Outputs panel and the header): adding a stream and a recording; a stream can't start
  or go live until its key is set; the key field empties on save and the key never
  appears in the page; go live and end stream; the header's Record starts and stops a
  recording; clearing the key; deleting; editing the
  server URL and choosing an encoder profile, both read back by a fresh page; a second
  client sees outputs created, started and deleted.
- **Docking** (`docking.spec.js`): the Editing layout's zones (Transitions as a tab
  beside Outputs), with empty zones hidden;
  the View menu hiding and adding a panel, and the layout marked "(modified)"; dragging
  a tab onto a tab strip groups it, and onto an empty zone, which shows while dragging,
  docks it there; a lone panel collapses by its title, and closes on a second click of
  its ×; a splitter resizes a zone, and a saved layout and its sizes survive a reload;
  the Layout menu switches to Live and resets to Editing.
- **Canvas** (`canvas.spec.js`): the selected scene's box is drawn where the seed put
  it, under the scene's caption; a click on the box selects its row in the sources list
  and shows the size readout, a click on empty canvas clears both, and selecting another
  scene in the list switches the canvas to it; a Ctrl drag sends nothing until the drop
  and shows the top-left readout meanwhile, then one edit that Ctrl+Z and Ctrl+Shift+Z
  reverse; Shift drags at half speed; the Snapping tool toggles; arrow keys nudge
  (Shift by 10) and Ctrl+Z undoes; a second client sees a move; a marquee started on
  the margin selects the box it touches and Escape clears it; with a second item placed
  (the Intro scene, nested), Ctrl+A selects both, one nudge moves both as one edit, Shift-click on the
  canvas takes one out and Shift-click on its row in the sources list adds it back; a
  double click brings the closed Inspector back showing the item, and a double click
  on the canvas tools over an item leaves the selection alone.
- **Menus and Add source** (`menus.spec.js`): the canvas's right-click menu on the
  frame (its items, Paste disabled, turning snapping off and Select all) and on the
  margin, which Escape closes; the arrow keys move through a menu, skipping a disabled
  item and wrapping, Enter chooses, and a press outside closes it; the Scene objects
  panel's menu opens the Add source dialog, which places the seed's Backdrop again as
  a reference 40 px down and right, selected (the mock lists no kinds, so its Color
  type can't create); a scene nests once and isn't offered again, and the backdrop
  cancels the dialog.
- **Studio mode** (`studio.spec.js`, on the Transitions tab): a scene picked for
  preview goes to program with Transition, and the old program scene comes back on
  preview; Transition and Cut are off while preview is the program scene; a value the
  preset form refused keeps the preset from being added. The mock engine refuses
  preset edits, so the edits the preset form sends are tested natively.

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
