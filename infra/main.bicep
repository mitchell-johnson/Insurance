// The insurance core on Azure: one region's worth of platform.
//
// Deploy at resource group scope:
//   az deployment group create -g <rg> -f infra/main.bicep -p infra/main.bicepparam
//
// Everything authenticates with Microsoft Entra ID. There are no account
// keys, connection strings or admin passwords anywhere in this template.

targetScope = 'resourceGroup'

@description('Azure region. The primary is New Zealand North; data stays in New Zealand unless a second region is added deliberately.')
param location string = resourceGroup().location

@description('Short name used as a prefix for every resource. Lowercase letters and digits.')
@minLength(3)
@maxLength(12)
param name string

@description('Environment tag: dev, test or prod.')
@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('Kubernetes namespace the core runs in. The service accounts are ic-node, ic-worker and ic-api (see deploy/k8s); each federated credential trusts exactly one of them in this namespace.')
param workloadNamespace string = 'insurance-core'

@description('Days a Merkle root or sealed segment cannot be changed or deleted. Seven years plus a margin by default.')
param retentionDays int = 2920

@description('Node size and count for the workload pool.')
param nodeVmSize string = 'Standard_D4ds_v5'
param nodeCount int = 3

@description('Deploy PostgreSQL Flexible Server. Off by default: the ledger does not use it. It is the persistence store for a self-hosted Temporal, if Temporal Cloud is not chosen; see docs/decisions.md.')
param deployPostgres bool = false

@description('Object ID of the Entra group that administers PostgreSQL. Required when deployPostgres is true.')
param postgresAdminGroupObjectId string = ''
param postgresAdminGroupName string = ''

var tags = {
  system: 'insurance-core'
  environment: environment
}
var unique = uniqueString(resourceGroup().id, name)
var storageName = take('${name}${unique}', 24)
var roles = {
  acrPull: '7f951dda-4ed3-4680-a7ca-43fe172d538d'
  blobDataContributor: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
  keyVaultCryptoUser: '12338af0-0e69-4776-bea7-57ae8d297424'
  eventHubsDataSender: '2b629674-e913-4c01-ae53-ef4638d8f975'
  keyVaultSecretsUser: '4633458b-17de-408a-b874-0445c86b69e6'
}

// --- observability ----------------------------------------------------------

resource logs 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: '${name}-logs'
  location: location
  tags: tags
  properties: {
    sku: { name: 'PerGB2018' }
    retentionInDays: 90
  }
}

resource insights 'Microsoft.Insights/components@2020-02-02' = {
  name: '${name}-insights'
  location: location
  tags: tags
  kind: 'web'
  properties: {
    Application_Type: 'web'
    WorkspaceResourceId: logs.id
  }
}

// --- identity ---------------------------------------------------------------

// One identity per kind of process, each with only what that process needs:
//   ic-node    seals journal segments and publishes events
//   ic-worker  signs and publishes the day's Merkle root
//   ic-api     the gateway; reaches nothing in Azure but the cluster token
// Pods get their identity through workload identity federation: no secret is
// ever issued for any of them.
resource nodeIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-node'
  location: location
  tags: tags
}

resource workerIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-worker'
  location: location
  tags: tags
}

resource apiIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-api'
  location: location
  tags: tags
}

resource nodeFederation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: nodeIdentity
  name: 'aks-${workloadNamespace}-ic-node'
  properties: {
    issuer: aks.properties.oidcIssuerProfile.issuerURL
    subject: 'system:serviceaccount:${workloadNamespace}:ic-node'
    audiences: ['api://AzureADTokenExchange']
  }
}

resource workerFederation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: workerIdentity
  name: 'aks-${workloadNamespace}-ic-worker'
  properties: {
    issuer: aks.properties.oidcIssuerProfile.issuerURL
    subject: 'system:serviceaccount:${workloadNamespace}:ic-worker'
    audiences: ['api://AzureADTokenExchange']
  }
}

resource apiFederation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: apiIdentity
  name: 'aks-${workloadNamespace}-ic-api'
  properties: {
    issuer: aks.properties.oidcIssuerProfile.issuerURL
    subject: 'system:serviceaccount:${workloadNamespace}:ic-api'
    audiences: ['api://AzureADTokenExchange']
  }
}

// --- container registry -------------------------------------------------------

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' = {
  name: take('${name}${unique}acr', 50)
  location: location
  tags: tags
  sku: { name: 'Premium' }
  properties: {
    adminUserEnabled: false
    anonymousPullEnabled: false
  }
}

// --- compute ------------------------------------------------------------------

