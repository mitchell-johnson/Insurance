#!/usr/bin/env bash
# Locks the customer master's erasure register (`customers-erasures`) so that
# nobody, the subscription owner included, can shorten its retention or delete
# it before every page in it is 10 years old. Agreed by Mitchell on 2026-11-13.
#
# IRREVERSIBLE. Once locked the period can only be lengthened, and the storage
# account cannot be deleted while the container holds a page inside its period.
# Run once, in prod only, after `infra/customers.bicep` has been deployed:
#
#   tools/lock-erasure-register.sh <resource-group> <storage-account>
#
# The account name is the `registerAccountName` output of the deployment. The
# script refuses unless the policy is the agreed period and still unlocked,
# and asks you to type the account name before it locks.
set -euo pipefail

AGREED_DAYS=3660 # registerYears (10) * 366, as infra/customers.bicep sets it
CONTAINER=customers-erasures

rg=${1:?resource group}
account=${2:?storage account (output registerAccountName)}

policy=$(az storage container immutability-policy show \
  --resource-group "$rg" --account-name "$account" --container-name "$CONTAINER" \
  --query '{days: immutabilityPeriodSinceCreationInDays, state: state, etag: etag}' -o tsv)
read -r days state etag <<<"$policy"

if [[ "$state" == "Locked" ]]; then
  echo "Already locked at $days days. Nothing to do."
  exit 0
fi
if [[ "$days" != "$AGREED_DAYS" ]]; then
  echo "Policy is $days days, not the agreed $AGREED_DAYS. Redeploy infra/customers.bicep first." >&2
  exit 1
fi

echo "About to lock $account/$CONTAINER at $days days. This cannot be undone."
read -r -p "Type the account name to confirm: " typed
if [[ "$typed" != "$account" ]]; then
  echo "Not confirmed; nothing changed." >&2
  exit 1
fi

az storage container immutability-policy lock \
  --resource-group "$rg" --account-name "$account" --container-name "$CONTAINER" \
  --if-match "$etag" -o none

az storage container immutability-policy show \
  --resource-group "$rg" --account-name "$account" --container-name "$CONTAINER" \
  --query '{days: immutabilityPeriodSinceCreationInDays, state: state}' -o table
