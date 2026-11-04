#!/bin/bash
# JStream for macOS, built from source in one command: checks the Mac, installs what the
# build needs (Apple's Command Line Tools, Rust, pinned UI tools), builds the web UI and
# the daemon, and installs them with a launcher, ~/Applications/JStream.command, that
# opens in Terminal. Running it again updates it. For testers:
#
#   curl -fsSL https://raw.githubusercontent.com/jeffmhopkins/JStream/main/scripts/mac/install.sh | bash
#
# Nothing is signed beyond what the linker does, and there is no app bundle (ADR-0063 §13
# leaves packaging and signing for later): JStream runs from Terminal, so macOS attributes
# its permission prompts to Terminal (ADR-0063 §11), and those grants survive updates.
#
# Docs: docs/runbooks/mac-testing.md#script
#
# Everything runs from `main` at the end, so a download cut short runs nothing.

set -euo pipefail

usage() {
    cat <<'EOF_USAGE'
JStream for macOS: builds JStream from source and installs ~/Applications/JStream.command.

  curl -fsSL https://raw.githubusercontent.com/jeffmhopkins/JStream/main/scripts/mac/install.sh | bash -s -- [options]

  --ref <ref>     build this tag, branch or commit (default: the newest release tag that
                  has this installer, else main)
  --source <dir>  build this checkout as it is, without cloning or fetching (CI, developers)
  --no-open       don't start JStream when done
  --uninstall     remove JStream.command and the build folder; add --purge to remove
                  your scenes, token and logs too
  -h, --help      show this
EOF_USAGE
}

REPO_URL=https://github.com/jeffmhopkins/JStream.git
# The floor (ADR-0063 §2) and the release process taps need (§4).
MIN_MACOS=14
TAPS_MACOS=14.2
# Free disk the first build wants, in GB: the toolchain, the crates and the target folder.
MIN_FREE_GB=15

BUILD_DIR="${JSTREAM_BUILD_DIR:-$HOME/Library/Application Support/JStream-build}"
# What the launcher runs: the daemon, the built UI and the launcher script.
INSTALL_DIR="$BUILD_DIR/install"
LAUNCHER_DIR="${JSTREAM_APP_DIR:-$HOME/Applications}"
LAUNCHER="$LAUNCHER_DIR/JStream.command"
# The daemon's data folder on macOS (jstream-platform's `paths::data_dir`).
DATA_DIR="$HOME/Library/Application Support/JStream"

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
note() { printf '    %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() {
    printf '\033[31merror:\033[0m %s\n' "$*" >&2
    exit 1
}

# `a.b[.c]` at least `x.y`?
version_at_least() {
    local -a have want
    local i
    IFS=. read -r -a have <<<"$1"
    IFS=. read -r -a want <<<"$2"
    for i in 0 1 2; do
        local h=${have[i]:-0} w=${want[i]:-0}
        ((h > w)) && return 0
        ((h < w)) && return 1
    done
    return 0
}

check_mac() {
    step "Checking this Mac"
    [ "$(uname -s)" = Darwin ] || die "this installer is for macOS; on Windows use the portable zip"
    local version
    version=$(sw_vers -productVersion)
    version_at_least "$version" "$MIN_MACOS" ||
        die "macOS $version is older than JStream's floor, macOS $MIN_MACOS (Sonoma)"
    if ! version_at_least "$version" "$TAPS_MACOS"; then
        warn "macOS $version: desktop and application audio need macOS $TAPS_MACOS, a free update in Software Update"
    fi
    if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
        die "this Terminal runs under Rosetta: open Terminal without \"Open using Rosetta\" and run it again"
    fi
    ARCH=$(uname -m)
    case "$ARCH" in
        arm64) note "macOS $version on Apple Silicon" ;;
        x86_64)
            note "macOS $version on Intel"
            warn "Intel Macs are best effort (ADR-0063): JStream builds, but isn't tested on them"
            ;;
        *) die "unknown processor: $ARCH" ;;
    esac
    local free_gb
    free_gb=$(df -g "$HOME" | awk 'NR == 2 { print $4 }')
    if [ -n "$free_gb" ] && [ "$free_gb" -lt "$MIN_FREE_GB" ]; then
        warn "only ${free_gb} GB free: the first build needs about ${MIN_FREE_GB} GB"
    fi
}

