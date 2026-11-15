// One side of a virtual network peering. infra/regions.bicep makes both
// sides of each pair, each in its own network's resource group.

param localVnetName string
param remoteVnetId string
param peeringName string

resource local 'Microsoft.Network/virtualNetworks@2024-05-01' existing = {
  name: localVnetName
}

resource peering 'Microsoft.Network/virtualNetworks/virtualNetworkPeerings@2024-05-01' = {
  parent: local
  name: peeringName
  properties: {
    remoteVirtualNetwork: { id: remoteVnetId }
    allowVirtualNetworkAccess: true
    // Nothing transits: each network reaches the others directly.
    allowForwardedTraffic: false
    allowGatewayTransit: false
    useRemoteGateways: false
  }
}
