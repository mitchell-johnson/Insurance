// The customer master: what it needs on Azure.
//
// - A PostgreSQL Flexible Server of its own, Entra ID sign-in only, TLS
//   required, zone-redundant in prod, with the `customers` database. Personal
//   data is kept apart from every other store, so its backups, access and
//   restore drill are the customer master's alone.
// - An RSA key in Key Vault, `customers-kek`, that wraps each party's data
//   key, and the blind-index key as a secret. Neither ever leaves the vault
//   except the index key, which is mounted into the pod as a file.
// - Two Event Hubs on the namespace main.bicep created: `customer-events`
//   for every change and `customer-priority` for changes that take access
//   away (deaths, holds, merges), which gateways read first.
// - A storage account whose `customers-erasures` container is write-once:
//   the erasure register, kept for longer than any backup, which a restore
//   is replayed against (docs/customers.md, "Restoring a backup").
// - A managed identity for the service, federated to its Kubernetes service
//   account, holding exactly the rights above.
//
// Deployed on its own into the same resource group as main.bicep:
//   az deployment group create -g <rg> -f infra/customers.bicep \
//     -p name=<name> keyVaultName=<vault> eventHubsNamespaceName=<namespace> \
//        aksOidcIssuer=<url> privateNetwork=<main.bicep output privateNetwork> \
//        postgresAdminGroupObjectId=<id> postgresAdminGroupName=<name> \
//        indexKey=<64 random characters>
//
// One step needs a person afterwards, signed in as a member of the admin
// group: create the service's database role from its managed identity
//   psql "host=<server> dbname=postgres user=<admin-group> sslmode=require" \
//     -c "select * from pgaadauth_create_principal('<name>-customers', false, false)"
//   psql ... dbname=customers -c "grant all on schema public to \"<name>-customers\""
// The service creates its own tables on first start.

@description('Azure region.')
param location string = resourceGroup().location

@description('Short name used as a prefix, as in main.bicep.')
@minLength(3)
@maxLength(12)
param name string

@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('The Key Vault main.bicep created.')
param keyVaultName string

@description('The Event Hubs namespace main.bicep created.')
param eventHubsNamespaceName string

@description('The OIDC issuer URL of the AKS cluster, for workload identity.')
param aksOidcIssuer string

@description('The Kubernetes namespace the services run in.')
param workloadNamespace string = 'insurance-core'

@description('Only without private networking (a throwaway dev environment): the public address the AKS cluster connects out from (main.bicep output egressPrefix), the only address the database then accepts. With private networking the database has no public address at all and this is ignored.')
param aksOutboundIp string = ''

@description('Object ID of the Entra group that administers the customer database.')
param postgresAdminGroupObjectId string

@description('Display name of that group.')
param postgresAdminGroupName string

@description('The key for blind indexes: at least 32 random characters. Changing it means rebuilding every index (docs/customers.md).')
@secure()
@minLength(32)
param indexKey string

@description('Partitions of each hub. A party always lands on the same one, so this cannot change once events flow.')
param hubPartitions int = 8

@description('Years the erasure register is kept: 10, agreed 2026-11-13. Longer than any backup the database can be restored from. Once the policy is locked it can only grow.')
param registerYears int = 10


// --- private networking (main.bicep output privateNetwork) ---------------------------

@description('main.bicep\'s output privateNetwork: the subnet and the private DNS zones for private endpoints, and what the firewalls admit beside them. Empty: no private endpoints, and the services here stay reachable from anywhere, with Entra ID still required.')
param privateNetwork object = {}

