// The insurance core on Azure: the home region's platform.
//
// The ledger runs in two regions and a witness in a third (docs/decisions.md,
// "Where the ledger's voters run"). This template is the home region,
// Australia East (Sydney): it holds every stateful service (the ledger's
// write-once evidence, the event stream, the vaults, the databases) and a
// cluster with two ledger voters. infra/second-region.bicep adds Australia
// Southeast (Melbourne): a cluster with two more voters and the stateless
// services, reaching these services over peering. infra/ledger-witness.bicep
// adds the witness, and infra/regions.bicep joins the three networks.
//
// Deploy at resource group scope:
//   az deployment group create -g <rg> -f infra/main.bicep -p infra/main.bicepparam
//
// Everything authenticates with Microsoft Entra ID. There are no account
// keys, connection strings or admin passwords anywhere in this template.

targetScope = 'resourceGroup'

@description('Azure region of the home: Australia East (Sydney). Its pair, Australia Southeast, is the second region, which is where the geo-redundant copies of the ledger evidence go.')
param location string = resourceGroup().location

@description('Short name used as a prefix for every resource. Lowercase letters and digits.')
@minLength(3)
@maxLength(12)
param name string

@description('Environment tag: dev, test or prod.')
@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('Kubernetes namespace the core runs in. The service accounts are ic-node, ic-worker, ic-api and ic-portal (see deploy/k8s); each federated credential trusts exactly one of them in this namespace.')
param workloadNamespace string = 'insurance-core'

@description('Days a Merkle root or sealed segment cannot be changed or deleted. Seven years plus a margin by default.')
param retentionDays int = 2920

@description('The workloads\' cluster tokens exist in Key Vault (deploy/cluster-access/issue.sh has run). Until then no identity is granted one.')
param clusterTokensIssued bool = false

// --- node pools (sized with ic-simulate: docs/azure-resources.md, "Sizing") ---

@description('The system pool runs only AKS\'s own pods (it is tainted CriticalAddonsOnly). One node per zone.')
param systemVmSize string = 'Standard_D2ds_v5'

@description('The ledger pool runs only the ic-node voters, one per zone, each with its Premium SSD v2 disk. A voter cannot move to another zone, because its disk is zonal.')
param ledgerVmSize string = 'Standard_D8ds_v5'

@description('The apps pool runs every other workload. The cluster autoscaler keeps it between these counts, balancing the zones.')
param appsVmSize string = 'Standard_D4ds_v5'
param appsMinCount int = 3
param appsMaxCount int = 9

// --- networking ---------------------------------------------------------------

@description('Address space of the virtual network. The pods use an overlay network of their own, so only nodes, private endpoints and the internal load balancer take addresses here.')
param vnetAddressSpace string = '10.40.0.0/16'

@description('Reach every PaaS service only through private endpoints in the virtual network. Off only for a throwaway dev environment.')
param privateNetworking bool = true

@description('Address ranges (CIDR) of the administrators\' workstations or build agents, admitted to the AKS API server, Key Vault, the registry and the ledger storage account beside the private endpoints. Empty: nothing is reachable from outside the virtual network, and deployment steps run from a host inside it (docs/operations.md, "Private networking").')
param adminIpRanges array = []

// --- monitoring -----------------------------------------------------------------

@description('E-mail addresses the core\'s alerts go to. Empty: the rules exist and fire, and are seen in Azure Monitor, but notify nobody.')
param alertEmails array = []

@description('Days logs and traces can be queried in the workspace.')
param logInteractiveDays int = 90

@description('Days logs and traces are kept in all, the rest in the archive tier. PLACEHOLDER: seven years, the best estimate of the insurer\'s audit need (docs/decisions.md, "Log retention"); confirm with compliance. Allowed values are 4 to 730, or whole years up to 4383.')
param logTotalRetentionDays int = 2556

@description('Ledger data voters expected to be running in both regions\' clusters, which both report to this workspace. An alert fires when fewer are. The witness is a virtual machine and is watched by ic-watch instead.')
param ledgerVoters int = 4

@description('The second region (infra/second-region.bicep).')
param secondRegion string = 'australiasoutheast'

@description('Keep copies of the event stream and the container images in the second region, so that it carries on when this one is lost. On in prod.')
param geoReplicate bool = environment == 'prod'

@description('Machines in the ledger pool: one per voter in this region, in different zones.')
param ledgerPoolCount int = 2

// --- protection -------------------------------------------------------------------

@description('Back up the ledger nodes\' disks with Azure Backup for AKS (snapshots every four hours, kept 7 days, and daily to the vault, kept 35 days).')
param backupLedgerDisks bool = true

@description('Deploy PostgreSQL Flexible Server. Off by default: the ledger does not use it. It is the persistence store for a self-hosted Temporal, if Temporal Cloud is not chosen; see docs/decisions.md.')
param deployPostgres bool = false

@description('Object ID of the Entra group that administers PostgreSQL. Required when deployPostgres is true.')
param postgresAdminGroupObjectId string = ''
param postgresAdminGroupName string = ''

@description('Temporal is self-hosted on this cluster with the PostgreSQL server above as its store (deploy/temporal), rather than Temporal Cloud. Needs deployPostgres and temporalPostgresPassword.')
param selfHostedTemporal bool = false

@description('Only with selfHostedTemporal: the password of its database login temporaladmin. Temporal cannot sign in with Entra ID, so this one login has a password, kept in Key Vault as temporal-postgres-password for the Temporal pods (deploy/temporal). ')
@secure()
param temporalPostgresPassword string = ''

var tags = {
  system: 'insurance-core'
  environment: environment
}
var unique = uniqueString(resourceGroup().id, name)
var storageName = take('${name}${unique}', 24)
var isProd = environment == 'prod'
// What every PaaS service's firewall admits beside its private endpoints.
var publicAccess = !privateNetworking || !empty(adminIpRanges) ? 'Enabled' : 'Disabled'
var roles = {
  networkContributor: '4d97b98b-1d4f-4787-a291-c67834d212e7'
  reader: 'acdd72a7-3385-48ef-bd42-f606fba81ae7'
  diskSnapshotContributor: '7efff54f-a5b4-42b5-a1c5-5411624893ce'
  diskBackupReader: '3e5e47e6-65f7-47ef-90b5-e5dd4d455f24'
  storageBlobDataContributorBackup: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
  contributor: 'b24988ac-6180-42a0-ab88-20f7382dd24c'
  acrPull: '7f951dda-4ed3-4680-a7ca-43fe172d538d'
  blobDataContributor: 'ba92f5b4-2d11-453d-a403-e96b0029c9fe'
  keyVaultCryptoUser: '12338af0-0e69-4776-bea7-57ae8d297424'
  eventHubsDataSender: '2b629674-e913-4c01-ae53-ef4638d8f975'
  keyVaultSecretsUser: '4633458b-17de-408a-b874-0445c86b69e6'
}

