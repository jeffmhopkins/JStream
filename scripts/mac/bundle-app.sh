#!/bin/bash
# Bundles the desktop app for macOS (ADR-0144): JStream.app, with the `jstream` binary,
# the web UI in Contents/Resources, the usage strings and the icon, inside a disk image
# with a link to /Applications. Ad-hoc signed by default, as an unsigned build on Apple
# Silicon must be; with --sign it signs with that Developer ID identity and the hardened
# runtime, and with --notarize it also submits the image to Apple's notary service and
# staples the ticket. Runs on a Mac: it needs codesign, hdiutil, sips, iconutil, ditto and
# PlistBuddy, which macOS has.
#
# With --browser-helper and --cef it also bundles browser sources (ADR-0157): the helper
# in Contents/MacOS beside the app, CEF's framework in Contents/Frameworks, and the
# helper apps CEF's subprocesses run from beside it, each a copy of the helper. Every
# piece is signed ad hoc from the inside out (the framework's libraries, the framework,
# each helper app, the helper), then the app.
#
#   cargo build --release -p jstream-desktop --features cef
#   cargo build --release -p jstream-browser-helper --bin jstream-browser-helper --features cef
#   (cd crates/jstream-ui && trunk build --release)
#   scripts/mac/bundle-app.sh --exe target/release/jstream --ui-dir crates/jstream-ui/dist \
#     --browser-helper target/release/jstream-browser-helper --cef <CEF's distribution folder>
#
# CEF's distribution folder is the one `cef-dll-sys`'s build downloaded, holding
# "Chromium Embedded Framework.framework"; `scripts/mac/build-browser-helper.sh` builds the
# helper and prints it.
#
# Docs: apps/jstream-desktop/README.md#usage

set -euo pipefail

usage() {
    cat <<'EOF_USAGE'
Bundles JStream.app and JStream.dmg from a built `jstream` binary and the built web UI.

  scripts/mac/bundle-app.sh --exe <jstream> --ui-dir <dist> [options]

  --exe <file>         the desktop app's release binary (target/release/jstream)
  --ui-dir <dir>       the web UI's release build (crates/jstream-ui/dist)
  --browser-helper <file>
                       the browser helper, built with `cef`, for browser sources (needs --cef;
                       the app must be built with `cef` too)
  --cef <dir>          CEF's distribution folder the helper was built against
  --out <dir>          where JStream.app and the .dmg go (default: dist)
  --revision <text>    added to the image's name, such as a commit (default: none)
  --sign <identity>    sign with this Developer ID Application identity instead of ad hoc
  --notarize <profile> submit the image with `xcrun notarytool` using this keychain
                       profile, then staple it (needs --sign)
  -h, --help           show this
EOF_USAGE
}

die() {
    echo "error: $*" >&2
    exit 1
}

# CEF's framework, in its distribution folder and in Contents/Frameworks.
FRAMEWORK="Chromium Embedded Framework.framework"
# The plain helper app's name; `jstream-browser`'s macOS helper code names the same.
HELPER_APP="JStream Helper"

