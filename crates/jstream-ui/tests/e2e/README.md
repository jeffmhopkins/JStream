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
verified = { hash = "b3:6ff97f4df6dd53cc", by = "agent:claude@session_01StfvJc4TCXD8poALM3hjzY", note = "T1 merge of main (G3b virtual_cam #297, Q3 UI e2e #309, #295 clock-swap re-plan, #236): re-read main's added text beside T1's transition/preview text; independent paragraphs; protocol line now 0.19 since G3b took 0.18; e2e mock-daemon only gained preset_id: None; holds" }
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
  the UI; the connect form's token field can then stay empty.
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
verified = { hash = "b3:8c305e610d98ee84", by = "agent:claude@session_016cfwAwenZjqNGDUSCJ89jp", note = "Q3: written with the harness, specs and config; 13 tests (one test.fail for #305) pass locally in headless shell and full Chromium 141" }
-->

- **Connecting** (`connect.spec.js`): the form offers the serving engine's URL and
  connects with the browser's credentials; the status bar shows the URL, protocol and
  revision; disconnecting returns to the form with the token field empty; a refused
  upgrade shows its reason; the token never appears in the page, its storage or its
  cookies. A URL the page's policy refuses leaves the form on "Connecting…" (#305): that
  test is marked `test.fail` until the browser client is fixed.
- **Outputs** (`outputs.spec.js`): adding a stream and a recording; a stream can't start
  or go live until its key is set; the key field empties on save and the key never
  appears in the page; go live and end stream; clearing the key; deleting; editing the
  server URL and choosing an encoder profile, both read back by a fresh page; a second
  client sees outputs created, started and deleted.
- **Canvas** (`canvas.spec.js`): the program scene's box is drawn where the seed put it;
  a drag sends nothing until the drop, then one edit that undo and redo reverse; arrow
  keys nudge (Shift by 10) and Ctrl+Z undoes; a second client sees a move.

<a id="running"></a>
## Running them
<!-- @section
covers = ["crates/jstream-ui/tests/e2e/playwright.config.js", "crates/jstream-ui/tests/e2e/package.json"]
verified = { hash = "b3:dbac1e8d9fa742df", by = "agent:claude@session_016cfwAwenZjqNGDUSCJ89jp", note = "Q3: written with the harness, specs and config; 13 tests (one test.fail for #305) pass locally in headless shell and full Chromium 141" }
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
