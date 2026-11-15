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
// - A storage account with SFTP, where each reinsurer collects its
//   statements and listings from a container of its own (`ri-<reinsurer>`)
//   as a local user that can read only that container, by SSH key. The
//   driver may write to those containers. Listed in `sftpReinsurers`.
// - Two log alerts on the workspace main.bicep created, which holds the
//   driver's log through the cluster's monitoring add-on: one for every
//   exception the module opens, one for delivery that has stopped. They go
//   to an action group that mails `alertEmails`; with no addresses the
//   alerts still fire and show in Azure Monitor, but tell nobody.
//
// The reinsurance module itself is hosted by the ledger nodes and needs
// nothing here.
//
// Deployed on its own into the same resource group as main.bicep:
//   az deployment group create -g <rg> -f infra/reinsurance.bicep \
//     -p name=<name> storageAccountName=<account> keyVaultName=<vault> \
//        eventHubsNamespaceName=<namespace> aksOidcIssuer=<url> \
//        alertEmails='["reinsurance-oncall@example.com"]'

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

@description('The Log Analytics workspace main.bicep created, which receives the cluster\'s container logs.')
param logAnalyticsWorkspaceName string = '${name}-logs'

@description('Who is told when reinsurance needs a person: the operations on-call addresses.')
param alertEmails array = []

@description('Reinsurers that collect statements over SFTP: [{ "id": "reinsurer-north", "sshPublicKey": "ssh-ed25519 AAAA..." }]. The id is the reinsurer ID in the treaties; its container is ri-<id> in lower case with underscores as hyphens.')
param sftpReinsurers array = []


// --- private networking (main.bicep output privateNetwork) ---------------------------

@description('main.bicep\'s output privateNetwork: the subnet and the private DNS zones for private endpoints, and what the firewalls admit beside them. Empty: no private endpoints, and the services here stay reachable from anywhere, with Entra ID still required.')
param privateNetwork object = {}

@description('Address ranges (CIDR) of the outside party that reaches the SFTP drop account (the reinsurers). Empty: that account stays reachable from anywhere, with Entra ID still required; given, it admits only these ranges beside the private endpoint and the administrators.')
param reinsurerIpRanges array = []

var tags = {
  system: 'insurance-core'
  environment: environment
  component: 'reinsurance'
}
var isPrivate = !empty(privateNetwork)
var adminIpRules = map(privateNetwork.?adminIpRanges ?? [], range => { value: range, action: 'Allow' })
// An account an outside party reaches too: its ranges, or anywhere if none are given.
var reinsurerAcls = {
  defaultAction: isPrivate && !empty(reinsurerIpRanges) ? 'Deny' : 'Allow'
  bypass: 'AzureServices'
  ipRules: concat(adminIpRules, map(reinsurerIpRanges, range => { value: range, action: 'Allow' }))
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

// Its own cluster token only (deploy/cluster-access/issue.sh makes it first).
resource driverTokenSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-reinsurance'
}

