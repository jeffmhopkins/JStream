#!/bin/bash
# JStream for macOS, built from source in one command: checks the Mac, installs what the
# build needs (Apple's Command Line Tools, Rust, pinned UI tools), builds the web UI and
# the daemon, and installs a signed JStream.app in ~/Applications. Running it again
# updates it. For testers:
#
#   curl -fsSL https://raw.githubusercontent.com/jeffmhopkins/JStream/main/scripts/mac/install.sh | bash
#
# Options (after `bash -s --` when piped):
#   --ref <ref>     build this tag, branch or commit (default: the newest release tag that
#                   has this installer, else main)
#   --source <dir>  build this checkout as it is, without cloning or fetching (CI, developers)
#   --adhoc         sign ad hoc instead of with the local signing identity (CI); macOS then
#                   asks for every permission again after each update
#   --no-open       don't open JStream when done
#   --uninstall     remove JStream.app, the build folder and the signing identity; add
#                   --purge to remove the collection, token and logs too
#
# Docs: docs/runbooks/mac-testing.md#script
#
# Everything runs from `main` at the end, so a download cut short runs nothing.

set -euo pipefail

REPO_URL=https://github.com/jeffmhopkins/JStream.git
BUNDLE_ID=io.github.jeffmhopkins.jstream
SIGNING_NAME="JStream Local Signing"
# The floor (ADR-0063 §2) and the release process taps need (§4).
MIN_MACOS=14
TAPS_MACOS=14.2
# Free disk the first build wants, in GB: the toolchain, the crates and the target folder.
MIN_FREE_GB=15

BUILD_DIR="${JSTREAM_BUILD_DIR:-$HOME/Library/Application Support/JStream-build}"
APP_DIR="${JSTREAM_APP_DIR:-$HOME/Applications}"
APP="$APP_DIR/JStream.app"
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

check_not_running() {
    if pgrep -f "$APP/Contents/" >/dev/null 2>&1; then
        die "JStream is running: quit it (JStream menu › Quit), then run this again"
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
    (cd "$SRC" && cargo fetch --locked)
    UI_DIST="$BUILD_DIR/ui-dist"
    rm -rf "$UI_DIST"
    (cd "$SRC/crates/jstream-ui" && trunk build --release --locked --offline --dist "$UI_DIST")
    (cd "$SRC" && cargo build --release --locked -p jstream-daemon)
    DAEMON="$SRC/target/release/jstream-daemon"
    [ -x "$DAEMON" ] || die "the build left no $DAEMON"
    note "built in $(((SECONDS - started) / 60)) min $(((SECONDS - started) % 60)) s"
}

# JStream.app: an AppleScript applet (a real app macOS attributes the permission prompts
# to), the daemon beside it, the UI and the launcher script in Resources.
make_app() {
    step "Making JStream.app"
    local stage="$BUILD_DIR/stage" version
    version=$(version_of)
    rm -rf "$stage"
    mkdir -p "$stage"
    local app="$stage/JStream.app"
    osacompile -s -o "$app" "$SRC/scripts/mac/JStream.applescript"
    local contents="$app/Contents"
    cp "$DAEMON" "$contents/MacOS/jstream-daemon"
    cp -R "$UI_DIST" "$contents/Resources/ui"
    install -m 0755 "$SRC/scripts/mac/start-daemon.sh" "$contents/Resources/start-daemon.sh"
    make_icon "$contents/Resources/applet.icns"
    # Newer applets name an icon in Assets.car, which would win over applet.icns.
    rm -f "$contents/Resources/Assets.car"
    local plist="$contents/Info.plist" key
    set_plist() {
        /usr/libexec/PlistBuddy -c "Delete :$1" "$plist" >/dev/null 2>&1 || true
        /usr/libexec/PlistBuddy -c "Add :$1 string $2" "$plist"
    }
    /usr/libexec/PlistBuddy -c "Delete :CFBundleIconName" "$plist" >/dev/null 2>&1 || true
    set_plist CFBundleIdentifier "$BUNDLE_ID"
    set_plist CFBundleName JStream
    set_plist CFBundleDisplayName JStream
    set_plist CFBundleShortVersionString "$version"
    set_plist CFBundleVersion "$version"
    set_plist LSMinimumSystemVersion "$MIN_MACOS.0"
    # The same usage strings as the daemon's embedded Info.plist (ADR-0063 §11).
    for key in NSCameraUsageDescription NSMicrophoneUsageDescription NSAudioCaptureUsageDescription; do
        set_plist "$key" "$(/usr/libexec/PlistBuddy -c "Print :$key" "$SRC/apps/jstream-daemon/Info.plist")"
    done
    plutil -lint -s "$plist" || die "the app's Info.plist doesn't parse"
    STAGED_APP=$app
    note "JStream $version"
}

make_icon() {
    local out=$1 png="$SRC/apps/jstream-desktop/icons/icon.png" set="$BUILD_DIR/stage/JStream.iconset" size
    mkdir -p "$set"
    for size in 16 32 128 256; do
        sips -z "$size" "$size" "$png" --out "$set/icon_${size}x${size}.png" >/dev/null
    done
    for size in 16 32 128; do
        sips -z $((size * 2)) $((size * 2)) "$png" --out "$set/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil -c icns -o "$out" "$set" || warn "couldn't make the app icon: it keeps the default one"
    rm -rf "$set"
}

