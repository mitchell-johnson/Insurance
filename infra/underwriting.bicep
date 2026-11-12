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
// The underwriting driver (deploy/k8s/underwriting.yaml) does not use any of
// this: it talks only to the ledger nodes. This identity is for the process
// that takes disclosures and reads them back for an underwriter.
//
// Deployed on its own into the same resource group as main.bicep:
//   az deployment group create -g <rg> -f infra/underwriting.bicep \
//     -p name=<name> keyVaultName=<vault> aksOidcIssuer=<url>

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

output clinicalIdentityClientId string = identity.properties.clientId
output clinicalBlobEndpoint string = clinical.properties.primaryEndpoints.blob
output clinicalContainer string = records.name
output clinicalKeyVaultUrl string = vault.properties.vaultUri
output clinicalWrappingKey string = wrappingKey.name
