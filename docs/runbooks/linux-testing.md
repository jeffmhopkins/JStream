<!-- @doc
id = "RUNBOOK-LINUX-TESTING"
title = "Testing JStream on Linux"
kind = "guide"
status = "active"
review_every = "180d"
-->

# Testing JStream on Linux

How a tester gets JStream on Linux before there is a package: one command builds it from
source on their own machine and installs it in their home folder, with an entry in the
app menu. Packaging (a tarball, AppImage or Flatpak) is left for later
([ADR-0060](../adr/0060-linux-platform-support.md) §9). Building on the tester's machine
also keeps Cisco's OpenH264 licence simple: the codec is still downloaded by the tester's
own copy of JStream, on request ([ADR-0140](../adr/0140-source-types-and-decoding-scope-v3.md)).

<a id="tester"></a>
## For testers
<!-- @section
depends_on = ["docs/runbooks/linux-testing.md#script"]
-->

You need 64-bit Linux, ideally Ubuntu 24.04 or later, or a maintained Fedora, with GNOME
or KDE Plasma in a Wayland session (other desktops and distributions work, but aren't
tested), and about 20 GB free. In a terminal, run:

`curl -fsSL https://raw.githubusercontent.com/jeffmhopkins/JStream/main/scripts/linux/install.sh | bash`

Run it as yourself, not with `sudo`. The first run takes a while. It asks for your
password (through `sudo`) at most twice:

1. **To install the distribution packages** the build and JStream need, if any are
   missing. It lists them first, and never removes anything.
2. **To make Chromium's sandbox helper setuid root**, for browser sources, on systems that
   restrict user namespaces, such as Ubuntu 24.04 and later.

When it finishes, JStream starts. It copies your access token to the clipboard for two
minutes and opens JStream in your browser. When the browser asks you to sign in, paste the token as the
password (any user name will do), then click Connect with the Token field left empty.
Later, open JStream from your app menu, or run `jstream`. JStream has no window of its
own: it keeps running until you choose **Stop JStream** (right-click its menu entry) or
run `jstream --stop`. Opening it again while it runs opens the browser again.

The first time you add a screen or window, your desktop shows its own picker. JStream
remembers the choice, so the picker doesn't come back each time.

Streaming H.264 uses your GPU through VA-API on Intel and AMD. The script says which
driver package to install if it's missing (on Fedora, the full Mesa from RPM Fusion).
Without one, as on NVIDIA or in a virtual machine, JStream encodes with Cisco's OpenH264,
which it downloads from Cisco when you ask it to.

- **Update:** run the same command again.
- **A specific version:** put `bash -s -- --ref <tag>` in place of the final `bash`. By
  default it builds the newest release that has this installer, else `main`.
- **Without browser sources:** `bash -s -- --no-browser-sources` skips CEF's download and
  the sandbox step.
- **Remove:** put `bash -s -- --uninstall` in place of the final `bash` (and `--purge`
  to delete your scenes, token and logs too). The distribution packages stay.
- **Scenes, token and logs:** `~/.local/share/jstream`. Attach `logs/` to a bug report.

What isn't on Linux yet: media-file sources, the virtual camera, NVIDIA's NVENC encoder,
and a desktop window (JStream runs in your browser instead).

<a id="script"></a>
## The script
<!-- @section
covers = ["scripts/linux/**", "scripts/install-tools.txt", ".github/workflows/linux-install.yml"]
depends_on = ["docs/adr/0060-linux-platform-support.md", "docs/adr/0095-browser-helper-on-linux.md", "docs/runbooks/mac-testing.md#script", "apps/jstream-daemon/README.md#purpose"]
-->

