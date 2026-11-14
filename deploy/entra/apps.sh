#!/usr/bin/env bash
# Every Entra app registration the insurance core needs, made the same way
# each time. Safe to run again: what exists is brought up to date, what is
# missing is made, and a role is assigned once. Group schemes keeps its own
# script (group-app.sh), which this one follows.
#
#   deploy/entra/apps.sh workforce <name> <host>
#       In the insurer's Entra ID tenant: the core API, the portal service
#       and the staff consoles' single-page application, the customer master,
#       documents, the underwriting intake and the reinsurer API. Each API
#       gets its app roles, an application ID URI api://<name>-<service> and
#       version 2 access tokens. Prints the values for deploy/k8s.
#
#   deploy/entra/apps.sh customers <name> <host>
#       In the Microsoft Entra External ID tenant (sign in to it first with
#       az login --tenant <external tenant> --allow-no-subscriptions): the
#       customer-facing API and the single-page application the adviser and
#       employer portals sign in through.
#
#   deploy/entra/apps.sh grant <application-id-uri> <role> <principal-object-id>
#       Give a managed identity (az identity show --query principalId), a
#       user or a group one app role on one API, once.
#
#   deploy/entra/apps.sh reinsurer <name> <reinsurer> <certificate.pem>
#       A reinsurer's own client application: the certificate the reinsurer
#       sent (public part only; it keeps the key), and the role
#       Reinsurer.Statements.Read on the reinsurer API. Prints the
#       <client id>=<reinsurer> entry for IC_REINSURANCE_API_CLIENTS.
#
# <name> is the prefix used in infra/*.bicep; <host> the portal's public host
# name. Needs the Azure CLI, signed in as someone who may create app
# registrations and grant app roles (Cloud Application Administrator or
# above), and jq.
set -euo pipefail

graph="https://graph.microsoft.com/v1.0"

usage() {
  sed -n '2,35p' "$0" >&2
  exit 2
}