command_line_tools() {
    step "Apple's Command Line Tools"
    if xcode-select -p >/dev/null 2>&1; then
        note "installed at $(xcode-select -p)"
        return
    fi
    note "macOS will ask to install them: choose Install (Xcode itself isn't needed)"
    xcode-select --install >/dev/null 2>&1 || true
    local waited=0
    until xcode-select -p >/dev/null 2>&1; do
        ((waited >= 3600)) && die "the Command Line Tools didn't install within an hour: install them, then run this again"
        sleep 10
        waited=$((waited + 10))
    done
    note "installed"
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
        if git -C "$SRC" cat-file -e "$tag:scripts/mac/install.sh" 2>/dev/null; then
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
        return
    fi
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
}

version_of() {
    sed -n 's/^version = "\(.*\)"/\1/p' "$SRC/Cargo.toml" | head -1
}

# Downloads `url` to `file` unless it is there with `sha`, and checks the sum either way.
fetch() {
    local url=$1 file=$2 sha=$3
    if ! { [ -f "$file" ] && echo "$sha  $file" | shasum -a 256 -c - >/dev/null 2>&1; }; then
        curl -sSLf -o "$file" "$url"
        echo "$sha  $file" | shasum -a 256 -c - >/dev/null ||
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
    [ -f "$SRC/scripts/install-tools.txt" ] ||
        die "this version of JStream predates the installer: pick a newer --ref"
    TOOLS="$BUILD_DIR/tools"
    local dl="$TOOLS/downloads" triple binaryen_platform version sha
    mkdir -p "$dl" "$TOOLS/bin"
    if [ "$ARCH" = arm64 ]; then
        triple=aarch64-apple-darwin binaryen_platform=arm64-macos
    else
        triple=x86_64-apple-darwin binaryen_platform=x86_64-macos
    fi

    read -r version sha < <(tool_pin trunk "$triple")
    local trunk_version=$version
    fetch "https://github.com/trunk-rs/trunk/releases/download/v$version/trunk-$triple.tar.gz" \
        "$dl/trunk-$version-$triple.tar.gz" "$sha"
    tar xzf "$dl/trunk-$version-$triple.tar.gz" -C "$TOOLS/bin" trunk

    read -r version sha < <(tool_pin wasm-bindgen "$triple")
    local locked wb_version=$version
    locked=$(awk '/^name = "wasm-bindgen"$/ { getline; gsub(/"/, "", $3); print $3; exit }' "$SRC/Cargo.lock")
    [ "$locked" = "$version" ] ||
        die "Cargo.lock has wasm-bindgen $locked but scripts/install-tools.txt pins $version"
    local wb="wasm-bindgen-$version-$triple"
    fetch "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/$version/$wb.tar.gz" \
        "$dl/$wb.tar.gz" "$sha"
    tar xzf "$dl/$wb.tar.gz" --strip-components=1 -C "$TOOLS/bin" "$wb/wasm-bindgen"

    # wasm-opt loads libbinaryen.dylib from ../lib, so binaryen keeps its own folder.
    read -r version sha < <(tool_pin binaryen "$binaryen_platform")
    local by="binaryen-version_$version"
    fetch "https://github.com/WebAssembly/binaryen/releases/download/version_$version/$by-$binaryen_platform.tar.gz" \
        "$dl/$by-$binaryen_platform.tar.gz" "$sha"
    rm -rf "$TOOLS/binaryen"
    mkdir -p "$TOOLS/binaryen"
    tar xzf "$dl/$by-$binaryen_platform.tar.gz" --strip-components=1 -C "$TOOLS/binaryen" \
        "$by/bin/wasm-opt" "$by/lib"

    export PATH="$TOOLS/bin:$TOOLS/binaryen/bin:$PATH"
    note "trunk $trunk_version, wasm-bindgen $wb_version, wasm-opt $version"
}