resource driverToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: driverTokenSecret
  name: guid(driverTokenSecret.id, identity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

// --- alerts ---------------------------------------------------------------------------

resource logs 'Microsoft.OperationalInsights/workspaces@2023-09-01' existing = {
  name: logAnalyticsWorkspaceName
}

resource onCall 'Microsoft.Insights/actionGroups@2023-01-01' = if (!empty(alertEmails)) {
  name: '${name}-reinsurance-oncall'
  location: 'global'
  tags: tags
  properties: {
    groupShortName: 'ri-oncall'
    enabled: true
    emailReceivers: [for (address, i) in alertEmails: {
      name: 'oncall-${i}'
      emailAddress: address
      useCommonAlertSchema: true
    }]
  }
}

var actionGroups = empty(alertEmails) ? [] : [resourceId('Microsoft.Insights/actionGroups', '${name}-reinsurance-oncall')]

// The driver's log, whichever container log table the cluster writes. The
// driver logs JSON lines; a notice for staff has the target
// `reinsurance.notice` and names its kind (docs/reinsurance.md, Staff).
var driverLog = '''
union isfuzzy=true
  (ContainerLogV2 | project TimeGenerated, Line = tostring(LogMessage)),
  (ContainerLog | project TimeGenerated, Line = LogEntry)
'''

resource exceptionOpened 'Microsoft.Insights/scheduledQueryRules@2022-06-15' = {
  name: '${name}-reinsurance-exception-opened'
  location: location
  tags: tags
  dependsOn: [onCall]
  properties: {
    displayName: 'Reinsurance exception opened'
    description: 'The reinsurance module opened an exception for a person. Open the reinsurance console in the staff portal; the alert lists each exception id and code.'
    severity: 2
    enabled: true
    scopes: [logs.id]
    evaluationFrequency: 'PT5M'
    windowSize: 'PT5M'
    criteria: {
      allOf: [
        {
          query: '${driverLog}| where Line has "reinsurance.notice" and Line has "exception_opened"\n| extend Id = extract(@\'"id":"([^"]+)"\', 1, Line), Code = extract(@\'"code":"([^"]+)"\', 1, Line)\n| project TimeGenerated, Id, Code'
          timeAggregation: 'Count'
          dimensions: [
            { name: 'Code', operator: 'Include', values: ['*'] }
          ]
          operator: 'GreaterThan'
          threshold: 0
          failingPeriods: { numberOfEvaluationPeriods: 1, minFailingPeriodsToAlert: 1 }
        }
      ]
    }
    autoMitigate: false
    actions: { actionGroups: actionGroups }
  }
}

// One failed pass is a blip; three in fifteen minutes is delivery stopped.
resource deliveryStopped 'Microsoft.Insights/scheduledQueryRules@2022-06-15' = {
  name: '${name}-reinsurance-delivery-stopped'
  location: location
  tags: tags
  dependsOn: [onCall]
  properties: {
    displayName: 'Reinsurance delivery stopped'
    description: 'The reinsurance driver has not delivered to the core or handed on notices for fifteen minutes. See docs/reinsurance.md, Hosting and the driver.'
    severity: 1
    enabled: true
    scopes: [logs.id]
    evaluationFrequency: 'PT5M'
    windowSize: 'PT15M'
    criteria: {
      allOf: [
        {
          query: '${driverLog}| where Line has "reinsurance delivery is stopped" or Line has "a reinsurance delivery pass could not finish" or Line has "reinsurance notices could not all be handed on"'
          timeAggregation: 'Count'
          operator: 'GreaterThanOrEqual'
          threshold: 3
          failingPeriods: { numberOfEvaluationPeriods: 1, minFailingPeriodsToAlert: 1 }
        }
      ]
    }
    autoMitigate: true
    actions: { actionGroups: actionGroups }
  }
}

// --- what reinsurers collect, over SFTP ---------------------------------------------

// SFTP needs a hierarchical namespace, which the ledger's account does not
// have, so reinsurers collect from an account of their own. No shared keys:
// the driver writes with its identity and each reinsurer reads with its SSH
// key as a local user scoped to its own container.
resource drops 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: take('${toLower(replace(name, '-', ''))}ridrop${uniqueString(resourceGroup().id)}', 24)
  location: location
  tags: tags
  kind: 'StorageV2'
  sku: { name: environment == 'prod' ? 'Standard_ZRS' : 'Standard_LRS' }
  properties: {
    publicNetworkAccess: 'Enabled'
    networkAcls: reinsurerAcls
    isHnsEnabled: true
    isSftpEnabled: true
    isLocalUserEnabled: true
    allowSharedKeyAccess: false
    allowBlobPublicAccess: false
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
  }
}

module dropsEndpoint 'modules/private-endpoint.bicep' = if (isPrivate) {
  name: 'pe-reinsurance-drops'
  params: {
    name: '${drops.name}-blob'
    location: location
    tags: tags
    subnetId: privateNetwork.?subnetId ?? ''
    targetId: drops.id
    groupId: 'blob'
    dnsZoneIds: [privateNetwork.?zones.?blob ?? '']
  }
}

resource dropBlobs 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: drops
  name: 'default'
}

resource dropContainers 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = [for r in sftpReinsurers: {
  parent: dropBlobs
  name: 'ri-${toLower(replace(r.id, '_', '-'))}'
  properties: { publicAccess: 'None' }
}]

resource dropUsers 'Microsoft.Storage/storageAccounts/localUsers@2023-05-01' = [for (r, i) in sftpReinsurers: {
  parent: drops
  name: take(toLower(replace(replace(r.id, '_', ''), '-', '')), 64)
  dependsOn: [dropContainers[i]]
  properties: {
    homeDirectory: 'ri-${toLower(replace(r.id, '_', '-'))}'
    permissionScopes: [
      {
        permissions: 'rl'
        service: 'blob'
        resourceName: 'ri-${toLower(replace(r.id, '_', '-'))}'
      }
    ]
    hasSshKey: true
    hasSshPassword: false
    hasSharedKey: false
    sshAuthorizedKeys: [
      { description: r.id, key: r.sshPublicKey }
    ]
  }
}]

resource driverDrops 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: drops
  name: guid(drops.id, identity.id, roles.blobDataContributor)
  properties: {
    principalId: identity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

output reinsuranceIdentityClientId string = identity.properties.clientId
output recordsContainer string = records.name
output eventHub string = notices.name
output dropsBlobEndpoint string = drops.properties.primaryEndpoints.blob
output dropsSftpHost string = '${drops.name}.blob.${az.environment().suffixes.storage}'
