#!/bin/bash
# JStream for Linux, built from source in one command: checks the machine, installs the
# distribution packages the build and the engine need (apt, dnf or pacman), Rust and the
# pinned UI tools, builds the web UI, the daemon and the browser helper, and installs
# JStream in your home folder with an entry in the app menu. Running it again updates it.
# For testers:
#
#   curl -fsSL https://raw.githubusercontent.com/jeffmhopkins/JStream/main/scripts/linux/install.sh | bash
#
# Options (after `bash -s --` when piped):
#   --ref <ref>             build this tag, branch or commit (default: the newest release
#                           tag that has this installer, else main)
#   --source <dir>          build this checkout as it is, without cloning or fetching (CI,
#                           developers)
#   --no-browser-sources    build without browser sources: no CEF download, no sandbox step
#   --no-packages           don't install distribution packages (say what's missing instead)
#   --no-open               don't open JStream when done
#   --uninstall             remove JStream, its menu entry and the build folder; add
#                           --purge to remove the collection, token and logs too
#
# Docs: docs/runbooks/linux-testing.md#script
#
# Everything runs from `main` at the end, so a download cut short runs nothing.

set -euo pipefail

REPO_URL=https://github.com/jeffmhopkins/JStream.git
# Free disk the first build wants, in GB: the toolchain, the crates, CEF and the target folder.
MIN_FREE_GB=20

XDG_DATA=${XDG_DATA_HOME:-$HOME/.local/share}
BUILD_DIR="${JSTREAM_BUILD_DIR:-$XDG_DATA/jstream-build}"
APP_DIR="${JSTREAM_APP_DIR:-$HOME/.local/lib/jstream}"
BIN_DIR="${JSTREAM_BIN_DIR:-$HOME/.local/bin}"
MENU_DIR="$XDG_DATA/applications"
ICON_DIR="$XDG_DATA/icons/hicolor/256x256/apps"
# The daemon's data folder on Linux (jstream-platform's `paths::data_dir`).
DATA_DIR="$XDG_DATA/jstream"

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
note() { printf '    %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() {
    printf '\033[31merror:\033[0m %s\n' "$*" >&2
    exit 1
}

# `ID`, `ID_LIKE` and `PRETTY_NAME` from os-release, in globals, read without sourcing it.
read_os_release() {
    local file=/etc/os-release
    [ -r "$file" ] || file=/usr/lib/os-release
    os_field() { [ -r "$file" ] && sed -n "s/^$1=//p" "$file" | tr -d "\"'" | head -1; }
    OS_ID=$(os_field ID || true)
    OS_LIKE=$(os_field ID_LIKE || true)
    OS_NAME=$(os_field PRETTY_NAME || true)
    OS_NAME=${OS_NAME:-Linux}
}

# Which package manager family this distribution uses: apt, dnf, pacman, or "".
package_family() {
    local id
    for id in $OS_ID $OS_LIKE; do
        case "$id" in
            debian | ubuntu) echo apt; return ;;
            fedora | rhel | centos) echo dnf; return ;;
            arch | archlinux) echo pacman; return ;;
        esac
    done
    if command -v apt-get >/dev/null 2>&1; then echo apt
    elif command -v dnf >/dev/null 2>&1; then echo dnf
    elif command -v pacman >/dev/null 2>&1; then echo pacman
    fi
}

