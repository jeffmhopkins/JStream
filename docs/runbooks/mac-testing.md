<!-- @doc
id = "RUNBOOK-MAC-TESTING"
title = "Testing JStream on a Mac"
kind = "guide"
status = "active"
review_every = "180d"
-->

# Testing JStream on a Mac

How a tester gets JStream on a Mac before there is a signed download: one command builds
it from source on their own Mac and installs `JStream.app`. A build made on the Mac it
runs on is never quarantined, so Gatekeeper doesn't block it, and no Apple Developer
account is needed ([ADR-0063](../adr/0063-macos-platform-support.md) leaves signing and
notarisation for later).

<a id="tester"></a>
## For testers
<!-- @section
depends_on = ["docs/runbooks/mac-testing.md#script"]
-->

You need macOS 14 (Sonoma) or later, on an Apple Silicon Mac (Intel Macs work, but
aren't tested), and about 15 GB free. In Terminal, run:

`curl -fsSL https://raw.githubusercontent.com/jeffmhopkins/JStream/main/scripts/mac/install.sh | bash`

The first run takes a while. macOS asks a few things along the way:

1. **Install the Command Line Tools?** Choose Install. Xcode itself isn't needed.
2. **Your password**, once, to trust "JStream Local Signing", the certificate the script
   makes so that each update keeps the permissions you grant.
3. **Can `codesign` use the key?** Choose Always Allow.

When it finishes, JStream opens. It copies your access token to the clipboard and opens
JStream in your browser. When the browser asks you to sign in, paste the token as the
password (any user name will do), then click Connect with the Token field left empty:
the browser's sign-in carries the token. Later, open JStream from
your Applications folder. Quit it from its Dock icon, which stops JStream.

The first time you add a screen or window, a camera, a microphone or desktop audio, macOS
asks for permission. Screen recording takes effect once you quit and reopen JStream. On
macOS 15 and later, macOS asks again every month whether JStream may keep recording the
screen.

- **Update:** run the same command again.
- **A specific version:** put `bash -s -- --ref <tag>` in place of the final `bash`. By
  default it builds the newest release that has this installer, else `main`.
- **Remove:** put `bash -s -- --uninstall` in place of the final `bash` (and `--purge`
  to delete your scenes, token and logs too).
- **Scenes, token and logs:** `~/Library/Application Support/JStream`. Attach
  `logs/` to a bug report.

What isn't on the Mac yet: browser sources, the virtual camera, and a desktop window
(JStream runs in your browser instead).

<a id="script"></a>
## The script
<!-- @section
covers = ["scripts/mac/**", "scripts/install-tools.txt", ".github/workflows/mac-install.yml"]
depends_on = ["docs/adr/0063-macos-platform-support.md", "apps/jstream-daemon/README.md#purpose"]
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
   `jstream-daemon`.
7. **`JStream.app`:** `JStream.applescript` compiled as a stay-open applet, with the
   daemon in `Contents/MacOS`, the UI and `start-daemon.sh` in `Contents/Resources`,
   the app icon, and the daemon's camera, microphone and system audio usage strings in
   its `Info.plist`. The daemon runs as the applet's child, so macOS attributes its
   permission prompts to JStream, not to Terminal (ADR-0063 §11).
8. **Signing:** with "JStream Local Signing", a self-signed code-signing certificate the
   script makes once in the login keychain and trusts for code signing. macOS ties
   permissions to the signature, which is the same on every build, so they survive
   updates. `--adhoc`, or a certificate that can't be made, signs ad hoc instead, and
   macOS then asks again after each update.
9. **Install:** into `~/Applications`, refusing while JStream runs, then opens it.

The applet runs `start-daemon.sh`, which starts the daemon with the data folder's token
file and `collection.json`, and the bundled UI, waits for its `listening on` line, and
hands back its process ID, the UI's URL and the token. The applet copies the token to the
clipboard, opens the URL, reopens it when the Dock icon is clicked, quits with an alert if
the daemon stops, and stops the daemon (SIGTERM) when it quits.

`--uninstall` removes the app, the build folder and the signing identity, and resets
JStream's permissions; `--purge` also removes the data folder.

The `mac-install` workflow runs when the scripts, `Cargo.lock` or what the UI build
reads change. `pins` runs shellcheck and checks every pin in `install-tools.txt`
against `ci.yml`, `after-review.yml` and `Cargo.lock`. `install` runs the script from the checkout on
`macos-latest`, signed ad hoc, checks the app's signature and `Info.plist`, then runs
`start-daemon.sh` and loads the UI. Permission prompts, the signing identity and the
applet itself need a real Mac.
