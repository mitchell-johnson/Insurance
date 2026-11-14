#!/usr/bin/env bash
# Give each workload its own cluster token and write the ledger nodes' access
# map (crates/raft/src/access.rs, docs/operations.md "Cluster access").
#
#   deploy/cluster-access/issue.sh <vault> [rotate <workload> | retire <workload>]
#
# Each workload in grants.json gets the Key Vault secret
# cluster-token-<workload>: a random 256-bit token, made once and kept. The
# map is grants.json with the SHA-256 of every token in each secret, written
# to access.json beside this script; apply it with
#
#   kubectl -n <namespace> create configmap ic-cluster-access \
#     --from-file=access.json --dry-run=client -o yaml | kubectl apply -f -
#
# The nodes read the map again within a minute (the ConfigMap's own delay
# plus 30 seconds), so no node restarts.
#
# rotate <workload>: put a new token first in the workload's secret and keep
# the old one second. Apply the map, restart the workload's pods (the CSI
# driver brings them the new secret within two minutes; they send the first
# line), then run retire <workload> and apply the map again.
#
# Needs the Azure CLI signed in as someone who may set secrets in the vault,
# openssl, sha256sum and jq. Tokens never leave Key Vault and this shell: the
# map holds only their digests and is not secret.
set -euo pipefail

if [ "$#" -lt 1 ]; then
  sed -n '2,24p' "$0" >&2
  exit 2
fi
vault=$1
action=${2:-}
target=${3:-}
here=$(cd "$(dirname "$0")" && pwd)
grants="$here/grants.json"

if [ -n "$action" ]; then
  case "$action" in rotate | retire) ;; *) echo "unknown action: $action" >&2; exit 2 ;; esac
  if [ -z "$target" ] || ! jq -e --arg w "$target" '.workloads | has($w)' "$grants" >/dev/null; then
    echo "no such workload in grants.json: ${target:-<none>}" >&2
    exit 2
  fi
fi

secret_of() { echo "cluster-token-$1"; }

read_secret() {
  az keyvault secret show --vault-name "$vault" --name "$(secret_of "$1")" --query value -o tsv 2>/dev/null || true
}

write_secret() {
  # The value goes by file, never on the command line where ps would show it.
  local file
  file=$(mktemp)
  chmod 600 "$file"
  printf '%s' "$2" >"$file"
  az keyvault secret set --vault-name "$vault" --name "$(secret_of "$1")" --file "$file" --encoding utf-8 >/dev/null
  rm -f "$file"
}

new_token() { openssl rand -hex 32; }

digests='{}'
for workload in $(jq -r '.workloads | keys[]' "$grants"); do
  value=$(read_secret "$workload")
  if [ -z "$value" ]; then
    value=$(new_token)
    write_secret "$workload" "$value"
    echo "issued $(secret_of "$workload")" >&2
  fi
  if [ "$workload" = "$target" ] && [ "$action" = rotate ]; then
    value="$(new_token)"$'\n'"$(head -n1 <<<"$value")"
    write_secret "$workload" "$value"
    echo "rotated $(secret_of "$workload"): new token first, old kept second" >&2
  elif [ "$workload" = "$target" ] && [ "$action" = retire ]; then
    value=$(head -n1 <<<"$value")
    write_secret "$workload" "$value"
    echo "retired the old token of $(secret_of "$workload")" >&2
  fi
  list='[]'
  while IFS= read -r token; do
    [ -z "$token" ] && continue
    list=$(jq -c --arg d "$(printf '%s' "$token" | sha256sum | cut -d' ' -f1)" '. + [$d]' <<<"$list")
  done <<<"$value"
  digests=$(jq -c --arg w "$workload" --argjson l "$list" '. + {($w): $l}' <<<"$digests")
done

jq --argjson d "$digests" '.workloads |= with_entries(.value.token_sha256 = $d[.key])' "$grants" >"$here/access.json"
echo "wrote $here/access.json" >&2