check_linux() {
    step "Checking this machine"
    [ "$(uname -s)" = Linux ] || die "this installer is for Linux; on macOS use scripts/mac/install.sh, on Windows the portable zip"
    read_os_release
    ARCH=$(uname -m)
    case "$ARCH" in
        x86_64) note "$OS_NAME on x86_64" ;;
        aarch64)
            note "$OS_NAME on aarch64"
            warn "aarch64 is best effort (ADR-0060): JStream builds, but isn't tested there"
            ;;
        *) die "JStream builds for x86_64 and aarch64, not $ARCH" ;;
    esac
    FAMILY=$(package_family)

    # ADR-0060 §2: the tested tier is GNOME or KDE Plasma on Wayland, on Ubuntu LTS or Fedora.
    local session=${XDG_SESSION_TYPE:-} desktop=${XDG_CURRENT_DESKTOP:-} tested=1
    if [ -z "$session" ]; then
        if [ -n "${WAYLAND_DISPLAY:-}" ]; then session=wayland
        elif [ -n "${DISPLAY:-}" ]; then session=x11
        else session="no desktop session"
        fi
    fi
    note "session: $session${desktop:+, $desktop}"
    case "$OS_ID" in ubuntu | fedora) ;; *) tested="" ;; esac
    [ "$session" = wayland ] || tested=""
    case "$desktop" in *GNOME* | *KDE*) ;; *) tested="" ;; esac
    if [ -z "$tested" ]; then
        note "support: best effort (tested: GNOME or KDE Plasma on Wayland, on Ubuntu LTS or Fedora)"
    else
        note "support: tested"
    fi
    if ! pgrep -u "$(id -u)" -x pipewire >/dev/null 2>&1; then
        warn "PipeWire isn't running: audio falls back to ALSA, without desktop or application audio"
    fi

    local free_gb
    mkdir -p "$(dirname "$BUILD_DIR")"
    free_gb=$(df -P -BG "$(dirname "$BUILD_DIR")" | awk 'NR == 2 { sub(/G$/, "", $4); print $4 }')
    if [ -n "$free_gb" ] && [ "$free_gb" -lt "$MIN_FREE_GB" ]; then
        warn "only ${free_gb} GB free: the first build needs about ${MIN_FREE_GB} GB"
    fi
}

check_not_running() {
    if [ -x "$APP_DIR/jstream" ] && "$APP_DIR/jstream" --running 2>/dev/null; then
        die "JStream is running: stop it (\"Stop JStream\" in its menu entry, or jstream --stop), then run this again"
    fi
}

# The packages to install, per family: what building needs, what the engine loads at run
# time (it opens libpipewire, libva, libasound and Vulkan with dlopen, ADR-0060 §8), and
# what CEF's libcef.so links (the same list as ci.yml's browser-linux job).
apt_packages() {
    echo build-essential pkg-config git curl ca-certificates cmake ninja-build
    echo libvulkan1 mesa-vulkan-drivers libva2 libva-drm2 xdg-desktop-portal xdg-utils libnotify-bin
    apt_pick libpipewire-0.3-0t64 libpipewire-0.3-0
    apt_pick libasound2t64 libasound2
    if [ -n "$BROWSER" ]; then
        echo libnss3 libxcomposite1 libxdamage1 libxrandr2 libgbm1 libpango-1.0-0 libcairo2 libxkbcommon0 libdrm2
        apt_pick libatk1.0-0t64 libatk1.0-0
        apt_pick libatk-bridge2.0-0t64 libatk-bridge2.0-0
        apt_pick libcups2t64 libcups2
    fi
    clipboard_package
}

dnf_packages() {
    echo gcc gcc-c++ make pkgconf-pkg-config git curl cmake ninja-build
    echo vulkan-loader mesa-vulkan-drivers libva pipewire-libs alsa-lib xdg-desktop-portal xdg-utils libnotify
    if [ -n "$BROWSER" ]; then
        echo nss atk at-spi2-atk cups-libs libXcomposite libXdamage libXrandr mesa-libgbm pango cairo libxkbcommon libdrm
    fi
    clipboard_package
}

pacman_packages() {
    echo base-devel git curl cmake ninja pkgconf
    echo vulkan-icd-loader libva pipewire alsa-lib xdg-desktop-portal xdg-utils libnotify
    if [ -n "$BROWSER" ]; then
        echo nss atk at-spi2-core libcups libxcomposite libxdamage libxrandr mesa pango cairo libxkbcommon libdrm
    fi
    clipboard_package
}

# The launcher copies the token with wl-copy on Wayland, xclip on X11.
clipboard_package() {
    if [ -n "${WAYLAND_DISPLAY:-}" ]; then echo wl-clipboard
    elif [ -n "${DISPLAY:-}" ]; then echo xclip
    fi
}

