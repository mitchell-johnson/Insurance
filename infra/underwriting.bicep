// Underwriting: what the clinical store needs on Azure (docs/underwriting.md).
//
// The store keeps health disclosures and evidence. Each record is sealed
// under a key of the person's own, and that key is wrapped by one key that
// never leaves Key Vault. Erasing a person is deleting their wrapped key, so
// two things here are deliberate and must not be "tidied":
//
// - A storage account of its own with no blob soft delete, no container soft
//   delete, no versioning and no change feed. Any of those would keep a
//   deleted wrapped key recoverable, and erasure would not be erasure.
// - The wrapping key is an HSM key that cannot be exported, with only wrap
//   and unwrap allowed, and the store's identity may do nothing else with it.
//
// The clinical identity is for the disclosure intake, the process that takes
// disclosures and seals them (deploy/k8s/underwriting-intake.yaml). The underwriting driver
// (deploy/k8s/underwriting.yaml) uses none of it: it talks to the ledger
// nodes and publishes staff notices to the `underwriting-notices` hub below,
// under the workers' identity from main.bicep, which may only send to it.
//
// Deployed on its own into the same resource group as main.bicep:
//   az deployment group create -g <rg> -f infra/underwriting.bicep \
//     -p name=<name> keyVaultName=<vault> aksOidcIssuer=<url> \
//     eventHubsNamespaceName=<namespace>

@description('Azure region.')
param location string = resourceGroup().location

@description('Short name used as a prefix, as in main.bicep.')
@minLength(3)
@maxLength(12)
param name string

@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('The Key Vault main.bicep created. The wrapping key is created in it.')
param keyVaultName string

@description('The OIDC issuer URL of the AKS cluster, for workload identity.')
param aksOidcIssuer string

@description('The Kubernetes namespace the services run in.')
param workloadNamespace string = 'insurance-core'

@description('The Event Hubs namespace main.bicep created.')
param eventHubsNamespaceName string

@description('The Kubernetes service account of the process that reads and writes the clinical store.')
param serviceAccount string = 'ic-underwriting-clinical'

var tags = {
  system: 'insurance-core'
  environment: environment
  component: 'underwriting'
}
var unique = uniqueString(resourceGroup().id, name)
var roles = {
  blobDataContributor: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
  // Key Vault Crypto Service Encryption User: read the key's metadata, wrap
  // and unwrap. No sign, no encrypt, no delete.
  keyVaultWrapUser: 'e147488a-f6f5-4113-8e2d-b22465e65bf6'
  eventHubsDataSender: '2b629674-e913-4c01-ae53-ef4638d8f975'
  keyVaultSecretsUser: '4633458b-17de-408a-b874-0445c86b69e6'
}

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-underwriting-clinical'
  location: location
  tags: tags
}

resource federation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: identity
  name: 'aks'
  properties: {
    issuer: aksOidcIssuer
    subject: 'system:serviceaccount:${workloadNamespace}:${serviceAccount}'
    audiences: ['api://AzureADTokenExchange']
  }
}

// --- the erasable store -----------------------------------------------------------

resource clinical 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: take('${name}uw${unique}', 24)
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

// Everything that would keep a deleted blob is off, and said so.
resource blobs 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: clinical
  name: 'default'
  properties: {
    deleteRetentionPolicy: { enabled: false }
    containerDeleteRetentionPolicy: { enabled: false }
    isVersioningEnabled: false
    changeFeed: { enabled: false }
  }
}

resource records 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobs
  name: 'clinical'
  properties: { publicAccess: 'None' }
}

resource storeRecords 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: records
  name: guid(records.id, identity.id, roles.blobDataContributor)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

// --- the wrapping key ---------------------------------------------------------------

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: keyVaultName
}

// RSA-OAEP-256 wrap and unwrap, as ic_azure::keywrap::KeyVaultWrapper asks
// for. A wrapped key names the key version that wrapped it, so rotating to a
// new version leaves older wrapped keys readable.
resource wrappingKey 'Microsoft.KeyVault/vaults/keys@2023-07-01' = {
  parent: vault
  name: 'clinical-key-wrap'
  tags: tags
  properties: {
    kty: 'RSA-HSM'
    keySize: 3072
    keyOps: ['wrapKey', 'unwrapKey']
    attributes: { exportable: false }
  }
}

resource storeWrap 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: wrappingKey
  name: guid(wrappingKey.id, identity.id, roles.keyVaultWrapUser)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultWrapUser)
  }
}

// The intake (deploy/k8s/underwriting-intake.yaml) runs under the clinical
// identity and reads the cluster token, as every process that talks to the
// ledger nodes does. Granted on the vault, as main.bicep grants the others:
// the secret is set by hand after the vault exists.
resource readClusterToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vault
  name: guid(vault.id, identity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

// --- notices for staff --------------------------------------------------------

resource eventHubs 'Microsoft.EventHub/namespaces@2024-01-01' existing = {
  name: eventHubsNamespaceName
}

// One partition: the notices are one ordered sequence. They name a proposal
// and who should act, never a reason or an answer.
resource notices 'Microsoft.EventHub/namespaces/eventhubs@2024-01-01' = {
  parent: eventHubs
  name: 'underwriting-notices'
  properties: {
    partitionCount: 1
    retentionDescription: {
      cleanupPolicy: 'Delete'
      retentionTimeInHours: 168
    }
  }
}

// The driver runs as ic-worker (deploy/k8s/underwriting.yaml).
resource workerIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' existing = {
  name: '${name}-worker'
}

resource driverSends 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: notices
  name: guid(notices.id, workerIdentity.id, roles.eventHubsDataSender)
  properties: {
    principalId: workerIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.eventHubsDataSender)
  }
}

output noticesHub string = notices.name
output clinicalIdentityClientId string = identity.properties.clientId
output clinicalBlobEndpoint string = clinical.properties.primaryEndpoints.blob
output clinicalContainer string = records.name
output clinicalKeyVaultUrl string = vault.properties.vaultUri
output clinicalWrappingKey string = wrappingKey.name