// --- observability ----------------------------------------------------------

// Container logs (Container Insights) and every trace and log the services
// send over OpenTelemetry (through the collector, deploy/k8s/telemetry.yaml,
// to Application Insights, whose data lives in this workspace). With private
// networking, both are ingested only through the Azure Monitor private link
// scope below; queries from the portal stay open to those with access.
resource logs 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: '${name}-logs'
  location: location
  tags: tags
  properties: {
    sku: { name: 'PerGB2018' }
    retentionInDays: logInteractiveDays
    publicNetworkAccessForIngestion: privateNetworking ? 'Disabled' : 'Enabled'
    publicNetworkAccessForQuery: 'Enabled'
  }
}

// What the audit needs kept: each table's interactive days and its total,
// the rest in the archive tier. The container logs carry the cluster access
// log and the reinsurer API log; the App tables carry the traces.
var retainedTables = [
  'ContainerLogV2'
  'ContainerLog'
  'KubeEvents'
  'KubePodInventory'
  'AppTraces'
  'AppRequests'
  'AppDependencies'
  'AppExceptions'
  'AzureDiagnostics'
]

resource retention 'Microsoft.OperationalInsights/workspaces/tables@2022-10-01' = [for table in retainedTables: {
  parent: logs
  name: table
  properties: {
    retentionInDays: logInteractiveDays
    totalRetentionInDays: logTotalRetentionDays
  }
}]

resource insights 'Microsoft.Insights/components@2020-02-02' = {
  name: '${name}-insights'
  location: location
  tags: tags
  kind: 'web'
  properties: {
    Application_Type: 'web'
    WorkspaceResourceId: logs.id
    IngestionMode: 'LogAnalytics'
    publicNetworkAccessForIngestion: privateNetworking ? 'Disabled' : 'Enabled'
    publicNetworkAccessForQuery: 'Enabled'
  }
}

// --- identity ---------------------------------------------------------------

// One identity per kind of process, each with only what that process needs:
//   ic-node    seals journal segments and publishes events
//   ic-worker  signs and publishes the day's Merkle root
//   ic-api     the gateway; reaches nothing in Azure but the cluster token
//   ic-payments  the payments pump: the bank keys, bank credentials and
//              payment evidence, in a vault and container of its own
//   ic-portal  the portal service for the app, the portals and the consoles;
//              likewise reaches nothing in Azure but the cluster token
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

resource portalIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-portal'
  location: location
  tags: tags
}

// ic-watch, the ledger watch (deploy/k8s/admin.yaml): reads its own cluster
// token, whose grant lets it ask the nodes' status and nothing else.
resource watchIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-watch'
  location: location
  tags: tags
}

resource watchFederation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: watchIdentity
  name: 'aks-${workloadNamespace}-ic-watch'
  properties: {
    issuer: aks.properties.oidcIssuerProfile.issuerURL
    subject: 'system:serviceaccount:${workloadNamespace}:ic-watch'
    audiences: ['api://AzureADTokenExchange']
  }
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

// --- network --------------------------------------------------------------------

// One virtual network. The node pools share one subnet (pods are on an
// overlay and take no addresses here); private endpoints have their own,
// and so does the internal load balancer Front Door reaches by Private
// Link. A second region, when it is chosen, is a peered network of its own
// with the same layout (docs/operations.md, "Losing a region").
var nodeSubnetId = resourceId('Microsoft.Network/virtualNetworks/subnets', vnet.name, 'aks-nodes')
var endpointSubnetId = resourceId('Microsoft.Network/virtualNetworks/subnets', vnet.name, 'private-endpoints')

resource endpointRules 'Microsoft.Network/networkSecurityGroups@2024-05-01' = {
  name: '${name}-endpoints-nsg'
  location: location
  tags: tags
  properties: {
    securityRules: [
      {
        name: 'from-the-network-only'
        properties: {
          priority: 100
          direction: 'Inbound'
          access: 'Allow'
          protocol: 'Tcp'
          sourceAddressPrefix: 'VirtualNetwork'
          sourcePortRange: '*'
          destinationAddressPrefix: 'VirtualNetwork'
          destinationPortRanges: ['443', '5432', '5671', '9093']
        }
      }
      {
        name: 'nothing-else'
        properties: {
          priority: 4000
          direction: 'Inbound'
          access: 'Deny'
          protocol: '*'
          sourceAddressPrefix: '*'
          sourcePortRange: '*'
          destinationAddressPrefix: '*'
          destinationPortRange: '*'
        }
      }
    ]
  }
}

resource vnet 'Microsoft.Network/virtualNetworks@2024-05-01' = {
  name: '${name}-vnet'
  location: location
  tags: tags
  properties: {
    addressSpace: { addressPrefixes: [vnetAddressSpace] }
    subnets: [
      {
        name: 'aks-nodes'
        properties: { addressPrefix: cidrSubnet(vnetAddressSpace, 22, 0) }
      }
      {
        name: 'private-endpoints'
        properties: {
          addressPrefix: cidrSubnet(vnetAddressSpace, 24, 4)
          // The security group above applies to the endpoints.
          privateEndpointNetworkPolicies: 'Enabled'
          networkSecurityGroup: { id: endpointRules.id }
        }
      }
      {
        name: 'ingress'
        properties: {
          addressPrefix: cidrSubnet(vnetAddressSpace, 24, 5)
          // The Private Link service of the internal load balancer lives here.
          privateLinkServiceNetworkPolicies: 'Disabled'
        }
      }
      {
        // The fixed private addresses at which the other regions reach each
        // ledger node: one internal load balancer address per voter
        // (deploy/regions), named in the ledger.internal zone (infra/regions.bicep).
        name: 'ledger-peers'
        properties: { addressPrefix: cidrSubnet(vnetAddressSpace, 24, 6) }
      }
    ]
  }
}

resource clusterIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-aks'
  location: location
  tags: tags
}

// The cluster places its nodes and its internal load balancer in the network.
resource clusterNetwork 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vnet
  name: guid(vnet.id, clusterIdentity.id, roles.networkContributor)
  properties: {
    principalId: clusterIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.networkContributor)
  }
}

// Where every connection out of the cluster comes from: four fixed
// addresses, zone-redundant, for the bank's and any provider's allow-list.
resource egress 'Microsoft.Network/publicIPPrefixes@2024-05-01' = {
  name: '${name}-egress'
  location: location
  tags: tags
  sku: { name: 'Standard', tier: 'Regional' }
  zones: ['1', '2', '3']
  properties: {
    prefixLength: 30
    publicIPAddressVersion: 'IPv4'
  }
}