# The first of these package names apt can install (Debian 13 and Ubuntu 24.04 renamed
# some libraries with a t64 suffix).
apt_pick() {
    local name
    for name in "$@"; do
        if apt-cache show "$name" >/dev/null 2>&1; then
            echo "$name"
            return
        fi
    done
    echo "$1"
}

# The packages of the list that aren't installed.
missing_packages() {
    local pkg
    case "$FAMILY" in
        apt)
            for pkg in "$@"; do
                dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | grep -q 'install ok installed' || echo "$pkg"
            done
            ;;
        dnf)
            for pkg in "$@"; do
                rpm -q --whatprovides "$pkg" >/dev/null 2>&1 || echo "$pkg"
            done
            ;;
        pacman)
            for pkg in "$@"; do
                pacman -Qq "$pkg" >/dev/null 2>&1 || pacman -Qqg "$pkg" >/dev/null 2>&1 || echo "$pkg"
            done
            ;;
    esac
}

# Runs a command as root: directly when we are root (containers), else through sudo, which
# asks for the password on the terminal even when this script is piped from curl.
as_root() {
    if [ "$(id -u)" = 0 ]; then "$@"
    elif command -v sudo >/dev/null 2>&1; then sudo "$@" </dev/null
    else die "installing packages needs root, and sudo isn't installed: install them as root, then run this again"
    fi
}

packages() {
    step "Distribution packages"
    if [ -z "$FAMILY" ]; then
        warn "unknown package manager: install a C/C++ compiler, git, curl, cmake, ninja, the"
        warn "Vulkan loader and a Vulkan driver, libva, PipeWire, ALSA and xdg-desktop-portal"
        warn "(and CEF's libraries: nss, atk, cups, the X libraries, gbm, pango, cairo) yourself"
        return
    fi
    local -a want missing
    read -r -a want <<<"$("${FAMILY}_packages" | tr '\n' ' ')"
    mapfile -t missing < <(missing_packages "${want[@]}")
    if [ "${#missing[@]}" = 0 ]; then
        note "all installed"
        return
    fi
    note "missing: ${missing[*]}"
    if [ -n "$NO_PACKAGES" ]; then
        warn "--no-packages: install those yourself; the build or the engine may fail without them"
        return
    fi
    [ "$(id -u)" = 0 ] || note "installing them with sudo, which asks for your password"
    case "$FAMILY" in
        apt)
            as_root env DEBIAN_FRONTEND=noninteractive apt-get update -qq
            as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "${missing[@]}"
            ;;
        dnf) as_root dnf install -y -q "${missing[@]}" ;;
        pacman) as_root pacman -S --needed --noconfirm "${missing[@]}" ;;
    esac
    note "installed"
}

# What VA-API needs on this GPU, said rather than installed: driver packages depend on the
# GPU and the distribution's patent policy, and OpenH264 covers machines without one.
gpu_encode_hint() {
    local vendor="" card
    for card in /sys/class/drm/card*/device/vendor; do
        [ -r "$card" ] || continue
        vendor="$vendor $(cat "$card")"
    done
    local intel="" amd="" nvidia=""
    case "$vendor" in *0x8086*) intel=1 ;; esac
    case "$vendor" in *0x1002*) amd=1 ;; esac
    case "$vendor" in *0x10de*) nvidia=1 ;; esac
    local -a hint=()
    case "$FAMILY" in
        apt)
            [ -z "$intel" ] || hint+=(intel-media-va-driver)
            [ -z "$amd" ] || hint+=(mesa-va-drivers)
            ;;
        dnf)
            [ -z "$intel" ] || hint+=(intel-media-driver)
            [ -z "$amd" ] || hint+=("mesa-va-drivers-freeworld (from RPM Fusion: Fedora's own Mesa has H.264 encode switched off)")
            ;;
        pacman)
            [ -z "$intel" ] || hint+=(intel-media-driver)
            [ -z "$amd" ] || hint+=(libva-mesa-driver)
            ;;
    esac
    if [ "${#hint[@]}" -gt 0 ]; then
        note "hardware H.264 encoding on this GPU uses VA-API: if JStream says it has no"
        note "hardware encoder, install ${hint[*]}"
    fi
    if [ -n "$nvidia" ] || [ "${#hint[@]}" = 0 ]; then
        note "without a VA-API encoder (NVIDIA, virtual machines) JStream streams H.264 with"
        note "Cisco's OpenH264, which it downloads from Cisco when you ask it to"
    fi
}