var tags = {
  system: 'insurance-core'
  environment: environment
  component: 'customers'
}
var isPrivate = !empty(privateNetwork)
var adminIpRules = map(privateNetwork.?adminIpRanges ?? [], range => { value: range, action: 'Allow' })
// An account only this system reaches: the private endpoint and the administrators.
var internalAccess = privateNetwork.?publicAccess ?? 'Enabled'
var internalAcls = { defaultAction: isPrivate ? 'Deny' : 'Allow', bypass: 'AzureServices', ipRules: adminIpRules }
var unique = uniqueString(resourceGroup().id, name)
var roles = {
  blobDataContributor: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
  keyVaultSecretsUser: '4633458b-17de-408a-b874-0445c86b69e6'
  keyVaultCryptoUser: '12338af0-0e69-4776-bea7-57ae8d297424'
  eventHubsDataSender: '2b629674-e913-4c01-ae53-ef4638d8f975'
}

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-customers'
  location: location
  tags: tags
}

resource federation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: identity
  name: 'aks-${workloadNamespace}-ic-customers'
  properties: {
    issuer: aksOidcIssuer
    subject: 'system:serviceaccount:${workloadNamespace}:ic-customers'
    audiences: ['api://AzureADTokenExchange']
  }
}

// --- the database --------------------------------------------------------------------

resource postgres 'Microsoft.DBforPostgreSQL/flexibleServers@2024-08-01' = {
  name: '${name}-${unique}-customers'
  location: location
  tags: tags
  sku: environment == 'prod'
    ? { name: 'Standard_D4ds_v5', tier: 'GeneralPurpose' }
    : { name: 'Standard_B2ms', tier: 'Burstable' }
  properties: {
    version: '16'
    storage: { storageSizeGB: 128, autoGrow: 'Enabled' }
    // Entra ID only. No password exists for this server.
    authConfig: {
      activeDirectoryAuth: 'Enabled'
      passwordAuth: 'Disabled'
      tenantId: subscription().tenantId
    }
    highAvailability: { mode: environment == 'prod' ? 'ZoneRedundant' : 'Disabled' }
    // Geo-redundant backups let the region-loss plan restore elsewhere; every
    // restore is followed by erasing again from the register.
    backup: {
      backupRetentionDays: 35
      geoRedundantBackup: environment == 'prod' ? 'Enabled' : 'Disabled'
    }
    // Reached only through its private endpoint when private.
    network: { publicNetworkAccess: isPrivate ? 'Disabled' : 'Enabled' }
  }
}

module postgresEndpoint 'modules/private-endpoint.bicep' = if (isPrivate) {
  name: 'pe-customers-postgres'
  params: {
    name: '${postgres.name}-postgres'
    location: location
    tags: tags
    subnetId: privateNetwork.?subnetId ?? ''
    targetId: postgres.id
    groupId: 'postgresqlServer'
    dnsZoneIds: [privateNetwork.?zones.?postgres ?? '']
  }
}

resource admin 'Microsoft.DBforPostgreSQL/flexibleServers/administrators@2024-08-01' = {
  parent: postgres
  name: postgresAdminGroupObjectId
  properties: {
    principalType: 'Group'
    principalName: postgresAdminGroupName
    tenantId: subscription().tenantId
  }
}

resource database 'Microsoft.DBforPostgreSQL/flexibleServers/databases@2024-08-01' = {
  parent: postgres
  name: 'customers'
  properties: { charset: 'UTF8', collation: 'en_US.utf8' }
}

resource fromCluster 'Microsoft.DBforPostgreSQL/flexibleServers/firewallRules@2024-08-01' = if (!isPrivate && !empty(aksOutboundIp)) {
  parent: postgres
  name: 'aks-outbound'
  properties: { startIpAddress: aksOutboundIp, endIpAddress: aksOutboundIp }
}

// The service refuses a connection without TLS; the server does too.
resource requireTls 'Microsoft.DBforPostgreSQL/flexibleServers/configurations@2024-08-01' = {
  parent: postgres
  name: 'require_secure_transport'
  properties: { value: 'on', source: 'user-override' }
}

// --- the wrapping key and the index key -------------------------------------------------

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: keyVaultName
}