// --- private endpoints --------------------------------------------------------------

// The private DNS zones, one per kind of service, linked to the network. The
// order matters: privateNetwork (an output) and the endpoints below name
// them by position.
var zoneNames = [
  'privatelink.blob.${az.environment().suffixes.storage}' // 0
  'privatelink.vaultcore.azure.net' // 1
  'privatelink.servicebus.windows.net' // 2
  'privatelink.azurecr.io' // 3
  'privatelink.postgres.database.azure.com' // 4
  'privatelink.monitor.azure.com' // 5
  'privatelink.oms.opinsights.azure.com' // 6
  'privatelink.ods.opinsights.azure.com' // 7
  'privatelink.agentsvc.azure-automation.net' // 8
]

resource zone 'Microsoft.Network/privateDnsZones@2020-06-01' = [for z in zoneNames: if (privateNetworking) {
  name: z
  location: 'global'
  tags: tags
}]

resource zoneLink 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2020-06-01' = [for (z, i) in zoneNames: if (privateNetworking) {
  parent: zone[i]
  name: '${name}-vnet'
  location: 'global'
  tags: tags
  properties: {
    virtualNetwork: { id: vnet.id }
    registrationEnabled: false
  }
}]

module ledgerBlobEndpoint 'modules/private-endpoint.bicep' = if (privateNetworking) {
  name: 'pe-ledger-blob'
  params: {
    name: '${storage.name}-blob'
    location: location
    tags: tags
    subnetId: endpointSubnetId
    targetId: storage.id
    groupId: 'blob'
    dnsZoneIds: [zone[0].id]
  }
}

module vaultEndpoint 'modules/private-endpoint.bicep' = if (privateNetworking) {
  name: 'pe-vault'
  params: {
    name: '${vault.name}-vault'
    location: location
    tags: tags
    subnetId: endpointSubnetId
    targetId: vault.id
    groupId: 'vault'
    dnsZoneIds: [zone[1].id]
  }
}

module paymentsVaultEndpoint 'modules/private-endpoint.bicep' = if (privateNetworking) {
  name: 'pe-payments-vault'
  params: {
    name: '${paymentsVault.name}-vault'
    location: location
    tags: tags
    subnetId: endpointSubnetId
    targetId: paymentsVault.id
    groupId: 'vault'
    dnsZoneIds: [zone[1].id]
  }
}

module eventsEndpoint 'modules/private-endpoint.bicep' = if (privateNetworking) {
  name: 'pe-events'
  params: {
    name: '${eventHubs.name}-namespace'
    location: location
    tags: tags
    subnetId: endpointSubnetId
    targetId: eventHubs.id
    groupId: 'namespace'
    dnsZoneIds: [zone[2].id]
  }
}

module registryEndpoint 'modules/private-endpoint.bicep' = if (privateNetworking) {
  name: 'pe-registry'
  params: {
    name: '${registry.name}-registry'
    location: location
    tags: tags
    subnetId: endpointSubnetId
    targetId: registry.id
    groupId: 'registry'
    dnsZoneIds: [zone[3].id]
  }
}

module postgresEndpoint 'modules/private-endpoint.bicep' = if (privateNetworking && deployPostgres) {
  name: 'pe-temporal-postgres'
  params: {
    name: '${name}-${unique}-pg-postgres'
    location: location
    tags: tags
    subnetId: endpointSubnetId
    targetId: postgres.id
    groupId: 'postgresqlServer'
    dnsZoneIds: [zone[4].id]
  }
}

// Azure Monitor through one private link scope: the workspace and
// Application Insights take data only through it.
resource monitorScope 'Microsoft.Insights/privateLinkScopes@2021-09-01' = if (privateNetworking) {
  name: '${name}-monitor'
  location: 'global'
  tags: tags
  properties: {
    accessModeSettings: {
      ingestionAccessMode: 'PrivateOnly'
      queryAccessMode: 'Open'
    }
  }
}

resource monitorScopeLogs 'Microsoft.Insights/privateLinkScopes/scopedResources@2021-09-01' = if (privateNetworking) {
  parent: monitorScope
  name: 'logs'
  properties: { linkedResourceId: logs.id }
}

resource monitorScopeInsights 'Microsoft.Insights/privateLinkScopes/scopedResources@2021-09-01' = if (privateNetworking) {
  parent: monitorScope
  name: 'insights'
  properties: { linkedResourceId: insights.id }
}

module monitorEndpoint 'modules/private-endpoint.bicep' = if (privateNetworking) {
  name: 'pe-monitor'
  dependsOn: [monitorScopeLogs, monitorScopeInsights]
  params: {
    name: '${name}-monitor'
    location: location
    tags: tags
    subnetId: endpointSubnetId
    targetId: monitorScope.id
    groupId: 'azuremonitor'
    dnsZoneIds: [zone[5].id, zone[6].id, zone[7].id, zone[8].id, zone[0].id]
  }
}

// --- the OpenTelemetry collector -------------------------------------------------------

// The collector (deploy/k8s/telemetry.yaml) receives every service's
// traces and logs over OTLP and sends them to Application Insights. Its
// connection string is the one secret it reads, kept in the vault rather
// than in a deployment output or a manifest.
resource collectorIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-collector'
  location: location
  tags: tags
}

resource collectorFederation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: collectorIdentity
  name: 'aks-${workloadNamespace}-ic-otel-collector'
  properties: {
    issuer: aks.properties.oidcIssuerProfile.issuerURL
    subject: 'system:serviceaccount:${workloadNamespace}:ic-otel-collector'
    audiences: ['api://AzureADTokenExchange']
  }
}

resource insightsSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' = {
  parent: vault
  name: 'appinsights-connection-string'
  properties: {
    value: insights.properties.ConnectionString
    contentType: 'text/plain'
  }
}

resource collectorReadsSecret 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: insightsSecret
  name: guid(insightsSecret.id, collectorIdentity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: collectorIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
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
    publicNetworkAccess: publicAccess
    networkRuleBypassOptions: 'AzureServices'
    networkRuleSet: {
      defaultAction: privateNetworking ? 'Deny' : 'Allow'
      ipRules: [for range in adminIpRanges: { action: 'Allow', value: range }]
    }
    // Images and their layers survive a zone.
    zoneRedundancy: isProd ? 'Enabled' : 'Disabled'
  }
}

// The images in the second region too: its cluster pulls there, and still
// can when this region is lost.
resource registryReplica 'Microsoft.ContainerRegistry/registries/replications@2023-07-01' = if (geoReplicate) {
  parent: registry
  name: secondRegion
  location: secondRegion
  tags: tags
  properties: {
    zoneRedundancy: 'Enabled'
    regionEndpointEnabled: true
  }
}

