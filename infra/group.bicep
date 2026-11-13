// Group scheme administration: what the group service needs on Azure.
//
// - A storage account of its own for employer files in quarantine. It is
//   separate from the write-once ledger account on purpose: these blobs come
//   from outside, are scanned, and are deleted after a few weeks.
// - Microsoft Defender for Storage with malware scanning on upload. It tags
//   each blob with the scan result; the group service reads no blob whose
//   tag does not say it is clean.
// - An event hub for the module's own events.
// - A managed identity for the service, federated to its Kubernetes service
//   account, with only the roles it needs.
//
// Deployed on its own into the same resource group as main.bicep:
//   az deployment group create -g <rg> -f infra/group.bicep \
//     -p name=<name> eventHubsNamespaceName=<namespace> aksOidcIssuer=<url> \
//     keyVaultName=<vault>
//
// Then a person who may create app registrations runs
// deploy/entra/group-app.sh: the service's app registration, its app roles
// and who holds them, and the customer master's group-admin role for this
// identity. Bicep does not make app registrations here.

@description('Azure region.')
param location string = resourceGroup().location

@description('Short name used as a prefix, as in main.bicep.')
@minLength(3)
@maxLength(12)
param name string

@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('The Event Hubs namespace main.bicep created.')
param eventHubsNamespaceName string

@description('The Key Vault main.bicep created. It holds cluster-token and group-fingerprint-salt.')
param keyVaultName string

@description('The OIDC issuer URL of the AKS cluster, for workload identity.')
param aksOidcIssuer string

@description('The Kubernetes namespace the services run in.')
param workloadNamespace string = 'insurance-core'

@description('Days an uploaded file is kept before it is deleted.')
@minValue(7)
param quarantineRetentionDays int = 35

@description('Malware scanning cap per month, in GB, for the quarantine account.')
param scanCapGbPerMonth int = 1000

var tags = {
  system: 'insurance-core'
  environment: environment
  component: 'group'
}
var unique = uniqueString(resourceGroup().id, name)
var roles = {
  blobDataContributor: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
  eventHubsDataSender: '2b629674-e913-4c01-ae53-ef4638d8f975'
  keyVaultSecretsUser: '4633458b-17de-408a-b874-0445c86b69e6'
}

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-group'
  location: location
  tags: tags
}

resource federation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: identity
  name: 'aks'
  properties: {
    issuer: aksOidcIssuer
    subject: 'system:serviceaccount:${workloadNamespace}:ic-group'
    audiences: ['api://AzureADTokenExchange']
  }
}

// --- quarantine storage ---------------------------------------------------------

resource quarantine 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: take('${name}grp${unique}', 24)
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
  parent: quarantine
  name: 'default'
}

resource files 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobs
  name: 'group-quarantine'
  properties: { publicAccess: 'None' }
}

// Employer files carry names and dates of birth. They are needed until the
// file is applied and for the reinstatement runbook after it, and no longer.
resource retention 'Microsoft.Storage/storageAccounts/managementPolicies@2023-05-01' = {
  parent: quarantine
  name: 'default'
  properties: {
    policy: {
      rules: [
        {
          name: 'delete-old-files'
          enabled: true
          type: 'Lifecycle'
          definition: {
            filters: {
              blobTypes: ['blockBlob']
              prefixMatch: ['group-quarantine/']
            }
            actions: {
              baseBlob: {
                delete: { daysAfterCreationGreaterThan: quarantineRetentionDays }
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
  scope: quarantine
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

resource serviceFiles 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: files
  name: guid(files.id, identity.id, roles.blobDataContributor)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

// --- the module's events --------------------------------------------------------

resource eventHubs 'Microsoft.EventHub/namespaces@2024-01-01' existing = {
  name: eventHubsNamespaceName
}

// One partition: the module's events are one ordered sequence.
resource groupEvents 'Microsoft.EventHub/namespaces/eventhubs@2024-01-01' = {
  parent: eventHubs
  name: 'group-events'
  properties: {
    partitionCount: 1
    retentionDescription: {
      cleanupPolicy: 'Delete'
      retentionTimeInHours: 168
    }
  }
}

resource serviceEvents 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: groupEvents
  name: guid(groupEvents.id, identity.id, roles.eventHubsDataSender)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.eventHubsDataSender)
  }
}

// --- the service's secrets --------------------------------------------------------

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

output groupIdentityClientId string = identity.properties.clientId
output quarantineEndpoint string = quarantine.properties.primaryEndpoints.blob
output eventHub string = groupEvents.name