// Wrapped keys name the key version they were wrapped under, so a new
// version takes over for new parties and old ones still unwrap until
// POST /v1/admin/rewrap moves them.
resource wrappingKey 'Microsoft.KeyVault/vaults/keys@2023-07-01' = {
  parent: vault
  name: 'customers-kek'
  tags: tags
  properties: {
    kty: 'RSA'
    keySize: 3072
    keyOps: ['wrapKey', 'unwrapKey']
    rotationPolicy: {
      lifetimeActions: [
        {
          trigger: { timeAfterCreate: 'P1Y' }
          action: { type: 'rotate' }
        }
      ]
    }
  }
}

resource wrapWithKey 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: wrappingKey
  name: guid(wrappingKey.id, identity.id, roles.keyVaultCryptoUser)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultCryptoUser)
  }
}

resource indexSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' = {
  parent: vault
  name: 'customers-index-key'
  tags: tags
  properties: {
    value: indexKey
    contentType: 'text/plain'
  }
}

resource readIndexSecret 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: indexSecret
  name: guid(indexSecret.id, identity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

// Deaths are told to claims through the ledger cluster.
resource clusterToken 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-customers'
}

resource readClusterToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: clusterToken
  name: guid(clusterToken.id, identity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

// --- the erasure register ---------------------------------------------------------------

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: take('${name}cus${unique}', 24)
  location: location
  tags: tags
  kind: 'StorageV2'
  sku: { name: environment == 'prod' ? 'Standard_GZRS' : 'Standard_LRS' }
  properties: {
    publicNetworkAccess: internalAccess
    networkAcls: internalAcls
    accessTier: 'Cool'
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    allowBlobPublicAccess: false
    allowSharedKeyAccess: false
    defaultToOAuthAuthentication: true
  }
}

module accountEndpoint 'modules/private-endpoint.bicep' = if (isPrivate) {
  name: 'pe-customers-blob'
  params: {
    name: '${account.name}-blob'
    location: location
    tags: tags
    subnetId: privateNetwork.?subnetId ?? ''
    targetId: account.id
    groupId: 'blob'
    dnsZoneIds: [privateNetwork.?zones.?blob ?? '']
  }
}

resource blobs 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: account
  name: 'default'
}

resource register 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobs
  name: 'customers-erasures'
  properties: { publicAccess: 'None' }
}

// Write-once: a page, once written, cannot be changed or deleted for the
// retention period (10 years, agreed 2026-11-13). Bicep cannot lock a policy;
// in prod run tools/lock-erasure-register.sh once after the first deployment.
resource registerPolicy 'Microsoft.Storage/storageAccounts/blobServices/containers/immutabilityPolicies@2023-05-01' = {
  parent: register
  name: 'default'
  properties: {
    immutabilityPeriodSinceCreationInDays: registerYears * 366
    allowProtectedAppendWrites: false
  }
}

resource writeRegister 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: register
  name: guid(register.id, identity.id, roles.blobDataContributor)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

// --- events out ------------------------------------------------------------------------

resource eventHubs 'Microsoft.EventHub/namespaces@2024-01-01' existing = {
  name: eventHubsNamespaceName
}

resource hubs 'Microsoft.EventHub/namespaces/eventhubs@2024-01-01' = [
  for hub in ['customer-events', 'customer-priority']: {
    parent: eventHubs
    name: hub
    properties: {
      partitionCount: hubPartitions
      retentionDescription: {
        cleanupPolicy: 'Delete'
        retentionTimeInHours: 168
      }
    }
  }
]

resource publish 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for i in range(0, 2): {
    scope: hubs[i]
    name: guid(hubs[i].id, identity.id, roles.eventHubsDataSender)
    properties: {
      principalId: identity.properties.principalId
      principalType: 'ServicePrincipal'
      roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.eventHubsDataSender)
    }
  }
]

output customersClientId string = identity.properties.clientId
output customersPrincipalName string = identity.name
output postgresHost string = postgres.properties.fullyQualifiedDomainName
output registerEndpoint string = account.properties.primaryEndpoints.blob
output registerAccountName string = account.name
output wrappingKeyName string = wrappingKey.name
