#!/bin/bash
# The launcher scripts/mac/install.sh installs, run by ~/Applications/JStream.command in
# Terminal: starts the daemon beside it with the built UI, waits until it serves, puts
# the control token on the clipboard for two minutes (clipboard.js), opens the UI in the
# default browser, and runs until Ctrl-C or the window closes, which stop the daemon.
# Running from Terminal, the daemon's permission prompts are Terminal's (ADR-0063 §11).
#
# JSTREAM_NO_BROWSER=1 skips the clipboard and the browser, and prints the UI's URL
# (the mac-install workflow uses it).
#
# Docs: docs/runbooks/mac-testing.md#script

set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# The daemon's data folder on macOS (jstream-platform's `paths::data_dir`): the token,
# the collection (the desktop app's file name) and the logs live there.
data="$HOME/Library/Application Support/JStream"
mkdir -p "$data/logs"
chmod 700 "$data"
out="$data/logs/launcher-stdout.txt"

"$here/jstream-daemon" --token-file "$data/token" --collection "$data/collection.json" \
    --ui-dir "$here/ui" </dev/null >"$out" &
pid=$!
stop() {
    trap - INT TERM HUP EXIT
    if kill -0 "$pid" 2>/dev/null; then
        echo "Stopping JStream…"
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
}
trap stop INT TERM HUP EXIT

# Its last startup line is `listening on …`; the `ui: <url>` line comes just before it.
url=""
for _ in $(seq 1 300); do
    if grep -q '^listening on ' "$out" 2>/dev/null; then
        url=$(sed -n 's/^ui: \(http[^[:space:]]*\).*/\1/p' "$out" | head -1)
        break
    fi
    kill -0 "$pid" 2>/dev/null || {
        echo "JStream stopped while starting: its log is in $data/logs" >&2
        exit 1
    }
    sleep 0.1
done
[ -n "$url" ] || {
    echo "JStream didn't start within 30 seconds: its log is in $data/logs" >&2
    exit 1
}

if [ -n "${JSTREAM_NO_BROWSER:-}" ]; then
    echo "$url"
else
    # The token goes to clipboard.js on stdin, never on a command line. It keeps the
    # copy to this Mac and clears it after two minutes; it runs in the background.
    tr -d '[:space:]' <"$data/token" | osascript -l JavaScript "$here/clipboard.js" >/dev/null 2>&1 &
    open "$url"
    cat <<EOF

JStream is running at $url
Your access token is on the clipboard for two minutes. When the browser asks you to sign
in, paste it as the password (any user name), then click Connect with the Token field
left empty. Missed the two minutes? The token is the text in:
  $data/token

Keep this window open while you use JStream. Press Ctrl-C (or close it) to stop.
EOF
fi
wait "$pid"
