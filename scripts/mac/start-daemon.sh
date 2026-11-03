#!/bin/bash
# JStream.app's launcher (scripts/mac/install.sh puts it in Contents/Resources): starts
# the daemon beside it with the web UI in Resources/ui, waits until it serves, and prints
# three lines for JStream.applescript: the daemon's process ID, the UI's URL and the
# control token. On failure it prints the daemon's last output and exits 1.
#
# Docs: docs/runbooks/mac-testing.md#script

set -euo pipefail

resources=$(cd "$(dirname "$0")" && pwd)
daemon="$resources/../MacOS/jstream-daemon"
# The daemon's data folder on macOS (jstream-platform's `paths::data_dir`): the token,
# the collection (the desktop app's file name) and the logs live there.
data="$HOME/Library/Application Support/JStream"
mkdir -p "$data/logs"
chmod 700 "$data"
out="$data/logs/launcher-stdout.txt"
err="$data/logs/launcher-stderr.txt"

# Detached from this script's output, so AppleScript's `do shell script` returns. The
# daemon keeps its own rotating log files in logs/; these two hold one run's output.
"$daemon" --token-file "$data/token" --collection "$data/collection.json" \
    --ui-dir "$resources/ui" </dev/null >"$out" 2>"$err" &
pid=$!

# Its last startup line is `listening on …`; the `ui: <url>` line comes just before it.
for _ in $(seq 1 300); do
    if grep -q '^listening on ' "$out" 2>/dev/null; then
        url=$(sed -n 's/^ui: \(http[^[:space:]]*\).*/\1/p' "$out" | head -1)
        [ -n "$url" ] || { echo "the daemon serves no UI" >&2; kill "$pid"; exit 1; }
        token=$(tr -d '[:space:]' <"$data/token")
        printf '%s\n%s\n%s\n' "$pid" "$url" "$token"
        exit 0
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
        tail -n 15 "$err" >&2
        exit 1
    fi
    sleep 0.1
done
kill "$pid" 2>/dev/null || true
echo "JStream didn't start within 30 seconds. Its output is in $data/logs." >&2
exit 1
