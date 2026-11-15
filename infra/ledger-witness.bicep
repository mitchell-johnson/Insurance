// The ledger's witness: the fifth voter, in a third region.
//
// Two voters run in each of the two main regions (infra/main.bicep,
// infra/second-region.bicep). The fifth is here so that either main region
// can be lost with a majority left (docs/decisions.md, "Where the ledger's
// voters run"). It votes and keeps the Raft log but applies nothing and
// holds no state (crates/raft/src/witness.rs), so it is one small machine:
// no cluster, no Key Vault of its own, no storage but its disk.
//
// Australia Central (Canberra) by default: it is about 4 ms from Sydney and
// 8 ms from Melbourne, so a write that a region's leader commits with the
// witness's acknowledgement waits less than it would for the other region,
// and the data stays in Australia like the rest of the ledger. Australia
// Central offers fewer services and no availability zones; this template
// uses only a virtual machine, a disk and a network. If the subscription
// cannot deploy there, New Zealand North takes the same template
// (location=newzealandnorth), at about 25 ms.
//
// Deploy into a resource group of its own in the third region, after
// main.bicep, then run infra/regions.bicep to peer the networks:
//   az deployment group create -g <rg-witness> -f infra/ledger-witness.bicep \
//     -p name=<name> environment=<env> homeResourceGroup=<rg-home> \
//        registryName=<registryName> keyVaultName=<keyVaultName> \
//        image=<registry>.azurecr.io/insurance-core:<sha> adminPublicKey=@~/.ssh/id_ed25519.pub
//
// The machine runs the ledger node image as the witness (IC_NODE_ID 5,
// IC_WITNESS_NODES 5) under systemd. It presents the nodes' own cluster
// token, read from the home's vault under the machine's identity at every
// start, and accepts only that token: nothing but the other voters calls it.

targetScope = 'resourceGroup'

@description('The third region. Australia Central by default; newzealandnorth also works.')
param location string = resourceGroup().location

@description('The same short name as the home region\'s deployment.')
@minLength(3)
@maxLength(12)
param name string

@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('The home region\'s resource group (infra/main.bicep).')
param homeResourceGroup string

@description('The home\'s container registry (main.bicep output registryName).')
param registryName string

@description('The home\'s vault (main.bicep output keyVaultName), which holds cluster-token-ic-node.')
param keyVaultName string

@description('The ledger node image, as deployed to the clusters: <registry>.azurecr.io/insurance-core:<git sha>.')
param image string

@description('Address space of the witness\'s network. Must not overlap the home\'s (10.40.0.0/16) or the second region\'s (10.41.0.0/16).')
param vnetAddressSpace string = '10.42.0.0/24'

@description('The witness\'s fixed private address, named ic-witness.ledger.internal by infra/regions.bicep.')
param witnessAddress string = '10.42.0.10'

@description('The address spaces of the two main regions: only their voters may reach the witness, on port 7000.')
param voterAddressSpaces array = ['10.40.0.0/16', '10.41.0.0/16']

@description('Peers, partitions and timings: the same values as the ic-ledger ConfigMap in deploy/k8s/settings.yaml and the overlays in deploy/regions.')
param nodePeers string = '1=ic-node-1.ledger.internal:7000,2=ic-node-2.ledger.internal:7000,3=ic-node-3.ledger.internal:7000,4=ic-node-4.ledger.internal:7000,5=ic-witness.ledger.internal:7000'
param partitions int = 64
param heartbeatMs int = 100

@description('Two vCPUs and 8 GiB: the witness applies nothing, so it needs memory only for the log since the data voters\' last mark.')
param vmSize string = 'Standard_D2ds_v5'

@description('The disk for the logs. Premium SSD: every acknowledgement waits for its flush.')
param dataDiskSizeGB int = 256

@description('The break-glass administrator\'s name on the machine.')
param adminUsername string = 'icadmin'

@description('SSH public key of the break-glass administrator. Day-to-day access is Entra ID sign-in (the AADSSHLoginForLinux extension) or run-command.')
@secure()
param adminPublicKey string

var tags = {
  system: 'insurance-core'
  environment: environment
  region: 'witness'
}
var roles = {
  keyVaultSecretsUser: '4633458b-17de-408a-b874-0445c86b69e6'
  acrPull: '7f951dda-4ed3-4680-a7ca-43fe172d538d'
}

// --- network ------------------------------------------------------------------------

