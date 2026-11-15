// What the home region (infra/main.bicep) grants the second region's
// cluster, deployed into the home's resource group by infra/second-region.bicep:
//
// - each named home identity trusts the second cluster's service account of
//   the same name, so a pod there acts with the same rights as here;
// - the second cluster's machines may pull from the home's registry (its
//   replica in the second region in prod);
// - the home's private DNS zones resolve in the second network, so the home's
//   private endpoints are reached over the peering.

param name string
param workloadNamespace string
param oidcIssuer string
param kubeletObjectId string
param registryName string
param vnetId string
param privateNetworking bool
param federations array

var acrPull = '7f951dda-4ed3-4680-a7ca-43fe172d538d'
// The same zones, in the same order, as main.bicep's zoneNames.
var zoneNames = [
  'privatelink.blob.${az.environment().suffixes.storage}'
  'privatelink.vaultcore.azure.net'
  'privatelink.servicebus.windows.net'
  'privatelink.azurecr.io'
  'privatelink.postgres.database.azure.com'
  'privatelink.monitor.azure.com'
  'privatelink.oms.opinsights.azure.com'
  'privatelink.ods.opinsights.azure.com'
  'privatelink.agentsvc.azure-automation.net'
]

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' existing = [for f in federations: {
  name: '${name}-${f.identity}'
}]

resource federation 'Microsoft.ManagedIdentity/userAssignedIdentities/federatedIdentityCredentials@2023-01-31' = [for (f, i) in federations: {
  parent: identity[i]
  name: 'second-${workloadNamespace}-${f.serviceAccount}'
  properties: {
    issuer: oidcIssuer
    subject: 'system:serviceaccount:${workloadNamespace}:${f.serviceAccount}'
    audiences: ['api://AzureADTokenExchange']
  }
}]

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' existing = {
  name: registryName
}

resource pull 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: registry
  name: guid(registry.id, kubeletObjectId, acrPull)
  properties: {
    principalId: kubeletObjectId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', acrPull)
  }
}

resource zone 'Microsoft.Network/privateDnsZones@2020-06-01' existing = [for z in zoneNames: if (privateNetworking) {
  name: z
}]

resource link 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2020-06-01' = [for (z, i) in zoneNames: if (privateNetworking) {
  parent: zone[i]
  name: '${name}-second-vnet'
  location: 'global'
  properties: {
    virtualNetwork: { id: vnetId }
    registrationEnabled: false
  }
}]