rust() {
    step "Rust"
    export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
    export PATH="$CARGO_HOME/bin:$PATH"
    if command -v rustup >/dev/null 2>&1; then
        note "rustup $(rustup --version 2>/dev/null | awk '{ print $2 }')"
    else
        note "installing rustup (the repository picks its own pinned toolchain)"
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
            sh -s -- -y --no-modify-path --profile minimal --default-toolchain none
    fi
}

# The newest release tag that has this installer, else main.
default_ref() {
    local tag
    while read -r tag; do
        if git -C "$SRC" cat-file -e "$tag:scripts/linux/install.sh" 2>/dev/null; then
            echo "$tag"
            return
        fi
    done < <(git -C "$SRC" tag -l 'v*' --sort=-v:refname)
    echo main
}

source_tree() {
    step "JStream's source"
    if [ -n "$SOURCE_DIR" ]; then
        SRC=$(cd "$SOURCE_DIR" && pwd)
        [ -f "$SRC/Cargo.toml" ] || die "$SRC is not a JStream checkout"
        note "building $SRC as it is"
    else
        SRC="$BUILD_DIR/src"
        mkdir -p "$BUILD_DIR"
        if [ -d "$SRC/.git" ]; then
            git -C "$SRC" fetch --quiet --tags --force origin
        else
            git clone --quiet "$REPO_URL" "$SRC"
        fi
        local ref="${REF:-$(default_ref)}" commit
        commit=$(git -C "$SRC" rev-parse --verify --quiet "origin/$ref^{commit}" ||
            git -C "$SRC" rev-parse --verify --quiet "$ref^{commit}") ||
            die "no tag, branch or commit called $ref"
        # The build folder belongs to this script: whatever is checked out there is replaced.
        git -C "$SRC" checkout --quiet --force --detach "$commit"
        note "$ref ($(git -C "$SRC" log -1 --format='%h, %cs'))"
    fi
    [ -f "$SRC/scripts/linux/jstream" ] ||
        die "this version of JStream predates the Linux installer: pick a newer --ref"
    # The toolchain rust-toolchain.toml pins, with the wasm target the UI needs.
    (cd "$SRC" && rustup toolchain install >/dev/null)
}

version_of() {
    sed -n 's/^version = "\(.*\)"/\1/p' "$SRC/Cargo.toml" | head -1
}

# Downloads `url` to `file` unless it is there with `sha`, and checks the sum either way.
fetch() {
    local url=$1 file=$2 sha=$3
    if ! { [ -f "$file" ] && echo "$sha  $file" | sha256sum -c - >/dev/null 2>&1; }; then
        curl -sSLf -o "$file" "$url"
        echo "$sha  $file" | sha256sum -c - >/dev/null ||
            die "$(basename "$file") doesn't match its pinned SHA-256: not using it"
    fi
}

# `version sha256` of a pinned UI tool for a platform, from the checkout's
# scripts/install-tools.txt, so the pins always match that checkout's Cargo.lock.
tool_pin() {
    local found
    found=$(awk -v t="$1" -v p="$2" '$1 == t && $3 == p { print $2, $4; exit }' "$SRC/scripts/install-tools.txt")
    [ -n "$found" ] || die "scripts/install-tools.txt has no $1 for $2"
    echo "$found"
}

