# Azure resources: inventory and specifications

Every Azure resource the system needs, with its size and what it serves. It is read from `infra/*.bicep`, `infra/main.bicepparam` and `deploy/k8s` on main (15 Nov 2026), so it matches what a deployment of those templates creates. The resources the system needs that the templates do not create are listed separately in [Needed but not in the templates](#needed-but-not-in-the-templates), and [Gaps](#gaps-between-the-templates-and-what-the-services-need) lists where the templates and the services do not yet agree.

**Nothing is deployed.** No Azure subscription has been deployed to. CI compiles every template and renders the manifests on each push, and the whole arrangement runs on one machine under Docker Compose. [The Azure deployment diagram](architecture/azure-deployment.png) draws the same resources.

Where a template sizes something differently by environment, the table gives **prod** and **dev/test** (the `environment` parameter, `dev` by default). The figures for VM sizes, IOPS and service limits are Azure's published figures for that SKU. Check them against Azure's current documentation when you deploy.

## Region and placement

| | Today in the templates | Pending |
| --- | --- | --- |
| Region | Three. Australia East (Sydney) is home: `infra/main.bicep` (`main.bicepparam` sets `australiaeast`) with every stateful service. Australia Southeast (Melbourne): `infra/second-region.bicep`, a second AKS cluster with no stateful services of its own. Australia Central (Canberra): `infra/ledger-witness.bicep`, one VM for the ledger's witness. `infra/regions.bicep` peers the three networks and names the voters. | Decided 15 Nov 2026 (`decisions.md`, "Where the ledger's voters run"). Check that the subscription can deploy to Australia Central; New Zealand North is the fallback for the witness. Policy data is held in Australia, which is for the insurer and its regulator to accept. |
| Ledger voters | Five per group: `ic-node-1` and `-2` in Sydney, `-3` and `-4` in Melbourne, the witness (node 5, no ledger data) in Canberra. Both main regions lead (even partitions from Sydney, odd from Melbourne); losing any one region leaves three of five. | None. |
| Replication of the stateful services to Melbourne | Ledger storage RA-GZRS (Melbourne is Sydney's pair); Event Hubs geo-replicated to Melbourne in prod (`geoReplicate`); a registry replica in Melbourne; Key Vault's own paired-region copy; PostgreSQL geo-redundant backups. | None. |
| Zones | Every AKS node pool spans zones 1, 2 and 3, and each region's two `ic-node` pods are on different zones of its ledger pool. Event Hubs is zone-redundant. In prod, storage is ZRS or GZRS and the PostgreSQL servers are zone-redundant. | None. |
| Fail-back | On: `IC_LEADER_REGIONS` and `IC_REGION_NODES` in `deploy/k8s/settings.yaml`, so each group goes back to its home region after a loss. | None. |
| Communication Services data | `communicationDataLocation` = `Australia` (`infra/documents.bicep`), matching the regions. | None. |

## Resource count

What one deployment of every template creates in prod, with no reinsurers configured for SFTP and no alert emails:

| Resource type | Count | Created by |
| --- | ---: | --- |
| AKS cluster | 2 (Sydney, Melbourne) | main, second-region |
| AKS node pools | 3 pools in each: system 3 VMs, ledger 2 VMs, apps 3 to 9 VMs | main, second-region |
| Virtual network, NSG, public IP prefix (egress) | 1 each in Sydney (4 subnets) and Melbourne (3 subnets); the witness's network (1 subnet) with an NSG and a NAT gateway | main, second-region, ledger-witness |
| Network peerings | 6 (each pair of the three networks, both ways) | regions |
| Private DNS zone `ledger.internal` | 1, with 5 records and 3 links | regions |
| Witness VM | 1 × `Standard_D2ds_v5` with a 256 GB Premium SSD data disk, its NIC and identity | ledger-witness |
| Container Registry replication | 1 (Melbourne) | main |
| Private DNS zones and links | 9 each | main |
| Private endpoints | 6 in main (ledger blob, 2 vaults, Event Hubs, registry, Azure Monitor), 1 more with `deployPostgres`; 1 per module storage account (8) and the customer database; 1 for the backup account | main, every module |
| Azure Monitor private link scope | 1 | main |
| Front Door Premium profile, WAF policy | 1 each | edge |
| Backup vault, policy, instance; AKS backup extension and its account | 1 each | main (`backupLedgerDisks`) |
| Email Communication Service and domain | 1 each, with `emailDomain` | documents |
| Container Registry | 1 | main |
| Storage accounts | 11 | main 2 (ledger, backup), customers 1, documents 1, finance 1, group 1, reinsurance 1 (SFTP), underwriting 1, unit-pricing 2 |
| Blob containers | 14, plus one per SFTP reinsurer | see [Storage](#storage) |
| PostgreSQL Flexible Servers | 1, or 2 with `deployPostgres` | customers; main (optional, for Temporal) |
| Key Vaults | 2 | main: the core vault and the payments vault |
| Key Vault keys | 4 | main 1, customers 1, documents 1, underwriting 1 |
| Event Hubs namespaces | 1 | main |
| Event hubs | 10 | main 1, customers 2, documents 2, group 1, reinsurance 1, underwriting 3 |
| Communication Services | 1 | documents |
| Event Grid system topic and subscription | 1 each | documents (the subscription only once `deliveryReportsUrl` is set) |
| Defender for Storage settings | 2 | group, unit-pricing |
| Log Analytics workspace | 1 | main |
| Application Insights | 1 | main |
| Log alert rules | 12 | main 7 (the core), reinsurance 2, unit-pricing 3 |
| Action groups | up to 3 | main, reinsurance and unit-pricing, only when `alertEmails` is set |
| User-assigned managed identities | 15, or 16 with a self-hosted Temporal | main 7 (five workloads, the collector, and the cluster's own identity, which has no federated credential), the module templates 8 |
| Role assignments | about 50 | every template |

## Compute

### AKS cluster (`infra/main.bicep`)

| Setting | Value |
| --- | --- |
| Name | `<name>-aks` |
| Tier | Standard in prod (uptime SLA), Free in dev/test |
| Pool `system` | Mode System, tainted `CriticalAddonsOnly`: AKS's own pods only. **3 × `Standard_D2ds_v5`** (2 vCPU, 8 GiB), zones 1 to 3 (`systemVmSize`). |
| Pool `ledger` | Mode User, tainted `insurance-core/ledger`, label `insurance-core/pool: ledger`: the region's two `ic-node` voters only. **2 × `Standard_D8ds_v5`** (8 vCPU, 32 GiB), on different zones, fixed (`ledgerVmSize`, `ledgerPoolCount`); a voter's disk is zonal, so it cannot move. |
| Pool `apps` | Mode User: every other workload. **`Standard_D4ds_v5`** (4 vCPU, 16 GiB), the cluster autoscaler between **3 and 9** (`appsMinCount`, `appsMaxCount`), zones balanced, scale-down after 10 idle minutes. |
| Total at rest | 34 vCPU and 136 GiB per cluster; up to 58 vCPU and 232 GiB at the apps pool's maximum. All pools Azure Linux. The Melbourne cluster is built from the same block (`infra/second-region.bicep`; `tools/check_regions.py` fails CI if the two differ). |
| Networking | Nodes in the `aks-nodes` subnet (/22) of the virtual network `10.40.0.0/16` (`vnetAddressSpace`; Melbourne's is `10.41.0.0/16`), with `private-endpoints` (/24), `ingress` (/24) and `ledger-peers` (/24, the voters' fixed internal load balancer addresses, `deploy/regions`) beside it. Outbound through the standard load balancer from one public IP prefix (/30, output `egressPrefix`), for providers to allow-list. Private API server unless `adminIpRanges` is given. Azure CNI overlay with Cilium as dataplane and network policy. Advanced Container Networking Services with **WireGuard** encrypts traffic between nodes on UDP 51871. |
| Identity | Entra ID with Azure RBAC and local accounts off. OIDC issuer and Workload Identity are on. |
| Add-ons | Key Vault Secrets Store CSI driver (rotation every 2 minutes), the monitoring agent to the Log Analytics workspace (Entra auth, through the private link scope), the Azure Disk CSI driver with the snapshot controller, application routing (managed NGINX, an internal controller of our own in `deploy/cluster.yaml`), and the Azure Backup extension. |
| Upgrades | Patch channel; node OS on NodeImage. |

### The ledger's witness (`infra/ledger-witness.bicep`)

| Setting | Value |
| --- | --- |
| Where | Australia Central, its own resource group and network `10.42.0.0/24`, fixed address `10.42.0.10` (`ic-witness.ledger.internal`) |
| VM | **1 × `Standard_D2ds_v5`** (2 vCPU, 8 GiB), Ubuntu 24.04, Trusted Launch, encryption at host; 256 GB Premium SSD data disk for the log |
| Runs | `ic-node` as node 5 with `IC_WITNESS_NODES=5`, from the same image, under systemd; it reads the nodes' cluster token from Key Vault with its own identity at start |
| Network | NSG admits port 7000 from the two clusters' networks and SSH from home only; outbound through a NAT gateway; sign-in by Entra ID (`AADSSHLoginForLinux`) |
| Holds | The log of every partition until every data voter has it, and its vote. No ledger state, no personal data. |

### Container Registry (`infra/main.bicep`)

One registry on the **Premium** SKU, with the admin user off, reached through its private endpoint (public access only for `adminIpRanges`). The kubelet identity has AcrPull. It holds the image `insurance-core`, which contains every binary, and the mirrored OpenTelemetry Collector (`opentelemetry-collector-contrib`).

### Workloads on the cluster (`deploy/k8s`)

These are the requests and limits as written in the manifests. Memory has limits; CPU has requests only.

| Workload | Kind | Replicas | CPU request | Memory request / limit | Storage |
| --- | --- | ---: | --- | --- | --- |
| `ic-node` | StatefulSet, two per region on different zones, on the ledger pool | 2 per region | 1 | 2 GiB / 4 GiB | 128 GiB `ic-ledger-premiumv2` each (Premium SSD v2, zonal LRS: 3,000 IOPS and 125 MB/s provisioned, sub-millisecond latency; see Sizing) |
| `ic-api` | Deployment | 2 | 250m | 256 MiB / 1 GiB | none |
| `ic-portal` | Deployment | 2 | 250m | 256 MiB / 1 GiB | none |
| `ic-worker` | Deployment | 2 | 250m | 256 MiB / 1 GiB | none |
| `ic-customers` | Deployment | 2 | 250m | 256 MiB / 1 GiB | none |
| `ic-documents` | Deployment | 2 | 250m | 256 MiB / 1 GiB | none |
| `ic-group` | Deployment | 2 | 250m | 256 MiB / 1 GiB | none |
| `ic-payments-pump` | Deployment | 2 | 100m | 128 MiB / 512 MiB | none |
| `ic-underwriting-intake` | Deployment | 2 | 100m | 128 MiB / 512 MiB | none |
| `ic-unit-pricing` | Deployment | 2 | 50m | 128 MiB / 512 MiB | none |
| `ic-finance-close` | Deployment | 2 | 50m | 128 MiB / 1 GiB | none |
| `ic-reinsurance` | Deployment | 2 | 50m | 64 MiB / 256 MiB | none |
| `ic-underwriting` | Deployment | 2 | 50m | 64 MiB / 256 MiB | none |
| `ic-otel-collector` | Deployment | 2 | 200m | 256 MiB / 512 MiB | none |
| **Long-running total** | | **29 pods** | **7.2 vCPU** | **10.75 GiB / 31 GiB** | **384 GiB Premium SSD v2** |
| `ic-close` | CronJob, 00:05 NZ | 1 | 100m | 128 MiB / 512 MiB | none |
| `ic-unit-orders` | CronJob, every 15 minutes | 1 | 100m | 128 MiB / 512 MiB | none |
| `ic-payments-drill` | CronJob, Tuesdays at 09:30 NZ | 1 | 50m | 64 MiB / 256 MiB | none |
| `ic-admin` | CronJob, suspended, run by hand | 1 | 100m | 128 MiB / 512 MiB | none |
| `ic-watch` | CronJob, every 5 minutes (`ic-admin watch`) | 1 | 50m | 64 MiB / 256 MiB | none |
| `ic-bootstrap` | Job, once | 1 | 100m | 128 MiB / 512 MiB | none |

Each Deployment and the StatefulSet has a pod disruption budget. `ic-api`, `ic-portal`, `ic-customers`, `ic-documents` and `ic-group` spread their two replicas across zones; the other Deployments leave placement to the scheduler.

**Fit.** The ledger voters have a pool to themselves: 8 vCPU and 32 GiB each for one `ic-node` (1 vCPU, 2 to 4 GiB requested), so a voter is never starved by another workload and the margin covers snapshots and catch-up. The other workloads request about 4.3 vCPU and 8.6 GiB, on an apps pool of at least 3 × D4ds_v5 (about 11 vCPU and 35 GiB allocatable). When a zone is lost, its third of the apps pool's pods reschedule onto the other two zones and the autoscaler adds nodes there; its voter stays down until the zone returns. See Sizing.

## Storage

All accounts are StorageV2, TLS 1.2 minimum, with public blob access off. They are reached by Entra ID with no shared keys. With private networking each has a private endpoint and refuses everything else, except the four an outside party reaches (the document vault's print outbox, the two unit pricing accounts and the SFTP drops), which admit their party's ranges when given (`operations.md`, "Private networking").

| Account | Template | Redundancy (prod / dev) | Containers and protection | Serves |
| --- | --- | --- | --- | --- |
| Ledger account `<name><unique>` | main | RA-GZRS / LRS, Hot (`ledgerStorageSku`); a delete lock in prod; deleted containers kept 365 days | `merkle-roots`: write-once for `retentionDays` (2,920 days, 8 years). `cold-segments`: write-once for 8 years; moves to Cool after 30 days and Cold after 180. `payments-evidence`: write-once for 8 years. `reinsurance-records` (added by `reinsurance.bicep`): write-once for 8 years. | Signed daily Merkle roots, sealed journal segments, bank evidence, statements and releases to reinsurers |
| Customer master register | customers | GZRS / LRS | `customers-erasures`: write-once for `registerYears` × 366 days (10 years). The lock is applied once in prod with `tools/lock-erasure-register.sh`. | The erasure register a restore is replayed against |
| Document vault | documents | GZRS / LRS | `documents-vault`: versioning with version-level immutability, each document locked to its own retention date. `print-outbox`: for the print house to collect. | Issued documents and print files |
| Finance extracts | finance | ZRS / LRS | `finance-extracts`: write-once for 8 years. Also reads `merkle-roots`. | Valuation and experience extracts |
| Group quarantine | group | ZRS / LRS | `group-quarantine`, deleted after 35 days. Scanned by Defender. | Employer membership files |
| Reinsurer SFTP drops | reinsurance | ZRS / LRS, hierarchical namespace, SFTP on | One `ri-<reinsurer>` container and one SFTP local user (SSH key) per entry of `sftpReinsurers` | Listings and statements that reinsurers collect |
| Clinical store | underwriting | ZRS / LRS | `clinical`: no soft delete, no versioning and no change feed, on purpose, so that deleting a person's wrapped key erases them | Health disclosures and evidence |
| Backup account `<name>bk<unique>` | main | ZRS / LRS | `aks-backup`: the Kubernetes objects Azure Backup saves beside the disk snapshots | Restoring the ledger nodes |
| Valuations in | unit-pricing | ZRS / LRS | `unit-pricing-inbound`, deleted after 90 days. Scanned by Defender. The fund administrator writes it under its own identity. | Valuation files |
| Holdings extract out | unit-pricing | GZRS / LRS | `unit-pricing-outbound`: write-once for 2,555 days (7 years) | Unit holdings extract for the fund administrator |

## Databases

| Server | Template | SKU (prod / dev) | Storage | Availability and backup | Serves |
| --- | --- | --- | --- | --- | --- |
| Customer master `<name>-<unique>-customers` | customers | `Standard_D4ds_v5` General Purpose (4 vCores, 16 GiB) / `Standard_B2ms` Burstable (2 vCores, 8 GiB) | 128 GiB, grows automatically | Zone-redundant HA in prod; 35-day backups, geo-redundant in prod | PostgreSQL 16, database `customers`. Entra sign-in only with TLS required. No public address: reached through its private endpoint. |
| Temporal store `<name>-<unique>-pg` (only with `deployPostgres`) | main | `Standard_D2ds_v5` General Purpose (2 vCores, 8 GiB) | 128 GiB | Zone-redundant HA in prod; 35-day backups, geo-redundant in prod | PostgreSQL 16 for a self-hosted Temporal (`selfHostedTemporal`), databases `temporal` and `temporal_visibility`, one password login for Temporal, private endpoint |

The ledger does not use a database. Its state is on the `ic-node` disks, replicated by Raft.

## Key Vault

| Vault | SKU | Settings | Holds |
| --- | --- | --- | --- |
| Core vault (main) | Premium | RBAC, purge protection, 90-day soft delete | Key `merkle-root-signing` (signs the daily root). Secret `cluster-token-<workload>` for each of the 13 workloads, each readable only by its own identity. Module keys: `customers-kek` (RSA 3072), `documents-kek` (RSA 3072), `clinical-key-wrap` (RSA-HSM 3072, wrap and unwrap only, which needs the Premium SKU). Module secrets: `customers-index-key` and `documents-webhook-key`, which the templates write from their parameters, and `group-fingerprint-salt`, which a person creates before `group.bicep` is deployed. |
| Payments vault (main) | Premium | Same, and only the payments identity may use it | The bank signing key, and the bank credentials and certificates. These are created by the connectivity administrator when a bank channel is set up, never by the template. |

## Event Hubs

There is one namespace, `<name>-<unique>-events`: **Premium, 1 processing unit**, zone-redundant, local auth off. Every hub keeps events for 168 hours (7 days).

| Hub | Partitions | Template | Carries |
| --- | ---: | --- | --- |
| `policy-events` | 64 (one per ledger partition) | main | Every core event, in order per partition |
| `customer-events` | 8 (`hubPartitions`) | customers | Every change to a party |
| `customer-priority` | 8 | customers | Changes that take access away (deaths, holds, merges), read first |
| `documents-events` | 1 | documents | Document issue and delivery |
| `complaints-events` | 1 | documents | Complaints and conduct |
| `group-events` | 1 | group | Group scheme notices |
| `reinsurance-events` | 1 | reinsurance | Exceptions, supplementary listings due, lapsed capacity holds |
| `underwriting-notices` | 1 | underwriting | Staff notices |
| `underwriting-evidence` | 1 | underwriting | Evidence orders for providers' adapters |
| `underwriting-delegates` | 1 | underwriting | File requests for delegates' adapters |
| **Total** | **87** | | |

87 partitions fit inside one Premium processing unit's partition allowance. Throughput has not been measured against it.

## Communication, security and monitoring

| Resource | Template | Spec | Serves |
| --- | --- | --- | --- |
| Communication Services | documents | Global resource, data at rest in Australia | Email and SMS to customers, under the documents identity |
| Event Grid system topic and subscription | documents | Email and SMS delivery reports, posted to `https://<host>/v1/delivery-reports` | Proof of delivery |
| Defender for Storage | group | Malware scanning on upload, capped at 1,000 GB a month | Employer files |
| Defender for Storage | unit-pricing | Malware scanning on upload, capped at 100 GB a month | Valuation files |
| Log Analytics workspace | main | PerGB2018; 90 days interactive and 2,556 days in all (archive tier) on the container log and App tables, a placeholder (`decisions.md`, "Log retention"); ingestion only through the private link scope | Container logs (JSON lines on stdout, each with its trace ID) from the monitoring add-on, and the traces and log records from the collector |
| Application Insights | main | Workspace-based, ingestion private | Traces and log records over OpenTelemetry, through the collector (`deploy/k8s/telemetry.yaml`); its connection string is in Key Vault for the collector alone |
| Azure Monitor private link scope | main | Private only for ingestion, open for queries | The monitoring agent and the collector |
| Log alert rules | main (7), reinsurance (2), unit-pricing (3) | Every 5 or 15 minutes on the workspace; severity 0 to 2 | The core: no leader, state hashes differ, node down, close failed, housekeeping backlog, unit orders left, watch silent (`operations.md`, "Alerts"). Reinsurance exception opened and delivery stopped; a price that needs a person, failing passes, a silent driver |
| Action groups | main, reinsurance, unit-pricing | Email to `alertEmails` | On-call |
| Front Door Premium and WAF | edge | DRS 2.1 and bot manager in Prevention; rate limit 600 a minute per client (placeholder), and 10 membership claims in 5 minutes; managed TLS | The only way in, over Private Link to the internal ingress |
| Backup vault | main | Geo-redundant and immutable in prod; snapshots every 4 hours kept 7 days, daily vault copies kept 35 days | The `ic-node` disks |
| Email Communication Service | documents (with `emailDomain`) | Customer-managed domain, sender `letters@<domain>`, SPF, DKIM and DMARC records | Letters by e-mail |

## Identity

| Identity | Template | Kubernetes service account | Used by |
| --- | --- | --- | --- |
| `ic-node` | main | `ic-node` | Ledger nodes, plus `ic-admin` and `ic-bootstrap` |
| `ic-worker` | main | `ic-worker` | Close, unit orders |
| `ic-api` | main | `ic-api` | Core API |
| `ic-portal` | main | `ic-portal` | Portal service |
| `ic-payments` | main | `ic-payments` | Payments pump and drill |
| `ic-customers` | customers | `ic-customers` | Customer master |
| `ic-documents` | documents | `ic-documents` | Documents service |
| `ic-finance-close` | finance | `ic-finance-close` | Finance close |
| `ic-group` | group | `ic-group` | Group service |
| `ic-reinsurance` | reinsurance | `ic-reinsurance` | Reinsurance driver and reinsurer API |
| `ic-underwriting` | underwriting | `ic-underwriting` | Underwriting driver |
| `ic-underwriting-clinical` | underwriting | `ic-underwriting-clinical` | Disclosure intake |
| `ic-unit-pricing` | unit-pricing | `ic-unit-pricing` | Unit pricing driver |
| `ic-otel-collector` | main | `ic-otel-collector` | The OpenTelemetry Collector; reads only the Application Insights connection string |
| `temporal` (only with `selfHostedTemporal`) | main | `temporal` in namespace `temporal` | Self-hosted Temporal; reads only its database password |
| `<name>-aks-control-plane` | main | none | The cluster's own identity, Network Contributor on the virtual network and Disk Snapshot Contributor for the backups |

Each identity is federated to exactly one service account in the namespace `insurance-core` and holds only its own roles and its own cluster token. The Entra app registrations and app roles are made by `deploy/entra/apps.sh` (workforce and External ID tenants) and `deploy/entra/group-app.sh`, with one registration per reinsurer that uses the API.

## Sizing

Measured with `ic-simulate` on 15 Nov 2026: 2,000 policies and 5,000 group members over 61 business days on 8 partitions made **7,286 commands and 74,666 journal entries** (29 seconds on one machine). That is about **0.017 commands and 0.18 journal entries per life per day**, most of them at the close and the billing run.

| Book | Commands a day | Journal entries a day | Average entries a second | Peak (the close, 50 times the average over an hour) |
| --- | ---: | ---: | ---: | ---: |
| 100,000 lives | 1,700 | 18,000 | 0.2 | about 10 a second |
| 1,000,000 lives | 17,000 | 180,000 | 2 | about 100 a second |

Raft makes one `fsync` per append for each partition group (`crates/raft/src/store.rs`), batched across the commands waiting, so the disk's write latency, not its size or IOPS, sets the ledger's write rate. **The disks are Premium SSD v2** (`deploy/k8s/storage.yaml`) at the free baseline of 3,000 IOPS and 125 MB/s: sub-millisecond writes, against 2 to 5 ms on the old P10 with its 500 IOPS. At a million lives the peak is under a twentieth of the baseline. 128 GiB holds the journal until it is sealed to Blob Storage, a year many times over at these rates. These are estimates from the simulation; measure on the first deployment with `fio --name=raft --rw=write --bs=4k --fsync=1 --size=1G` on a ledger node's disk and with a business day's load, and raise IOPS (`az disk update --disk-iops-read-write`) without downtime if the p99 write latency passes 2 ms.

The apps pool's minimum of three D4ds_v5 covers the requests with a zone lost; the autoscaler adds nodes for the close and for a burst of portal use. The ledger pool is sized for the voter's peak with room for snapshot transfer and catch-up after a node returns.

## Needed but not in the templates

| Need | What it is | Who decides or provides it |
| --- | --- | --- |
| **Temporal** | Templated both ways: Temporal Cloud (address and API key in `settings.yaml`), or self-hosted with `selfHostedTemporal` and `deploy/temporal` (Helm values for the official chart on the PostgreSQL server). Which one is the insurer's decision. | The insurer (`decisions.md`) |
| **Public names** | Templated in `infra/edge.bicep` (Front Door, WAF, managed TLS, Azure DNS records). The domain names themselves, and the DNS zone if it is not in Azure. | The insurer |
| **Email domain and SMS number** | The e-mail domain is templated (`emailDomain` in `infra/documents.bicep`; records written when its zone is in Azure DNS, then verification started once). An SMS number or short code is bought in the portal: New Zealand numbers need an application to Azure and a provider. | The insurer, and a provider for SMS |
| **Bank connectivity** | Calls to the bank leave from the fixed egress range (`egressPrefix`) for the bank to allow-list. The mutual TLS channel, its credentials in the payments vault, and a private link (ExpressRoute or VPN) if the bank requires one. No bank is chosen. | The insurer and its bank |
| **Partners' address ranges** | The print house, the fund administrator and each SFTP reinsurer, to restrict their accounts (`printHouseIpRanges`, `fundAdministratorIpRanges`, `reinsurerIpRanges`). | Each partner |
| **Alert recipients** | `alertEmails` for main and the module templates. | The insurer |
| **Fund administrator's identity** | An Entra identity of the administrator's, granted write on the inbound valuations container (`administratorPrincipalId`). | The administrator |
| **AIA Vitality intake** | A small service and its identity, once the feed format is known. | AIA |

## Gaps between the templates and what the services need

Closed on 15 Nov 2026: core alerts, OpenTelemetry to Application Insights, separate system, ledger and autoscaled apps pools, Premium SSD v2 ledger disks, private endpoints for every PaaS service that has them, RA-GZRS ledger storage, disk backups, log retention with an archive tier, Defender on a GA API (`2025-06-01`), the "Losing a region" runbook, the edge, the e-mail domain and the Temporal templates.

1. **The module templates are separate deployments.** `main.bicep` does not reference them, so the deployment order in `operations.md` (main, tokens, the seven module templates, then the manifests) is the only thing that keeps them in step.
2. **Communication Services has no private endpoint** (Azure offers none). It is reached over TLS with Entra ID.
3. **Losing Sydney pauses what depends on its stateful services** until they fail over: sealing and root publication (storage account failover), events (promote the Event Hubs secondary), and the PostgreSQL databases (restore from geo backup). The ledger itself carries on in Melbourne (`operations.md`, "Losing a region").
4. **Placeholders to confirm:** log retention (seven years), the WAF rate limit (600 a minute) and country list (none), and the disk baseline (3,000 IOPS), which a load test on the first deployment should confirm.