resource rules 'Microsoft.Network/networkSecurityGroups@2024-05-01' = {
  name: '${name}-witness-nsg'
  location: location
  tags: tags
  properties: {
    securityRules: [
      {
        name: 'raft-from-the-voters'
        properties: {
          priority: 100
          direction: 'Inbound'
          access: 'Allow'
          protocol: 'Tcp'
          sourceAddressPrefixes: voterAddressSpaces
          sourcePortRange: '*'
          destinationAddressPrefix: witnessAddress
          destinationPortRange: '7000'
        }
      }
      {
        // Entra ID SSH from the home network only (a jump host or Bastion there).
        name: 'ssh-from-home'
        properties: {
          priority: 110
          direction: 'Inbound'
          access: 'Allow'
          protocol: 'Tcp'
          sourceAddressPrefix: voterAddressSpaces[0]
          sourcePortRange: '*'
          destinationAddressPrefix: witnessAddress
          destinationPortRange: '22'
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

// Outbound for operating system updates only: the image and the token come
// from the home's private endpoints over the peering.
resource natAddress 'Microsoft.Network/publicIPAddresses@2024-05-01' = {
  name: '${name}-witness-nat'
  location: location
  tags: tags
  sku: { name: 'Standard' }
  properties: { publicIPAllocationMethod: 'Static' }
}

resource nat 'Microsoft.Network/natGateways@2024-05-01' = {
  name: '${name}-witness-nat'
  location: location
  tags: tags
  sku: { name: 'Standard' }
  properties: {
    publicIpAddresses: [{ id: natAddress.id }]
  }
}

resource vnet 'Microsoft.Network/virtualNetworks@2024-05-01' = {
  name: '${name}-witness-vnet'
  location: location
  tags: tags
  properties: {
    addressSpace: { addressPrefixes: [vnetAddressSpace] }
    subnets: [
      {
        name: 'witness'
        properties: {
          addressPrefix: cidrSubnet(vnetAddressSpace, 26, 0)
          networkSecurityGroup: { id: rules.id }
          natGateway: { id: nat.id }
        }
      }
    ]
  }
}

// --- the machine ------------------------------------------------------------------------

resource identity 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${name}-witness'
  location: location
  tags: tags
}

resource nic 'Microsoft.Network/networkInterfaces@2024-05-01' = {
  name: '${name}-witness-nic'
  location: location
  tags: tags
  properties: {
    ipConfigurations: [
      {
        name: 'ipconfig'
        properties: {
          subnet: { id: vnet.properties.subnets[0].id }
          privateIPAllocationMethod: 'Static'
          privateIPAddress: witnessAddress
        }
      }
    ]
  }
}

var environmentFile = join([
  'IC_NODE_ID=5'
  'IC_WITNESS_NODES=5'
  'IC_NODE_PEERS=${nodePeers}'
  'IC_PARTITIONS=${partitions}'
  'IC_RAFT_HEARTBEAT_MS=${heartbeatMs}'
  'IC_NODE_DATA_DIR=/var/lib/ic/data'
  'IC_CLUSTER_TOKEN_FILE=/run/ic/cluster-token'
  'RUST_LOG=info'
], '\n')

// Docker and the Azure CLI; a unit that reads the token from the vault and
// logs in to the registry under the machine's identity, then runs the node.
var cloudInit = '''
#cloud-config
package_update: true
packages: [docker.io, jq]
disk_setup:
  /dev/disk/azure/scsi1/lun0: {table_type: gpt, layout: true, overwrite: false}
fs_setup:
  - {device: /dev/disk/azure/scsi1/lun0, partition: 1, filesystem: ext4, overwrite: false}
mounts:
  - [/dev/disk/azure/scsi1/lun0-part1, /var/lib/ic, ext4, "defaults,nofail", "0", "2"]
write_files:
  - path: /etc/ic/witness.env
    permissions: "0644"
    content: |
      ENVIRONMENT_FILE
  - path: /usr/local/bin/ic-witness-prepare
    permissions: "0755"
    content: |
      #!/bin/sh
      # The nodes' token, from the home's vault, into memory only.
      set -eu
      az login --identity --client-id CLIENT_ID --allow-no-subscriptions >/dev/null
      install -d -m 0700 -o 65532 -g 65532 /run/ic
      umask 077
      az keyvault secret show --vault-name VAULT --name cluster-token-ic-node --query value -o tsv > /run/ic/cluster-token
      chown 65532:65532 /run/ic/cluster-token
      az acr login --name REGISTRY_NAME
      install -d -o 65532 -g 65532 /var/lib/ic/data
  - path: /etc/systemd/system/ic-witness.service
    content: |
      [Unit]
      Description=Ledger witness (crates/raft/src/witness.rs)
      After=docker.service network-online.target
      Requires=docker.service
      [Service]
      Restart=always
      RestartSec=5
      ExecStartPre=/usr/local/bin/ic-witness-prepare
      ExecStartPre=-/usr/bin/docker rm -f ic-witness
      ExecStart=/usr/bin/docker run --name ic-witness --network host --user 65532:65532 --read-only --cap-drop ALL --security-opt no-new-privileges --env-file /etc/ic/witness.env -v /var/lib/ic/data:/var/lib/ic/data -v /run/ic:/run/ic:ro --log-driver journald IMAGE /usr/local/bin/ic-node
      ExecStop=/usr/bin/docker stop -t 60 ic-witness
      [Install]
      WantedBy=multi-user.target
runcmd:
  - curl -sL https://aka.ms/InstallAzureCLIDeb | bash
  - systemctl daemon-reload
  - systemctl enable --now ic-witness.service
'''
var userData = replace(replace(replace(replace(replace(cloudInit, 'ENVIRONMENT_FILE', replace(environmentFile, '\n', '\n      ')), 'CLIENT_ID', identity.properties.clientId), 'VAULT', keyVaultName), 'REGISTRY_NAME', registryName), 'IMAGE', image)

resource vm 'Microsoft.Compute/virtualMachines@2024-07-01' = {
  name: '${name}-witness'
  location: location
  tags: tags
  dependsOn: [homeGrants]
  // The user-assigned identity reads the token and pulls the image; the
  // system-assigned one is what Entra ID sign-in needs.
  identity: {
    type: 'SystemAssigned, UserAssigned'
    userAssignedIdentities: { '${identity.id}': {} }
  }
  properties: {
    hardwareProfile: { vmSize: vmSize }
    securityProfile: {
      securityType: 'TrustedLaunch'
      uefiSettings: { secureBootEnabled: true, vTpmEnabled: true }
      encryptionAtHost: true
    }
    osProfile: {
      computerName: 'ic-witness'
      adminUsername: adminUsername
      customData: base64(userData)
      linuxConfiguration: {
        disablePasswordAuthentication: true
        ssh: { publicKeys: [{ path: '/home/${adminUsername}/.ssh/authorized_keys', keyData: adminPublicKey }] }
        patchSettings: { patchMode: 'AutomaticByPlatform', assessmentMode: 'AutomaticByPlatform' }
      }
    }
    storageProfile: {
      imageReference: {
        publisher: 'Canonical'
        offer: 'ubuntu-24_04-lts'
        sku: 'server'
        version: 'latest'
      }
      osDisk: {
        createOption: 'FromImage'
        managedDisk: { storageAccountType: 'Premium_LRS' }
        deleteOption: 'Delete'
      }
      dataDisks: [
        {
          lun: 0
          name: '${name}-witness-data'
          createOption: 'Empty'
          diskSizeGB: dataDiskSizeGB
          managedDisk: { storageAccountType: 'Premium_LRS' }
          // The log is the witness's vote: losing the disk loses its
          // identity, and it is replaced under a new number (docs/operations.md).
          deleteOption: 'Detach'
        }
      ]
    }
    networkProfile: {
      networkInterfaces: [{ id: nic.id }]
    }
    diagnosticsProfile: { bootDiagnostics: { enabled: true } }
  }
}

// Entra ID sign-in instead of keys for day-to-day access.
resource entraLogin 'Microsoft.Compute/virtualMachines/extensions@2024-07-01' = {
  parent: vm
  name: 'AADSSHLoginForLinux'
  location: location
  properties: {
    publisher: 'Microsoft.Azure.ActiveDirectory'
    type: 'AADSSHLoginForLinux'
    typeHandlerVersion: '1.0'
    autoUpgradeMinorVersion: true
  }
}

// --- what the home grants the witness ----------------------------------------------------

module homeGrants 'modules/witness-grants.bicep' = {
  name: 'witness-grants'
  scope: resourceGroup(homeResourceGroup)
  params: {
    name: name
    principalId: identity.properties.principalId
    keyVaultName: keyVaultName
    registryName: registryName
    vnetId: vnet.id
    roles: roles
  }
}

output vnetId string = vnet.id
output witnessAddress string = witnessAddress
output identityPrincipalId string = identity.properties.principalId
