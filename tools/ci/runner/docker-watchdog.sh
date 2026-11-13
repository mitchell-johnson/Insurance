#!/bin/bash
# Keeps Docker Desktop, and with it the CI runners, running on the Mac mini.
# setup.sh installs it as a login agent that runs at login and every five
# minutes.
#
# At login it opens Docker Desktop. After that, if the Docker engine has not
# answered for ten minutes, it quits Docker Desktop and opens it again. That
# is the recovery Docker needs after its engine stops on an error it does not
# retry: on 13 Nov 2026 the Mac's internal disk filled for a moment, Docker
# could not write its VM log, stopped the engine and waited for someone to
# click Quit, and CI sat queued for over an hour.
#
# It does not restart while the internal disk has less than 2 GB free, since
# Docker would only fail again; it logs that instead.

set -u
PATH=/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin

STATE=${HOME}/Library/Application\ Support/insurance-ci
LOG=$STATE/docker-watchdog.log
SINCE=$STATE/docker-down-since
GRACE=600
MIN_FREE_KB=$((2 * 1024 * 1024))

mkdir -p "$STATE"
log() { printf '%s %s\n' "$(date -u +%FT%TZ)" "$*" >>"$LOG"; }

# Keep the log short.
if [[ -f $LOG && $(wc -l <"$LOG") -gt 2000 ]]; then
  tail -n 1000 "$LOG" >"$LOG.tmp" && mv "$LOG.tmp" "$LOG"
fi

if docker info >/dev/null 2>&1; then
  rm -f "$SINCE"
  exit 0
fi

if ! pgrep -qf 'Docker.app/Contents/MacOS/com.docker.backend'; then
  log "Docker Desktop is not running: opening it"
  open -g -a Docker
  date +%s >"$SINCE"
  exit 0
fi

now=$(date +%s)
[[ -f $SINCE ]] || { echo "$now" >"$SINCE"; exit 0; }
down=$((now - $(cat "$SINCE")))
((down >= GRACE)) || exit 0

free_kb=$(df -k / | awk 'NR == 2 { print $4 }')
if ((free_kb < MIN_FREE_KB)); then
  log "engine down ${down}s but only $((free_kb / 1024)) MB free on the internal disk: not restarting"
  exit 0
fi

log "engine down ${down}s: restarting Docker Desktop"
osascript -e 'quit app "Docker Desktop"' >/dev/null 2>&1
for _ in $(seq 1 60); do
  pgrep -qf 'Docker.app/Contents/MacOS/com.docker.backend' || break
  sleep 2
done
pkill -f 'Docker.app/Contents/MacOS/com.docker.backend' 2>/dev/null
open -g -a Docker
echo "$now" >"$SINCE"
