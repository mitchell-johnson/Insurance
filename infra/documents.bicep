// The documents service: what it needs on Azure.
//
// - A storage account of its own for the document vault, with version-level
//   immutability on the vault container so each document can be locked to
//   its own retention date, and a second container the print house collects
//   from.
// - An RSA key in Key Vault that wraps the document keys. It never leaves
//   the vault.
// - Azure Communication Services for email and SMS, and an Event Grid
//   subscription that posts its delivery reports to the service's webhook.
// - Two Event Hubs of one partition each on the namespace main.bicep
//   created: `documents-events` and `complaints-events`.
// - A managed identity for the service, federated to its Kubernetes service
//   account, holding exactly the rights above and the cluster token.
//
// The documents and complaints modules themselves are hosted by the ledger
// nodes and need nothing here.
//
// Deployed on its own into the same resource group as main.bicep:
//   az deployment group create -g <rg> -f infra/documents.bicep \
//     -p name=<name> keyVaultName=<vault> eventHubsNamespaceName=<namespace> \
//        aksOidcIssuer=<url> webhookKey=<32 or more random characters>
//
// Deploy it twice. The first time leave deliveryReportsUrl empty: Event Grid
// proves a webhook is ours by calling it, so the subscription can only be
// created once the service is running behind its public address. Then deploy
// again with deliveryReportsUrl=https://<host>/v1/delivery-reports.
//
// Not done here, because each needs a person: verifying the sending domain
// for email and linking it to the Communication Services resource, and
// buying the SMS number or short code. docs/documents.md lists them.

@description('Azure region.')
param location string = resourceGroup().location

@description('Short name used as a prefix, as in main.bicep.')
@minLength(3)
@maxLength(12)
param name string

@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('The Key Vault main.bicep created. It holds cluster-token, and will hold the wrapping key and the webhook key.')
param keyVaultName string

@description('The Event Hubs namespace main.bicep created.')
param eventHubsNamespaceName string

@description('The OIDC issuer URL of the AKS cluster, for workload identity.')
param aksOidcIssuer string

@description('The Kubernetes namespace the services run in.')
param workloadNamespace string = 'insurance-core'

@description('Where Communication Services keeps data at rest.')
param communicationDataLocation string = 'Australia'

@description('The key Event Grid presents to the webhook in the x-webhook-key header. At least 32 characters.')
@secure()
@minLength(32)
param webhookKey string

@description('The public address of the delivery-report webhook, https://<host>/v1/delivery-reports. Empty until the service is reachable; see the note at the top.')
param deliveryReportsUrl string = ''

@description('The principal the print house collects with, if it collects under an Entra identity. Empty to grant nothing.')
param printHousePrincipalId string = ''

@description('The role that lets the service send email and SMS under its Entra identity: Communication and Email Service Owner. Confirm the ID with `az role definition list --name "Communication and Email Service Owner" --query [0].name` before the first deployment.')
param communicationRoleDefinitionId string = '09976791-48a7-449e-bb21-39d1a415f350'

var tags = {
  system: 'insurance-core'
  environment: environment
  component: 'documents'
}
var unique = uniqueString(resourceGroup().id, name)
var roles = {
  // Writing a blob together with its immutability date, and deleting it
  // when the date has passed, needs the owner role on the container.
  blobDataOwner: 'b7e6dc6d-f1e8-4753-8033-0f276bb0955b'
  blobDataContributor: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
  keyVaultSecretsUser: '4633458b-17de-408a-b874-0445c86b69e6'
  keyVaultCryptoUser: '12338af0-0e69-4776-bea7-57ae8d297424'
  eventHubsDataSender: '2b629674-e913-4c01-ae53-ef4638d8f975'
}

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-documents'
  location: location
  tags: tags
}

resource federation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: identity
  name: 'aks-${workloadNamespace}-ic-documents'
  properties: {
    issuer: aksOidcIssuer
    subject: 'system:serviceaccount:${workloadNamespace}:ic-documents'
    audiences: ['api://AzureADTokenExchange']
  }
}

// --- the vault and the print outbox -------------------------------------------------

resource account 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: take('${name}doc${unique}', 24)
  location: location
  tags: tags
  kind: 'StorageV2'
  sku: { name: environment == 'prod' ? 'Standard_GZRS' : 'Standard_LRS' }
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
  properties: {
    // Version-level immutability is built on versioning.
    isVersioningEnabled: true
    deleteRetentionPolicy: { enabled: true, days: 14 }
  }
}

