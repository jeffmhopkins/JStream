<!-- @doc
id = "RUNBOOK-WINDOWS-HW-PASS"
title = "Windows hardware pass"
kind = "guide"
status = "active"
review_every = "180d"
-->

# Windows hardware pass

Every open Windows hardware run, as one checklist for one session on the maintainer's
Windows machine. Each item gives its steps, what passes, and the issue (and PR) it
closes. Report results on that issue, as each one asks; a failure becomes a follow-up PR
that links back to it.

The items are grouped by the build they need, so each build is made once. Some depend
on PRs that haven't merged yet. Run those from the PR's branch, or wait until it merges
and run them from its wave.

`scripts\windows-pass.ps1` runs every item, each from the commit it needs, and writes one
report ([The script](#script)). The steps below are what it does, and how to do any of
them by hand.

A release's own run is different: [`scripts\release-pass.ps1`](#release-pass) checks the
downloaded zip, with no build, and [the alpha.5 checklist](#alpha5) is what it asks.

<a id="script"></a>
## The script
<!-- @section
covers = ["scripts/windows-pass.ps1", "scripts/windows-pass.bat"]
-->

From a checkout on the Windows machine, in Windows PowerShell 5.1 or PowerShell 7:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\windows-pass.ps1
scripts\windows-pass.bat -List           # the checks, their sources and what they need
scripts\windows-pass.bat -Only render,nv12
scripts\windows-pass.bat -Unattended     # only the checks that need no one
```

- **Sources.** Each check runs from one source: `main`, `wave/m1-3`, or an open PR's head
  (`pull/<n>/head`). The script fetches each into `refs/jstream-pass/<name>` and checks it
  out as a detached worktree under `%LOCALAPPDATA%\JStream-windows-pass\src\<name>`, where
  its build stays for the next run. The checkout the script runs from, its branch and its
  files are not changed. `-NoBuild` reuses the worktrees as they are, without fetching or building the daemons
  and apps; the `cargo test` steps, the browser helper and the desktop UI still compile what
  they need. The source table at
  the top of the script is the one place to change when a PR merges.
- **What it asks first.** Every step that changes the machine asks before it runs, and
  a no is recorded as skipped: anything run as administrator (each is one UAC prompt),
  registering or unregistering the virtual camera, a forced GPU reset, installing a tool
  with `cargo install`, and reading the stream key file. The files it creates under
  `%PUBLIC%` for the ACL checks are removed when the check ends. `-Unattended` declines
  all of these and skips every question. Its timed steps still run, since their automatic
  checks need no one: it records the display, and the webcam with its microphone.
- **Questions.** A check only a person can judge (the Camera app shows the frames, Studio
  goes live) asks pass, fail or skip where it happens.
- **When you act.** Every line that needs you starts with a magenta `>>> YOU:` and a beep.
  A timed step (a recording, the producer, a GPU reset, the unplug test) first says what
  is coming and waits for Enter; its timer starts only then, with a yellow `>>> NOW:` line
  saying what to do. Recordings open in VLC when it is installed.
- **One run at a time.** A lock file in the work folder stops a second run from starting
  while one is going, since two runs fight over the webcam and the worktrees.
- **Daemons.** Each daemon it starts has its own token file and log folder in the run
  folder, drives its checks over MCP as the smoke script does, and is stopped at the end
  of its check. The graceful stop sends Ctrl-C from a helper process that attaches to the
  daemon's console.
- **Report.** The run folder (`%LOCALAPPDATA%\JStream-windows-pass\runs\<time>`, or
  `-OutDir`) gets `report.md`, `results.json`, every step's log and the recordings. The zip
  beside it holds the text files only, with the run's tokens replaced by `***`, and the
  stream key too once you let the YouTube check search the logs for it. Token files, key
  files and recordings stay out. The script ends by printing the zip's path
  and a summary, one line per check, to paste back with it. If the run left something on
  the machine (a virtual camera still registered, the Camera app open), the report and
  the last lines say what and how to undo it.
- **Not checked by the script:** what isn't visible outside the engine, such as
  `OutputStats::readbacks` and `gpu_fallbacks` (it checks the log lines that report the
  same events), item 19's log search (its step 3, by hand), item 20 (all by hand), and
  the items the report lists under "Not in this pass".

| Check | Item | Source |
|---|---|---|
| `smoke` | [11](#main) | main |
| `render` | [1](#main) | main |
| `nv12` | [5](#m3-2) | main |
| `media` | [4](#main) | main |
| `browser` | [2](#main) | main |
| `youtube` | [3](#main) | main |
| `preview` | [12](#main) | main |
| `audio` | [18](#main) | main |
| `vcam-owner` | [6](#prs) | wave/m1-3 |
| `token-acl` | [7](#prs) | wave/m1-3 |
| `crash` | [13](#prs) | wave/m1-3 |
| `vcam-register` | [9](#prs) | wave/m1-3 |
| `gpu-reset` | [10 and 14](#prs) | wave/m1-3 |
| `private-files` | [8](#prs) | PR #574 |
| `webcam` | [15](#prs) | PR #726 |
| `cursor` | [16](#prs) | PR #685 |
| `clap` | [17](#prs) | PR #573 |
| `help-links` | [19](#prs) | PR #1083 |

<a id="release-pass"></a>
## The release pass
<!-- @section
covers = ["scripts/release-pass.ps1", "scripts/release-pass.bat"]
-->

`scripts\release-pass.ps1` checks a downloaded release instead of a build. It runs the
release's own `jstream-daemon.exe` and `jstream-cli.exe`, so it needs no Rust, git or
build: only the zip, and the script beside it. From a checkout, or with the two script
files downloaded from `scripts/` on GitHub into one folder:

```powershell
scripts\release-pass.bat                                   # finds the zip in Downloads
scripts\release-pass.bat -Zip C:\path\jstream-v0.1.0-alpha.5-windows-x64.zip
scripts\release-pass.bat -Root C:\path\to\unzipped\folder  # already unzipped
scripts\release-pass.bat -AutomatedOnly                    # the automatic checks only
scripts\release-pass.bat -ManualOnly -PtzController 192.168.1.20
```

- **Order.** The automatic checks run first and need no one. The script then walks
  [the release's manual checklist](#alpha5), one item at a time: it says what to do in
  `jstream.exe` and asks pass, fail or skip. Every line that needs you starts with a
  magenta `>>> YOU:` and a beep.
- **The zip.** With `-Zip`, or the zip found in Downloads, it checks the zip against the
  `.sha256` file beside it, when there is one, and unzips it under
  `%LOCALAPPDATA%\JStream-release-pass\release`. `-Expect` names the version it should
  report, `0.1.0-alpha.5` by default.
- **Nothing of yours is touched by the automatic checks.** Their daemon listens on a free
  port with its own token and log folder, and `APPDATA` points into the run folder for
  it alone. Your settings, projects, recent list, effect packs and stream keys are never
  read or changed, and no output goes live. Two manual items use your own app on purpose:
  the agent check starts `jstream.exe --mcp` and reads your `%APPDATA%\JStream\token` in
  memory only, and the PTZ controller check listens on UDP port 52381 on your LAN address,
  answering only the controller's address. Windows may ask once whether the daemon may
  use the network: allow private networks.
- **Report.** The run folder (`%LOCALAPPDATA%\JStream-release-pass\runs\<time>`) gets
  `report.md`, `results.json`, the daemons' logs, the snapshots and the bench's reports.
  The zip beside it holds the text files and the snapshots, with every token replaced by
  `***`; token files and recordings stay out. The script ends by printing the zip's path
  and one line per item to paste back with it.

| Check | What passes | Issues |
|---|---|---|
| zip checksum | the zip's SHA-256 matches its `.sha256` file | |
| package files | `jstream.exe`, the daemon, the CLI, the virtual camera DLL, the plugin host, the browser helper and the UI are all there | |
| versions | `jstream-daemon --version` and `jstream-cli version` name `-Expect` | |
| daemon starts | the daemon prints its `listening on` line | |
| kind.list | it lists the alpha.5 kinds: the new filters, the depth-aware 3D filters, `model`, `light`, `follow_pointer`, the MIDI sources and `network_audio` | #1226, #1227, #1231, #1225, #1222, #1223, #1224 |
| profile.get | `thread_cpu` is on, `jstream-render` and `jstream-frames` are among the threads, and the views, warm, streams and PTZ counters are there; a program frame over budget or skipped is a warning | #1221 |
| snapshot of a browser page | a green HTML page snapshots green | #1184, #1194, #1215 |
| snapshot of a display capture | the snapshot isn't one flat colour | #1194, #1215 |
| save, new, open | `collection.new`, `save_as` and `open` round-trip a scene, and `collection.info` lists the project first in the recent list | #1218 |
| webcam formats | each webcam lists its formats, and its controls when it has any; no webcam is a warning | #1195, #1196 |
| bench | `jstream-cli bench --scenario all,display` (and `webcam` with one plugged in) passes every default gate; the media scenario runs with `-Media` or a file ffmpeg makes | #1220, #1140 |
| graceful stop | Ctrl-C makes the daemon print `stopped` and exit 0 | |

<a id="alpha5"></a>
## The alpha.5 checklist
<!-- @section
-->

What the alpha.5 run checks: the fifteen alpha.5 targets that wait on a Windows run
(the README's progress table, now under beta.1) and a real PTZ controller. Run it on the
released zip, `v0.1.0-alpha.5`, with [the release pass](#release-pass). Report each
item's result on its issues; a failure gets its own issue, labelled `windows-hardware`,
that links back to the item.

**You need:** the zip; a second monitor; a webcam; a microphone or any sound playing;
a MIDI keyboard; a DAW with the JTunnel plugin; a PTZ controller (a joystick keyboard or
a control app) on the same network; a VISCA-over-IP camera if you have one; ffmpeg on
`PATH` for the bench's media scenario (optional).

### Automated

The script runs these first. Each one's pass line is in [the table above](#release-pass).

| Target | Covered by | What is still by hand |
|---|---|---|
| The thread model (#1221) | profile.get, bench | M1 |
| The bench (#1220) | bench | none |
| A seamless preview (#1217) | bench (its view and stream gates) | M1 |
| Animated sources (#1138, #1139) | bench (`small_2d`, `layered`) | M2 |
| Snapshots on Windows (#1184, #1194, #1215) | the two snapshots | M7 |
| Save and load projects (#1218) | save, new, open | M5 |
| Webcam Properties (#1195, #1196) | webcam formats, bench (`webcam`) | M12 |
| Filters, 3D, MIDI and network audio kinds | kind.list | M8, M10, M14 |

A bench fail blocks beta.1; report it on #1220 with the bench's `summary.md`.

### Manual

Each item: what to do in `jstream.exe` from the release folder, then what passes. The
script asks each one in this order.

**M1. Thread model and a seamless preview (#1221, #1217).** Open a busy scene: a
browser page, a display capture, the webcam and animated text. Turn on studio mode and
switch preview and program ten times, half with Fade and half with Cut, while Task
Manager's Performance tab shows the CPU's logical processors.
**Pass:** no switch shows a placeholder, a blank frame or a stall; the preview looks as
sharp as the program; the work spreads over several cores rather than one at 100%.

**M2. Animated sources (#1138, #1139).** Make a scene of about twenty small animated
items (timers, text with motion) and one small still image, and watch the header's fps
and CPU for a minute.
**Pass:** the fps holds at the base rate and every item moves smoothly.

**M3. Pop-out projectors (#1208).** View › Projectors › Program in a window. Drag it to
the second monitor and double-click it. Then Pop out on a scene row, and Pop out on an
item (its source alone). Escape closes each.
**Pass:** each window opens with a live picture, never black; a double click fills that
monitor; it runs at the full frame rate.

**M4. Settings dialog (#1207, #1210, #1212, #1192, #1230).** File › Settings…: visit
General, Plugins, Audio plugins (press Rescan) and Devices. Help › Check for updates.
File › Video settings…: 1280×720 at 60 fps, Apply, then back to 1920×1080 at 30 fps.
Switch Blending between Browser-style and Linear light on a scene with overlapping
blended items.
**Pass:** every page opens; the scan lists your CLAP plugins; the update check says
you're up to date; the canvas changes size and rate; the blending switch changes how the
overlaps mix.

**M5. Save and load projects (#1218).** File › New project, add a scene with a text
source, File › Save As to `Documents\alpha5-test.json`. File › New project again, then
File › Open recent › alpha5-test. Quit `jstream.exe` and start it again.
**Pass:** the scene comes back on open, and alpha5-test is still in Open recent after the
restart.

**M6. UI scrub (#1219, #1198, #1202, #1204, #1206).** Select an item and use the
Inspector's tabs and its Properties & filters button. Drag that dialog by its header.
Rotate an item. Use a Browse… button on a file field. Look at the Outputs panel.
**Pass:** Properties and Filters are one dialog; dialogs drag by their header; the
item's name turns with its box; Browse… opens a picker that fills the field; the Outputs
panel is a compact list with each output's settings in a dialog.

**M7. Snapshots on Windows (#1184, #1194, #1215).** In a scene with a browser source and
a display capture, choose Screenshot (scene) on its row and open the PNG.
**Pass:** the PNG shows the page and the screen capture, not black boxes.

**M8. Follow pointer, the 3D camera and hotkeys (#1225, #1216, #1214, #1188, #1346).**
Add the Follow pointer filter to a display capture item and move the mouse to each edge.
Edit the scene's 3D camera in Scene properties. Bind a hotkey to a scene and Ctrl+WheelUp
to zoom, then click into Notepad and use both.
**Pass:** the pointer stays in view with a margin; the 3D camera section edits the
camera; the hotkey and the wheel chord work while Notepad has the focus, one zoom step a
notch.

**M9. Small fixes (#1209, #1189).** Switch scenes with the Cut preset. Make a new project
and look at File › Video settings….
**Pass:** Cut is an instant cut; a new project is 30 fps, with 60 fps offered.

**M10. Filters (#1226, #1227, #1232, #1295).** On an opaque image: Glow, then Bloom, then
the Vignette. Then a Border, its 9-slice image frame, and a pulse from audio. Add a Custom
shader filter, edit it in its editor and Save as pack.
**Pass:** Glow shows past the item's box; Bloom and the Vignette are easy to tune; the
borders draw; the shader edits, saves as a pack and loads again from it.

**M11. Page API audio (#1228, #1060).** Add a browser source with a visualiser page that
reads `jstream.levels`, tick its audio levels checkbox, and play sound or speak. Then
untick it.
**Pass:** the page moves with the sound and shows the reading badge; unticked, it gets no
levels.

**M12. Webcam Properties (#1195, #1196).** A webcam source's Properties: pick the
device, a resolution, a frame rate and a format, then move a camera control (brightness
or white balance).
**Pass:** each choice takes effect in the picture, and the control changes it live.

**M13. Ask before a new agent connects (#1229).** Close
`jstream.exe` first; the script starts it again with `--mcp`, then sends an agent's
hello to `127.0.0.1:4455` twice. Choose Deny the first time and Always allow the second.
Then open File › Settings… › Devices.
**Pass:** "Agent wants to connect" names `release-pass-agent`; Deny turns it away;
Always allow lets it in; it is listed under Approved agents, and Remove takes it off.

**M14. MIDI and network audio (#1222, #1223, #1224).** With a MIDI keyboard, add a MIDI
piano roll and a MIDI keyboard source and play. Bind a note to a scene switch. Send a
MIDI clock from the DAW and set a motion's speed in beats. With the JTunnel plugin in the
DAW, turn on File › Settings… › Devices › Receive from JTunnel and add a Network audio
source.
**Pass:** the MIDI sources draw what you play; the note switches scenes; the motion
follows the DAW's tempo; the network audio source's meter moves with the DAW, with no
dropouts in a minute.

**M15. PTZ cameras over VISCA (#1182).** With a VISCA-over-IP camera: add it, drive it,
store two presets, and recall a preset in each of two keyframes.
**Pass:** the camera moves, stores and recalls presets, and a switch between the
keyframes moves it to each one's preset. Without a camera, skip it and say so on #1182.

**M16. A real PTZ controller drives a scene camera (#1181).** Pass `-PtzController
<its IP>`, or type it when asked. The script starts a daemon whose endpoint listens on
this PC's address on the controller's network, port 52381 (VISCA over IP, camera address
1), answering only that IP, with a scene camera on program. Point the controller at that
address, pan right, tilt up and zoom in, then press Home, and store and recall a preset.
**Pass:** the endpoint's `answered` counter in `profile.get` is above 0; the scene
camera's aim moved; the program picture changed between the before and after snapshots;
Home goes back to the stored camera and the preset comes back. Packets counted as
`unallowed` mean the controller sends from another address.

<a id="setup"></a>
## Setup
<!-- @section
-->

- **Machine:** Windows 11 with a DX12 GPU that has hardware H.264, HEVC and AV1
  encoders (the Radeon 780M used so far qualifies), a display, a microphone, speakers
  and an app that lists webcams (the Camera app, or
  `ffmpeg -list_devices true -f dshow`).
- **Toolchain:** the MSVC Rust toolchain. Items that build CEF also need an
  **x64 Native Tools Command Prompt for VS** with CMake and Ninja on `PATH`, as
  `spikes/s4-browser/run-s4.ps1` sets up.
- **Administrator rights** for the virtual camera registration items (one UAC prompt
  each).
- **Builds, in this order:** `main` (the [On main](#main) items and the
  [wave/m3-2](#m3-2) item, which landed with it), then `wave/m1-3` and each open PR's
  branch in [On wave/m1-3 and open PR branches](#prs).
- **Tools** for some items: ffmpeg and ffprobe on `PATH`, `dxcap` from the Graphics Tools
  optional feature, trunk and the wasm-bindgen CLI, a webcam with a microphone, and a
  CLAP effect in `C:\Program Files\Common Files\CLAP`.

<a id="moot"></a>
## Already done: close without a run
<!-- @section
-->

These issues already passed on hardware and were left open only until their PR merged.
Those PRs have since merged and landed in `main`, and the issues are closed, each with
a pointer to its pass comment:

| Issue | Passed on | PR, now in `main` |
|---|---|---|
| #366 (F6 HEVC and AV1 encode) | all four `hw-tests` | #364 |
| #378 (C3 display and window sources) | both `windows_gpu` tests | #365 |
| #390 (G3d virtual camera in the UI and desktop app) | every step | #316 |

Not in this pass: #333 (A3 on a Windows 11 Pro **N** VM). The maintainer decided not to
set up an N-edition VM, so the issue stays open to keep the gap on record.

<a id="main"></a>
## On main
<!-- @section
-->

Build `main` in release once: `cargo build --release -p jstream-daemon -p jstream-cli`.

### 1. Render pixel tests on the hardware adapter (#336, passed)

This passed on `main`, and #336 is closed. Run it again only after a change to the
render tests or their adapter choice.

Steps 2 to 4 of #336 passed. Step 1 ran on WARP, and then on the 780M with #437, but on
a branch without C8b5's three premultiplied-import tests. Both #326 and #437 are on
`main` now.

1. `cargo test -p jstream-render -- --nocapture`
2. **Pass:** every `jstream-render test adapter:` line names the hardware adapter (on
   the 780M, wgpu picks Vulkan), and every test passes, C8b5's three
   premultiplied-import tests included.

### 2. Browser page check, refused requests (#341)

#341 passed apart from one difference: `requests refused` was 2 where the built-in page
expects 1, and nothing named the second request (perhaps an implicit `favicon.ico`).

1. From the Native Tools prompt:
   ```bat
   cargo build --release -p jstream-browser-helper --features cef
   cargo run --release -p jstream-browser --features cef,helper --example page_check > page-check.txt
   ```
2. **Pass:** about 60 frames a second at 1920x1080, left ≈ 440 Hz and right ≈ 660 Hz
   moving to ≈ 880 Hz after the CSS change, 0 frames lost, the helper exits well under
   10 s, and the renderer processes run at Low, Untrusted or AppContainer integrity.
3. **Closes #341** if `requests refused: 1`. If it is still 2, post the count and leave
   the issue open: naming the extra request needs `page_check` to print the refused
   URLs, which is a code change.

### 3. YouTube without OAuth, live (part of #337)

Part A's steps 1 and 2, and a local-ingest stand-in for steps 2 to 4, passed. What is
left needs a real YouTube channel. Part B waits for JStream's built-in OAuth client
(ADR-0058), so this item doesn't close #337.

1. In YouTube Studio, turn on **auto-start** and **auto-stop** for the stream, and copy
   its ingest URL and stream key.
2. Start `target\release\jstream-daemon.exe --token-file %TEMP%\jstream-token`. Add an
   RTMPS output with that URL, set its key from `cmd` (not Windows PowerShell 5.1; see
   #438) with `echo <key>| jstream-cli … output set-key <id>`, and start it.
   **Pass:** within a minute Studio shows the stream receiving data, and the broadcast
   goes live by itself.
3. Stop the output. **Pass:** Studio ends the broadcast by itself.
4. Search `%APPDATA%\JStream\logs` and `audit.jsonl` for the key. **Pass:** it never
   appears.

### 4. Looping and MP3 media sound (#445)

#382 is closed: on `main` every file below passed but `audio.mp3`, which still leaves a
56 ms gap at each loop wrap and a dropout of about 12 ms in each pass. That residual is
#445; rerun this item once its fix lands.

Everything else in #382 passed. Its remaining steps failed on #445 (a gap at each loop
wrap) and #446 (an MP3 playing about 0.2 s of every second). #448 and #447 fixed them,
and both landed in `main` with `wave/m2` (#470). Use the same test files as #382's
re-run: `audio.m4a` (AAC mono 44.1 kHz, a 5 s sine), `audio.mp3` (the same tone as
MP3), `longsound.mp4` (3 s of video, 8 s of AAC) and `surround51.mp4` (H.264 with 5.1
AAC, 6 s).

1. Make one `media` source with `loop: true` alone in the program, and record 12 s for
   each file.
2. **Pass, with each file:** no silence below -40 dB that lasts 20 ms or more
   (`ffmpeg -i <recording> -af silencedetect=n=-40dB:d=0.02 -f null -` finds none).
   - `audio.m4a`: no hole at either wrap (#445 had 39 ms at each).
   - `audio.mp3`: continuous sound at about the same level as `audio.m4a` (#446 had
     -28 dBFS against -21).
   - `longsound.mp4`: the sound plays on under the last frame, with no blip (#445 had
     25 ms).
   - `surround51.mp4`: no holes at the wrap (#445 had two), still mixed down to stereo.
3. With a 1 s flash-and-beep sync clip looping: **Pass:** flashes in the recording stay
   1.000 s apart across the wrap (#445 had a 1.067 s step).

### 11. Go-live smoke test and the graceful stop (go-live runbook)

1. `scripts\smoke.ps1 -Release`
   ([Go-live › Smoke script](go-live.md#smoke)). **Pass:** it ends `SMOKE PASSED`.
2. Start the daemon, then press Ctrl-C ([Go-live](go-live.md#steps) step 7).
   **Pass:** it prints `stopped` and exits 0.

### 12. Canvas H.264 preview (#380)

Build the UI with trunk and run the desktop app, as in
[jstream-desktop](../../apps/jstream-desktop/README.md), with a text, image or media
source on the program scene (not a display source: the preview draws display capture as
nothing, by design). **Pass:** the preview shows under the canvas boxes with H.264 in the frames; it
follows committed edits and scene switches; after a slow moment it recovers at the next
keyframe, within 2 s; and a decoder failure falls back to JPEG, not "No preview".

### 18. Audio sources on real devices (closes #778)

C5's audio input, desktop audio and application audio sit on the WASAPI backend, whose
device tests need a microphone and speakers. On `main`:
`cargo test --release -p jstream-platform-windows --features hw-tests -- --ignored audio::device_tests --test-threads=1`.
The last test plays a quiet 440 Hz tone. **Pass:** all five pass: devices with one default
each, the input format, this process's loopback, the default input on the media clock, and
the default output's loopback.

### 21. Thread CPU in `profile.get` (closes #1089, PR #1087)

Run the daemon with a scene on the program for a few minutes, then
`jstream-cli profile get 10`. **Pass:** `threads` lists JStream's threads
(`jstream-render`, `jstream-preview`, `jstream-sources`, …) under `name`, their
`cpu_pct` adding up to about Task Manager's figure for the process; the render thread's
share grows with a heavier scene; and over ten minutes the daemon's handle count (Task
Manager › Details › Handles) stays flat.

<a id="m3-2"></a>
## From wave/m3-2, now on main
<!-- @section
-->

### 5. NV12 encode targets into Media Foundation (closes #492)

E3d (#280) merged into `wave/m3-2`, which has landed in `main`. Build `main`.

1. `cargo test -p jstream-render -- convert`. **Pass:** the native NV12 plane-view test
   runs (not skipped) and passes.
2. `cargo test -p jstream-platform-windows --features hw-tests -- --ignored`.
   **Pass:** every encode-target and encoder test passes.
3. Start a recording output with the release daemon.
   **Pass:** the log says `the video encoder takes GPU frames`; `OutputStats::readbacks`
   stays at 0 while no virtual camera runs; the file plays, with BT.709 limited-range
   colours and no out-of-order or stale frames.
4. Optional: make the encoder fall behind (a high bit rate on a weak GPU).
   **Pass:** `encode_frames` drops rise, and no frame repeats out of order.

Post the GPU, the driver version and the results on #492.

<a id="prs"></a>
## On wave/m1-3 and open PR branches
<!-- @section
-->

Items 6, 7, 9, 10, 13 and 14 run from `wave/m1-3`, where their PRs have merged; it hasn't
landed in `main`. The others run a PR that hasn't merged. Use the PR's branch, or its
wave once it has merged.

### 6. Virtual camera owner and DACL checks (#546, PR #540)

1. On `wave/m1-3`, run the `vcam_smoke` check in
   [jstream-vcam-windows › Hardware check](../../crates/jstream-vcam-windows/README.md#hardware-check).
   **Pass:** the camera shows the producer's frames, not the colour bars, and `vcam.log`
   has no `OpenSharedHandleByName` or owner failure. The same report answers G2's pacing
   rerun (closes #777): `samples per producer frame` is above 0 and at most about 1
   (1.05), since the DLL paces samples to 30 fps.
2. Restart the producer while Frame Server still holds the old control block.
   **Pass:** it starts (Windows' read-back DACL passes `security::check`).
3. Optional: pre-create `Local\JStream-VCam-Control` from another account, or with an
   `Everyone` grant. **Pass:** `start` fails with `InvalidState`.

### 7. User-only token, devices and log files (#546, PR #548)

On `wave/m1-3`:

1. `jstream-daemon --token-file C:\Users\Public\jstream-token`. **Pass:** `icacls` on the
   new token file lists only SYSTEM and you, with no inherited `(I)` entries. The same
   holds for `.devices` after a pairing, and for the log files.
2. `icacls <token> /grant *S-1-1-0:R`, then restart. **Pass:** the daemon exits 1 with
   "may be accessible to other users".
3. A token file that already exists under `%APPDATA%\JStream`, with SYSTEM,
   Administrators and you inherited. **Pass:** it is still accepted.
4. If one is at hand, repeat step 1 with a domain or Microsoft account and with the
   built-in Administrator.

### 8. User-only audit log and collection file (#546, PR #574)

On #574's branch:

1. `cargo test -p jstream-core --lib private_file`. **Pass:**
   `new_files_pass_the_private_file_check` passes.
2. Start the daemon with `--collection <file>` in a fresh data and log folder, and make
   one edit so the collection saves. **Pass:** `icacls <logs>\audit.jsonl` and
   `icacls <collection file>` list only SYSTEM and you, with no `(I)` entries.
3. `icacls audit.jsonl /grant Users:R`, then restart. **Pass:** the daemon exits 1 with
   an error naming the file and the foreign grant.
4. Let `audit.jsonl` rotate past 8 MiB. **Pass:** the new `audit.jsonl` is private too.

Items 6 to 8 together close #546.

### 9. Opt-in virtual camera registration from a Program Files copy (closes #563, PR #565)

On `wave/m1-3`. Use a built portable folder (`cargo xtask dist`, or the release `jstream-daemon.exe`
with `jstream_vcam_windows.dll` beside it) in a user-writable folder such as
`Downloads`.

1. Start `jstream-daemon` normally. **Pass:** no UAC prompt, and an `info` line says
   registering is optional.
2. `jstream-daemon --register-vcam` from a normal prompt. **Pass:** one UAC prompt, then
   `virtual camera: registered C:\Program Files\JStream\vcam\jstream_vcam_windows.dll, a copy of …`.
   - `icacls` on that folder and that DLL lists only SYSTEM (F), Administrators (F),
     LOCAL SERVICE (RX) and Users (RX), with no `(I)` entries.
   - `reg query "HKLM\Software\Classes\CLSID\{…}\InprocServer32"` names the copy.
   - Running it again prints `already registered`, with no prompt.
3. Start a `virtual_cam` output and pick "JStream Virtual Camera" in the Camera app.
   **Pass:** the frames show, and Process Explorer shows Frame Server's `svchost` loaded
   the Program Files copy, not the portable folder's DLL.
4. Replace the portable folder's DLL with a different build. **Pass:** startup warns
   the copy differs, and `--register-vcam` replaces it. With the camera open, the old
   copy is renamed `.old0` and removed by a later step.
5. `regsvr32` the portable folder's DLL. **Pass:** startup warns it is registered
   outside JStream's Program Files folder.
6. `jstream-daemon --unregister-vcam`. **Pass:** one prompt, then the CLSID key and
   `C:\Program Files\JStream` are gone. With the camera open, it says to close the
   camera's apps and run it again, and the second run finishes.
7. Decline the UAC prompt. **Pass:** it exits 1 with "the administrator prompt was
   declined".

### 10. Mid-run GPU capture fallback (closes #576, PR #569)

On `wave/m1-3`, where #569 and #580 (its deadline and panic fix) have merged, with a
display source on program and the screen changing:

1. Start a recording output and confirm `gpu_captures` rises in `stats.tick`.
2. Force the GPU path to break mid-run, for example with a driver reset from the
   Graphics Tools optional feature (`dxcap -forcetdr`).
3. **Pass:**
   - the output stays `active`;
   - one `warn` names the fallback, `OutputStats::gpu_fallbacks` is 1, and
     `gpu_captures` stops rising while readbacks keep rising;
   - WGC starts a CPU session on the same monitor while the GPU session is still
     alive;
   - the recording plays through the switch;
   - no device-removed errors follow the switch, and the D3D12 context drops after the
     imported textures when the output stops, with no leaks (Task Manager's GPU memory
     back to its level before the run).

### 13. Crash reports on Windows (#557, and #662 on #546)

On `wave/m1-3`: `cargo test -p jstream-crash` and `cargo test -p jstream-desktop`.
**Pass:** both pass. A report from a real panic (redacted, and user-only per #662) needs a
way to make the daemon or `jstream.exe` panic on purpose, which neither has yet.

### 14. Device-loss recovery under a forced TDR (closes #620, PR #604)

On `wave/m1-3`, in the same run as item 10:

1. After the first `dxcap -forcetdr`. **Pass:** no panic; the output stays `active`; one
   `warn` names the loss and its reason; WGC carries on with a CPU session.
2. Force three TDRs within a minute. **Pass:** the third fails the output with the loss
   count in its error, and the daemon keeps running.

### 15. MJPEG webcam and its paired microphone (#673, #726)

On #726's branch (stacked on `wave/m1-3`, so #658's MJPEG path is in it), with a webcam
that has a microphone. Close every app that holds the webcam first (the Camera app, Teams,
Zoom, OBS): a busy webcam fails the device tests with `Busy` and records black.

1. `cargo test -p jstream-platform-windows --features hw-tests -- --ignored video_device --test-threads=1`
   (one at a time: the tests open the same camera).
   **Pass:** the device tests pass, the largest MJPEG format included.
2. A `webcam` source on program, recorded for 10 s. **Pass:** the picture has the right
   colours, and the webcam's own microphone is in the recording.
3. Optional: with `JSTREAM_UNPLUG_TEST=1`, the unplug test. **Pass:** it passes.

A corrupt or truncated MJPEG frame (dropped, counted in `frames_undecodable`, the stream
goes on) isn't forced here.

### 16. CursorCapture::Hidden (part of #690, PR #685)

On #685's branch: `cargo test -p jstream-platform-windows`. **Pass:** it passes,
`only_a_hidden_cursor_is_left_out` included. The captures with the pointer drawn, hidden
and as metadata wait for the daemon's `show_cursor` setting (#682).

### 17. CLAP plugin host with the unnamed shared block (PR #573)

On #573's branch, #342's steps again: `jstream-plugin-host scan` on an installed `.clap`
exits 0 and names it, and on `kernel32.dll` exits 1; the daemon logs
`scanned the CLAP plugins` with `found` of at least 1; `kind.list` has `clap/<id>` and
`kind.describe` lists its parameters; no `jstream-plugin-host.exe` is left after the scan
or after Ctrl-C.

### 19. Help and About links open the default browser (closes #1084, PR #1083)

On #1083's branch, with the UI built (`trunk build --release` in `crates/jstream-ui`) and
`jstream.exe --ui-dir crates\jstream-ui\dist`:

1. Choose Help › Report an issue. **Pass:** the default browser opens
   `https://github.com/jeffmhopkins/JStream/issues/new`; the JStream window stays on the
   app, and no second JStream window opens.
2. Choose Help › About JStream, then click Source code on GitHub and MIT. **Pass:** each
   opens in the browser the same way, and the dialog shows the icon and the version.
3. **Pass:** the log has no `a new window refused`, `navigation away from the UI
   refused` or `could not open a link in the browser` line for these clicks.

### 20. Effect-pack writes on NTFS (closes #1088, PR #1085)

By hand: the script doesn't run this one yet. On #1085's branch, start the daemon with
`--effects-dir` set to an empty folder and connect a client, for example
`jstream-cli mcp` or the UI's devtools.

1. Send `pack.write` for a pack `tint`: an `effect.toml` naming `id = "tint"` and its
   `.wgsl`. It answers with `loaded: true`, and `kind.list` has `tint/…` straight away.
2. Open the `.wgsl` in an editor that holds it open (Notepad++ does), or start a
   Defender custom scan of the folder. While that is running, write `tint` again with
   another shader. It succeeds within about a second, or answers `internal`, and the old
   version stays loaded. Close the editor and check that `.trash` and `.staging` are
   empty or gone after the next write.
3. Rename the folder to `Tint` by hand, then write `tint`. It replaces the folder in
   place, and `pack.list` shows one pack.
4. `pack.write` with pack `con`, and with files `nul.png` and `com1/x.wgsl`: each is
   `invalid_argument`, and nothing appears in the folder.
5. Kill the daemon while a write is running, then start it again. `tint` loads as one of
   its versions, and `.staging` and `.deleted` are gone.

Pass: every step does as described. Report on #1088.

<a id="waiting"></a>
## Waiting on a fix
<!-- @section
-->

These reruns can't pass until an open bug is fixed. Run them in the next pass after the
fix lands:

- **#436 (#335's step 5, C8b4 daemon browser sources):** Ctrl-C with CEF running should print
  `stopped` and write the shutdown log. It waits on #436. Steps 1 to 4 passed.
