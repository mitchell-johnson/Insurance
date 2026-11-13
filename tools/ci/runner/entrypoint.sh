#!/usr/bin/env bash
# The runner container's entrypoint.
#
#   runner-entrypoint configure <url> <name> <labels>
#       Registers the runner with GitHub. Reads the one-hour registration
#       token on standard input, so it is never in an argument, an
#       environment variable or the container's configuration. Keeps the
#       registration in /data/state.
#   runner-entrypoint run
#       Restores the registration from /data/state and runs jobs until the
#       container is stopped.
#
# tools/ci/runner/setup.sh calls both; nothing else should need to.

set -euo pipefail

RUNNER_DIR=/opt/actions-runner
STATE=/data/state
# The files config.sh writes that make up a registration.
FILES=(.runner .credentials .credentials_rsaparams)

prepare() {
  mkdir -p "$STATE" /data/work /data/target "$CARGO_HOME" "$RUSTUP_HOME"
  # The toolchain named by rust-toolchain.toml is installed on first use;
  # rustup itself lives in the volume so its downloads are kept.
  if [[ ! -x $CARGO_HOME/bin/rustup ]]; then
    rustup-init -y --no-modify-path --profile minimal --default-toolchain none
  fi
}

case ${1:-run} in
  configure)
    url=$2 name=$3 labels=$4
    prepare
    IFS= read -r token
    [[ -n $token ]] || { echo "no registration token on standard input" >&2; exit 2; }
    cd "$RUNNER_DIR"
    ./config.sh --unattended --replace --url "$url" --token "$token" \
      --name "$name" --labels "$labels" \
      --work /data/work
    for f in "${FILES[@]}"; do cp "$f" "$STATE/$f"; done
    chmod 0600 "$STATE"/.credentials*
    echo "registered $name"
    ;;
  run)
    prepare
    for f in "${FILES[@]}"; do
      [[ -f $STATE/$f ]] || { echo "not registered: $STATE/$f is missing; run tools/ci/runner/setup.sh" >&2; exit 2; }
      cp "$STATE/$f" "$RUNNER_DIR/$f"
    done
    cd "$RUNNER_DIR"
    # run.sh restarts the listener after a self-update. Its exit status
    # reaches Docker, whose restart policy brings the runner back.
    exec ./run.sh
    ;;
  *)
    echo "usage: runner-entrypoint configure <url> <name> <labels> | run" >&2
    exit 2
    ;;
esac
