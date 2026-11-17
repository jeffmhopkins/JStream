<!-- @doc
id = "RUNBOOK-AGENT-PRE-PASS"
title = "The agent's pre-pass on a test machine"
kind = "guide"
status = "active"
review_every = "180d"
-->

# The agent's pre-pass on a test machine

Before the maintainer runs a hardware checklist by hand, an agent signs in to the test
machine and runs every check that needs no one: it installs or unpacks the build,
checks the install, and runs the release pass's automatic checks, the bench, a stream
and a recording, and the hardware tests. It then writes one report that marks each
checklist item **AUTO-PASS**, **AUTO-FAIL** or **NEEDS PERSON**, so the maintainer's own
pass is only the items that take eyes, ears or hands.

The checklists are the issues labelled `windows-hardware`, `linux-hardware` and
`macos-hardware`: the Windows one (#1355) with the release's own checklist in
[windows-hardware-pass › alpha.5](windows-hardware-pass.md#alpha5), the Linux one (#1357)
and the Mac one (#1356).

<a id="session"></a>
## The session
<!-- @section
-->

The agent works in a Claude Code Remote Control session on the test machine, started
by the maintainer, in a JStream checkout (a `git clone` of the repository, on `main`).
The session runs as the maintainer's own user, with their desktop session, so it can do
anything they could do in a terminal there, and nothing that asks for a password.

Rules for the agent:

- **No elevation.** Never run `sudo`, an elevated shell or anything that raises a UAC
  prompt: nobody is there to answer it, and a step that needs it is a NEEDS PERSON item.
  Missing tools (ffmpeg, vainfo, the Vulkan validation layer) are named in the report,
  not installed.
- **Nothing of the maintainer's.** The scripts give every daemon its own port, token,
  logs and data folder, so the maintainer's settings, projects, keys and keyring are
  never read or changed. Don't run the maintainer's own JStream, don't read a stream key,
  and never stream anywhere but the scripts' receiver on `127.0.0.1`.
- **Say what will appear on screen.** Some checks open windows or capture the screen
  (the bench's display scenario, the AppImage launch). Tell the maintainer before they
  run, since they may be at the machine.
- **Screenshots are private.** Check the run's files before anything leaves the machine.
  The Windows release pass's zip holds `snapshot-display.png`, a capture of the whole
  desktop, and the Linux pass keeps its desktop captures out of its zip unless
  `--zip-screenshots` is given. Say which files show the screen, and share the zip only
  once the maintainer agrees, or without those files.
- **Leave nothing running.** Each script stops its daemons, removes its temporary files
  and says what it left behind; the agent checks that line.

The agent ends with one reply: the checklist table from the report, the failures with
their details, the NEEDS PERSON items as the maintainer's to-do list, and where the report
and zip are. A failure gets its own issue, linked from its checklist line, as the
checklists ask.

<a id="windows"></a>
## Windows
<!-- @section
depends_on = ["docs/runbooks/windows-hardware-pass.md#release-pass", "docs/runbooks/windows-hardware-pass.md#script"]
-->

1. **Get the release.** Download the release zip and its `.sha256` from the release page
   into `Downloads` (`Invoke-WebRequest` with the assets' URLs), or use the `dist`
   artifact of the commit being released.
2. **The release pass's automatic checks.** From the checkout:
   `scripts\release-pass.bat -AutomatedOnly -Zip <the zip> -Expect <version>`
   ([The release pass](windows-hardware-pass.md#release-pass)). It asks nothing. It
   checks the zip, the package, the versions, the kinds, the profile, the snapshots, the
   projects, the webcams and the bench, and stops the daemon gracefully. The bench takes
   about seven minutes and captures the screen.
3. **The other unattended checks,** when the run is for #1355's older items:
   `scripts\windows-pass.bat -Unattended` ([The script](windows-hardware-pass.md#script)).
   It builds from source (Rust and git must be installed), declines every step that needs
   administrator rights or a person, and records each as skipped.
4. **The report.** Each script prints its `report.md` and zip. Mark the release's
   checklist from them: an automatic check's FAIL is AUTO-FAIL on the targets it covers
   (the Automated table in [alpha.5](windows-hardware-pass.md#alpha5) maps them); each
   manual item M1 to M16 is NEEDS PERSON, with the automatic checks it builds on noted;
   the targets whose only checks are automatic and passed are AUTO-PASS.

<a id="linux"></a>
## Linux
<!-- @section
covers = ["scripts/release-pass.py"]
depends_on = ["docs/runbooks/linux-testing.md#script"]
-->

`scripts/release-pass.py` is the Linux release pass. It needs Python 3.8 or later and
nothing to build for the package checks; ffmpeg and ffprobe for the stream, recording
and media checks, and vainfo for the VA-API check (each skipped, and named, without
them); and cargo, with `--checkout`, for the hardware tests.

1. **Get the release.** Download the `.deb`, the AppImage and their checksums from the
   release page into `~/Downloads` (`gh release download`, or `curl -LO` with the assets'
   URLs).
2. **Run it:**
   `python3 scripts/release-pass.py --deb <the .deb> --appimage <the AppImage> --checkout . --launch`.
   It takes about twenty minutes the first time, most of it building the hardware tests.
   `--list` prints the checklist and the checks behind each line.
3. **Read the report** (`report.md` in the run folder,
   `~/.local/share/jstream-release-pass/runs/<time>`). Its first table is #1357's
   checklist, each line AUTO-PASS, AUTO-FAIL or NEEDS PERSON with the reason.

What it checks, in order:

| Check | What passes |
|---|---|
| VA-API H.264 encode | `vainfo` lists an H.264 `VAEntrypointEncSlice` (HEVC is noted) |
| deb checksum, AppImage checksum | the SHA-256 matches the `.sha256` file or `SHA256SUMS` beside it; none is a warning |
| deb control | the package is `jstream` and depends on WebKitGTK 4.1 |
| deb chrome-sandbox setuid root | the package lists `browser/chrome-sandbox` as `-rws` and `root/root` |
| deb files, menu entry, AAC notice | the unpacked package has the programs, UI, browser helper and sandbox in `/usr/lib/jstream`, links in `/usr/bin`, the menu entry and icon, and `/usr/share/doc/jstream/NOTICE.txt` naming AAC |
| installed … | the same, for the installed package when `dpkg` lists one, and its `chrome-sandbox` is setuid root on disk; not installed is skipped, since installing needs `sudo` |
| AppImage contents | it unpacks (`--appimage-extract`) and holds `jstream`, the UI, the browser helper and WebKitGTK's processes, and no `chrome-sandbox` |
| AppImage starts | with `--launch`: it runs for 25 seconds, with its own data and config folders, and starts WebKitGTK's processes; it opens its window on the desktop |
| versions | `jstream-daemon --version` and `jstream-cli version` name the package's version (or `--expect`) |
| daemon starts, kind.list, profile.get | as [the Windows release pass](windows-hardware-pass.md#release-pass), on the package's programs (`--installed` runs the installed ones, `--root` a folder of them) |
| snapshot of a browser page, of a media file | a green page snapshots green, and a red clip ffmpeg makes snapshots red |
| snapshot of a display capture | only with `--display`: on Wayland it shows the screen picker, so someone must be there |
| save, new, open; webcam formats; bench | as the Windows release pass; the bench runs `all` (and `display` with `--display`, `webcam` with a webcam) |
| going live on VA-API | a second daemon, without the session bus so its test key goes to a file in the run folder, streams 1080p for ten seconds on an encoder profile that requires hardware to ffmpeg listening on `127.0.0.1`; the output used a hardware encoder and the receiver got H.264 at 1920×1080 and AAC |
| recording to a file, quitting stops outputs | it records for eight seconds, then SIGINT stops the daemon with the recording running: the daemon prints `stopped` and exits 0, and the MP4 is H.264 and AAC and decodes without an error |
| hw: … | with `--checkout`: `cargo test -p jstream-platform-linux --features hw-tests -- --ignored` (VA-API, V4L2, PipeWire, ALSA, D-Bus, the Secret Service), one row per test; the portal tests (they show the picker) and the X11 hotkey test are left out |
| hw+validation: … | the VA-API tests again with Vulkan's validation layer, when it is installed |
| e2e-stream on OpenH264 | with `--checkout`: `cargo xtask e2e-stream --openh264-download`, the receiver harness on Cisco's library |

The report and `results.json` go in the run folder, and the zip beside it holds the text
files and the snapshots, with every token and the test key replaced by `***`. Token files
and the daemons' data folders stay out, and so do the desktop captures unless
`--zip-screenshots` is given. It exits 1 when a check failed.

<a id="mac"></a>
## Mac
<!-- @section
-->

There is no Mac release pass script yet (#1356). By hand, the agent can check the
download's SHA-256 (`shasum -a 256`), mount the disk image (`hdiutil attach -nobrowse`)
and check the app's contents and signature (`codesign -dv`), run the bundled
`jstream-daemon --version`, and run the receiver harness from a checkout. Everything
behind a macOS permission prompt (Screen Recording, Camera, Microphone) and the
Gatekeeper first open are NEEDS PERSON.