ui_tools() {
    step "Web UI tools (trunk, wasm-bindgen, wasm-opt)"
    TOOLS="$BUILD_DIR/tools"
    local dl="$TOOLS/downloads" trunk_platform wb_platform binaryen_platform version sha
    mkdir -p "$dl" "$TOOLS/bin"
    # The platforms each project publishes; wasm-bindgen has a static musl build for x86_64.
    if [ "$ARCH" = aarch64 ]; then
        trunk_platform=aarch64-unknown-linux-gnu wb_platform=aarch64-unknown-linux-gnu binaryen_platform=aarch64-linux
    else
        trunk_platform=x86_64-unknown-linux-gnu wb_platform=x86_64-unknown-linux-musl binaryen_platform=x86_64-linux
    fi

    read -r version sha < <(tool_pin trunk "$trunk_platform")
    local trunk_version=$version
    fetch "https://github.com/trunk-rs/trunk/releases/download/v$version/trunk-$trunk_platform.tar.gz" \
        "$dl/trunk-$version-$trunk_platform.tar.gz" "$sha"
    tar xzf "$dl/trunk-$version-$trunk_platform.tar.gz" -C "$TOOLS/bin" trunk

    read -r version sha < <(tool_pin wasm-bindgen "$wb_platform")
    local locked wb_version=$version
    locked=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/"/, "", $3); print $3; exit }' "$SRC/Cargo.lock")
    [ "$locked" = "$version" ] ||
        die "Cargo.lock has wasm-bindgen $locked but scripts/install-tools.txt pins $version"
    local wb="wasm-bindgen-$version-$wb_platform"
    fetch "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/$version/$wb.tar.gz" \
        "$dl/$wb.tar.gz" "$sha"
    tar xzf "$dl/$wb.tar.gz" --strip-components=1 -C "$TOOLS/bin" "$wb/wasm-bindgen"

    # binaryen's Linux wasm-opt links libbinaryen statically, so the binary is enough.
    read -r version sha < <(tool_pin binaryen "$binaryen_platform")
    local by="binaryen-version_$version"
    fetch "https://github.com/WebAssembly/binaryen/releases/download/version_$version/$by-$binaryen_platform.tar.gz" \
        "$dl/$by-$binaryen_platform.tar.gz" "$sha"
    tar xzf "$dl/$by-$binaryen_platform.tar.gz" --strip-components=2 -C "$TOOLS/bin" "$by/bin/wasm-opt"

    export PATH="$TOOLS/bin:$PATH"
    note "trunk $trunk_version, wasm-bindgen $wb_version, wasm-opt $version"
}

build() {
    step "Building (the first build takes a while; updates are much faster)"
    local started=$SECONDS
    (cd "$SRC" && cargo fetch --locked)
    UI_DIST="$BUILD_DIR/ui-dist"
    rm -rf "$UI_DIST"
    (cd "$SRC/crates/jstream-ui" && trunk build --release --locked --offline --dist "$UI_DIST")
    if [ -n "$BROWSER" ]; then
        (cd "$SRC" && cargo build --release --locked -p jstream-daemon --features cef)
        # The helper and CEF's runtime (ADR-0095); its build downloads CEF's pinned release.
        rm -rf "$BUILD_DIR/browser-out"
        (cd "$SRC" && cargo xtask browser-linux --out "$BUILD_DIR/browser-out")
    else
        (cd "$SRC" && cargo build --release --locked -p jstream-daemon)
    fi
    DAEMON="${CARGO_TARGET_DIR:-$SRC/target}/release/jstream-daemon"
    [ -x "$DAEMON" ] || die "the build left no $DAEMON"
    note "built in $(((SECONDS - started) / 60)) min $(((SECONDS - started) % 60)) s"
}