// --- compute ------------------------------------------------------------------

resource aks 'Microsoft.ContainerService/managedClusters@2026-08-01' = {
  name: '${name}-aks'
  location: location
  tags: tags
  dependsOn: [clusterNetwork]
  // The control plane's own identity, made first so that it can be given the
  // virtual network before the cluster exists.
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: { '${clusterIdentity.id}': {} }
  }
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
    // The API server answers only the administrators' ranges when they are
    // given; otherwise it is reached from inside the virtual network.
    apiServerAccessProfile: empty(adminIpRanges)
      ? { enablePrivateCluster: privateNetworking }
      : { authorizedIPRanges: adminIpRanges }
    // The Azure Disk CSI driver with the snapshot controller: the ledger's
    // Premium SSD v2 disks (deploy/k8s/storage.yaml) and their backups.
    storageProfile: {
      diskCSIDriver: { enabled: true }
      snapshotController: { enabled: true }
    }
    // The managed NGINX ingress (application routing). Its controller for
    // this system is internal, published to Front Door by Private Link
    // (deploy/k8s/ingress.yaml, infra/edge.bicep).
    ingressProfile: {
      webAppRouting: { enabled: true }
    }
    // Scale the apps pool on pending pods, keeping the zones even.
    autoScalerProfile: {
      'balance-similar-node-groups': 'true'
      expander: 'least-waste'
      'scale-down-unneeded-time': '10m'
      'skip-nodes-with-local-storage': 'false'
    }
    networkProfile: {
      networkPlugin: 'azure'
      networkPluginMode: 'overlay'
      // Every connection out leaves from one fixed range, which a bank or
      // any other provider can allow-list.
      outboundType: 'loadBalancer'
      loadBalancerSku: 'standard'
      loadBalancerProfile: {
        outboundIPPrefixes: { publicIPPrefixes: [{ id: egress.id }] }
      }
      networkPolicy: 'cilium'
      networkDataplane: 'cilium'
      // Pod-to-pod traffic that leaves a node is encrypted with WireGuard
      // (Advanced Container Networking Services). The services talk plain
      // HTTP inside the cluster and carry bearer tokens, so this is what
      // keeps those tokens and the ledger's commands off the wire in the
      // clear. Traffic between two pods on one node never leaves it.
      // WireGuard uses UDP 51871 between nodes; nothing here blocks it.
      // As `az aks create --enable-acns --acns-transit-encryption-type
      // wireguard` sets it (docs: aks/how-to-apply-wireguard). Check after
      // deploying: `kubectl -n kube-system exec ds/cilium -- cilium-dbg
      // encrypt status` shows WireGuard with one peer per other node.
      advancedNetworking: {
        enabled: true
        observability: { enabled: true }
        security: {
          enabled: true
          transitEncryption: { type: 'WireGuard' }
        }
      }
    }
    agentPoolProfiles: [
      {
        name: 'system'
        mode: 'System'
        count: 3
        vmSize: systemVmSize
        osType: 'Linux'
        osSKU: 'AzureLinux'
        availabilityZones: ['1', '2', '3']
        vnetSubnetID: nodeSubnetId
        // AKS's own pods only.
        nodeTaints: ['CriticalAddonsOnly=true:NoSchedule']
      }
      {
        name: 'ledger'
        mode: 'User'
        count: ledgerPoolCount
        vmSize: ledgerVmSize
        osType: 'Linux'
        osSKU: 'AzureLinux'
        // One voter per zone. A zone, or this whole region, can be lost
        // without losing quorum: the other region and the witness hold it.
        availabilityZones: ['1', '2', '3']
        vnetSubnetID: nodeSubnetId
        nodeLabels: { 'insurance-core/pool': 'ledger' }
        nodeTaints: ['insurance-core/ledger=true:NoSchedule']
      }
      {
        name: 'apps'
        mode: 'User'
        count: appsMinCount
        enableAutoScaling: true
        minCount: appsMinCount
        maxCount: appsMaxCount
        vmSize: appsVmSize
        osType: 'Linux'
        osSKU: 'AzureLinux'
        availabilityZones: ['1', '2', '3']
        vnetSubnetID: nodeSubnetId
        nodeLabels: { 'insurance-core/pool': 'apps' }
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
        config: {
          logAnalyticsWorkspaceResourceID: logs.id
          // Entra ID to the workspace, through its private link when private.
          useAADAuth: 'true'
        }
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

@description('Redundancy of the ledger account. Empty chooses: RA-GZRS in prod (three zones here and a copy in the paired region, readable there while this region is lost), LRS otherwise. A region with no pair cannot take GZRS: there, choose Standard_ZRS and the region-loss plan copies the proofs (docs/operations.md, "Losing a region").')
param ledgerStorageSku string = ''

// The ledger's write-once evidence: Merkle roots, sealed segments, payment
// evidence and reinsurance records. Geo-redundant in prod so the proofs
// survive the region, and immutable (the container policies below, locked
// in prod with tools/lock-ledger-worm.sh).
resource storage 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: storageName
  location: location
  tags: tags
  kind: 'StorageV2'
  sku: { name: !empty(ledgerStorageSku) ? ledgerStorageSku : (isProd ? 'Standard_RAGZRS' : 'Standard_LRS') }
  properties: {
    accessTier: 'Hot'
    minimumTlsVersion: 'TLS1_2'
    supportsHttpsTrafficOnly: true
    allowBlobPublicAccess: false
    // Entra ID only: shared keys and SAS signed with them are refused.
    allowSharedKeyAccess: false
    defaultToOAuthAuthentication: true
    publicNetworkAccess: publicAccess
    networkAcls: {
      defaultAction: privateNetworking ? 'Deny' : 'Allow'
      bypass: 'AzureServices'
      ipRules: [for range in adminIpRanges: { value: range, action: 'Allow' }]
    }
  }
}

// An account that holds eight years of proof is not deleted by accident.
resource storageLock 'Microsoft.Authorization/locks@2020-05-01' = if (isProd) {
  scope: storage
  name: 'ledger-evidence'
  properties: {
    level: 'CanNotDelete'
    notes: 'Write-once ledger evidence (docs/azure-resources.md). Removing this lock is a change for two people.'
  }
}

resource blobs 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: storage
  name: 'default'
  properties: {
    // A container deleted by mistake can be brought back for a year.
    containerDeleteRetentionPolicy: { enabled: true, days: 365 }
  }
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
    publicNetworkAccess: publicAccess
    networkAcls: {
      defaultAction: privateNetworking ? 'Deny' : 'Allow'
      bypass: 'AzureServices'
      ipRules: [for range in adminIpRanges: { value: range }]
    }
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

// The cluster tokens: each workload's own secret, cluster-token-<workload>,
// presented to the ledger nodes, which hold it to its grant in the access map
// (deploy/cluster-access). Their values are made by
// deploy/cluster-access/issue.sh, never by this template, so they are in no
// deployment history. Each identity may read its own token and no other
// secret: granted on the secret, not the vault, so it exists first. Deploy
// once with clusterTokensIssued false, run issue.sh, then deploy again with
// it true (docs/operations.md).
resource nodeTokenSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-node'
}

