#!/usr/bin/env bash
# Lose a whole region on purpose and check the ledger carries on, then hands
# leadership back by itself, with the books intact (regions.yml).
#
#   deploy/compose/regions.sh          # build, run, tear down
#   KEEP=1 deploy/compose/regions.sh   # leave the cluster up afterwards
#
# What it does, against real containers, with nobody stepping in:
#   1. brings up five voters in three "regions" and checks both main regions
#      lead their share of the partitions and the witness leads none
#   2. cuts Sydney off from the network (its nodes keep running, as in a
#      real region outage) while policies are issued; Melbourne and the
#      witness carry every partition
#   3. joins Sydney back and waits for fail-back to return its partitions
#   4. stops Melbourne, issues policies (only Sydney and the witness hold
#      them), then cuts Sydney off too and starts Melbourne: the witness
#      alone holds those policies and must hand them to Melbourne
#   5. joins Sydney back; every policy is readable, the books reconcile and
#      the leadership is shared out as at the start
#
# Needs docker (with compose) and curl. Exits non-zero at the first surprise.

set -euo pipefail
cd "$(dirname "$0")"

COMPOSE=(docker compose -f regions.yml --profile tools)
API=http://localhost:${IC_COMPOSE_API_PORT:-8080}
DATE=2027-11-15
PARTITIONS=8
export OPENING_DATE=$DATE

step() { printf '\n== %s\n' "$*"; }

fail() {
  printf '\nFAILED: %s\n' "$*" >&2
  "${COMPOSE[@]}" ps >&2 || true
  "${COMPOSE[@]}" logs --no-color --tail 40 >&2 || true
  exit 1
}

api() {
  local roles=$1
  shift
  curl -sS -m 60 -H 'x-dev-subject: regions' -H "x-dev-roles: $roles" "$@"
}

issue() {
  local n body
  n=$(printf '%08d' "$1")
  body=$(sed "s/@N@/$n/g; s/@DATE@/$DATE/" application.json.tmpl)
  api new-business -X POST "$API/v1/policies" -H 'content-type: application/json' \
    -H "idempotency-key: regions-$n" -d "$body"
}

# Issue policies <from>..<to>; each must be applied. While a region is being
# lost the ledger may answer 503 for a few seconds: repeat with the same key.
issue_range() {
  local n out attempt
  for n in $(seq "$1" "$2"); do
    for attempt in $(seq 1 15); do
      out=$(issue "$n" || true)
      if grep -q '"status":"applied"' <<<"$out"; then continue 2; fi
      grep -q 'ledger_unavailable' <<<"$out" || fail "policy $n was not applied: $out"
      sleep 2
    done
    fail "policy $n was never confirmed: $out"
  done
}

policy() {
  api policy-servicing "$API/v1/policies/KTL$(printf '%08d' "$1")"
}

# Every policy <from>..<to> can be read back.
readable() {
  local n
  for n in $(seq "$1" "$2"); do
    policy "$n" | grep -q '"status":"in_force"' || fail "policy $n cannot be read: $(policy "$n")"
  done
}

wait_ready() {
  local i
  for i in $(seq 1 150); do
    if curl -fsS -m 5 "$API/readyz" 2>/dev/null | grep -q "\"business_date\":\"$1\""; then return; fi
    sleep 2
  done
  fail "the API never reported business date $1"
}

reconciles() {
  local out i
  for i in $(seq 1 12); do
    out=$(api operations "$API/v1/admin/reconciliation" || true)
    if grep -q '"clean":true' <<<"$out"; then return; fi
    sleep 5
  done
  fail "the books do not reconcile: $out"
}

# "partition=leader" for every partition whose leader answered, from the
# voters' own status: `ic-admin status` prints each voter's rows in the order
# id, partition, role.
leaders() {
  "${COMPOSE[@]}" run --rm -T admin status 2>/dev/null |
    awk '/"id":/ { gsub(/[^0-9]/, "", $2); id = $2 }
         /"partition":/ { gsub(/[^0-9]/, "", $2); p = $2 }
         /"role": "leader"/ { print p "=" id }' | sort -n | tr '\n' ' '
}

# The leadership as it should be with every region up: even partitions led
# in Sydney (1 or 2), odd ones in Melbourne (3 or 4), the witness none.
shared_out() {
  local map=$1 p want
  for p in $(seq 0 $((PARTITIONS - 1))); do
    if ((p % 2 == 0)); then want='[12]'; else want='[34]'; fi
    grep -Eq "(^| )$p=$want " <<<"$map" || return 1
  done
}

# Every partition led by one of the given voters, e.g. `led_by '[34]'`.
led_by() {
  local map=$1 who=$2 p
  for p in $(seq 0 $((PARTITIONS - 1))); do
    grep -Eq "(^| )$p=$who " <<<"$map" || return 1
  done
}

# wait_for <what> <seconds> <check> [args]: poll the leadership until the
# check holds.
wait_for() {
  local what=$1 seconds=$2 map i
  shift 2
  for i in $(seq 1 $((seconds / 3))); do
    map=$(leaders || true)
    if "$@" "$map"; then
      printf '   %s: %s\n' "$what" "$map"
      return
    fi
    sleep 3
  done
  fail "$what did not happen within $seconds seconds; leaders: ${map:-none answered}"
}

shared() { shared_out "$1"; }
melbourne() { led_by "$1" '[34]'; }
sydney() { led_by "$1" '[12]'; }

container() { "${COMPOSE[@]}" ps -q "$1"; }
NETWORK=insurance-core-regions_default
cut_off() {
  local node
  for node in "$@"; do docker network disconnect "$NETWORK" "$(container "$node")"; done
}
join() {
  local node
  for node in "$@"; do docker network connect "$NETWORK" "$(container "$node")"; done
}

cleanup() {
  if [[ ${KEEP:-0} != 1 ]]; then
    "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

step "Build and start five voters in three regions"
"${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
docker compose -f regions.yml up --build -d
wait_ready "$DATE"
wait_for "both regions lead their share" 180 shared
issue_range 1 10

step "Cut Sydney off while policies are issued"
cut_off node-1 node-2
issue_range 11 25
wait_for "Melbourne leads every partition" 120 melbourne
readable 1 25

step "Sydney comes back and takes its partitions back by itself"
join node-1 node-2
wait_for "fail-back shares the leadership out again" 240 shared
reconciles

step "Stop Melbourne; Sydney and the witness carry on"
"${COMPOSE[@]}" stop node-3 node-4
issue_range 26 35
wait_for "Sydney leads every partition" 120 sydney

step "Lose Sydney too, then start Melbourne: only the witness holds policies 26 to 35"
cut_off node-1 node-2
"${COMPOSE[@]}" start node-3 node-4
wait_for "Melbourne leads every partition, caught up by the witness" 180 melbourne
readable 1 35
issue_range 36 40

step "Sydney comes back; everything is where it should be"
join node-1 node-2
wait_for "fail-back shares the leadership out again" 240 shared
readable 1 40
reconciles

printf '\nPASSED: 40 policies issued, Sydney cut off twice and Melbourne stopped, the witness carried what only it held, and leadership came back by itself. The books reconcile.\n'
