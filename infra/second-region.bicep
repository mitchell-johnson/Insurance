// The second region: Australia Southeast (Melbourne).
//
// Two ledger voters and the stateless services, in a cluster of their own
// on a network of its own. Every stateful service stays in the home region
// (infra/main.bicep): this region reaches them over the peering that
// infra/regions.bicep makes, through the home's private endpoints, under
// the same managed identities, which trust this cluster too (the federated
// credentials below). With leadership shared between the regions, both take
// writes; when the home region is lost, this one carries on with the
// witness (docs/operations.md, "Losing a region").
//
// Deploy into a resource group of its own in Australia Southeast, after
// main.bicep, from its outputs:
//   az deployment group create -g <rg-second> -f infra/second-region.bicep \
//     -p name=<name> environment=<env> homeResourceGroup=<rg-home> \
//        registryName=<registryName output>
//
// The cluster is the same as the home's: tools/check_regions.py fails the
// build if the two definitions drift apart.

targetScope = 'resourceGroup'

@description('Azure region: Australia Southeast, the home region\'s pair.')
param location string = resourceGroup().location

@description('The same short name as the home region\'s deployment.')
@minLength(3)
@maxLength(12)
param name string

@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

param workloadNamespace string = 'insurance-core'

@description('The home region\'s resource group (infra/main.bicep).')
param homeResourceGroup string

@description('The home\'s container registry (main.bicep output registryName). Geo-replicated to this region in prod.')
param registryName string

@description('Address space of this region\'s network. Must not overlap the home\'s (10.40.0.0/16) or the witness\'s (10.42.0.0/24).')
param vnetAddressSpace string = '10.41.0.0/16'

param privateNetworking bool = true
param adminIpRanges array = []

param systemVmSize string = 'Standard_D2ds_v5'
param ledgerVmSize string = 'Standard_D8ds_v5'
param appsVmSize string = 'Standard_D4ds_v5'
param appsMinCount int = 3
param appsMaxCount int = 9

@description('Machines in the ledger pool: one per voter in this region, in different zones.')
param ledgerPoolCount int = 2

@description('The home\'s managed identities this cluster\'s service accounts may use, as identity name suffix and service account. Those of main.bicep by default; add a module template\'s (customers, documents, finance-close, group, reinsurance, underwriting-driver, unit-pricing) once it is deployed and its workload runs here.')
param federations array = [
  { identity: 'node', serviceAccount: 'ic-node' }
  { identity: 'worker', serviceAccount: 'ic-worker' }
  { identity: 'api', serviceAccount: 'ic-api' }
  { identity: 'portal', serviceAccount: 'ic-portal' }
  { identity: 'watch', serviceAccount: 'ic-watch' }
  { identity: 'collector', serviceAccount: 'ic-otel-collector' }
  { identity: 'payments', serviceAccount: 'ic-payments' }
]

var tags = {
  system: 'insurance-core'
  environment: environment
  region: 'second'
}
var roles = {
  networkContributor: '4d97b98b-1d4f-4787-a291-c67834d212e7'
}
var nodeSubnetId = resourceId('Microsoft.Network/virtualNetworks/subnets', vnet.name, 'aks-nodes')

// The home's workspace: both clusters report to one place, so the alerts
// see both regions.
resource logs 'Microsoft.OperationalInsights/workspaces@2023-09-01' existing = {
  name: '${name}-logs'
  scope: resourceGroup(homeResourceGroup)
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
        name: 'ingress'
        properties: {
          addressPrefix: cidrSubnet(vnetAddressSpace, 24, 5)
          privateLinkServiceNetworkPolicies: 'Disabled'
        }
      }
      {
        // This region's voters' fixed addresses (deploy/regions/australiasoutheast).
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

resource clusterNetwork 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: vnet
  name: guid(vnet.id, clusterIdentity.id, roles.networkContributor)
  properties: {
    principalId: clusterIdentity.properties.principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.networkContributor)
  }
}

// This region's own fixed outbound range, for the same allow-lists as the home's.
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

// --- the cluster: the same definition as the home's -----------------------------

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

// --- what the home grants this cluster ----------------------------------------------

module homeGrants 'modules/second-region-grants.bicep' = {
  name: 'second-region-grants'
  scope: resourceGroup(homeResourceGroup)
  params: {
    name: name
    workloadNamespace: workloadNamespace
    oidcIssuer: aks.properties.oidcIssuerProfile.issuerURL
    kubeletObjectId: aks.properties.identityProfile.kubeletidentity.objectId
    registryName: registryName
    vnetId: vnet.id
    privateNetworking: privateNetworking
    federations: federations
  }
}

output aksName string = aks.name
output vnetId string = vnet.id
output oidcIssuer string = aks.properties.oidcIssuerProfile.issuerURL
output egressPrefix string = egress.properties.ipPrefix
output ledgerPeerSubnet string = cidrSubnet(vnetAddressSpace, 24, 6)