resource nodeToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (clusterTokensIssued) {
  scope: nodeTokenSecret
  name: guid(nodeTokenSecret.id, nodeIdentity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: nodeIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

resource watchTokenSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-watch'
}

resource watchToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (clusterTokensIssued) {
  scope: watchTokenSecret
  name: guid(watchTokenSecret.id, watchIdentity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: watchIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

resource workerTokenSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-worker'
}

resource workerToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (clusterTokensIssued) {
  scope: workerTokenSecret
  name: guid(workerTokenSecret.id, workerIdentity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: workerIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

resource portalFederation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: portalIdentity
  name: 'aks-${workloadNamespace}-ic-portal'
  properties: {
    issuer: aks.properties.oidcIssuerProfile.issuerURL
    subject: 'system:serviceaccount:${workloadNamespace}:ic-portal'
    audiences: ['api://AzureADTokenExchange']
  }
}

resource portalTokenSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-portal'
}

resource portalToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (clusterTokensIssued) {
  scope: portalTokenSecret
  name: guid(portalTokenSecret.id, portalIdentity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: portalIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

resource apiTokenSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-api'
}

resource apiToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (clusterTokensIssued) {
  scope: apiTokenSecret
  name: guid(apiTokenSecret.id, apiIdentity.id, roles.keyVaultSecretsUser)
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
resource eventHubs 'Microsoft.EventHub/namespaces@2024-05-01-preview' = {
  name: '${name}-${unique}-events'
  location: location
  tags: tags
  sku: { name: 'Premium', tier: 'Premium', capacity: 1 }
  properties: {
    disableLocalAuth: true
    minimumTlsVersion: '1.2'
    zoneRedundant: true
    publicNetworkAccess: privateNetworking ? 'Disabled' : 'Enabled'
    // Geo-replication to the second region: the stream is kept there too,
    // and promoting that copy makes it the primary when this region is lost
    // (docs/operations.md, "Losing a region").
    geoDataReplication: !geoReplicate ? null : {
      maxReplicationLagDurationInSeconds: 60
      locations: [
        { locationName: location, roleType: 'Primary' }
        { locationName: secondRegion, roleType: 'Secondary' }
      ]
    }
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
    // Entra ID, and a password only for Temporal's own login when it is
    // self-hosted here (selfHostedTemporal).
    authConfig: {
      activeDirectoryAuth: 'Enabled'
      passwordAuth: selfHostedTemporal ? 'Enabled' : 'Disabled'
      tenantId: subscription().tenantId
    }
    administratorLogin: selfHostedTemporal ? 'temporaladmin' : null
    administratorLoginPassword: selfHostedTemporal ? temporalPostgresPassword : null
    highAvailability: { mode: environment == 'prod' ? 'ZoneRedundant' : 'Disabled' }
    backup: { backupRetentionDays: 35, geoRedundantBackup: isProd ? 'Enabled' : 'Disabled' }
    // Reached only through its private endpoint.
    network: { publicNetworkAccess: privateNetworking ? 'Disabled' : 'Enabled' }
  }
}

// Temporal's two databases.
resource temporalDatabases 'Microsoft.DBforPostgreSQL/flexibleServers/databases@2024-08-01' = [for db in ['temporal', 'temporal_visibility']: if (deployPostgres) {
  parent: postgres
  name: db
  properties: { charset: 'UTF8', collation: 'en_US.utf8' }
}]

resource temporalPassword 'Microsoft.KeyVault/vaults/secrets@2023-07-01' = if (deployPostgres && selfHostedTemporal) {
  parent: vault
  name: 'temporal-postgres-password'
  properties: { value: temporalPostgresPassword }
}

// The self-hosted Temporal's pods read that password, and only they do.
resource temporalIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = if (deployPostgres && selfHostedTemporal) {
  name: '${name}-temporal'
  location: location
  tags: tags
}

resource temporalFederation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = if (deployPostgres && selfHostedTemporal) {
  parent: temporalIdentity
  name: 'aks-temporal-temporal'
  properties: {
    issuer: aks.properties.oidcIssuerProfile.issuerURL
    subject: 'system:serviceaccount:temporal:temporal'
    audiences: ['api://AzureADTokenExchange']
  }
}

resource temporalReadsPassword 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (deployPostgres && selfHostedTemporal) {
  scope: temporalPassword
  name: guid(resourceGroup().id, 'temporal-postgres-password', roles.keyVaultSecretsUser)
  properties: {
    principalId: temporalIdentity!.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
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

// --- payments and bank channels ------------------------------------------------

// The pump is the only process that speaks to a bank. It has its own
// identity and its own vault, so no other process can read a bank
// credential or sign a payment file, and the pump can read nothing of the
// core's but the cluster token.
resource paymentsIdentity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-payments'
  location: location
  tags: tags
}

resource paymentsFederation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = {
  parent: paymentsIdentity
  name: 'aks-${workloadNamespace}-ic-payments'
  properties: {
    issuer: aks.properties.oidcIssuerProfile.issuerURL
    subject: 'system:serviceaccount:${workloadNamespace}:ic-payments'
    audiences: ['api://AzureADTokenExchange']
  }
}

resource paymentsTokenSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-payments'
}

resource paymentsToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (clusterTokensIssued) {
  scope: paymentsTokenSecret
  name: guid(paymentsTokenSecret.id, paymentsIdentity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: paymentsIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

// The payments vault. It holds, per bank channel and region, the key that
// signs what is sent to the bank and the credential the bank issued. Keys
// and credentials are created by the connectivity administrator when a bank
// channel is set up, never by this template, and are named as the payments
// configuration names them (docs/payments.md).
resource paymentsVault 'Microsoft.KeyVault/vaults@2023-07-01' = {
  name: 'pay-${take(unique, 8)}-${take(replace(name, '-', ''), 10)}'
  location: location
  tags: tags
  properties: {
    tenantId: subscription().tenantId
    sku: { family: 'A', name: 'premium' }
    enableRbacAuthorization: true
    enablePurgeProtection: true
    enableSoftDelete: true
    softDeleteRetentionInDays: 90
    publicNetworkAccess: publicAccess
    networkAcls: {
      defaultAction: privateNetworking ? 'Deny' : 'Allow'
      bypass: 'AzureServices'
      ipRules: [for range in adminIpRanges: { value: range }]
    }
  }
}

// Sign with the bank keys and read the bank credentials. The pump cannot
// create, export or delete either.
resource paymentsSigning 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: paymentsVault
  name: guid(paymentsVault.id, paymentsIdentity.id, roles.keyVaultCryptoUser)
  properties: {
    principalId: paymentsIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultCryptoUser)
  }
}

resource paymentsCredentials 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: paymentsVault
  name: guid(paymentsVault.id, paymentsIdentity.id, roles.keyVaultSecretsUser)
  properties: {
    principalId: paymentsIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

// Every file and request sent to a bank, with its signed manifest, kept
// unchanged for the retention period: the evidence of what was instructed.
resource paymentsEvidence 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobs
  name: 'payments-evidence'
  properties: { publicAccess: 'None' }
}

resource paymentsWorm 'Microsoft.Storage/storageAccounts/blobServices/containers/immutabilityPolicies@2023-05-01' = {
  parent: paymentsEvidence
  name: 'default'
  properties: {
    immutabilityPeriodSinceCreationInDays: retentionDays
    allowProtectedAppendWrites: false
  }
}

resource paymentsEvidenceWrite 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: paymentsEvidence
  name: guid(paymentsEvidence.id, paymentsIdentity.id, roles.blobDataContributor)
  properties: {
    principalId: paymentsIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.blobDataContributor)
  }
}

// --- alerts: the core ------------------------------------------------------------------
//
// Each rule reads what the core's own processes write: the JSON log lines of
// ic-worker and of the ledger watch (ic-admin watch, run every five minutes
// by the ic-watch CronJob in deploy/k8s/admin.yaml), and Kubernetes' own
// inventory of pods and jobs. docs/operations.md, "Alerts", says what to do
// for each.

resource onCall 'Microsoft.Insights/actionGroups@2023-01-01' = if (!empty(alertEmails)) {
  name: '${name}-core-oncall'
  location: 'global'
  tags: tags
  properties: {
    groupShortName: 'core-oncall'
    enabled: true
    emailReceivers: [for (address, i) in alertEmails: {
      name: 'oncall-${i}'
      emailAddress: address
      useCommonAlertSchema: true
    }]
  }
}

var alertActionGroups = empty(alertEmails) ? [] : [onCall.id]

// Every container's log line, whichever container log table the cluster writes.
var containerLines = '''
union isfuzzy=true
  (ContainerLogV2 | project TimeGenerated, Pod = PodName, Line = tostring(LogMessage)),
  (ContainerLog | project TimeGenerated, Pod = "", Line = LogEntry)
'''

var coreAlerts = [
  {
    key: 'close-failed'
    displayName: 'Daily close failed'
    description: 'The ic-close job failed, or an ic-worker command logged that it failed. The day is not closed and its Merkle root is not published. See docs/operations.md, "Alerts".'
    severity: 1
    frequency: 'PT15M'
    window: 'PT30M'
    query: '${containerLines}| where Line has "ic-worker command failed"\n| project TimeGenerated, Pod\n| union (KubePodInventory | where ControllerName startswith "ic-close" and PodStatus == "Failed" | project TimeGenerated, Pod = Name)'
    threshold: 0
    dimensions: []
  }
  {
    key: 'node-down'
    displayName: 'Ledger node down'
    description: 'Fewer ic-node voters are running than expected, or the ledger watch cannot reach one. One more lost loses quorum. See docs/operations.md, "Alerts".'
    severity: 1
    frequency: 'PT5M'
    window: 'PT10M'
    query: 'KubePodInventory\n| where Namespace == "${workloadNamespace}" and ControllerName == "ic-node"\n| summarize Running = dcountif(Name, PodStatus == "Running") by bin(TimeGenerated, 5m)\n| where Running < ${ledgerVoters}\n| project TimeGenerated, Running\n| union (${containerLines}| where Line has "ledger.watch" and Line has "ledger node unreachable" | project TimeGenerated, Running = -1)'
    threshold: 0
    dimensions: []
  }
  {
    key: 'no-leader'
    displayName: 'Partition has no leader'
    description: 'A ledger partition has no leader: it accepts no commands. See docs/operations.md, "Alerts".'
    severity: 0
    frequency: 'PT5M'
    window: 'PT10M'
    query: '${containerLines}| where Line has "ledger.watch" and Line has "partition has no leader"\n| extend Partition = extract(@\'"partition":(\\d+)\', 1, Line)\n| project TimeGenerated, Partition'
    threshold: 0
    dimensions: [{ name: 'Partition', operator: 'Include', values: ['*'] }]
  }
  {
    key: 'hashes-differ'
    displayName: 'Ledger state hashes differ'
    description: 'The voters of a partition report different state hashes at the same index. Stop and investigate before anything else. See docs/operations.md, "Alerts".'
    severity: 0
    frequency: 'PT5M'
    window: 'PT10M'
    query: '${containerLines}| where Line has "ledger.watch" and Line has "state hashes differ"\n| extend Partition = extract(@\'"partition":(\\d+)\', 1, Line)\n| project TimeGenerated, Partition'
    threshold: 0
    dimensions: [{ name: 'Partition', operator: 'Include', values: ['*'] }]
  }
  {
    key: 'housekeeping-backlog'
    displayName: 'Housekeeping backlog'
    description: 'A partition has more entries waiting to be sealed, published or delivered than the ic-watch limit, in three checks running. See docs/operations.md, "Alerts".'
    severity: 2
    frequency: 'PT5M'
    window: 'PT15M'
    query: '${containerLines}| where Line has "ledger.watch" and Line has "housekeeping backlog"\n| extend Partition = extract(@\'"partition":(\\d+)\', 1, Line)\n| project TimeGenerated, Partition'
    threshold: 2
    dimensions: [{ name: 'Partition', operator: 'Include', values: ['*'] }]
  }
  {
    key: 'unit-orders-left'
    displayName: 'Unit orders left unpriced'
    description: 'Unit orders are still waiting for a price after the close or the unit-orders run. See docs/operations.md, "Alerts", and docs/unit-pricing.md.'
    severity: 2
    frequency: 'PT15M'
    window: 'PT30M'
    query: '${containerLines}| where Line has "unit orders left"\n| extend Partition = extract(@\'"partition":(\\d+)\', 1, Line)\n| project TimeGenerated, Partition'
    threshold: 0
    dimensions: [{ name: 'Partition', operator: 'Include', values: ['*'] }]
  }
]

resource coreAlert 'Microsoft.Insights/scheduledQueryRules@2022-06-15' = [for alert in coreAlerts: {
  name: '${name}-core-${alert.key}'
  location: location
  tags: tags
  properties: {
    displayName: alert.displayName
    description: alert.description
    severity: alert.severity
    enabled: true
    scopes: [logs.id]
    evaluationFrequency: alert.frequency
    windowSize: alert.window
    criteria: {
      allOf: [
        {
          query: alert.query
          timeAggregation: 'Count'
          dimensions: alert.dimensions
          operator: 'GreaterThan'
          threshold: alert.threshold
          failingPeriods: { numberOfEvaluationPeriods: 1, minFailingPeriodsToAlert: 1 }
        }
      ]
    }
    autoMitigate: true
    actions: { actionGroups: alertActionGroups }
  }
}]

// The watch itself stopped: no summary line for fifteen minutes, so every
// rule above that reads it is blind.
resource watchSilent 'Microsoft.Insights/scheduledQueryRules@2022-06-15' = {
  name: '${name}-core-watch-silent'
  location: location
  tags: tags
  properties: {
    displayName: 'Ledger watch silent'
    description: 'The ic-watch CronJob has written no "ledger watch" line for fifteen minutes, so the leader, node and backlog alerts cannot fire. See docs/operations.md, "Alerts".'
    severity: 2
    enabled: true
    scopes: [logs.id]
    evaluationFrequency: 'PT5M'
    windowSize: 'PT15M'
    criteria: {
      allOf: [
        {
          query: '${containerLines}| where Line has "ledger.watch" and Line has "\\"ledger watch\\""'
          timeAggregation: 'Count'
          operator: 'LessThan'
          threshold: 1
          failingPeriods: { numberOfEvaluationPeriods: 1, minFailingPeriodsToAlert: 1 }
        }
      ]
    }
    autoMitigate: true
    actions: { actionGroups: alertActionGroups }
  }
}

// --- backups: the ledger nodes' disks ---------------------------------------------------
//
// Azure Backup for AKS snapshots each ic-node disk every four hours (kept 7
// days, in the snapshot resource group) and copies one a day to the vault
// (kept 35 days). The ledger is replicated three ways and every sealed segment
// is in write-once storage, so a backup is the last line, not the first: a
// restored disk is behind its peers and rejoins as a new member (docs/
// operations.md, "Backups and restore").

resource backupVault 'Microsoft.DataProtection/backupVaults@2024-04-01' = if (backupLedgerDisks) {
  name: '${name}-backup'
  location: location
  tags: tags
  identity: { type: 'SystemAssigned' }
  properties: {
    storageSettings: [
      {
        datastoreType: 'VaultStore'
        type: isProd ? 'GeoRedundant' : 'LocallyRedundant'
      }
    ]
    securitySettings: {
      softDeleteSettings: { state: 'On', retentionDurationInDays: 14 }
      immutabilitySettings: { state: isProd ? 'Locked' : 'Unlocked' }
    }
  }
}

resource backupPolicy 'Microsoft.DataProtection/backupVaults/backupPolicies@2024-04-01' = if (backupLedgerDisks) {
  parent: backupVault
  name: 'ledger-disks'
  properties: {
    objectType: 'BackupPolicy'
    datasourceTypes: ['Microsoft.ContainerService/managedClusters']
    policyRules: [
      {
        objectType: 'AzureBackupRule'
        name: 'every-four-hours'
        backupParameters: { objectType: 'AzureBackupParams', backupType: 'Incremental' }
        dataStore: { objectType: 'DataStoreInfoBase', dataStoreType: 'OperationalStore' }
        trigger: {
          objectType: 'ScheduleBasedTriggerContext'
          schedule: { repeatingTimeIntervals: ['R/2026-01-01T00:00:00+00:00/PT4H'], timeZone: 'UTC' }
          taggingCriteria: [
            {
              isDefault: true
              tagInfo: { tagName: 'Default' }
              taggingPriority: 99
            }
          ]
        }
      }
      {
        objectType: 'AzureRetentionRule'
        name: 'Default'
        isDefault: true
        lifecycles: [
          {
            deleteAfter: { objectType: 'AbsoluteDeleteOption', duration: 'P7D' }
            sourceDataStore: { objectType: 'DataStoreInfoBase', dataStoreType: 'OperationalStore' }
            targetDataStoreCopySettings: [
              {
                dataStore: { objectType: 'DataStoreInfoBase', dataStoreType: 'VaultStore' }
                copyAfter: { objectType: 'CopyOnExpiryOption' }
              }
            ]
          }
          {
            deleteAfter: { objectType: 'AbsoluteDeleteOption', duration: 'P35D' }
            sourceDataStore: { objectType: 'DataStoreInfoBase', dataStoreType: 'VaultStore' }
          }
        ]
      }
    ]
  }
}

// The backup extension in the cluster, with a storage account for the
// Kubernetes objects it saves beside the disks.
resource backupStorage 'Microsoft.Storage/storageAccounts@2023-05-01' = if (backupLedgerDisks) {
  name: take('${name}bk${unique}', 24)
  location: location
  tags: tags
  sku: { name: isProd ? 'Standard_ZRS' : 'Standard_LRS' }
  kind: 'StorageV2'
  properties: {
    minimumTlsVersion: 'TLS1_2'
    allowBlobPublicAccess: false
    allowSharedKeyAccess: false
    publicNetworkAccess: publicAccess
    networkAcls: {
      defaultAction: 'Deny'
      bypass: 'AzureServices'
      ipRules: [for range in adminIpRanges: { value: range, action: 'Allow' }]
    }
  }
}

resource backupBlobs 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = if (backupLedgerDisks) {
  parent: backupStorage
  name: 'default'
}

resource backupContainer 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = if (backupLedgerDisks) {
  parent: backupBlobs
  name: 'aks-backup'
}

module backupBlobEndpoint 'modules/private-endpoint.bicep' = if (backupLedgerDisks && privateNetworking) {
  name: 'backup-blob-endpoint'
  params: {
    name: '${backupStorage.name}-blob'
    location: location
    tags: tags
    subnetId: endpointSubnetId
    targetId: backupStorage.id
    groupId: 'blob'
    dnsZoneIds: [zone[0].id]
  }
}

resource backupExtension 'Microsoft.KubernetesConfiguration/extensions@2023-05-01' = if (backupLedgerDisks) {
  scope: aks
  name: 'azure-aks-backup'
  properties: {
    extensionType: 'microsoft.dataprotection.kubernetes'
    autoUpgradeMinorVersion: true
    releaseTrain: 'stable'
    configurationSettings: {
      'configuration.backupStorageLocation.bucket': backupContainer.name
      'configuration.backupStorageLocation.config.storageAccount': backupStorage.name
      'configuration.backupStorageLocation.config.resourceGroup': resourceGroup().name
      'configuration.backupStorageLocation.config.subscriptionId': subscription().subscriptionId
      'credentials.tenantId': subscription().tenantId
      'configuration.backupStorageLocation.config.useAAD': 'true'
      'configuration.backupStorageLocation.config.storageAccountURI': backupStorage!.properties.primaryEndpoints.blob
    }
  }
}

// The extension's identity writes the Kubernetes objects to its account.
resource backupExtensionWrites 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (backupLedgerDisks) {
  scope: backupStorage
  name: guid(backupStorage.id, 'aks-backup-extension', roles.storageBlobDataContributorBackup)
  properties: {
    principalId: backupExtension!.properties.aksAssignedIdentity.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.storageBlobDataContributorBackup)
  }
}

// The vault reaches into the cluster through trusted access.
resource backupTrustedAccess 'Microsoft.ContainerService/managedClusters/trustedAccessRoleBindings@2024-02-01' = if (backupLedgerDisks) {
  parent: aks
  name: 'backup-vault'
  properties: {
    roles: ['Microsoft.DataProtection/backupVaults/backup-operator']
    sourceResourceId: backupVault.id
  }
}

// What the vault needs: to read the cluster and its disks, to take snapshots
// in this resource group, and to read the extension's account.
resource backupVaultReadsCluster 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (backupLedgerDisks) {
  scope: aks
  name: guid(aks.id, '${name}-backup', roles.reader)
  properties: {
    principalId: backupVault!.identity.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.reader)
  }
}

