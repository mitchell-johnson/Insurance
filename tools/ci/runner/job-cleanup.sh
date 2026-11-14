#!/bin/bash
# Runs at the start and end of every job (the runner's job-started and
# job-completed hooks, set in the Dockerfile) and kills whatever an earlier
# job left running in this runner container.
#
# When a job hits its time limit the runner kills the step's shell, but the
# cargo, rustc and clippy processes under it are re-parented to tini (PID 1)
# and keep running. On 14 Nov 2026 such leftovers from a timed-out run kept
# compiling for over an hour, used much of the Docker machine's memory and
# held the lock on the shared build directory, so the next runs waited
# behind them and timed out too.
#
# A runner takes one job at a time and starts each step under its
# Runner.Worker, so any process whose parent is PID 1, other than the
# runner's own run.sh, is left from an earlier job. This kills those and
# everything under them.

set -u

children() { ps -o pid= --ppid "$1" 2>/dev/null; }

tree() {
  local pid=$1 child
  for child in $(children "$pid"); do tree "$child"; done
  echo "$pid"
}

# This script and the shell that started it are not to be killed.
mine=" "
pid=$$
while [[ $pid -gt 1 ]]; do
  mine+="$pid "
  pid=$(ps -o ppid= -p "$pid" | tr -d ' ')
done

victims=()
while read -r pid comm; do
  [[ $mine == *" $pid "* ]] && continue
  case $comm in run.sh | Runner.Listener | Runner.Worker) continue ;; esac
  for p in $(tree "$pid"); do victims+=("$p"); done
done < <(ps -o pid=,comm= --ppid 1)

((${#victims[@]})) || exit 0
echo "Killing ${#victims[@]} processes left by an earlier job:"
ps -o pid,etime,rss,args -p "$(IFS=,; echo "${victims[*]}")" | cut -c1-160
kill -TERM "${victims[@]}" 2>/dev/null
for _ in $(seq 1 10); do
  alive=$(ps -o pid= -p "$(IFS=,; echo "${victims[*]}")" 2>/dev/null)
  [[ -z $alive ]] && exit 0
  sleep 1
done
# shellcheck disable=SC2086
kill -KILL $alive 2>/dev/null
exit 0