main() {
    local exe="" ui="" out="dist" revision="" identity="" profile="" helper="" cef=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --exe) exe=${2:?--exe needs a file}; shift 2 ;;
            --ui-dir) ui=${2:?--ui-dir needs a folder}; shift 2 ;;
            --browser-helper) helper=${2:?--browser-helper needs a file}; shift 2 ;;
            --cef) cef=${2:?--cef needs a folder}; shift 2 ;;
            --out) out=${2:?--out needs a folder}; shift 2 ;;
            --revision) revision=${2:?--revision needs a value}; shift 2 ;;
            --sign) identity=${2:?--sign needs an identity}; shift 2 ;;
            --notarize) profile=${2:?--notarize needs a keychain profile}; shift 2 ;;
            -h | --help) usage; exit 0 ;;
            *) usage >&2; die "unknown option $1" ;;
        esac
    done
    [ "$(uname -s)" = Darwin ] || die "this bundles a macOS app; run it on a Mac"
    [ -n "$exe" ] && [ -x "$exe" ] || die "--exe must name the built jstream binary"
    [ -n "$ui" ] && [ -f "$ui/index.html" ] || die "--ui-dir must name a built UI (no index.html in '$ui')"
    [ -z "$profile" ] || [ -n "$identity" ] || die "--notarize needs --sign: Apple notarises only Developer ID builds"
    if [ -n "$helper" ] || [ -n "$cef" ]; then
        [ -n "$helper" ] && [ -x "$helper" ] || die "--browser-helper must name the built jstream-browser-helper"
        [ -n "$cef" ] && [ -d "$cef/$FRAMEWORK" ] || die "--cef must name CEF's distribution folder (no $FRAMEWORK in '$cef')"
        # CEF's helpers need hardened-runtime entitlements a Developer ID build would have
        # to add; macOS builds stay unsigned (ROADMAP#macos-unsigned), so there are none.
        [ -z "$identity" ] || die "--sign doesn't take browser sources: the helper apps have no hardened-runtime entitlements"
    fi

    local here root app version short arch name
    here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
    root=$(cd "$here/../.." && pwd)
    version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -1)
    [ -n "$version" ] || die "no workspace version in Cargo.toml"
    # Bundle versions are up to three numbers: the release without its pre-release part.
    short=${version%%-*}
    arch=$(lipo -archs "$exe" 2>/dev/null || uname -m)
    arch=${arch// /-}
    name="JStream-$version-macos-$arch${revision:+-$revision}"

    mkdir -p "$out"
    app="$out/JStream.app"
    rm -rf "$app"
    mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
    cp "$exe" "$app/Contents/MacOS/jstream"
    # The app finds its UI in Contents/Resources (`app::bundled_ui_dir`).
    cp -R "$ui" "$app/Contents/Resources/ui"
    icon "$root/apps/jstream-desktop/icons/icon.png" "$app/Contents/Resources/JStream.icns"
    plist "$root/apps/jstream-desktop/Info.plist" "$app/Contents/Info.plist" "$short"
    if [ -n "$helper" ]; then
        browser "$app" "$helper" "$cef" "$short"
    fi

    if [ -n "$identity" ]; then
        codesign --force --timestamp --options runtime \
            --entitlements "$root/apps/jstream-desktop/entitlements.plist" \
            --sign "$identity" "$app"
    else
        codesign --force --sign - "$app"
    fi
    codesign --verify --deep --strict --verbose=2 "$app"

    local dmg
    # Global, for the trap.
    STAGE=$(mktemp -d)
    trap 'rm -rf "$STAGE"' EXIT
    local stage=$STAGE
    cp -R "$app" "$stage/"
    ln -s /Applications "$stage/Applications"
    dmg="$out/$name.dmg"
    rm -f "$dmg"
    hdiutil create -quiet -volname JStream -srcfolder "$stage" -ov -format UDZO "$dmg"
    if [ -n "$identity" ]; then
        codesign --force --timestamp --sign "$identity" "$dmg"
    fi
    if [ -n "$profile" ]; then
        xcrun notarytool submit "$dmg" --keychain-profile "$profile" --wait
        xcrun stapler staple "$dmg"
    fi
    echo "$app"
    echo "$dmg"
}

# Browser sources (ADR-0157): the helper beside the app, CEF's framework, the helper apps
# its subprocesses run from, and CEF's version and credits; each signed ad hoc from the
# inside out, before the app is.
browser() {
    local app=$1 helper=$2 cef=$3 version=$4 frameworks id variant name bundle lib
    frameworks="$app/Contents/Frameworks"
    mkdir -p "$frameworks"
    # The browser process: CEF's library loader finds the framework in ../Frameworks.
    cp "$helper" "$app/Contents/MacOS/jstream-browser-helper"
    # `ditto` keeps the framework's links and modes as CEF ships them.
    ditto "$cef/$FRAMEWORK" "$frameworks/$FRAMEWORK"
    id=$(/usr/libexec/PlistBuddy -c "Print :CFBundleIdentifier" "$app/Contents/Info.plist")
    # The helper apps: the plain one the helper names to CEF, and the variants Chromium
    # finds beside it by name. Each runs a copy of the helper, which loads the framework
    # three folders up.
    for variant in "" " (GPU)" " (Renderer)" " (Plugin)" " (Alerts)"; do
        name="$HELPER_APP$variant"
        bundle="$frameworks/$name.app"
        mkdir -p "$bundle/Contents/MacOS"
        cp "$helper" "$bundle/Contents/MacOS/$name"
        helper_plist "$bundle/Contents/Info.plist" "$name" "$id.helper$(suffix "$variant")" "$version"
    done
    cp "$cef/CREDITS.html" "$app/Contents/Resources/CEF-CREDITS.html"
    cef_version "$cef/archive.json" > "$app/Contents/Resources/CEF-VERSION.txt"

    # Inside out: the framework's own libraries, the framework, each helper app, then the
    # helper; the app itself is signed last, by the caller.
    while IFS= read -r -d '' lib; do
        codesign --force --sign - "$lib"
    done < <(find "$frameworks/$FRAMEWORK" -type f \( -name '*.dylib' -o -name '*.so' \) -print0)
    codesign --force --sign - "$frameworks/$FRAMEWORK"
    for variant in "" " (GPU)" " (Renderer)" " (Plugin)" " (Alerts)"; do
        codesign --force --sign - "$frameworks/$HELPER_APP$variant.app"
    done
    codesign --force --sign - "$app/Contents/MacOS/jstream-browser-helper"
}

