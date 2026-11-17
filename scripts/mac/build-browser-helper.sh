#!/bin/bash
# Builds the browser helper for macOS with CEF (ADR-0157) and prints CEF's distribution
# folder, the one `cef-dll-sys`'s build downloaded the pinned CEF into and built its C++
# wrapper against (with CMake and Ninja, which must be on the path). Pass both to
# `scripts/mac/bundle-app.sh`:
#
#   cef=$(scripts/mac/build-browser-helper.sh)
#   scripts/mac/bundle-app.sh ... --browser-helper target/release/jstream-browser-helper --cef "$cef"
#
# Extra arguments go to `cargo build` (such as `--locked`). Needs jq.
#
# Docs: apps/jstream-desktop/README.md#usage

set -euo pipefail

framework="Chromium Embedded Framework.framework"
[ "$(uname -s)" = Darwin ] || { echo "error: this builds the macOS helper; run it on a Mac" >&2; exit 1; }
# The build script's link search paths are CEF's folder and the wrapper's build folder;
# CEF's is the one holding the framework.
messages=$(cargo build --release -p jstream-browser-helper --bin jstream-browser-helper \
    --features cef --message-format=json-render-diagnostics "$@")
paths=$(jq -r 'select(.reason == "build-script-executed" and (.package_id | contains("cef-dll-sys")))
    | .linked_paths[] | sub("^native="; "")' <<< "$messages")
while IFS= read -r dir; do
    if [ -n "$dir" ] && [ -d "$dir/$framework" ]; then
        echo "$dir"
        exit 0
    fi
done <<< "$paths"
echo "error: the build named no CEF folder holding $framework" >&2
exit 1
