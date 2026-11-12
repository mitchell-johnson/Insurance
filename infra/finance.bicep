// The finance close service: what it needs on Azure.
//
// - A storage account of its own for the valuation and experience extracts,
//   with time-based retention (WORM) on the container. An extract is written
//   once, registered in the finance ledger by its hash, and never changed.
// - A managed identity for the service, federated to its Kubernetes service
//   account, that may write extracts, read the signed Merkle roots the core
//   publishes, and read the cluster token.
//
// The finance ledger itself is a module of the ledger nodes and needs
// nothing here.
//
// Deployed on its own into the same resource group as main.bicep:
//   az deployment group create -g <rg> -f infra/finance.bicep \
//     -p name=<name> ledgerStorageName=<account> aksOidcIssuer=<url> \
//     keyVaultName=<vault>

@description('Azure region.')
param location string = resourceGroup().location

@description('Short name used as a prefix, as in main.bicep.')
@minLength(3)
@maxLength(12)
param name string

@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('The write-once storage account main.bicep created, which holds the merkle-roots container.')
param ledgerStorageName string

@description('The Key Vault main.bicep created. It holds cluster-token.')
param keyVaultName string

@description('The OIDC issuer URL of the AKS cluster, for workload identity.')
param aksOidcIssuer string

@description('The Kubernetes namespace the services run in.')
param workloadNamespace string = 'insurance-core'

@description('Days an extract cannot be changed or deleted. Seven years plus a margin by default.')
param retentionDays int = 2920

var tags = {
  system: 'insurance-core'
  environment: environment
  component: 'finance'
}
var unique = uniqueString(resourceGroup().id, name)
var roles = {
  blobDataContributor: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
  blobDataReader: '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'
  keyVaultSecretsUser: '4633458b-17de-408a-b874-0445c86b69e6'
}

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-finance-close'
  location: location
  tags: tags
}

resource federation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: identity
  name: 'aks'
  properties: {
    issuer: aksOidcIssuer
    subject: 'system:serviceaccount:${workloadNamespace}:ic-finance-close'
    audiences: ['api://AzureADTokenExchange']
  }
}

// --- extracts ---------------------------------------------------------------------

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: take('${name}fin${unique}', 24)
  location: location
  tags: tags
  kind: 'StorageV2'
  sku: { name: environment == 'prod' ? 'Standard_ZRS' : 'Standard_LRS' }
  properties: {
    accessTier: 'Hot'
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    allowBlobPublicAccess: false
    // Entra ID only: shared keys and SAS signed with them are refused.
    allowSharedKeyAccess: false
    defaultToOAuthAuthentication: true
  }
}

resource blobs 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: account
  name: 'default'
}

resource extracts 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobs
  name: 'finance-extracts'
  properties: { publicAccess: 'None' }
}

// Time-based retention (WORM). Created unlocked so a test environment can be
// torn down. Lock it in production: locking cannot be undone, which is the
// point, so it is a deliberate step and not a default.
resource extractsWorm 'Microsoft.Storage/storageAccounts/blobServices/containers/immutabilityPolicies@2023-05-01' = {
  parent: extracts
  name: 'default'
  properties: {
    immutabilityPeriodSinceCreationInDays: retentionDays
    allowProtectedAppendWrites: false
  }
}

resource writeExtracts 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: extracts
  name: guid(extracts.id, identity.id, roles.blobDataContributor)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

// --- the core's signed Merkle roots: read only -------------------------------------

resource ledgerStorage 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: ledgerStorageName
}

resource ledgerBlobs 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' existing = {
  parent: ledgerStorage
  name: 'default'
}

resource merkleRoots 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' existing = {
  parent: ledgerBlobs
  name: 'merkle-roots'
}

resource readRoots 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: merkleRoots
  name: guid(merkleRoots.id, identity.id, roles.blobDataReader)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataReader)
  }
}

// --- the cluster token ---------------------------------------------------------------

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: keyVaultName
}

resource serviceSecrets 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vault
  name: guid(vault.id, identity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

output financeCloseIdentityClientId string = identity.properties.clientId
output extractsEndpoint string = account.properties.primaryEndpoints.blob
output rootsEndpoint string = ledgerStorage.properties.primaryEndpoints.blob