# A helper app's bundle identifier suffix: "" for the plain one, else ".gpu", ".renderer"
# and so on, as CEF's own helpers are named.
suffix() {
    local variant=$1
    variant=${variant# (}
    variant=${variant%)}
    [ -z "$variant" ] || echo ".$(echo "$variant" | tr '[:upper:]' '[:lower:]')"
}

# A helper app's Info.plist: an agent app (no Dock icon) of the app's version, with CEF's
# recommended environment.
# Adds one key to the plist at $1: `plist_key <plist> <key> <type> [<value>]`.
plist_key() { /usr/libexec/PlistBuddy -c "Add :$2 $3 ${4-}" "$1" >/dev/null; }

helper_plist() {
    local to=$1 name=$2 id=$3 version=$4
    rm -f "$to"
    plist_key "$to" CFBundleDevelopmentRegion string en
    plist_key "$to" CFBundleExecutable string "$name"
    plist_key "$to" CFBundleIdentifier string "$id"
    plist_key "$to" CFBundleInfoDictionaryVersion string 6.0
    plist_key "$to" CFBundleName string "$name"
    plist_key "$to" CFBundlePackageType string APPL
    plist_key "$to" CFBundleShortVersionString string "$version"
    plist_key "$to" CFBundleVersion string "$version"
    plist_key "$to" LSEnvironment dict
    # CEF's helpers turn the nano allocator off (its own helper Info.plist does).
    plist_key "$to" LSEnvironment:MallocNanoZone string 0
    plist_key "$to" LSFileQuarantineEnabled bool true
    plist_key "$to" LSMinimumSystemVersion string 14.0
    plist_key "$to" LSUIElement bool true
    plist_key "$to" NSSupportsAutomaticGraphicsSwitching bool true
    plutil -lint "$to" >/dev/null
}

# CEF-VERSION.txt: the CEF build the framework came from, named in its archive.json, and
# where its and Chromium's sources are, as the Windows zip and Linux packages say.
cef_version() {
    local name
    name=$(plutil -extract name raw -o - "$1") || die "no archive name in $1"
    name=${name%.tar.bz2}
    printf '%s\n\nThe Chromium Embedded Framework, from https://cef-builds.spotifycdn.com.\nIts sources and Chromium'"'"'s: https://bitbucket.org/chromiumembedded/cef.\n' "$name"
}

# The app icon: the 256 px PNG at each size an .icns holds up to it.
icon() {
    local png=$1 icns=$2 set size
    set=$(mktemp -d)/JStream.iconset
    mkdir -p "$set"
    for size in 16 32 128 256; do
        sips -z "$size" "$size" "$png" --out "$set/icon_${size}x${size}.png" >/dev/null
    done
    for size in 16 32 128; do
        sips -z $((size * 2)) $((size * 2)) "$png" --out "$set/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil --convert icns --output "$icns" "$set"
    rm -rf "$(dirname "$set")"
}

# Contents/Info.plist: the usage strings the binary embeds, plus the bundle's own keys.
plist() {
    local from=$1 to=$2 version=$3 key
    cp "$from" "$to"
    set_key() {
        /usr/libexec/PlistBuddy -c "Delete :$1" "$to" >/dev/null 2>&1 || true
        /usr/libexec/PlistBuddy -c "Add :$1 $2 $3" "$to"
    }
    set_key CFBundleName string JStream
    set_key CFBundleDisplayName string JStream
    set_key CFBundleExecutable string jstream
    set_key CFBundleIconFile string JStream
    set_key CFBundlePackageType string APPL
    set_key CFBundleShortVersionString string "$version"
    set_key CFBundleVersion string "$version"
    set_key CFBundleInfoDictionaryVersion string 6.0
    # ADR-0063's floor.
    set_key LSMinimumSystemVersion string 14.0
    set_key NSHighResolutionCapable bool true
    set_key NSSupportsAutomaticGraphicsSwitching bool true
    for key in NSCameraUsageDescription NSMicrophoneUsageDescription NSAudioCaptureUsageDescription; do
        /usr/libexec/PlistBuddy -c "Print :$key" "$to" >/dev/null || die "Info.plist has no $key"
    done
    plutil -lint "$to" >/dev/null
}

main "$@"
