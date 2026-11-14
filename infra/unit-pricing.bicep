// Unit pricing: what the driver, ic-unit-pricing, needs on Azure.
//
// - An inbound storage account the fund administrator writes valuation
//   files to, under `valuations/<YYYY-MM-DD>/`. Microsoft Defender for
//   Storage scans each blob as it lands and tags it with the result; the
//   driver reads no blob whose tag does not say it is clean.
// - An outbound account the administrator collects the unit holdings
//   extract from, write-once: each extract is locked for its retention
//   period as it is written.
// - A managed identity for the driver, federated to its Kubernetes service
//   account, holding exactly the rights above and the cluster token.
// - Optionally, the administrator's own Entra service principal, allowed to
//   write only under `valuations/` and to read only the outbound container.
//
// Why not SFTP. Blob Storage's SFTP endpoint needs a hierarchical
// namespace, and an account with a hierarchical namespace has no blob index
// tags, which is where Defender for Storage writes its scan result. The
// administrator writes over HTTPS instead (AzCopy, or any Blob client)
// under an Entra identity, which also means no password or SSH key to rotate.
//
// The unit pricing module itself is hosted by the ledger nodes and needs
// nothing here.
//
// Deployed on its own into the same resource group as main.bicep:
//   az deployment group create -g <rg> -f infra/unit-pricing.bicep \
//     -p name=<name> keyVaultName=<vault> aksOidcIssuer=<url> \
//        administratorPrincipalId=<object id, or empty>

@description('Azure region.')
param location string = resourceGroup().location

@description('Short name used as a prefix, as in main.bicep.')
@minLength(3)
@maxLength(12)
param name string

@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('The Key Vault main.bicep created. It holds cluster-token.')
param keyVaultName string

@description('The OIDC issuer URL of the AKS cluster, for workload identity.')
param aksOidcIssuer string

@description('The Kubernetes namespace the services run in.')
param workloadNamespace string = 'insurance-core'

@description('The object ID of the fund administrator\'s Entra service principal. Empty to grant nothing.')
param administratorPrincipalId string = ''

@description('Days an inbound valuation file is kept. The driver looks back seven days; the module keeps what it read.')
@minValue(14)
param inboundRetentionDays int = 90

@description('Days each extract is locked against change or deletion once written.')
@minValue(1)
param extractRetentionDays int = 2555

@description('Malware scanning cap per month, in GB, for the inbound account.')
param scanCapGbPerMonth int = 100

var tags = {
  system: 'insurance-core'
  environment: environment
  component: 'unit-pricing'
}
var unique = uniqueString(resourceGroup().id, name)
var roles = {
  blobDataContributor: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
  blobDataReader: '2a2b9908-6ea1-4ae2-8e65-a410df84e7d1'
  keyVaultSecretsUser: '4633458b-17de-408a-b874-0445c86b69e6'
}
var blobAction = 'Microsoft.Storage/storageAccounts/blobServices/containers/blobs'

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-unit-pricing'
  location: location
  tags: tags
}

resource federation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: identity
  name: 'aks-${workloadNamespace}-ic-unit-pricing'
  properties: {
    issuer: aksOidcIssuer
    subject: 'system:serviceaccount:${workloadNamespace}:ic-unit-pricing'
    audiences: ['api://AzureADTokenExchange']
  }
}

// --- inbound: the administrator's valuation files -------------------------------------

resource inbound 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: take('${name}upin${unique}', 24)
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
    // Index tags, where the scan result is written, need a flat namespace.
    isHnsEnabled: false
  }
}

resource inboundBlobs 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: inbound
  name: 'default'
  properties: {
    deleteRetentionPolicy: { enabled: true, days: 14 }
  }
}

resource inboundContainer 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: inboundBlobs
  name: 'unit-pricing-inbound'
  properties: { publicAccess: 'None' }
}

