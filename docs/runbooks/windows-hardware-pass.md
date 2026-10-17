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
- **Builds, in this order:** `main` (the [On main](#main) items), `wave/m3-2` (the
  [On wave/m3-2](#m3-2) item), then each open PR's branch in
  [On open PR branches](#prs).

<a id="moot"></a>
## Already done: close without a run
<!-- @section
-->

These issues already passed on hardware and were left open only until their PR merged.
Those PRs have since merged and landed in `main`, so the issue can be closed with a
pointer to its pass comment:

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

<a id="m3-2"></a>
## On wave/m3-2
<!-- @section
-->

### 4. NV12 encode targets into Media Foundation (closes #492)

E3d (#280) merged into `wave/m3-2`, which hasn't landed in `main`. Build that wave.

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
## On open PR branches
<!-- @section
-->

Each of these runs a PR that hasn't merged. Use the PR's branch, or its wave once it
has merged.

### 5. Virtual camera owner and DACL checks (#546, PR #540)

1. On #540's branch, run the `vcam_smoke` check in
   [jstream-vcam-windows › Hardware check](../../crates/jstream-vcam-windows/README.md#hardware-check).
   **Pass:** the camera shows the producer's frames, not the colour bars, and `vcam.log`
   has no `OpenSharedHandleByName` or owner failure.
2. Restart the producer while Frame Server still holds the old control block.
   **Pass:** it starts (Windows' read-back DACL passes `security::check`).
3. Optional: pre-create `Local\JStream-VCam-Control` from another account, or with an
   `Everyone` grant. **Pass:** `start` fails with `InvalidState`.

### 6. User-only token, devices and log files (#546, PR #548)

On #548's branch (stacked on #540):

1. `jstream-daemon --token-file C:\Users\Public\jstream-token`. **Pass:** `icacls` on the
   new token file lists only SYSTEM and you, with no inherited `(I)` entries. The same
   holds for `.devices` after a pairing, and for the log files.
2. `icacls <token> /grant *S-1-1-0:R`, then restart. **Pass:** the daemon exits 1 with
   "may be accessible to other users".
3. A token file that already exists under `%APPDATA%\JStream`, with SYSTEM,
   Administrators and you inherited. **Pass:** it is still accepted.
4. If one is at hand, repeat step 1 with a domain or Microsoft account and with the
   built-in Administrator.

### 7. User-only audit log and collection file (#546, PR #574)

On #574's branch:

1. `cargo test -p jstream-core --lib private_file`. **Pass:**
   `new_files_pass_the_private_file_check` passes.
2. Start the daemon with `--collection <file>` in a fresh data and log folder, and make
   one edit so the collection saves. **Pass:** `icacls <logs>\audit.jsonl` and
   `icacls <collection file>` list only SYSTEM and you, with no `(I)` entries.
3. `icacls audit.jsonl /grant Users:R`, then restart. **Pass:** the daemon exits 1 with
   an error naming the file and the foreign grant.
4. Let `audit.jsonl` rotate past 8 MiB. **Pass:** the new `audit.jsonl` is private too.

Items 5 to 7 together close #546.

### 8. Opt-in virtual camera registration from a Program Files copy (closes #563, PR #565)

Use a built portable folder (`cargo xtask dist`, or the release `jstream-daemon.exe`
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

### 9. Mid-run GPU capture fallback (closes #576, PR #569)

On #569's branch (#580, its deadline and panic fix, is stacked on it), with a display
source on program and the screen changing:

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

<a id="waiting"></a>
## Waiting on a fix
<!-- @section
-->

These reruns can't pass until an open bug is fixed. Run them in the next pass after the
fix lands:

- **#382, steps 3a to 3d (C9b media sound):** a looping audio-only file with no gap at
  the wrap, sound playing on under the last frame with no blip, and no picture skip at
  each wrap wait on #445. MP3 playback waits on #446. Everything else in #382 passed.
- **#335, step 5 (C8b4 daemon browser sources):** Ctrl-C with CEF running should print
  `stopped` and write the shutdown log. It waits on #436. Steps 1 to 4 passed.
