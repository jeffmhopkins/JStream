<!-- @doc
id = "RUNBOOK-MAC-TESTING"
title = "Testing JStream on a Mac"
kind = "guide"
status = "active"
review_every = "180d"
-->

# Testing JStream on a Mac

How a tester gets JStream on a Mac before there is a packaged download: one command
builds it from source on their own Mac and installs a launcher that runs it from
Terminal. A build made on the Mac it runs on is never quarantined, so Gatekeeper doesn't
block it, and nothing needs signing beyond what the linker does. This stays inside
[ADR-0063](../adr/0063-macos-platform-support.md): there is no app bundle (packaging and
signing are later decisions, §13), and JStream runs from Terminal, so macOS attributes
its permission prompts to Terminal (§11). The hardware run that checks what CI can't is
#1099.

<a id="tester"></a>
## For testers
<!-- @section
depends_on = ["docs/runbooks/mac-testing.md#script"]
-->

You need macOS 14 (Sonoma) or later, on an Apple Silicon Mac (Intel Macs work, but
aren't tested), and about 15 GB free. In Terminal, run:

`curl -fsSL https://raw.githubusercontent.com/jeffmhopkins/JStream/main/scripts/mac/install.sh | bash`

The first run takes a while. If macOS offers to install the Command Line Tools, choose
Install (Xcode itself isn't needed); the script waits for them.

When it finishes, JStream starts in a Terminal window. Later, open **JStream.command**
from your Applications folder (or Spotlight). Keep its window open while you use
JStream, and press Ctrl-C there (or close the window) to stop it.

JStream opens in your browser, with your access token on the clipboard for two minutes.
When the browser asks you to sign in, paste the token as the password (any user name
will do), then click Connect with the Token field left empty: the browser's sign-in
carries the token. Missed the two minutes? The token is the text in the `token` file in
the data folder below.

The first time you add a screen or window, a camera, a microphone or desktop audio, macOS
asks whether **Terminal** may use it, because Terminal runs JStream. Those permissions
stay through updates. Screen recording takes effect once you restart JStream. On macOS
15 and later, macOS asks again every month whether Terminal may keep recording the
screen. Saving a stream key may ask for your login keychain password again after an
update.

- **Update:** run the same command again.
- **A specific version:** put `bash -s -- --ref <tag>` in place of the final `bash`. By
  default it builds the newest release that has this installer, else `main`.
- **Remove:** put `bash -s -- --uninstall` in place of the final `bash` (add `--purge`
  to delete your scenes, token and logs too). Terminal keeps the permissions you gave
  it; remove them in System Settings › Privacy & Security if you want.
- **Scenes, token and logs:** `~/Library/Application Support/JStream`. Attach
  `logs/` to a bug report.

What isn't on the Mac yet: browser sources, the virtual camera, and a desktop window
(JStream runs in your browser instead).

<a id="script"></a>
## The script
<!-- @section
covers = ["scripts/mac/**", "scripts/install-tools.txt", ".github/workflows/mac-install.yml"]
depends_on = ["docs/adr/0063-macos-platform-support.md", "apps/jstream-daemon/README.md#purpose", "docs/security/threat-model.md#server"]
-->

`scripts/mac/install.sh` does each step only when it is needed, so running it again
updates what changed:

1. **Checks the Mac:** macOS 14.0 or later (ADR-0063's floor), with a warning below 14.2,
   which process taps need, and on Intel, which is best effort. It refuses a Terminal
   under Rosetta, and `sudo`.
2. **Apple's Command Line Tools**, through `xcode-select --install`, for the linker and
   git.
3. **Rust:** rustup, if missing, installed without touching the shell profile. The
   repository's `rust-toolchain.toml` picks the toolchain.
4. **Source:** a clone in `~/Library/Application Support/JStream-build/src`, checked
   out at `--ref`, else the newest release tag that has the installer, else `main`.
   `--source <dir>` builds a checkout as it is instead.
5. **UI tools:** trunk, the wasm-bindgen CLI and binaryen's wasm-opt for macOS, at the
   versions and SHA-256 sums `scripts/install-tools.txt` pins in the checked-out source
   (one line per tool and platform, the releases `ci.yml` and `after-review.yml` use), so
   they always match that source's `Cargo.lock`; a mismatch stops the install.
6. **Build:** `trunk build --release` for the web UI and `cargo build --release` for
   `jstream-daemon`, which the linker signs ad hoc, as every Apple Silicon binary is,
   and which embeds its usage strings (ADR-0063 §11).
7. **Install:** the daemon, the UI, `jstream.sh` and `clipboard.js` into
   `JStream-build/install`, swapped in whole, refusing while JStream runs; and
   `~/Applications/JStream.command`, which Finder opens in Terminal and which runs
   `jstream.sh`. Then it opens that, unless `--no-open`.

`jstream.sh` starts the daemon with the data folder's token file and
`collection.json`, and the built UI, waits for its `listening on` line, and opens the
`ui:` URL in the default browser. It runs until Ctrl-C or the window closes, and stops
the daemon (SIGTERM) on the way out. `JSTREAM_NO_BROWSER=1` prints the URL instead (CI).

The token reaches the browser through the clipboard, with the least reach the clipboard
allows. `jstream.sh` pipes it to `clipboard.js` (JavaScript for Automation) on stdin,
never on a command line. `clipboard.js` writes it for this Mac only
(`NSPasteboardContentsCurrentHostOnly`, so not to Universal Clipboard), marks it
concealed and transient (the nspasteboard.org `org.nspasteboard.*` types clipboard managers honour), and
clears it after two minutes unless something else was copied since. The threat model
records this ([Server › Accepted](../security/threat-model.md#server)).

`--uninstall` removes the launcher and the build folder; `--purge` also removes the
data folder. It leaves Terminal's permissions alone: they may be the user's for other
tools.

The `mac-install` workflow runs when the scripts, `Cargo.lock` or what the UI build
reads change. `pins` runs shellcheck and checks every pin in `install-tools.txt`
against `ci.yml`, `after-review.yml` and `Cargo.lock`. `install` runs the installer from
the checkout on `macos-latest`, checks what it installed, runs `jstream.sh` without the
browser, loads the UI with the token, checks that SIGTERM stops the daemon, and compiles
`clipboard.js`. Dialogs, permission prompts and the clipboard's behaviour need a person
at a Mac: #1099.
