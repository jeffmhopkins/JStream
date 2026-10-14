#!/bin/bash
# SessionStart hook for Claude Code cloud sessions: makes the workspace's tests,
# lints and docs checks runnable, including the headless GPU tests.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

cd "${CLAUDE_PROJECT_DIR:-.}"

# Software Vulkan (lavapipe), as CI's test job installs it, so jstream-render's and
# jstream-core's GPU tests render here instead of skipping.
if ! find /usr/share/vulkan/icd.d -name 'lvp_icd*.json' -print -quit 2>/dev/null | grep -q .; then
  SUDO=""
  if [ "$(id -u)" != "0" ]; then SUDO="sudo"; fi
  $SUDO apt-get update -qq
  DEBIAN_FRONTEND=noninteractive $SUDO apt-get install -y -qq mesa-vulkan-drivers libvulkan1 >/dev/null
fi
icd=$(find /usr/share/vulkan/icd.d -name 'lvp_icd*.json' -print -quit 2>/dev/null || true)
if [ -n "$icd" ] && [ -n "${CLAUDE_ENV_FILE:-}" ]; then
  # Keep wgpu on lavapipe, as CI does.
  echo "export VK_DRIVER_FILES=$icd" >> "$CLAUDE_ENV_FILE"
fi

# The pinned toolchain (rust-toolchain.toml), then dependencies and the xtask
# binary, so the first cargo command in the session doesn't pay for them.
rustup toolchain install
cargo fetch --locked
cargo build -q -p xtask --locked