# The app folder, staged beside the real one and swapped in: the daemon, the UI, the
# launcher, the browser helper with CEF, and the licence files with ADR-0062's AAC notice.
stage_app() {
    step "Staging JStream $(version_of)"
    STAGE="$APP_DIR.new"
    rm -rf "$STAGE"
    mkdir -p "$STAGE"
    cp "$DAEMON" "$STAGE/jstream-daemon"
    cp -R "$UI_DIST" "$STAGE/ui"
    install -m 0755 "$SRC/scripts/linux/jstream" "$STAGE/jstream"
    cp "$SRC/LICENSE-MIT" "$SRC/LICENSE-APACHE" "$STAGE/"
    cat >"$STAGE/NOTICE.txt" <<'EOF'
JStream is licensed under the MIT or Apache-2.0 licence, at your option (LICENSE-MIT,
LICENSE-APACHE).

AAC: JStream encodes AAC audio with its own AAC-LC encoder. JStream holds no AAC patent
licence. Via Licensing Alliance (Via LA) administers the AAC patent pool.

H.264: JStream encodes H.264 with your GPU's driver through VA-API or, when you ask it
to, with Cisco's OpenH264 binary, which it downloads from Cisco. OpenH264 Video Codec
provided by Cisco Systems, Inc.

Browser sources use the Chromium Embedded Framework (CEF) and Chromium, under their own
licences, in browser/.
EOF
    if [ -n "$BROWSER" ]; then
        cp -R "$BUILD_DIR/browser-out/browser" "$STAGE/browser"
    fi
    echo "$(version_of) $(git -C "$SRC" rev-parse --short HEAD 2>/dev/null || echo source)" >"$STAGE/VERSION"
    "$STAGE/jstream-daemon" --version >/dev/null || die "the built daemon doesn't run"
}

# Chromium sandboxes CEF's subprocesses with unprivileged user namespaces where the kernel
# allows them, and needs its setuid-root chrome-sandbox where it doesn't, such as Ubuntu
# 24.04 and later, whose AppArmor restricts them (ADR-0095).
needs_setuid_sandbox() {
    local value
    value=$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null || echo 0)
    [ "$value" = 1 ] && return 0
    value=$(cat /proc/sys/kernel/unprivileged_userns_clone 2>/dev/null || echo 1)
    [ "$value" = 0 ] && return 0
    value=$(cat /proc/sys/user/max_user_namespaces 2>/dev/null || echo 1)
    [ "$value" = 0 ] && return 0
    return 1
}

browser_sandbox() {
    [ -n "$BROWSER" ] || return 0
    step "Browser sandbox"
    local sandbox="$STAGE/browser/chrome-sandbox"
    [ -f "$sandbox" ] || die "CEF's distribution left no chrome-sandbox"
    if ! needs_setuid_sandbox; then
        note "this kernel allows user namespaces: Chromium's own sandbox needs no setup"
        return
    fi
    note "this kernel restricts user namespaces, so Chromium's sandbox helper must be setuid root"
    [ "$(id -u)" = 0 ] || note "sudo asks for your password to set that up"
    as_root chown root:root "$sandbox"
    as_root chmod 4755 "$sandbox"
    if findmnt -n -o OPTIONS --target "$sandbox" 2>/dev/null | grep -qw nosuid; then
        warn "$(dirname "$APP_DIR") is on a nosuid mount: browser sources won't start there"
    fi
    note "done"
}

install_app() {
    step "Installing"
    check_not_running
    if [ -d "$APP_DIR" ]; then
        rm -rf "$APP_DIR.old"
        mv "$APP_DIR" "$APP_DIR.old"
    fi
    mv "$STAGE" "$APP_DIR"
    rm -rf "$APP_DIR.old"
    mkdir -p "$BIN_DIR" "$MENU_DIR" "$ICON_DIR"
    ln -sfn "$APP_DIR/jstream" "$BIN_DIR/jstream"
    install -m 0644 "$SRC/apps/jstream-desktop/icons/icon.png" "$ICON_DIR/jstream.png"
    cat >"$MENU_DIR/jstream.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=JStream
GenericName=Live streaming studio
Comment=Mix scenes and stream them live
Exec="$APP_DIR/jstream"
Icon=jstream
Terminal=false
Categories=AudioVideo;Video;
Keywords=stream;live;broadcast;rtmp;youtube;
Actions=stop;

[Desktop Action stop]
Name=Stop JStream
Exec="$APP_DIR/jstream" --stop
EOF
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$MENU_DIR" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t "$XDG_DATA/icons/hicolor" 2>/dev/null || true
    fi
    note "$APP_DIR"
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) note "$BIN_DIR isn't on your PATH: open JStream from the app menu, or run $APP_DIR/jstream" ;;
    esac
}