build() {
    step "Building (the first build takes a while; updates are much faster)"
    local started=$SECONDS
    # The pinned toolchain, by name: rustup 1.28.0 stopped installing it on the first
    # cargo call, and rustup before 1.28 needs the name. Its components and targets then
    # come from rust-toolchain.toml on the first cargo call.
    local channel
    channel=$(sed -n 's/^channel = "\(.*\)"/\1/p' "$SRC/rust-toolchain.toml")
    [ -n "$channel" ] || die "no channel in $SRC/rust-toolchain.toml"
    rustup toolchain install --profile minimal "$channel"
    rustup target add --toolchain "$channel" wasm32-unknown-unknown
    (cd "$SRC" && cargo fetch --locked)
    UI_DIST="$BUILD_DIR/ui-dist"
    rm -rf "$UI_DIST"
    (cd "$SRC/crates/jstream-ui" && trunk build --release --locked --offline --dist "$UI_DIST")
    (cd "$SRC" && cargo build --release --locked -p jstream-daemon)
    DAEMON="$SRC/target/release/jstream-daemon"
    [ -x "$DAEMON" ] || die "the build left no $DAEMON"
    note "built in $(((SECONDS - started) / 60)) min $(((SECONDS - started) % 60)) s"
}

check_not_running() {
    if pgrep -f "$INSTALL_DIR/jstream-daemon" >/dev/null 2>&1; then
        die "JStream is running: press Ctrl-C in its Terminal window (or close it), then run this again"
    fi
}

install_files() {
    step "Installing"
    check_not_running
    local staged="$BUILD_DIR/install.new"
    rm -rf "$staged"
    mkdir -p "$staged"
    cp "$DAEMON" "$staged/jstream-daemon"
    cp -R "$UI_DIST" "$staged/ui"
    install -m 0755 "$SRC/scripts/mac/jstream.sh" "$staged/jstream.sh"
    install -m 0644 "$SRC/scripts/mac/clipboard.js" "$staged/clipboard.js"
    rm -rf "$INSTALL_DIR"
    mv "$staged" "$INSTALL_DIR"
    # A double-clickable launcher: Finder opens a .command file in Terminal.
    mkdir -p "$LAUNCHER_DIR"
    printf '#!/bin/bash\nexec "%s/jstream.sh"\n' "$INSTALL_DIR" >"$LAUNCHER"
    chmod 0755 "$LAUNCHER"
    note "$LAUNCHER (JStream $(version_of))"
}

done_message() {
    cat <<EOF_DONE

JStream is installed. To start it, open JStream.command from your Applications folder
(or Spotlight). It runs in a Terminal window: keep that window open while you use
JStream, and press Ctrl-C there (or close the window) to stop it.

It opens JStream in your browser with your access token on the clipboard for two
minutes. When the browser asks you to sign in, paste the token as the password (any
user name), then click Connect with the Token field left empty.

The first time you add a screen, camera, microphone or desktop audio, macOS asks for
permission for Terminal, which runs JStream. Screen recording takes effect once you
restart JStream. Saving a stream key may ask for your login keychain password after
each update.

Your scenes, token and logs:  $DATA_DIR
To update:                    run the same command again
To remove:                    the same command with  bash -s -- --uninstall  at the end
EOF_DONE
}

uninstall() {
    step "Removing JStream"
    check_not_running
    rm -rf "$LAUNCHER" "$BUILD_DIR"
    note "removed $LAUNCHER and $BUILD_DIR"
    if [ -n "$PURGE" ]; then
        rm -rf "$DATA_DIR"
        note "removed $DATA_DIR (scenes, token and logs)"
    else
        note "kept $DATA_DIR (scenes, token and logs); --purge removes it"
    fi
    note "Terminal keeps the permissions you gave it: remove them in System Settings ›"
    note "Privacy & Security if you no longer want them"
}

main() {
    REF="" SOURCE_DIR="" OPEN=1 UNINSTALL="" PURGE=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --ref) REF=${2:?--ref needs a tag, branch or commit}; shift ;;
            --source) SOURCE_DIR=${2:?--source needs a folder}; shift ;;
            --no-open) OPEN="" ;;
            --uninstall) UNINSTALL=1 ;;
            --purge) PURGE=1 ;;
            -h | --help) usage; exit 0 ;;
            *) die "unknown option: $1 (try --help)" ;;
        esac
        shift
    done
    [ -z "$REF" ] || [ -z "$SOURCE_DIR" ] || die "--ref and --source can't be used together"
    [ -z "$PURGE" ] || [ -n "$UNINSTALL" ] || die "--purge goes with --uninstall"
    [ "$(id -u)" != 0 ] || die "run this as yourself, not with sudo"

    if [ -n "$UNINSTALL" ]; then
        uninstall
        return
    fi
    check_mac
    check_not_running
    command_line_tools
    rust
    source_tree
    ui_tools
    build
    install_files
    done_message
    if [ -n "$OPEN" ]; then open "$LAUNCHER"; fi
}

main "$@"
