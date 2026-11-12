// Reinsurance: what the reinsurance driver needs on Azure (docs/reinsurance.md).
//
// - A write-once container, `reinsurance-records`, in the ledger's storage
//   account: every statement, listing and supplementary listing sent to a
//   reinsurer, and the log of what personal information was released to
//   which reinsurer and why. Time-based retention, like the Merkle roots.
// - An event hub of one partition, `reinsurance-events`, on the namespace
//   main.bicep created: the notices for reinsurance operations (exceptions
//   opened and closed, supplementary listings due, capacity holds that
//   lapsed), in order.
// - A managed identity for the driver, federated to its Kubernetes service
//   account, that may write to that container, send to that hub and read
//   the cluster token. Nothing else.
//
// The reinsurance module itself is hosted by the ledger nodes and needs
// nothing here.
//
// Deployed on its own into the same resource group as main.bicep:
//   az deployment group create -g <rg> -f infra/reinsurance.bicep \
//     -p name=<name> storageAccountName=<account> keyVaultName=<vault> \
//        eventHubsNamespaceName=<namespace> aksOidcIssuer=<url>

@description('Azure region.')
param location string = resourceGroup().location

@description('Short name used as a prefix, as in main.bicep.')
@minLength(3)
@maxLength(12)
param name string

@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('The ledger storage account main.bicep created (the host name of its blobEndpoint output, up to the first dot).')
param storageAccountName string

@description('The Key Vault main.bicep created. It holds cluster-token.')
param keyVaultName string

@description('The Event Hubs namespace main.bicep created.')
param eventHubsNamespaceName string

@description('The OIDC issuer URL of the AKS cluster, for workload identity.')
param aksOidcIssuer string

@description('The Kubernetes namespace the services run in.')
param workloadNamespace string = 'insurance-core'

@description('Days a filed record cannot be changed or deleted. The default is eight years, as in main.bicep.')
@minValue(1)
param retentionDays int = 2920

var tags = {
  system: 'insurance-core'
  environment: environment
  component: 'reinsurance'
}
var roles = {
  blobDataContributor: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
  eventHubsDataSender: '2b629674-e913-4c01-ae53-ef4638d8f975'
  keyVaultSecretsUser: '4633458b-17de-408a-b874-0445c86b69e6'
}

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-reinsurance'
  location: location
  tags: tags
}

resource federation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: identity
  name: 'aks'
  properties: {
    issuer: aksOidcIssuer
    subject: 'system:serviceaccount:${workloadNamespace}:ic-reinsurance'
    audiences: ['api://AzureADTokenExchange']
  }
}

// --- the records ------------------------------------------------------------------

resource storage 'Microsoft.Storage/storageAccounts@2023-05-01' existing = {
  name: storageAccountName
}

resource blobs 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' existing = {
  parent: storage
  name: 'default'
}

resource records 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobs
  name: 'reinsurance-records'
  properties: { publicAccess: 'None' }
}

// Time-based retention (WORM). Created unlocked so a test environment can be
// torn down. Lock it in production, as for the containers in main.bicep:
// locking cannot be undone, so it is a deliberate step and not a default.
resource recordsWorm 'Microsoft.Storage/storageAccounts/blobServices/containers/immutabilityPolicies@2023-05-01' = {
  parent: records
  name: 'default'
  properties: {
    immutabilityPeriodSinceCreationInDays: retentionDays
    allowProtectedAppendWrites: false
  }
}

resource driverRecords 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: records
  name: guid(records.id, identity.id, roles.blobDataContributor)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

// --- notices for reinsurance operations -----------------------------------------------

resource eventHubs 'Microsoft.EventHub/namespaces@2024-01-01' existing = {
  name: eventHubsNamespaceName
}

// One partition: the notices are one ordered sequence. Seven days is long
// enough for a consumer that was down over a long weekend to catch up; the
// open exceptions themselves stay in the module and can always be asked for.
resource notices 'Microsoft.EventHub/namespaces/eventhubs@2024-01-01' = {
  parent: eventHubs
  name: 'reinsurance-events'
  properties: {
    partitionCount: 1
    retentionDescription: {
      cleanupPolicy: 'Delete'
      retentionTimeInHours: 168
    }
  }
}

resource driverNotices 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: notices
  name: guid(notices.id, identity.id, roles.eventHubsDataSender)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.eventHubsDataSender)
  }
}

// --- the cluster token ----------------------------------------------------------------

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: keyVaultName
}

// On the vault, as for the other processes in main.bicep: the secret's value is
// set by an operator and may not exist yet when this is deployed.
resource driverToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vault
  name: guid(vault.id, identity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

output reinsuranceIdentityClientId string = identity.properties.clientId
output recordsContainer string = records.name
output eventHub string = notices.name