`scripts/linux/install.sh` has the Mac installer's shape
([mac-testing › script](mac-testing.md#script)): the same order of steps, the same pins
file, and its `--ref`, `--source`, `--no-open` and `--uninstall [--purge]` flags. Linux
adds the package and sandbox steps, and `--no-browser-sources` and `--no-packages`. It
does each step only when it is needed, so running it again updates what changed:

1. **Checks the machine:** x86_64, or aarch64 with a best-effort warning; the
   distribution from `os-release`; and the session, desktop and PipeWire, from which it
   prints ADR-0060's support tier. It refuses root and `sudo`, because Chromium won't run
   its sandbox as root and the install is per user (`JSTREAM_ALLOW_ROOT` lets CI's
   containers build anyway).
2. **Distribution packages,** for apt (Debian, Ubuntu), dnf (Fedora) or pacman (Arch):
   the compiler, git, curl, cmake and ninja (CEF's wrapper library builds with them); what
   the engine loads at run time (the Vulkan loader and Mesa's Vulkan drivers, libva,
   libpipewire, libasound, xdg-desktop-portal); CEF's shared libraries, the list
   `ci.yml`'s `browser-linux` job uses; and `wl-copy` or `xclip` for the launcher. It
   installs only what's missing, through `sudo`, and picks Debian's `t64` names where the
   release has them. On another distribution, or with `--no-packages`, it lists what to
   install. GPU encode drivers are suggested at the end, not installed: they depend on
   the GPU and on the distribution's patent policy.
3. **Rust:** rustup, if missing, installed without touching the shell profile. After the
   checkout, the toolchain `rust-toolchain.toml` pins is installed by name, as the Mac
   script does, since rustup before 1.28 needs the name, and the UI's wasm target is added.
4. **Source:** a clone in `~/.local/share/jstream-build/src`, checked out at `--ref`,
   else the newest release tag that has the installer, else `main`. `--source <dir>`
   builds a checkout as it is instead.
5. **UI tools:** trunk, the wasm-bindgen CLI and binaryen's wasm-opt for Linux, at the
   versions and SHA-256 sums `scripts/install-tools.txt` pins in the checked-out source.
6. **Build:** `trunk build --release` for the web UI, `cargo build --release` for
   `jstream-daemon` with its `cef` feature, and `cargo xtask browser-linux` for the
   browser helper and CEF's runtime (ADR-0095). `--no-browser-sources` builds the daemon
   without CEF.
7. **Stage:** the daemon, the UI, the launcher, the `browser` folder (with `libcef.so`
   stripped of its debug symbols, which takes it from about 1.4 GB to about 270 MB), the licence files
   and a `NOTICE.txt` that says JStream holds no AAC patent licence and names Via LA
   ([ADR-0062](../adr/0062-aac-lc-encoder-patent-check.md)), beside the install.
8. **Browser sandbox:** where the kernel restricts unprivileged user namespaces (Ubuntu's
   AppArmor setting, or the older Debian switch), `chrome-sandbox` is made setuid root, as
   ADR-0095 says an install must, in one root step that refuses a symlink or a file the
   user doesn't own, which catches a mistake in the staged folder. It doesn't protect
   against code already running as the user: the file sits in a folder the user owns, so
   such code can put its own binary there before the step, or swap it in between the
   checks and the `chmod`, and have it made setuid root during the one `sudo` the tester
   approves. That is the risk of Chromium's setuid sandbox installed per user, and is
   accepted for a tester build. Elsewhere Chromium's namespace sandbox needs nothing.
9. **Install:** the staged folder replaces `~/.local/lib/jstream`, refusing while JStream
   runs; `~/.local/bin/jstream` links the launcher; and a `jstream.desktop` entry and the
   app icon go in `~/.local/share`. It then checks the daemon runs, warns when no Vulkan
   driver is installed, says which H.264 encoder the GPU will use, and opens JStream.

The launcher, `scripts/linux/jstream`, starts the daemon in its own session with the data
folder's token file and `collection.json`, and the installed UI, waits for its
`listening on` line, and keeps its process ID and the UI's URL in the data folder. It
copies the token to the clipboard, and clears it two minutes later if it is still there, as
on Mac; says so in a notification, and opens the URL with
`xdg-open`; run again while the daemon runs, it only does those last steps. `--stop`
stops the daemon (SIGTERM), which the menu entry's "Stop JStream" action runs. `--print`
starts it and prints the process ID, the URL and the token instead, for CI.

`--uninstall` removes the install, the link, the menu entry and the build folder;
`--purge` also removes the data folder.

The `linux-install` workflow runs when the scripts, `Cargo.lock`, the browser helper's
layout or what the UI build reads change. `lint` runs shellcheck. `ubuntu` runs the
script from the checkout on `ubuntu-latest`, checks the install's files, the AAC notice
and the setuid sandbox, starts the daemon through `jstream --print`, loads the UI, stops
it, then starts it again and has a red browser source painted on program through MCP
until the program snapshot shows red, and uninstalls. `fedora` runs the script in a Fedora container, which checks the dnf
package names. mac-install's `pins` job checks every pin's version, the Linux ones
included, against CI and `Cargo.lock`; `ubuntu` and `fedora` check the x86_64 sums when
they download, and the aarch64 sums were recorded by hand from the release files. The
screen picker, PipeWire, the clipboard and the notification need a real desktop.
