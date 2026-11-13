#!/usr/bin/env bash
# Builds, registers and starts the self-hosted GitHub Actions runners that
# ci.yml and drill.yml run on, as containers in Docker Desktop on the Mac
# mini. Run it on that Mac from a clone of this repository:
#
#   tools/ci/runner/setup.sh            # build the image, register any
#                                       # runner not yet registered, and
#                                       # (re)start every runner container
#   tools/ci/runner/setup.sh status     # what is running, and GitHub's view
#   tools/ci/runner/setup.sh remove     # unregister and delete them all
#
# Needs Docker Desktop and the GitHub CLI signed in as an admin of the
# repository. Registration tokens come from `gh` and go to the container on
# standard input; none is printed or stored. Re-running is safe: a runner
# already registered keeps its registration, work directory and build cache,
# which live in a Docker volume named after it.
#
# Why containers: launchd jobs on this Mac may not write to the external
# disk and the internal one is nearly full, while Docker Desktop's disk is on
# the external one. It also keeps CI on Linux, as on GitHub's own runners.

set -euo pipefail
cd "$(dirname "$0")"

REPO=${REPO:-mitchell-johnson/Insurance}
URL=https://github.com/$REPO
IMAGE=insurance-ci-runner:latest
HOST=mac-mini

# name label. Two runners take ci.yml's jobs so its checks and its tests run
# side by side; one takes the failure drill, which has its own queue and
# owns the Compose cluster's fixed project name, so only one can run.
RUNNERS=(
  "insurance-ci-1 insurance-ci"
  "insurance-ci-2 insurance-ci"
  "insurance-drill insurance-drill"
)

# A login agent that opens Docker Desktop, so the runners come back after a
# reboot even if Docker's own "start when you sign in" setting is off.
AGENT_LABEL=fyi.johnson.docker-desktop
AGENT=$HOME/Library/LaunchAgents/$AGENT_LABEL.plist

registered() { docker run --rm -v "$1:/data" --entrypoint test "$IMAGE" -f /data/state/.runner; }

start() {
  local name=$1
  docker rm -f "$name" >/dev/null 2>&1 || true
  # Host networking puts the runner in the Docker machine's network, where
  # the ports of the containers its jobs start are published, so a job
  # reaches them at 127.0.0.1 as it would on a hosted runner. Group 0 owns
  # the Docker socket.
  docker run -d --name "$name" --hostname "$HOST-$name" \
    --restart unless-stopped --network host --group-add 0 \
    --stop-timeout 120 \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v "$name:/data" \
    --log-opt max-size=20m --log-opt max-file=3 \
    "$IMAGE" run >/dev/null
  echo "started $name"
}

install_agent() {
  [[ -f $AGENT ]] && return
  cat >"$AGENT" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$AGENT_LABEL</string>
  <key>ProgramArguments</key>
  <array><string>/usr/bin/open</string><string>-g</string><string>-a</string><string>Docker</string></array>
  <key>RunAtLoad</key><true/>
</dict>
</plist>
EOF
  launchctl bootstrap "gui/$(id -u)" "$AGENT" 2>/dev/null || true
  echo "installed $AGENT"
}

case ${1:-up} in
  up)
    docker info >/dev/null
    install_agent
    docker build --pull -t "$IMAGE" .
    for entry in "${RUNNERS[@]}"; do
      read -r name label <<<"$entry"
      if ! registered "$name"; then
        gh api -X POST "repos/$REPO/actions/runners/registration-token" --jq .token |
          docker run --rm -i -v "$name:/data" "$IMAGE" configure "$URL" "$HOST-$name" "$label"
      fi
      start "$name"
    done
    ;;
  status)
    docker ps -a --filter "ancestor=$IMAGE" --format '{{.Names}}\t{{.Status}}'
    gh api "repos/$REPO/actions/runners" --jq '.runners[] | [.name, .status, (.busy|tostring), ([.labels[].name]|join(","))] | @tsv'
    ;;
  remove)
    for entry in "${RUNNERS[@]}"; do
      read -r name _ <<<"$entry"
      docker rm -f "$name" >/dev/null 2>&1 || true
      id=$(gh api "repos/$REPO/actions/runners" --jq ".runners[] | select(.name == \"$HOST-$name\") | .id")
      [[ -z $id ]] || gh api -X DELETE "repos/$REPO/actions/runners/$id"
      docker volume rm "$name" >/dev/null 2>&1 || true
      echo "removed $name"
    done
    ;;
  *)
    echo "usage: $0 [up|status|remove]" >&2
    exit 2
    ;;
esac
