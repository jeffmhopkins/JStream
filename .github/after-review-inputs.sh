#!/usr/bin/env bash
# Prints a hash of what the Windows `clippy` and `test` jobs build and run from: every file
# in HEAD's tree but the docs no build reads, which are `docs/` (except `docs/facts.toml`,
# which a test includes) and the Markdown files at the root (ADR-0248). Two heads with the
# same hash give those jobs the same inputs, so a pass at one carries to the other.
set -euo pipefail
git ls-tree -r --full-tree HEAD \
  | awk -F '\t' '!(($2 ~ /^docs\// && $2 != "docs/facts.toml") || $2 ~ /^[^\/]+\.md$/)' \
  | sha256sum | cut -d ' ' -f 1