resource aks 'Microsoft.ContainerService/managedClusters@2024-05-01' = {
  name: '${name}-aks'
  location: location
  tags: tags
  identity: { type: 'SystemAssigned' }
  sku: {
    name: 'Base'
    tier: environment == 'prod' ? 'Standard' : 'Free'
  }
  properties: {
    dnsPrefix: '${name}-${environment}'
    enableRBAC: true
    disableLocalAccounts: true
    aadProfile: {
      managed: true
      enableAzureRBAC: true
    }
    oidcIssuerProfile: { enabled: true }
    securityProfile: {
      workloadIdentity: { enabled: true }
    }
    networkProfile: {
      networkPlugin: 'azure'
      networkPluginMode: 'overlay'
      networkPolicy: 'cilium'
      networkDataplane: 'cilium'
    }
    agentPoolProfiles: [
      {
        name: 'system'
        mode: 'System'
        count: nodeCount
        vmSize: nodeVmSize
        osType: 'Linux'
        osSKU: 'AzureLinux'
        // One voter per zone: a zone can be lost without losing quorum.
        availabilityZones: ['1', '2', '3']
      }
    ]
    addonProfiles: {
      // Mounts the cluster token from Key Vault into the pods and follows
      // its rotation.
      azureKeyvaultSecretsProvider: {
        enabled: true
        config: {
          enableSecretRotation: 'true'
          rotationPollInterval: '2m'
        }
      }
      omsagent: {
        enabled: true
        config: { logAnalyticsWorkspaceResourceID: logs.id }
      }
    }
    autoUpgradeProfile: {
      upgradeChannel: 'patch'
      nodeOSUpgradeChannel: 'NodeImage'
    }
  }
}

resource kubeletPull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: registry
  name: guid(registry.id, aks.id, roles.acrPull)
  properties: {
    principalId: aks.properties.identityProfile.kubeletidentity.objectId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.acrPull)
  }
}

// --- write-once storage ---------------------------------------------------------

resource storage 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: storageName
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
  parent: storage
  name: 'default'
}

// Signed daily Merkle roots. Small, kept hot, never changed.
resource merkleRoots 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobs
  name: 'merkle-roots'
  properties: { publicAccess: 'None' }
}

// Sealed cold-tier segments of the journal.
resource coldSegments 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobs
  name: 'cold-segments'
  properties: { publicAccess: 'None' }
}

// Time-based retention (WORM). The policies are created unlocked so a test
// environment can be torn down. Lock them in production: locking cannot be
// undone, which is the point, so it is a deliberate step and not a default.
resource merkleWorm 'Microsoft.Storage/storageAccounts/blobServices/containers/immutabilityPolicies@2023-05-01' = {
  parent: merkleRoots
  name: 'default'
  properties: {
    immutabilityPeriodSinceCreationInDays: retentionDays
    allowProtectedAppendWrites: false
  }
}

resource coldWorm 'Microsoft.Storage/storageAccounts/blobServices/containers/immutabilityPolicies@2023-05-01' = {
  parent: coldSegments
  name: 'default'
  properties: {
    immutabilityPeriodSinceCreationInDays: retentionDays
    allowProtectedAppendWrites: false
  }
}

// Old segments move down the tiers. Tiering does not rewrite a blob, so it
// is allowed under the retention policy.
resource lifecycle 'Microsoft.Storage/storageAccounts/managementPolicies@2023-05-01' = {
  parent: storage
  name: 'default'
  properties: {
    policy: {
      rules: [
        {
          name: 'cold-segments-tiering'
          enabled: true
          type: 'Lifecycle'
          definition: {
            filters: {
              blobTypes: ['blockBlob']
              prefixMatch: ['cold-segments/']
            }
            actions: {
              baseBlob: {
                tierToCool: { daysAfterModificationGreaterThan: 30 }
                tierToCold: { daysAfterModificationGreaterThan: 180 }
              }
            }
          }
        }
      ]
    }
  }
}