resource backupVaultSnapshots 'Microsoft.Authorization/roleAssignments@2022-04-01' = [for r in [roles.diskSnapshotContributor, roles.diskBackupReader]: if (backupLedgerDisks) {
  name: guid(resourceGroup().id, '${name}-backup', r)
  properties: {
    principalId: backupVault!.identity.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', r)
  }
}]

resource backupVaultReadsStorage 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (backupLedgerDisks) {
  scope: backupStorage
  name: guid(backupStorage.id, '${name}-backup', roles.reader)
  properties: {
    principalId: backupVault!.identity.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.reader)
  }
}

// The cluster's identity takes the snapshots into this resource group.
resource clusterSnapshots 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (backupLedgerDisks) {
  name: guid(resourceGroup().id, clusterIdentity.id, roles.diskSnapshotContributor)
  properties: {
    principalId: clusterIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.diskSnapshotContributor)
  }
}

// What is backed up: the ic-node pods' volumes and nothing else.
resource ledgerBackup 'Microsoft.DataProtection/backupVaults/backupInstances@2024-04-01' = if (backupLedgerDisks) {
  parent: backupVault
  name: 'ledger-nodes'
  dependsOn: [backupExtension, backupTrustedAccess, backupVaultReadsCluster, backupVaultSnapshots, backupVaultReadsStorage, clusterSnapshots]
  properties: {
    objectType: 'BackupInstance'
    friendlyName: 'ledger-nodes'
    dataSourceInfo: {
      objectType: 'Datasource'
      resourceID: aks.id
      resourceName: aks.name
      resourceType: 'Microsoft.ContainerService/managedClusters'
      resourceLocation: location
      datasourceType: 'Microsoft.ContainerService/managedClusters'
    }
    dataSourceSetInfo: {
      objectType: 'DatasourceSet'
      resourceID: aks.id
      resourceName: aks.name
      resourceType: 'Microsoft.ContainerService/managedClusters'
      resourceLocation: location
      datasourceType: 'Microsoft.ContainerService/managedClusters'
    }
    policyInfo: {
      policyId: backupPolicy.id
      policyParameters: {
        dataStoreParametersList: [
          {
            objectType: 'AzureOperationalStoreParameters'
            dataStoreType: 'OperationalStore'
            resourceGroupId: resourceGroup().id
          }
        ]
        backupDatasourceParametersList: [
          {
            objectType: 'KubernetesClusterBackupDatasourceParameters'
            includedNamespaces: [workloadNamespace]
            labelSelectors: ['app=ic-node']
            snapshotVolumes: true
            includeClusterScopeResources: false
          }
        ]
      }
    }
  }
}

