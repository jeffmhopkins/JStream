#!/bin/bash
# Bundles the desktop app for macOS (ADR-0141): JStream.app, with the `jstream` binary,
# the web UI in Contents/Resources, the usage strings and the icon, inside a disk image
# with a link to /Applications. Ad-hoc signed by default, as an unsigned build on Apple
# Silicon must be; with --sign it signs with that Developer ID identity and the hardened
# runtime, and with --notarize it also submits the image to Apple's notary service and
# staples the ticket. Runs on a Mac: it needs codesign, hdiutil, sips, iconutil and
# PlistBuddy, which macOS has.
#
#   cargo build --release -p jstream-desktop
#   (cd crates/jstream-ui && trunk build --release)
#   scripts/mac/bundle-app.sh --exe target/release/jstream --ui-dir crates/jstream-ui/dist
#
# Docs: apps/jstream-desktop/README.md#usage

set -euo pipefail

usage() {
    cat <<'EOF_USAGE'
Bundles JStream.app and JStream.dmg from a built `jstream` binary and the built web UI.

  scripts/mac/bundle-app.sh --exe <jstream> --ui-dir <dist> [options]

  --exe <file>         the desktop app's release binary (target/release/jstream)
  --ui-dir <dir>       the web UI's release build (crates/jstream-ui/dist)
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

main() {
    local exe="" ui="" out="dist" revision="" identity="" profile=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --exe) exe=${2:?--exe needs a file}; shift 2 ;;
            --ui-dir) ui=${2:?--ui-dir needs a folder}; shift 2 ;;
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

    if [ -n "$identity" ]; then
        codesign --force --timestamp --options runtime \
            --entitlements "$root/apps/jstream-desktop/entitlements.plist" \
            --sign "$identity" "$app"
    else
        codesign --force --sign - "$app"
    fi
    codesign --verify --strict --verbose=2 "$app"

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
