#!/usr/bin/env bash
# Break the Compose cluster on purpose and check the insurer's books survive.
#
#   deploy/compose/chaos.sh          # build, run, tear down
#   KEEP=1 deploy/compose/chaos.sh   # leave the cluster up afterwards
#
# What it does, against real containers:
#   1. brings up three ledger nodes, Temporal, two workers and the API
#   2. kills a ledger node while policies are being issued
#   3. kills a worker and a ledger node in the middle of the end-of-day close
#   4. runs the close again and expects the same published root
#   5. destroys a node's disk, checks the node is shut out under its old
#      number, replaces it with a new node, then loses another old node
#   6. checks after every step that the books reconcile
#
# Needs docker (with compose) and curl. Exits non-zero at the first surprise.

set -euo pipefail
cd "$(dirname "$0")"

COMPOSE=(docker compose -f docker-compose.yml --profile replacement --profile tools)
# The host ports can be moved, as CI does, with IC_COMPOSE_API_PORT and the
# other IC_COMPOSE_*_PORT variables in docker-compose.yml.
API=http://localhost:${IC_COMPOSE_API_PORT:-8080}
DATE=2027-11-15
NEXT=2027-11-16
PARTITIONS=4
export OPENING_DATE=$DATE

step() { printf '\n== %s\n' "$*"; }

fail() {
  printf '\nFAILED: %s\n' "$*" >&2
  "${COMPOSE[@]}" ps >&2 || true
  # The cluster is torn down on exit, so this is the only look at its logs.
  "${COMPOSE[@]}" logs --no-color --tail 40 >&2 || true
  exit 1
}

# api <roles> <curl arguments...>
api() {
  local roles=$1
  shift
  curl -sS -m 60 -H 'x-dev-subject: chaos' -H "x-dev-roles: $roles" "$@"
}

issue() {
  local n body
  n=$(printf '%08d' "$1")
  body=$(sed "s/@N@/$n/g; s/@DATE@/$DATE/" application.json.tmpl)
  api new-business -X POST "$API/v1/policies" -H 'content-type: application/json' \
    -H "idempotency-key: chaos-$n" -d "$body"
}

# Issue policies <from>..<to>; each must be applied. A 503 means the ledger
# could not confirm in time: repeat with the same key, as a real caller does.
issue_range() {
  local n out attempt
  for n in $(seq "$1" "$2"); do
    for attempt in 1 2 3 4 5 6; do
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
  for i in 1 2 3 4 5 6; do
    out=$(api operations "$API/v1/admin/reconciliation" || true)
    if grep -q '"clean":true' <<<"$out"; then return; fi
    sleep 5
  done
  fail "the books do not reconcile: $out"
}

# A close that has not finished in fifteen minutes will not: fail here, with
# the cluster's logs, instead of waiting for the job's own time limit.
close_day() {
  timeout 900 "${COMPOSE[@]}" run --rm -T close close "$DATE" "$PARTITIONS"
}

root_of() {
  tr -d ' \n' <"$1" | sed -n 's/.*"root":"\([0-9a-f]*\)".*/\1/p'
}

cleanup() {
  if [[ ${KEEP:-0} != 1 ]]; then
    "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
  fi
  rm -f "$WORK"/close-*.json
  rmdir "$WORK" 2>/dev/null || true
}
WORK=$(mktemp -d)
trap cleanup EXIT

step "Build and start a fresh cluster"
"${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
docker compose -f docker-compose.yml up --build -d
wait_ready "$DATE"

step "Issue policies, then kill a ledger node and keep issuing"
issue_range 1 20
"${COMPOSE[@]}" kill node-2
issue_range 21 30
policy 1 | grep -q '"status":"in_force"' || fail "policy 1 cannot be read with a node down"
"${COMPOSE[@]}" start node-2
reconciles

step "A payment quoting the policy number is applied once"
due=$(policy 7 | sed -n 's/.*"outstanding_minor":\([0-9]*\).*/\1/p')
[[ -n $due && $due -gt 0 ]] || fail "policy 7 owes nothing after issue"
pay="{\"id\":\"rail-7\",\"amount\":{\"minor\":$due,\"currency\":\"NZD\"},\"reference\":\"KTL 0000 0007\",\"payer_account\":null,\"received_on\":\"$DATE\"}"
api cash-matching -X POST "$API/v1/payments" -H 'content-type: application/json' -d "$pay" | grep -q '"status":"applied"' || fail "the payment was not applied"
api cash-matching -X POST "$API/v1/payments" -H 'content-type: application/json' -d "$pay" | grep -q '"duplicate":true' || fail "the payment was applied twice"
policy 7 | grep -q '"outstanding_minor":0' || fail "policy 7 still owes after its payment"

step "Close the day while a worker and a ledger node are killed"
close_day >"$WORK/close-1.json" &
closing=$!
sleep 2
"${COMPOSE[@]}" kill worker-1
"${COMPOSE[@]}" kill node-1
wait "$closing" || fail "the close did not finish: $(cat "$WORK/close-1.json")"
grep -q '"breaks": \[\]' "$WORK/close-1.json" || fail "the close reported breaks: $(cat "$WORK/close-1.json")"
"${COMPOSE[@]}" start node-1 worker-1
wait_ready "$NEXT"
reconciles

step "Closing the same day again changes nothing and gives the same root"
close_day >"$WORK/close-2.json" || fail "the repeated close failed"
first=$(root_of "$WORK/close-1.json")
second=$(root_of "$WORK/close-2.json")
[[ -n $first && $first == "$second" ]] || fail "the repeated close published another root: '$first' then '$second'"
wait_ready "$NEXT"

step "Destroy node 3's disk and start it again under its old number"
"${COMPOSE[@]}" rm -sf node-3
docker volume rm insurance-core_node-3 >/dev/null
docker compose -f docker-compose.yml up -d node-3
issue_range 31 35
shut_out=0
for _ in $(seq 1 30); do
  # To a file first: grep -q stops reading at the first match, and with
  # pipefail a log longer than the pipe then counts as a failure.
  "${COMPOSE[@]}" logs --no-color node-3 >"$WORK/node-3.log" 2>&1 || true
  if grep -q 'another incarnation' "$WORK/node-3.log"; then shut_out=1; break; fi
  sleep 2
done
[[ $shut_out == 1 ]] || fail "the wiped node was not shut out under its old number"

step "Replace it with a new node 4, then lose node 1 as well"
"${COMPOSE[@]}" up -d node-4
"${COMPOSE[@]}" run --rm -T admin members all 1=node-1:7000,2=node-2:7000,4=node-4:7000 >/dev/null || fail "the membership change failed"
"${COMPOSE[@]}" rm -sf node-3
"${COMPOSE[@]}" kill node-1
issue_range 36 40
policy 1 | grep -q '"status":"in_force"' || fail "policy 1 was lost with the replaced node"
policy 7 | grep -q '"outstanding_minor":0' || fail "policy 7's payment was lost with the replaced node"
"${COMPOSE[@]}" start node-1
reconciles

printf '\nPASSED: 40 policies issued, one day closed, three node failures, one worker failure, one disk lost and replaced. The books reconcile.\n'