resource inboundRetention 'Microsoft.Storage/storageAccounts/managementPolicies@2023-05-01' = {
  parent: inbound
  name: 'default'
  properties: {
    policy: {
      rules: [
        {
          name: 'delete-old-valuations'
          enabled: true
          type: 'Lifecycle'
          definition: {
            filters: {
              blobTypes: ['blockBlob']
              prefixMatch: ['unit-pricing-inbound/valuations/']
            }
            actions: {
              baseBlob: {
                delete: { daysAfterCreationGreaterThan: inboundRetentionDays }
              }
            }
          }
        }
      ]
    }
  }
}

// Scan every blob as it lands and tag it with the result.
resource defender 'Microsoft.Security/defenderForStorageSettings@2022-12-01-preview' = {
  scope: inbound
  name: 'current'
  properties: {
    isEnabled: true
    overrideSubscriptionLevelSettings: true
    malwareScanning: {
      onUpload: {
        isEnabled: true
        capGBPerMonth: scanCapGbPerMonth
      }
    }
    sensitiveDataDiscovery: { isEnabled: false }
  }
}

// The driver lists, reads blobs and their tags. It writes nothing inbound.
resource driverReadsInbound 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: inboundContainer
  name: guid(inboundContainer.id, identity.id, roles.blobDataReader)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataReader)
  }
}

// The administrator writes under valuations/ and nowhere else, and cannot
// delete or retag what is there (a forged clean tag would pass the scan):
// a file it sent is the record of what it sent.
resource administratorWrites 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(administratorPrincipalId)) {
  scope: inboundContainer
  name: guid(inboundContainer.id, administratorPrincipalId, roles.blobDataContributor)
  properties: {
    principalId: administratorPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
    conditionVersion: '2.0'
    condition: '((!(ActionMatches{\'${blobAction}/write\'}) AND !(ActionMatches{\'${blobAction}/add/action\'})) OR (@Resource[${blobAction}:path] StringStartsWith \'valuations/\')) AND (!(ActionMatches{\'${blobAction}/delete\'}) AND !(ActionMatches{\'${blobAction}/tags/write\'}))'
  }
}

// --- outbound: the unit holdings extract -----------------------------------------------

resource outbound 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: take('${name}upout${unique}', 24)
  location: location
  tags: tags
  kind: 'StorageV2'
  sku: { name: environment == 'prod' ? 'Standard_GZRS' : 'Standard_LRS' }
  properties: {
    accessTier: 'Hot'
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    allowBlobPublicAccess: false
    allowSharedKeyAccess: false
    defaultToOAuthAuthentication: true
  }
}

resource outboundBlobs 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: outbound
  name: 'default'
  properties: {
    isVersioningEnabled: true
  }
}

// Each extract is written once and kept unchanged: what the administrator
// was told about its holders is evidence in a price error remediation.
resource outboundContainer 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: outboundBlobs
  name: 'unit-pricing-outbound'
  properties: {
    publicAccess: 'None'
    immutableStorageWithVersioning: { enabled: true }
  }
}

resource outboundLock 'Microsoft.Storage/storageAccounts/blobServices/containers/immutabilityPolicies@2023-05-01' = {
  parent: outboundContainer
  name: 'default'
  properties: {
    immutabilityPeriodSinceCreationInDays: extractRetentionDays
    allowProtectedAppendWrites: false
  }
}

resource driverWritesOutbound 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: outboundContainer
  name: guid(outboundContainer.id, identity.id, roles.blobDataContributor)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

resource administratorReads 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(administratorPrincipalId)) {
  scope: outboundContainer
  name: guid(outboundContainer.id, administratorPrincipalId, roles.blobDataReader)
  properties: {
    principalId: administratorPrincipalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataReader)
  }
}

// --- the cluster token ----------------------------------------------------------------

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: keyVaultName
}

resource clusterToken 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-unit-pricing'
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

output unitPricingClientId string = identity.properties.clientId
output inboundEndpoint string = inbound.properties.primaryEndpoints.blob
output outboundEndpoint string = outbound.properties.primaryEndpoints.blob