# The local signing identity: a self-signed code-signing certificate in the login
# keychain, made once. Every build is signed with it, so the permissions macOS ties to
# the signature survive updates.
signing_identity() {
    if [ -n "$ADHOC" ]; then
        IDENTITY=-
        return
    fi
    step "Signing identity"
    if security find-identity -v -p codesigning 2>/dev/null | grep -q "\"$SIGNING_NAME\""; then
        note "using \"$SIGNING_NAME\""
        IDENTITY=$SIGNING_NAME
        return
    fi
    note "making \"$SIGNING_NAME\" in your login keychain: macOS asks for your password to"
    note "trust it, and may ask whether codesign can use its key (choose Always Allow)"
    local tmp keychain
    tmp=$(mktemp -d)
    keychain=$(security default-keychain -d user | sed -e 's/^[[:space:]]*"//' -e 's/"[[:space:]]*$//')
    cat >"$tmp/openssl.cnf" <<EOF
[req]
distinguished_name = dn
prompt = no
x509_extensions = ext
[dn]
CN = $SIGNING_NAME
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF
    # macOS's own LibreSSL: its PKCS#12 encryption is one `security import` reads.
    if /usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$tmp/openssl.cnf" \
        -keyout "$tmp/key.pem" -out "$tmp/cert.pem" >/dev/null 2>&1 &&
        /usr/bin/openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" \
            -name "$SIGNING_NAME" -out "$tmp/id.p12" -passout pass:jstream >/dev/null 2>&1 &&
        security import "$tmp/id.p12" -k "$keychain" -P jstream -T /usr/bin/codesign >/dev/null &&
        security add-trusted-cert -r trustRoot -p codeSign -k "$keychain" "$tmp/cert.pem"; then
        IDENTITY=$SIGNING_NAME
        note "made"
    else
        warn "couldn't make the signing identity: signing ad hoc, so macOS asks for permissions again after each update"
        IDENTITY=-
    fi
    rm -rf "$tmp"
}

sign_app() {
    step "Signing JStream.app"
    codesign --force --sign "$IDENTITY" --identifier "$BUNDLE_ID.daemon" \
        "$STAGED_APP/Contents/MacOS/jstream-daemon"
    codesign --force --sign "$IDENTITY" "$STAGED_APP"
    codesign --verify --deep --strict "$STAGED_APP" || die "the signed app doesn't verify"
    if [ "$IDENTITY" = - ]; then note "signed ad hoc"; else note "signed with \"$IDENTITY\""; fi
}

install_app() {
    step "Installing"
    mkdir -p "$APP_DIR"
    check_not_running
    rm -rf "$APP"
    mv "$STAGED_APP" "$APP"
    rm -rf "$BUILD_DIR/stage"
    note "$APP"
}

done_message() {
    cat <<EOF

JStream is installed: $APP

Open it from your Applications folder (or Spotlight). It starts JStream, copies your
access token to the clipboard and opens JStream in your browser. When the browser asks
you to sign in, paste the token as the password (any user name), then click Connect
with the Token field left empty.
The first time you add a screen, camera, microphone or desktop audio, macOS asks for
permission. A screen recording permission takes effect once JStream restarts.

Your scenes, token and logs:  $DATA_DIR
To update:                    run the same command again
To remove:                    the same command with --uninstall at the end:
                              curl -fsSL <installer URL> | bash -s -- --uninstall
EOF
}

uninstall() {
    step "Removing JStream"
    check_not_running
    rm -rf "$APP" "$BUILD_DIR"
    note "removed $APP and $BUILD_DIR"
    if security find-certificate -c "$SIGNING_NAME" >/dev/null 2>&1; then
        while security find-certificate -c "$SIGNING_NAME" >/dev/null 2>&1; do
            security delete-identity -c "$SIGNING_NAME" >/dev/null 2>&1 ||
                security delete-certificate -c "$SIGNING_NAME" >/dev/null 2>&1 || break
        done
        note "removed the signing identity \"$SIGNING_NAME\""
    fi
    tccutil reset All "$BUNDLE_ID" >/dev/null 2>&1 || true
    if [ -n "$PURGE" ]; then
        rm -rf "$DATA_DIR"
        note "removed $DATA_DIR (scenes, token and logs)"
    else
        note "kept $DATA_DIR (scenes, token and logs); --purge removes it"
    fi
}

main() {
    REF="" SOURCE_DIR="" ADHOC="" OPEN=1 UNINSTALL="" PURGE=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --ref) REF=${2:?--ref needs a tag, branch or commit}; shift ;;
            --source) SOURCE_DIR=${2:?--source needs a folder}; shift ;;
            --adhoc) ADHOC=1 ;;
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
    make_app
    signing_identity
    sign_app
    install_app
    done_message
    if [ -n "$OPEN" ]; then open "$APP"; fi
}

main "$@"
