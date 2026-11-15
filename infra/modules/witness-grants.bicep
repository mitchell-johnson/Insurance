// What the home region grants the ledger's witness (infra/ledger-witness.bicep),
// deployed into the home's resource group: the nodes' cluster token and no
// other secret, image pulls, and the home's private DNS for the vault and
// the registry, so both are reached through their private endpoints over
// the peering.

param name string
param principalId string
param keyVaultName string
param registryName string
param vnetId string
param roles object

resource vault 'Microsoft.KeyVault/vaults@2023-07-01' existing = {
  name: keyVaultName
}

resource nodeToken 'Microsoft.KeyVault/vaults/secrets@2023-07-01' existing = {
  parent: vault
  name: 'cluster-token-ic-node'
}

// The witness is a voter: it presents the nodes' own token. It may read that
// secret and nothing else in the vault.
resource readsToken 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: nodeToken
  name: guid(nodeToken.id, principalId, roles.keyVaultSecretsUser)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.keyVaultSecretsUser)
  }
}

resource registry 'Microsoft.ContainerRegistry/registries@2023-07-01' existing = {
  name: registryName
}

resource pulls 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  scope: registry
  name: guid(registry.id, principalId, roles.acrPull)
  properties: {
    principalId: principalId
    principalType: 'ServicePrincipal'
    roleDefinitionId: subscriptionResourceId('Microsoft.Authorization/roleDefinitions', roles.acrPull)
  }
}

resource vaultZone 'Microsoft.Network/privateDnsZones@2020-06-01' existing = {
  name: 'privatelink.vaultcore.azure.net'
}

resource registryZone 'Microsoft.Network/privateDnsZones@2020-06-01' existing = {
  name: 'privatelink.azurecr.io'
}

resource vaultLink 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2020-06-01' = {
  parent: vaultZone
  name: '${name}-witness-vnet'
  location: 'global'
  properties: {
    virtualNetwork: { id: vnetId }
    registrationEnabled: false
  }
}

resource registryLink 'Microsoft.Network/privateDnsZones/virtualNetworkLinks@2020-06-01' = {
  parent: registryZone
  name: '${name}-witness-vnet'
  location: 'global'
  properties: {
    virtualNetwork: { id: vnetId }
    registrationEnabled: false
  }
}
