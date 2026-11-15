#!/usr/bin/env bash
# Locks the ledger's write-once containers (merkle-roots, cold-segments,
# payments-evidence) so that nobody, the subscription owner included, can
# shorten their retention or delete them inside the period main.bicep sets
# (retentionDays, seven years plus a margin by default).
#
# IRREVERSIBLE. Once locked the period can only be lengthened, and the storage
# account cannot be deleted while a container holds a blob inside its period.
# Run once, in prod only, after infra/main.bicep has been deployed and the
# first day has closed cleanly:
#
#   tools/lock-ledger-worm.sh <resource-group> <storage-account> [retention-days]
#
# The account is the host name in the blobEndpoint output. The script refuses
# a container whose policy is not the expected period, and asks you to type
# the account name before it locks anything.
set -euo pipefail

rg=${1:?resource group}
account=${2:?storage account (from the blobEndpoint output)}
expected=${3:-2920} # main.bicep's retentionDays default
containers=(merkle-roots cold-segments payments-evidence)

declare -A etags
for c in "${containers[@]}"; do
  read -r days state etag < <(az storage container immutability-policy show \
    --resource-group "$rg" --account-name "$account" --container-name "$c" \
    --query '[immutabilityPeriodSinceCreationInDays, state, etag]' -o tsv)
  if [[ "$state" == "Locked" ]]; then
    echo "$c: already locked at $days days."
    continue
  fi
  if [[ "$days" != "$expected" ]]; then
    echo "$c: policy is $days days, not $expected. Redeploy infra/main.bicep first." >&2
    exit 1
  fi
  etags[$c]=$etag
done

if [[ ${#etags[@]} -eq 0 ]]; then
  echo "Nothing to do."
  exit 0
fi

echo "About to lock ${!etags[*]} in $account at $expected days. This cannot be undone."
read -r -p "Type the account name to confirm: " typed
if [[ "$typed" != "$account" ]]; then
  echo "Not confirmed; nothing changed." >&2
  exit 1
fi

for c in "${!etags[@]}"; do
  az storage container immutability-policy lock \
    --resource-group "$rg" --account-name "$account" --container-name "$c" \
    --if-match "${etags[$c]}" -o none
  echo "$c: locked."
done
