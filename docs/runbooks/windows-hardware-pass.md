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
  files are not changed. `-NoBuild` reuses the worktrees as they are. The source table at
  the top of the script is the one place to change when a PR merges.
- **What it asks first.** Every step that changes the machine asks before it runs, and
  a no is recorded as skipped: anything run as administrator (each is one UAC prompt),
  registering or unregistering the virtual camera, a forced GPU reset, installing a tool
  with `cargo install`, and reading the stream key file. The files it creates under
  `%PUBLIC%` for the ACL checks are removed when the check ends. `-Unattended` declines
  all of these and skips every question.
- **Questions.** A check only a person can judge (the Camera app shows the frames, Studio
  goes live) asks pass, fail or skip where it happens.
- **Daemons.** Each daemon it starts has its own token file and log folder in the run
  folder, drives its checks over MCP as the smoke script does, and is stopped at the end
  of its check. The graceful stop sends Ctrl-C from a helper process that attaches to the
  daemon's console.
- **Report.** The run folder (`%LOCALAPPDATA%\JStream-windows-pass\runs\<time>`, or
  `-OutDir`) gets `report.md`, `results.json`, every step's log and the recordings. The zip
  beside it holds the text files only, with the run's tokens replaced by `***`; token
  files, key files and recordings stay out. The script ends by printing the zip's path
  and a summary, one line per check, to paste back with it.
- **Not checked by the script:** what isn't visible outside the engine, such as
  `OutputStats::readbacks` and `gpu_fallbacks` (it checks the log lines that report the
  same events), and the items the report lists under "Not in this pass".

| Check | Item | Source |
|---|---|---|
| `smoke` | [11](#main) | main |
| `render` | [1](#main) | main |
| `nv12` | [5](#m3-2) | main |
| `media` | [4](#main) | main |
| `browser` | [2](#main) | main |
| `youtube` | [3](#main) | main |
| `preview` | [12](#main) | main |
| `vcam-owner` | [6](#prs) | wave/m1-3 |
| `token-acl` | [7](#prs) | wave/m1-3 |
| `crash` | [13](#prs) | wave/m1-3 |
| `vcam-register` | [9](#prs) | wave/m1-3 |
| `gpu-reset` | [10 and 14](#prs) | wave/m1-3 |
| `private-files` | [8](#prs) | PR #574 |
| `webcam` | [15](#prs) | PR #726 |
| `cursor` | [16](#prs) | PR #685 |
| `clap` | [17](#prs) | PR #573 |

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

### 1. Render pixel tests on the hardware adapter (closes #336)

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

### 4. Looping and MP3 media sound (closes #382)

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
[jstream-desktop](../../apps/jstream-desktop/README.md), with a display source in a
scene. **Pass:** the preview shows under the canvas boxes with H.264 in the frames; it
follows committed edits and scene switches; after a slow moment it recovers at the next
keyframe, within 2 s; and a decoder failure falls back to JPEG, not "No preview".

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
   has no `OpenSharedHandleByName` or owner failure.
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
that has a microphone:

1. `cargo test -p jstream-platform-windows --features hw-tests -- --ignored video_device`.
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

<a id="waiting"></a>
## Waiting on a fix
<!-- @section
-->

These reruns can't pass until an open bug is fixed. Run them in the next pass after the
fix lands:

- **#335, step 5 (C8b4 daemon browser sources):** Ctrl-C with CEF running should print
  `stopped` and write the shutdown log. It waits on #436. Steps 1 to 4 passed.
