#!/usr/bin/env bash
# The group service's Entra ID app registration, its app roles, and who holds
# them. Safe to run again: what exists is left as it is, what is missing is
# made, and a role is assigned once.
#
#   deploy/entra/group-app.sh <name> <group-identity-principal-id> \
#       [<role>=<principal-object-id> ...] [customers=<customer-master-app-id-uri>]
#
# <name> is the prefix used in infra/*.bicep. <group-identity-principal-id> is
# the object ID of the managed identity infra/group.bicep made (`az identity
# show -g <rg> -n <name>-group --query principalId`). Each <role>=<id> gives a
# calling service's managed identity one of the caller names this service
# takes: employer-portal, member-portal, group-admin, underwriting,
# reinsurance, cash-matching. With customers=<uri>, the group service's own
# identity is given the customer master's `group-admin` role, which its
# matching calls need.
#
# Needs the Azure CLI, signed in as someone who may create app registrations
# and grant app roles (Cloud Application Administrator or above), and jq.
# Prints the value for IC_GROUP_AUDIENCE in deploy/k8s/group.yaml.
set -euo pipefail

if [ "$#" -lt 2 ]; then
  sed -n '2,20p' "$0" >&2
  exit 2
fi
name=$1
group_principal=$2
shift 2
uri="api://${name}-group"
graph="https://graph.microsoft.com/v1.0"

# One app role per caller name. The IDs are fixed so that a rerun never
# changes a role that is already assigned.
roles='[
  {"value":"group-admin","id":"5d6f0a4e-3c1b-4a2e-9f10-6a1b2c3d4e01","description":"Group scheme operations staff, through the staff console"},
  {"value":"employer-portal","id":"5d6f0a4e-3c1b-4a2e-9f10-6a1b2c3d4e02","description":"The employer portal, acting for an employer administrator"},
  {"value":"member-portal","id":"5d6f0a4e-3c1b-4a2e-9f10-6a1b2c3d4e03","description":"The member app, acting for a scheme member"},
  {"value":"underwriting","id":"5d6f0a4e-3c1b-4a2e-9f10-6a1b2c3d4e04","description":"The underwriting module, with decisions on cover above the automatic amount"},
  {"value":"reinsurance","id":"5d6f0a4e-3c1b-4a2e-9f10-6a1b2c3d4e05","description":"The reinsurance module, requiring cover to be underwritten"},
  {"value":"cash-matching","id":"5d6f0a4e-3c1b-4a2e-9f10-6a1b2c3d4e06","description":"Cash matching, reporting a short payment"}
]'
app_roles=$(jq -c 'map({allowedMemberTypes:["Application"], displayName:.value, description, id, isEnabled:true, value})' <<<"$roles")

role_id() {
  jq -r --arg v "$1" '.[] | select(.value == $v) | .id' <<<"$roles"
}

# Check every argument before changing anything.
customers_uri=""
assignments=()
for pair in "$@"; do
  role=${pair%%=*}
  principal=${pair#*=}
  if [ "$role" = "$pair" ] || [ -z "$principal" ]; then
    echo "not <role>=<principal-id>: $pair" >&2
    exit 2
  fi
  if [ "$role" = "customers" ]; then
    customers_uri=$principal
    continue
  fi
  if [ -z "$(role_id "$role")" ]; then
    echo "no such caller name: $role" >&2
    exit 2
  fi
  assignments+=("$role=$principal")
done

roles_file=$(mktemp)
trap 'rm -f "$roles_file"' EXIT
printf '%s' "$app_roles" >"$roles_file"

app_id=$(az ad app list --identifier-uri "$uri" --query "[0].appId" -o tsv)
if [ -z "$app_id" ]; then
  app_id=$(az ad app create --display-name "${name}-group" --identifier-uris "$uri" \
    --sign-in-audience AzureADMyOrg --app-roles "@$roles_file" --query appId -o tsv)
  echo "created app registration $uri ($app_id)" >&2
else
  az ad app update --id "$app_id" --app-roles "@$roles_file"
  echo "app registration $uri ($app_id) exists; app roles brought up to date" >&2
fi

service=$(az ad sp list --filter "appId eq '$app_id'" --query "[0].id" -o tsv)
if [ -z "$service" ]; then
  service=$(az ad sp create --id "$app_id" --query id -o tsv)
fi

# Give <principal> the app role <id> on <resource>, unless it has it already.
grant() {
  local principal=$1 id=$2 resource=$3
  local held
  held=$(az rest --method GET --url "$graph/servicePrincipals/$principal/appRoleAssignments" \
    --query "value[?appRoleId=='$id' && resourceId=='$resource'] | length(@)" -o tsv)
  if [ "$held" != "0" ]; then
    echo "already held: $id on $resource by $principal" >&2
    return
  fi
  az rest --method POST --url "$graph/servicePrincipals/$principal/appRoleAssignments" \
    --headers "Content-Type=application/json" \
    --body "{\"principalId\":\"$principal\",\"resourceId\":\"$resource\",\"appRoleId\":\"$id\"}" >/dev/null
  echo "granted: $id on $resource to $principal" >&2
}

for pair in "${assignments[@]+"${assignments[@]}"}"; do
  grant "${pair#*=}" "$(role_id "${pair%%=*}")" "$service"
done

if [ -n "$customers_uri" ]; then
  customers_app=$(az ad app list --identifier-uri "$customers_uri" --query "[0].appId" -o tsv)
  if [ -z "$customers_app" ]; then
    echo "no app registration for the customer master at $customers_uri" >&2
    exit 1
  fi
  customers=$(az ad sp list --filter "appId eq '$customers_app'" --query "[0].id" -o tsv)
  matching=$(az ad sp show --id "$customers" --query "appRoles[?value=='group-admin'].id | [0]" -o tsv)
  if [ -z "$matching" ]; then
    echo "the customer master's app registration has no group-admin role" >&2
    exit 1
  fi
  grant "$group_principal" "$matching" "$customers"
fi

echo "$uri"
