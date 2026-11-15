// One private endpoint, with its address registered in the private DNS zone
// for that kind of service, so the service's ordinary host name resolves to
// the address inside the virtual network. Used by main.bicep and by every
// module template for each PaaS service it creates.

@description('Name of the private endpoint.')
param name string

param location string = resourceGroup().location
param tags object = {}

@description('The subnet private endpoints live in (main.bicep output privateEndpointSubnetId).')
param subnetId string

@description('The resource the endpoint reaches.')
param targetId string

@description('The sub-resource: blob, vault, namespace, registry, postgresqlServer, azuremonitor.')
param groupId string

@description('The private DNS zones the endpoint registers its addresses in (main.bicep output privateDnsZoneIds).')
param dnsZoneIds array

resource endpoint 'Microsoft.Network/privateEndpoints@2024-05-01' = {
  name: name
  location: location
  tags: tags
  properties: {
    subnet: { id: subnetId }
    privateLinkServiceConnections: [
      {
        name: name
        properties: {
          privateLinkServiceId: targetId
          groupIds: [groupId]
        }
      }
    ]
  }
}

resource zones 'Microsoft.Network/privateEndpoints/privateDnsZoneGroups@2024-05-01' = {
  parent: endpoint
  name: 'default'
  properties: {
    privateDnsZoneConfigs: [for (zone, i) in dnsZoneIds: {
      name: 'zone-${i}'
      properties: { privateDnsZoneId: zone }
    }]
  }
}

output id string = endpoint.id