// --- outputs: everything deploy/ needs ---------------------------------------------------

output aksName string = aks.name
output registryLoginServer string = registry.properties.loginServer
output nodeClientId string = nodeIdentity.properties.clientId
output workerClientId string = workerIdentity.properties.clientId
output apiClientId string = apiIdentity.properties.clientId
output portalClientId string = portalIdentity.properties.clientId
output watchClientId string = watchIdentity.properties.clientId
output keyVaultName string = vault.name
output paymentsClientId string = paymentsIdentity.properties.clientId
output paymentsKeyVaultName string = paymentsVault.name
output paymentsKeyVaultUrl string = paymentsVault.properties.vaultUri
output tenantId string = subscription().tenantId
output blobEndpoint string = storage.properties.primaryEndpoints.blob
output keyVaultUrl string = vault.properties.vaultUri
output eventHubsEndpoint string = 'https://${eventHubs.name}.servicebus.windows.net'
output logAnalyticsWorkspaceId string = logs.id
output logAnalyticsWorkspaceName string = logs.name
output alertActionGroupIds array = alertActionGroups
output backupVaultName string = backupLedgerDisks ? backupVault.name : ''
output collectorClientId string = collectorIdentity.properties.clientId
output vnetId string = vnet.id
output nodeSubnetId string = nodeSubnetId
output ingressSubnetName string = 'ingress'
// What each module template passes on to its private endpoints: the subnet,
// and the private DNS zone of each kind of service. Empty without private
// networking, and the module templates then make none.
output privateNetwork object = privateNetworking ? {
  subnetId: endpointSubnetId
  zones: {
    blob: zone[0].id
    vault: zone[1].id
    servicebus: zone[2].id
    postgres: zone[4].id
  }
  publicAccess: publicAccess
  adminIpRanges: adminIpRanges
} : {}
output postgresHost string = deployPostgres ? postgres!.properties.fullyQualifiedDomainName : ''
output temporalClientId string = deployPostgres && selfHostedTemporal ? temporalIdentity!.properties.clientId : ''
output egressPrefix string = egress.properties.ipPrefix
// What infra/second-region.bicep and infra/regions.bicep build on.
output oidcIssuer string = aks.properties.oidcIssuerProfile.issuerURL
output registryName string = registry.name
output privateDnsZoneNames array = privateNetworking ? zoneNames : []
output ledgerPeerSubnet string = cidrSubnet(vnetAddressSpace, 24, 6)