checks() {
    step "Checking the install"
    note "$("$APP_DIR/jstream-daemon" --version | head -1)"
    if ! ls /usr/share/vulkan/icd.d/*.json /etc/vulkan/icd.d/*.json >/dev/null 2>&1; then
        warn "no Vulkan driver is installed: JStream can't render without one; install your"
        warn "distribution's Mesa Vulkan drivers (Debian and Ubuntu: mesa-vulkan-drivers)"
    fi
    gpu_encode_hint
}

done_message() {
    cat <<EOF

JStream is installed: $APP_DIR

Open it from your app menu (JStream), or run: jstream
It starts JStream, copies your access token to the clipboard and opens JStream in your
browser. When the browser asks you to sign in, paste the token as the password (any
user name), then click Connect with the Token field left empty.
Stop it with "Stop JStream" (right-click its menu entry) or: jstream --stop
The first time you add a screen or window, your desktop asks which one to share.

Your scenes, token and logs:  $DATA_DIR
To update:                    run the same command again
To remove:                    the same command with --uninstall at the end:
                              curl -fsSL <installer URL> | bash -s -- --uninstall
EOF
}

uninstall() {
    step "Removing JStream"
    check_not_running
    # chrome-sandbox may be root's, but the folder is ours, so removing it needs no sudo.
    rm -rf "$APP_DIR" "$APP_DIR.new" "$APP_DIR.old" "$BUILD_DIR"
    if [ "$(readlink "$BIN_DIR/jstream" 2>/dev/null)" = "$APP_DIR/jstream" ]; then
        rm -f "$BIN_DIR/jstream"
    fi
    rm -f "$MENU_DIR/jstream.desktop" "$ICON_DIR/jstream.png"
    note "removed $APP_DIR, its menu entry and $BUILD_DIR"
    if [ -n "$PURGE" ]; then
        rm -rf "$DATA_DIR"
        note "removed $DATA_DIR (scenes, token and logs)"
    else
        note "kept $DATA_DIR (scenes, token and logs); --purge removes it"
    fi
    note "distribution packages stay installed"
}

main() {
    REF="" SOURCE_DIR="" BROWSER=1 NO_PACKAGES="" OPEN=1 UNINSTALL="" PURGE=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --ref) REF=${2:?--ref needs a tag, branch or commit}; shift ;;
            --source) SOURCE_DIR=${2:?--source needs a folder}; shift ;;
            --no-browser-sources) BROWSER="" ;;
            --no-packages) NO_PACKAGES=1 ;;
            --no-open) OPEN="" ;;
            --uninstall) UNINSTALL=1 ;;
            --purge) PURGE=1 ;;
            -h | --help) sed -n '2,22p' "$0" 2>/dev/null | sed 's/^# \{0,1\}//'; exit 0 ;;
            *) die "unknown option: $1 (try --help)" ;;
        esac
        shift
    done
    [ -z "$REF" ] || [ -z "$SOURCE_DIR" ] || die "--ref and --source can't be used together"
    [ -z "$PURGE" ] || [ -n "$UNINSTALL" ] || die "--purge goes with --uninstall"
    # Chromium refuses to run as root with its sandbox on, and the install is per user.
    [ "$(id -u)" != 0 ] || [ -n "${JSTREAM_ALLOW_ROOT:-}" ] ||
        die "run this as yourself, not as root or with sudo: it asks for sudo when it needs it"

    if [ -n "$UNINSTALL" ]; then
        uninstall
        return
    fi
    check_linux
    check_not_running
    packages
    rust
    source_tree
    ui_tools
    build
    stage_app
    browser_sandbox
    install_app
    checks
    done_message
    if [ -n "$OPEN" ] && { [ -n "${WAYLAND_DISPLAY:-}" ] || [ -n "${DISPLAY:-}" ]; }; then
        "$APP_DIR/jstream" || warn "JStream didn't start: its logs are in $DATA_DIR/logs"
    fi
}

main "$@"