// Nodes write sealed segments; workers write Merkle roots. Neither can
// write the other's container, and the retention policy stops both from
// changing or deleting what is written.
resource nodeCold 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: coldSegments
  name: guid(coldSegments.id, nodeIdentity.id, roles.blobDataContributor)
  properties: {
    principalId: nodeIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

resource workerRoots 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: merkleRoots
  name: guid(merkleRoots.id, workerIdentity.id, roles.blobDataContributor)
  properties: {
    principalId: workerIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

// --- signing key ------------------------------------------------------------------

// Premium vault: the key is generated in and never leaves an HSM. A Managed
// HSM is the step up if the regulator asks for a single-tenant HSM; the
// signer in ic-azure speaks to either.
resource vault 'Microsoft.KeyVault/vaults@2023-07-01' = {
  name: take('${name}-${unique}', 24)
  location: location
  tags: tags
  properties: {
    tenantId: subscription().tenantId
    sku: { family: 'A', name: 'premium' }
    enableRbacAuthorization: true
    enablePurgeProtection: true
    enableSoftDelete: true
    softDeleteRetentionInDays: 90
  }
}

resource merkleKey 'Microsoft.KeyVault/vaults/keys@2023-07-01' = {
  parent: vault
  name: 'merkle-root-signing'
  properties: {
    kty: 'EC-HSM'
    curveName: 'P-256'
    keyOps: ['sign', 'verify']
    attributes: { exportable: false }
  }
}

// Sign and verify with the key. The worker cannot read, export or delete it.
resource workerSigning 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: merkleKey
  name: guid(merkleKey.id, workerIdentity.id, roles.keyVaultCryptoUser)
  properties: {
    principalId: workerIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultCryptoUser)
  }
}

// The cluster token: the shared secret every caller of a ledger node
// presents. Its value is set by an operator, never by this template, so it
// is in no deployment history:
//   az keyvault secret set --vault-name <vault> --name cluster-token --value "$(openssl rand -hex 32)"
// To rotate, store the new and the old token on separate lines, new first;
// see docs/runbooks.md. Each process may read this one secret and no other.
// The vault holds this secret and the signing key and nothing else.
resource nodeToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vault
  name: guid(vault.id, nodeIdentity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: nodeIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

resource workerToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vault
  name: guid(vault.id, workerIdentity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: workerIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

resource apiToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vault
  name: guid(vault.id, apiIdentity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: apiIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

// --- event stream -------------------------------------------------------------------

// Premium: 64 hub partitions, one per ledger partition, so order within a
// ledger partition survives the stream. The Kafka endpoint is on by default
// at this tier for consumers that prefer it.
resource eventHubs 'Microsoft.EventHub/namespaces@2024-01-01' = {
  name: '${name}-${unique}-events'
  location: location
  tags: tags
  sku: { name: 'Premium', tier: 'Premium', capacity: 1 }
  properties: {
    disableLocalAuth: true
    minimumTlsVersion: '1.2'
    zoneRedundant: true
  }
}

resource policyEvents 'Microsoft.EventHub/namespaces/eventhubs@2024-01-01' = {
  parent: eventHubs
  name: 'policy-events'
  properties: {
    partitionCount: 64
    retentionDescription: {
      cleanupPolicy: 'Delete'
      retentionTimeInHours: 168
    }
  }
}

resource nodeEvents 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: policyEvents
  name: guid(policyEvents.id, nodeIdentity.id, roles.eventHubsDataSender)
  properties: {
    principalId: nodeIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.eventHubsDataSender)
  }
}

// --- read model store (optional) ------------------------------------------------------

resource postgres 'Microsoft.DBforPostgreSQL/flexibleServers@2024-08-01' = if (deployPostgres) {
  name: '${name}-${unique}-pg'
  location: location
  tags: tags
  sku: { name: 'Standard_D2ds_v5', tier: 'GeneralPurpose' }
  properties: {
    version: '16'
    storage: { storageSizeGB: 128 }
    // Entra ID only. No password exists for this server.
    authConfig: {
      activeDirectoryAuth: 'Enabled'
      passwordAuth: 'Disabled'
      tenantId: subscription().tenantId
    }
    highAvailability: { mode: environment == 'prod' ? 'ZoneRedundant' : 'Disabled' }
    backup: { backupRetentionDays: 35 }
  }
}

resource postgresAdmin 'Microsoft.DBforPostgreSQL/flexibleServers/administrators@2024-08-01' = if (deployPostgres) {
  parent: postgres
  name: postgresAdminGroupObjectId
  properties: {
    principalType: 'Group'
    principalName: postgresAdminGroupName
    tenantId: subscription().tenantId
  }
}

// --- outputs: everything deploy/ needs ---------------------------------------------------

output aksName string = aks.name
output registryLoginServer string = registry.properties.loginServer
output nodeClientId string = nodeIdentity.properties.clientId
output workerClientId string = workerIdentity.properties.clientId
output apiClientId string = apiIdentity.properties.clientId
output keyVaultName string = vault.name
output tenantId string = subscription().tenantId
output blobEndpoint string = storage.properties.primaryEndpoints.blob
output keyVaultUrl string = vault.properties.vaultUri
output eventHubsEndpoint string = 'https://${eventHubs.name}.servicebus.windows.net'
output applicationInsightsConnectionString string = insights.properties.ConnectionString