// Each document is written with its own retention date where the date is
// known at issue, and cannot be changed or deleted before it. Nothing is
// locked by default at the container, because contractual documents are
// kept from the end of the policy, which is not known when they are written.
resource vaultContainer 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobs
  name: 'documents-vault'
  properties: {
    publicAccess: 'None'
    immutableStorageWithVersioning: { enabled: true }
  }
}

resource printOutbox 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobs
  name: 'print-outbox'
  properties: { publicAccess: 'None' }
}

resource vaultAccess 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vaultContainer
  name: guid(vaultContainer.id, identity.id, roles.blobDataOwner)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataOwner)
  }
}

resource printWrite 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: printOutbox
  name: guid(printOutbox.id, identity.id, roles.blobDataContributor)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

// The print house reads a file and deletes it once printed.
resource printCollect 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (!empty(printHousePrincipalId)) {
  scope: printOutbox
  name: guid(printOutbox.id, printHousePrincipalId, roles.blobDataContributor)
  properties: {
    principalId: printHousePrincipalId
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

// --- the wrapping key, the webhook key and the cluster token ----------------------------

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: keyVaultName
}

// Wrapped values name the key version they were wrapped under, so a new
// version takes over for new documents and old ones still unwrap.
resource wrappingKey 'Microsoft.KeyVault/vaults/keys@2023-07-01' = {
  parent: vault
  name: 'documents-kek'
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

resource webhookSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' = {
  parent: vault
  name: 'documents-webhook-key'
  tags: tags
  properties: {
    value: webhookKey
    contentType: 'text/plain'
  }
}

resource readWebhookSecret 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: webhookSecret
  name: guid(webhookSecret.id, identity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

resource clusterToken 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-documents'
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

// --- email and SMS ---------------------------------------------------------------------

resource communication 'Microsoft.Communication/communicationServices@2023-04-01' = {
  name: '${name}-${unique}-comms'
  location: 'global'
  tags: tags
  properties: {
    dataLocation: communicationDataLocation
  }
}

resource send 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: communication
  name: guid(communication.id, identity.id, communicationRoleDefinitionId)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', communicationRoleDefinitionId)
  }
}

// Delivery reports are the proof that a notice arrived. Event Grid carries
// them to the webhook, retries for a day, and presents the webhook key.
resource reportsTopic 'Microsoft.EventGrid/systemTopics@2022-06-15' = {
  name: '${name}-${unique}-delivery-reports'
  location: 'global'
  tags: tags
  properties: {
    source: communication.id
    topicType: 'Microsoft.Communication.CommunicationServices'
  }
}

resource reportsSubscription 'Microsoft.EventGrid/systemTopics/eventSubscriptions@2022-06-15' = if (!empty(deliveryReportsUrl)) {
  parent: reportsTopic
  name: 'documents-delivery-reports'
  properties: {
    eventDeliverySchema: 'EventGridSchema'
    filter: {
      includedEventTypes: [
        'Microsoft.Communication.EmailDeliveryReportReceived'
        'Microsoft.Communication.SMSDeliveryReportReceived'
      ]
    }
    destination: {
      endpointType: 'WebHook'
      properties: {
        endpointUrl: deliveryReportsUrl
        maxEventsPerBatch: 20
        preferredBatchSizeInKilobytes: 64
        deliveryAttributeMappings: [
          {
            name: 'x-webhook-key'
            type: 'Static'
            properties: {
              value: webhookKey
              isSecret: true
            }
          }
        ]
      }
    }
    retryPolicy: {
      maxDeliveryAttempts: 30
      eventTimeToLiveInMinutes: 1440
    }
  }
}

// --- events out ------------------------------------------------------------------------

resource eventHubs 'Microsoft.EventHub/namespaces@2024-01-01' existing = {
  name: eventHubsNamespaceName
}

// One partition each: a module's events are one ordered sequence.
resource hubs 'Microsoft.EventHub/namespaces/eventhubs@2024-01-01' = [
  for hub in ['documents-events', 'complaints-events']: {
    parent: eventHubs
    name: hub
    properties: {
      partitionCount: 1
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

output documentsClientId string = identity.properties.clientId
output documentsVaultEndpoint string = account.properties.primaryEndpoints.blob
output communicationEndpoint string = 'https://${communication.properties.hostName}'
output wrappingKeyName string = wrappingKey.name
