#!/usr/bin/env bash
# Build everything the persona end-to-end suite runs: the system's own
# binaries (debug profile, as shipped code with no test features) and the two
# small tools under e2e/seed. Run from anywhere; it works from the repository
# root. Afterwards: `node e2e/stack/stack.js` in one terminal and
# `node e2e/run.js` in another.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --bins
CARGO_TARGET_DIR=target cargo build --manifest-path e2e/seed/Cargo.toml

# The browser driver, at the version the lockfile pins. A machine that already
# has Playwright and its Chromium (IC_E2E_SKIP_NPM=1) keeps what it has.
if [ "${IC_E2E_SKIP_NPM:-0}" != "1" ]; then
  (cd e2e && npm ci && npx playwright install chromium)
fi
