// Joins the ledger's three networks: the home region (infra/main.bicep),
// the second region (infra/second-region.bicep) and the witness
// (infra/ledger-witness.bicep).
//
// - Each network is peered with each other one (global peering over
//   Microsoft's backbone, where traffic between datacenters is encrypted at
//   the link layer).
// - The private zone ledger.internal names every voter at a fixed address:
//   ic-node-1 and -2 in the home, -3 and -4 in the second region, ic-witness
//   in the third. The nodes' peer list (deploy/k8s/settings.yaml) uses these
//   names; each data voter's address is an internal load balancer in its
//   region's ledger-peers subnet (deploy/regions).
//
// Deploy into the home's resource group, last:
//   az deployment group create -g <rg-home> -f infra/regions.bicep \
//     -p name=<name> secondResourceGroup=<rg-second> witnessResourceGroup=<rg-witness>

targetScope = 'resourceGroup'

param name string
param secondResourceGroup string
param witnessResourceGroup string

@description('Each voter\'s fixed address: the fourth address onwards of its region\'s ledger-peers subnet (main.bicep and second-region.bicep output ledgerPeerSubnet), and the witness\'s own.')
param voterAddresses object = {
  'ic-node-1': '10.40.6.4'
  'ic-node-2': '10.40.6.5'
  'ic-node-3': '10.41.6.4'
  'ic-node-4': '10.41.6.5'
  'ic-witness': '10.42.0.10'
}

var homeVnetName = '${name}-vnet'
var secondVnetName = '${name}-vnet'
var witnessVnetName = '${name}-witness-vnet'
var homeVnetId = resourceId('Microsoft.Network/virtualNetworks', homeVnetName)
var secondVnetId = resourceId(secondResourceGroup, 'Microsoft.Network/virtualNetworks', secondVnetName)
var witnessVnetId = resourceId(witnessResourceGroup, 'Microsoft.Network/virtualNetworks', witnessVnetName)

module homeToSecond 'modules/peering.bicep' = {
  name: 'peer-home-second'
  params: { localVnetName: homeVnetName, remoteVnetId: secondVnetId, peeringName: 'to-second' }
}

module secondToHome 'modules/peering.bicep' = {
  name: 'peer-second-home'
  scope: resourceGroup(secondResourceGroup)
  params: { localVnetName: secondVnetName, remoteVnetId: homeVnetId, peeringName: 'to-home' }
}

module homeToWitness 'modules/peering.bicep' = {
  name: 'peer-home-witness'
  params: { localVnetName: homeVnetName, remoteVnetId: witnessVnetId, peeringName: 'to-witness' }
}

module witnessToHome 'modules/peering.bicep' = {
  name: 'peer-witness-home'
  scope: resourceGroup(witnessResourceGroup)
  params: { localVnetName: witnessVnetName, remoteVnetId: homeVnetId, peeringName: 'to-home' }
}

module secondToWitness 'modules/peering.bicep' = {
  name: 'peer-second-witness'
  scope: resourceGroup(secondResourceGroup)
  params: { localVnetName: secondVnetName, remoteVnetId: witnessVnetId, peeringName: 'to-witness' }
}

module witnessToSecond 'modules/peering.bicep' = {
  name: 'peer-witness-second'
  scope: resourceGroup(witnessResourceGroup)
  params: { localVnetName: witnessVnetName, remoteVnetId: secondVnetId, peeringName: 'to-second' }
}

resource ledgerZone 'Microsoft.Network/privateDnsZones@2020-06-01' = {
  name: 'ledger.internal'
  location: 'global'
  tags: { system: 'insurance-core' }
}

resource voter 'Microsoft.Network/privateDnsZones/A@2020-06-01' = [for v in items(voterAddresses): {
  parent: ledgerZone
  name: v.key
  properties: {
    ttl: 60
    aRecords: [{ ipv4Address: v.value }]
  }
}]

resource ledgerLinks 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2020-06-01' = [for (vnet, i) in [homeVnetId, secondVnetId, witnessVnetId]: {
  parent: ledgerZone
  name: '${name}-ledger-${i}'
  location: 'global'
  properties: {
    virtualNetwork: { id: vnet }
    registrationEnabled: false
  }
}]
