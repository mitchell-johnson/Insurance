// The way in from the internet: Azure Front Door Premium with its web
// application firewall, reaching the cluster's internal NGINX controller
// over Private Link (deploy/cluster.yaml, deploy/k8s/ingress.yaml). Front
// Door holds the public names and their managed TLS certificates; the
// cluster has no public address.
//
// Deployed after deploy/cluster.yaml has made the Private Link Service:
//   az deployment group create -g <rg> -f infra/edge.bicep \
//     -p name=<name> privateLinkServiceName=insurance-core-ingress \
//        privateLinkServiceResourceGroup=<the cluster's node resource group> \
//        dnsZoneName=<zone> portalHost=<host> apiHost=<host> ...
// then approve Front Door's connection once (docs/operations.md, "The way in").

targetScope = 'resourceGroup'

@description('Short name used as a prefix, as for main.bicep.')
param name string

@description('Environment tag: dev, test or prod.')
@allowed(['dev', 'test', 'prod'])
param environment string = 'dev'

@description('Region of the Private Link Service, the cluster\'s region.')
param location string = resourceGroup().location

@description('The Private Link Service the internal NGINX controller made (deploy/cluster.yaml names it insurance-core-ingress).')
param privateLinkServiceName string = 'insurance-core-ingress'

@description('The resource group it is in: the cluster\'s node resource group (MC_...).')
param privateLinkServiceResourceGroup string

@description('The Azure DNS zone the public names live in, in this resource group. Empty: the names are made by whoever runs DNS, from the outputs (a CNAME to the endpoint and a TXT record to validate each).')
param dnsZoneName string = ''

@description('Public host names, each fully qualified: the portals and the app\'s service, the API, the reinsurers\' API, and Event Grid\'s delivery reports. Empty leaves that one out.')
param portalHost string
param apiHost string = ''
param reinsurersHost string = ''
param hooksHost string = ''

@description('Requests per minute from one client address before the WAF blocks it for the rest of the minute. PLACEHOLDER: a best estimate for a portal used by people, not machines; raise it once real traffic is seen.')
param rateLimitPerMinute int = 600

@description('Countries (ISO 3166 alpha-2) that the WAF admits; empty admits all. PLACEHOLDER: empty, because customers travel and advisers and reinsurers may be abroad.')
param allowedCountries array = []

var tags = {
  system: 'insurance-core'
  environment: environment
  component: 'edge'
}

var hosts = filter([
  { key: 'portal', host: portalHost }
  { key: 'api', host: apiHost }
  { key: 'reinsurers', host: reinsurersHost }
  { key: 'hooks', host: hooksHost }
], h => !empty(h.host))

resource pls 'Microsoft.Network/privateLinkServices@2024-05-01' existing = {
  scope: resourceGroup(privateLinkServiceResourceGroup)
  name: privateLinkServiceName
}

resource profile 'Microsoft.Cdn/profiles@2024-09-01' = {
  name: '${name}-edge'
  location: 'global'
  tags: tags
  sku: { name: 'Premium_AzureFrontDoor' }
  properties: {
    originResponseTimeoutSeconds: 60
  }
}

resource endpoint 'Microsoft.Cdn/profiles/afdEndpoints@2024-09-01' = {
  parent: profile
  name: '${name}-${uniqueString(resourceGroup().id, name)}'
  location: 'global'
  tags: tags
  properties: { enabledState: 'Enabled' }
}

resource origins 'Microsoft.Cdn/profiles/originGroups@2024-09-01' = {
  parent: profile
  name: 'cluster'
  properties: {
    loadBalancingSettings: { sampleSize: 4, successfulSamplesRequired: 3, additionalLatencyInMilliseconds: 50 }
    // One origin: Front Door recommends no health probe, since there is
    // nowhere else to send a request. A second region's origin would add one.
    sessionAffinityState: 'Disabled'
  }
}

// The internal load balancer, through its Private Link Service. Front Door
// talks plain HTTP over the private link; TLS ends at Front Door. The link
// itself is private and the cluster encrypts what crosses its nodes.
resource origin 'Microsoft.Cdn/profiles/originGroups/origins@2024-09-01' = {
  parent: origins
  name: 'ingress'
  properties: {
    hostName: pls.properties.alias
    originHostHeader: null
    httpPort: 80
    httpsPort: 443
    priority: 1
    weight: 1000
    enabledState: 'Enabled'
    enforceCertificateNameCheck: true
    sharedPrivateLinkResource: {
      privateLink: { id: pls.id }
      privateLinkLocation: location
      requestMessage: 'Front Door ${profile.name} for the insurance core'
    }
  }
}

resource zone 'Microsoft.Network/dnsZones@2018-05-01' existing = if (!empty(dnsZoneName)) {
  name: empty(dnsZoneName) ? 'unset' : dnsZoneName
}