# A role's ID, the same on every run: a version 5 UUID from the API's URI and
# the role's name, so a rerun never changes a role that is already assigned.
role_uuid() {
  local h
  h=$(printf '%s' "$1/$2" | sha1sum | cut -c1-32)
  printf '%s-%s-5%s-%x%s-%s\n' "${h:0:8}" "${h:8:4}" "${h:13:3}" $(((16#${h:16:1} & 3) | 8)) "${h:17:3}" "${h:20:12}"
}

app_by_uri() { az ad app list --identifier-uri "$1" --query "[0].appId" -o tsv; }

service_principal() {
  local app=$1 sp
  sp=$(az ad sp list --filter "appId eq '$app'" --query "[0].id" -o tsv)
  if [ -z "$sp" ]; then
    sp=$(az ad sp create --id "$app" --query id -o tsv)
  fi
  echo "$sp"
}

# ensure_api <uri> <display name> <role> [<role> ...]: an API with these app
# roles, assignable to people and to applications, and version 2 tokens.
ensure_api() {
  local uri=$1 display=$2
  shift 2
  local roles='[]' role
  for role in "$@"; do
    roles=$(jq -c --arg v "$role" --arg id "$(role_uuid "$uri" "$role")" \
      '. + [{allowedMemberTypes:["User","Application"], displayName:$v, description:("Acts as " + $v), id:$id, isEnabled:true, value:$v}]' <<<"$roles")
  done
  local file app
  file=$(mktemp)
  printf '%s' "$roles" >"$file"
  app=$(app_by_uri "$uri")
  if [ -z "$app" ]; then
    app=$(az ad app create --display-name "$display" --identifier-uris "$uri" \
      --sign-in-audience AzureADMyOrg --app-roles "@$file" --query appId -o tsv)
    echo "created $uri ($app)" >&2
  else
    az ad app update --id "$app" --app-roles "@$file"
    echo "$uri ($app) exists; app roles brought up to date" >&2
  fi
  rm -f "$file"
  az ad app update --id "$app" --set api.requestedAccessTokenVersion=2
  service_principal "$app" >/dev/null
  echo "$app"
}

# ensure_spa <display name> <api uri> <redirect uri> [...]: a single-page
# application signing people in with PKCE, allowed to call the API's
# `access` scope without asking each person to consent.
ensure_spa() {
  local display=$1 api_uri=$2
  shift 2
  local api spa scope_id scopes body
  api=$(app_by_uri "$api_uri")
  scope_id=$(role_uuid "$api_uri" "scope:access")
  scopes=$(jq -cn --arg id "$scope_id" '[{id:$id, value:"access", type:"User", isEnabled:true,
    adminConsentDisplayName:"Use the service", adminConsentDescription:"Sign in and use the service as yourself",
    userConsentDisplayName:"Use the service", userConsentDescription:"Sign in and use the service as yourself"}]')
  spa=$(az ad app list --display-name "$display" --query "[0].appId" -o tsv)
  if [ -z "$spa" ]; then
    spa=$(az ad app create --display-name "$display" --sign-in-audience AzureADMyOrg --query appId -o tsv)
    echo "created $display ($spa)" >&2
  fi
  local object
  object=$(az ad app show --id "$api" --query id -o tsv)
  body=$(jq -cn --argjson s "$scopes" --arg spa "$spa" --arg id "$scope_id" \
    '{api:{requestedAccessTokenVersion:2, oauth2PermissionScopes:$s, preAuthorizedApplications:[{appId:$spa, delegatedPermissionIds:[$id]}]}}')
  az rest --method PATCH --url "$graph/applications/$object" --headers "Content-Type=application/json" --body "$body" >/dev/null
  local spa_object redirects
  spa_object=$(az ad app show --id "$spa" --query id -o tsv)
  redirects=$(printf '%s\n' "$@" | jq -R . | jq -sc .)
  body=$(jq -cn --argjson r "$redirects" --arg api "$api" --arg id "$scope_id" \
    '{spa:{redirectUris:$r}, requiredResourceAccess:[{resourceAppId:$api, resourceAccess:[{id:$id, type:"Scope"}]}]}')
  az rest --method PATCH --url "$graph/applications/$spa_object" --headers "Content-Type=application/json" --body "$body" >/dev/null
  echo "$spa"
}

# grant <api uri> <role> <principal object id>
grant() {
  local uri=$1 role=$2 principal=$3 app resource id held
  app=$(app_by_uri "$uri")
  [ -n "$app" ] || { echo "no app registration at $uri" >&2; exit 1; }
  resource=$(service_principal "$app")
  id=$(az ad sp show --id "$resource" --query "appRoles[?value=='$role'].id | [0]" -o tsv)
  [ -n "$id" ] || { echo "$uri has no app role $role" >&2; exit 1; }
  held=$(az rest --method GET --url "$graph/servicePrincipals/$resource/appRoleAssignedTo" \
    --query "value[?appRoleId=='$id' && principalId=='$principal'] | length(@)" -o tsv)
  if [ "$held" != "0" ]; then
    echo "already held: $role on $uri by $principal" >&2
    return
  fi
  az rest --method POST --url "$graph/servicePrincipals/$resource/appRoleAssignedTo" \
    --headers "Content-Type=application/json" \
    --body "{\"principalId\":\"$principal\",\"resourceId\":\"$resource\",\"appRoleId\":\"$id\"}" >/dev/null
  echo "granted: $role on $uri to $principal" >&2
}

command=${1:-}
case "$command" in
  workforce)
    [ "$#" -eq 3 ] || usage
    name=$2 host=$3
    tenant=$(az account show --query tenantId -o tsv)
    # The core API (docs/operations.md): the roles callers of /v1/commands hold.
    api=$(ensure_api "api://$name-api" "$name-api" new-business policy-servicing cash-matching group-admin claims \
      collections config operations remediation transfer reinstate migration underwriting reinsurance payments \
      finance documents distribution counterparty-finance)
    # The portal service (docs/channels.md): the operations console, and each
    # module that calls /internal/v1 under its own name.
    portal=$(ensure_api "api://$name-portal" "$name-portal" operations identity customer-master group-admin \
      distribution commission finance reinsurance claims underwriting documents complaints payments entitlements)
    staff=$(ensure_spa "$name-staff-consoles" "api://$name-portal" "https://$host/staff" "https://$host/ops" \
      "https://$host/pricing" "https://$host/complaints" "https://$host/finance" "https://$host/claims" \
      "https://$host/reinsurance" "https://$host/underwriting" "https://$host/payments")
    # The customer master (docs/customers.md, config/cedar/customers.cedar).
    customers=$(ensure_api "api://$name-customers" "$name-customers" portal staff claims complaints compliance core \
      distribution documents group-admin identity legal migration new-business operations payments \
      policy-servicing privacy reinsurance underwriting break-glass)
    # Documents (docs/documents.md): the portal for a person's own, staff with a purpose.
    documents=$(ensure_api "api://$name-documents" "$name-documents" portal staff)
    # The underwriting intake (docs/underwriting.md).
    intake=$(ensure_api "api://$name-underwriting-intake" "$name-underwriting-intake" new-business \
      underwriting-console evidence delegate-api)
    # The reinsurer API (docs/reinsurance.md): reinsurers' own applications hold the one role.
    reinsurers=$(ensure_api "api://$name-reinsurer-api" "$name-reinsurer-api" Reinsurer.Statements.Read)
    jq -n --arg t "$tenant" --arg api "api://$name-api" --arg portal "api://$name-portal" --arg staff "$staff" \
      --arg customers "api://$name-customers" --arg documents "api://$name-documents" \
      --arg intake "api://$name-underwriting-intake" --arg reinsurers "api://$name-reinsurer-api" \
      --arg ids "$api $portal $customers $documents $intake $reinsurers" \
      '{IC_ENTRA_TENANT_ID:$t, "api.yaml IC_ENTRA_AUDIENCE":$api, "portal.yaml IC_ENTRA_AUDIENCE":$portal,
        "portal.yaml IC_ENTRA_CLIENT_ID":$staff, IC_CUSTOMERS_AUDIENCE:$customers, IC_DOCUMENTS_AUDIENCE:$documents,
        IC_UNDERWRITING_INTAKE_AUDIENCE:$intake, IC_REINSURANCE_API_AUDIENCE:$reinsurers,
        "application ids":($ids | split(" "))}'
    echo "Next: grant each workload's managed identity the roles it calls with (apps.sh grant ...), and staff their roles; see docs/operations.md." >&2
    ;;
  customers)
    [ "$#" -eq 3 ] || usage
    name=$2 host=$3
    tenant=$(az account show --query tenantId -o tsv)
    ensure_api "api://$name-customer-api" "$name-customer-api" >/dev/null
    spa=$(ensure_spa "$name-adviser-employer-portals" "api://$name-customer-api" "https://$host/adviser" "https://$host/employer")
    jq -n --arg t "$tenant" --arg aud "api://$name-customer-api" --arg spa "$spa" \
      '{IC_CUSTOMER_TENANT_ID:$t, IC_CUSTOMER_AUDIENCE:$aud, IC_CUSTOMER_CLIENT_ID:$spa}'
    echo "The mobile app's own registration belongs with the bank's app; give it the access scope on api://$name-customer-api." >&2
    ;;
  grant)
    [ "$#" -eq 4 ] || usage
    grant "$2" "$3" "$4"
    ;;
  reinsurer)
    [ "$#" -eq 4 ] || usage
    name=$2 reinsurer=$3 certificate=$4
    [ -r "$certificate" ] || { echo "cannot read $certificate" >&2; exit 2; }
    if grep -q "PRIVATE KEY" "$certificate"; then
      echo "$certificate holds a private key: ask the reinsurer for the certificate alone" >&2
      exit 2
    fi
    display="$name-reinsurer-$reinsurer"
    client=$(az ad app list --display-name "$display" --query "[0].appId" -o tsv)
    if [ -z "$client" ]; then
      client=$(az ad app create --display-name "$display" --sign-in-audience AzureADMyOrg --query appId -o tsv)
      echo "created $display ($client)" >&2
    fi
    az ad app credential reset --id "$client" --cert "@$certificate" --append >/dev/null
    principal=$(service_principal "$client")
    grant "api://$name-reinsurer-api" Reinsurer.Statements.Read "$principal"
    echo "$client=$reinsurer"
    ;;
  *)
    usage
    ;;
esac