resource domains 'Microsoft.Cdn/profiles/customDomains@2024-09-01' = [for h in hosts: {
  parent: profile
  name: h.key
  properties: {
    hostName: h.host
    tlsSettings: {
      certificateType: 'ManagedCertificate'
      minimumTlsVersion: 'TLS12'
    }
    azureDnsZone: empty(dnsZoneName) ? null : { id: zone.id }
  }
}]

resource routes 'Microsoft.Cdn/profiles/afdEndpoints/routes@2024-09-01' = [for (h, i) in hosts: {
  parent: endpoint
  name: h.key
  dependsOn: [origin]
  properties: {
    customDomains: [{ id: domains[i].id }]
    originGroup: { id: origins.id }
    // The host the browser asked for is passed on, so the ingress rule for it matches.
    originPath: null
    supportedProtocols: ['Http', 'Https']
    httpsRedirect: 'Enabled'
    forwardingProtocol: 'HttpOnly'
    linkToDefaultDomain: 'Disabled'
    patternsToMatch: ['/*']
    enabledState: 'Enabled'
  }
}]

// The public names: a CNAME to the endpoint and the TXT record that proves
// the name is ours for the certificate.
resource cnames 'Microsoft.Network/dnsZones/CNAME@2018-05-01' = [for (h, i) in hosts: if (!empty(dnsZoneName)) {
  parent: zone
  name: replace(h.host, '.${dnsZoneName}', '')
  properties: {
    TTL: 3600
    CNAMERecord: { cname: endpoint.properties.hostName }
  }
}]

resource validations 'Microsoft.Network/dnsZones/TXT@2018-05-01' = [for (h, i) in hosts: if (!empty(dnsZoneName)) {
  parent: zone
  name: '_dnsauth.${replace(h.host, '.${dnsZoneName}', '')}'
  properties: {
    TTL: 3600
    TXTRecords: [{ value: [domains[i].properties.validationProperties.validationToken] }]
  }
}]

// --- the web application firewall ---------------------------------------------------

resource waf 'Microsoft.Network/FrontDoorWebApplicationFirewallPolicies@2024-02-01' = {
  name: '${replace(name, '-', '')}waf'
  location: 'global'
  tags: tags
  sku: { name: 'Premium_AzureFrontDoor' }
  properties: {
    policySettings: {
      enabledState: 'Enabled'
      // Blocks, not just logs. Run in Detection for a week first in a new
      // environment if the portal's traffic is not yet known
      // (docs/operations.md, "The way in").
      mode: 'Prevention'
      requestBodyCheck: 'Enabled'
      customBlockResponseStatusCode: 403
    }
    managedRules: {
      managedRuleSets: [
        {
          ruleSetType: 'Microsoft_DefaultRuleSet'
          ruleSetVersion: '2.1'
          ruleSetAction: 'Block'
        }
        {
          ruleSetType: 'Microsoft_BotManagerRuleSet'
          ruleSetVersion: '1.1'
        }
      ]
    }
    customRules: {
      rules: concat([
        {
          name: 'RateLimitPerClient'
          priority: 100
          ruleType: 'RateLimitRule'
          rateLimitDurationInMinutes: 1
          rateLimitThreshold: rateLimitPerMinute
          action: 'Block'
          enabledState: 'Enabled'
          matchConditions: [
            {
              matchVariable: 'RemoteAddr'
              operator: 'IPMatch'
              negateCondition: true
              matchValue: ['255.255.255.255/32']
            }
          ]
        }
      ], empty(allowedCountries) ? [] : [
        {
          name: 'AllowedCountries'
          priority: 200
          ruleType: 'MatchRule'
          action: 'Block'
          enabledState: 'Enabled'
          matchConditions: [
            {
              matchVariable: 'SocketAddr'
              operator: 'GeoMatch'
              negateCondition: true
              matchValue: allowedCountries
            }
          ]
        }
      ])
    }
  }
}

resource security 'Microsoft.Cdn/profiles/securityPolicies@2024-09-01' = {
  parent: profile
  name: 'waf'
  properties: {
    parameters: {
      type: 'WebApplicationFirewall'
      wafPolicy: { id: waf.id }
      associations: [
        {
          domains: [for (h, i) in hosts: { id: domains[i].id }]
          patternsToMatch: ['/*']
        }
      ]
    }
  }
}

output frontDoorId string = profile.properties.frontDoorId
output endpointHostName string = endpoint.properties.hostName
// For whoever runs DNS when the zone is not here: each name's CNAME target
// is endpointHostName, and its TXT record _dnsauth.<name> is this token.
output validationTokens array = [for (h, i) in hosts: {
  host: h.host
  token: domains[i].properties.validationProperties.validationToken
}]
