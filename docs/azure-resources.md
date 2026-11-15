# Azure resources: inventory and specifications

Every Azure resource the system needs, with its size and what it serves. It is read from `infra/*.bicep`, `infra/main.bicepparam` and `deploy/k8s` on main (15 Nov 2026), so it matches what a deployment of those templates creates. The resources the system needs that the templates do not create are listed separately in [Needed but not in the templates](#needed-but-not-in-the-templates), and [Gaps](#gaps-between-the-templates-and-what-the-services-need) lists where the templates and the services do not yet agree.

**Nothing is deployed.** No Azure subscription has been deployed to. CI compiles every template and renders the manifests on each push, and the whole arrangement runs on one machine under Docker Compose. [The Azure deployment diagram](architecture/azure-deployment.png) draws the same resources.

Where a template sizes something differently by environment, the table gives **prod** and **dev/test** (the `environment` parameter, `dev` by default). The figures for VM sizes, IOPS and service limits are Azure's published figures for that SKU. Check them against Azure's current documentation when you deploy.

## Region and placement

| | Today in the templates | Pending |
| --- | --- | --- |
| Region | One region. `infra/main.bicepparam` sets `newzealandnorth`, and every template takes the resource group's location. | **The Raft placement is still to be chosen.** One option is three voters across the three zones of Australia East (Sydney), with Australia Southeast (Melbourne) as a standby. The other is to add a voter in a third region as a tiebreaker, so that losing any one region keeps a majority (five voters across three regions; `decisions.md`, "A second region"). Neither is written in the templates. Both put policy data outside New Zealand, which is a decision for the insurer and its regulator. |
| Zones | The AKS node pool spans zones 1, 2 and 3, and the three `ic-node` pods are spread one per zone. Event Hubs is zone-redundant. In prod, storage is ZRS or GZRS and the PostgreSQL servers are zone-redundant. | None. |
| Fail-back | Built, but `IC_PREFERRED_NODES` is unset, so nothing fails back until voters exist in a second region. | Depends on the placement choice. |
| Communication Services data | `communicationDataLocation` = `Australia` (`infra/documents.bicep`). | Confirm with the placement choice. |

## Resource count

What one deployment of every template creates in prod, with no reinsurers configured for SFTP and no alert emails:

| Resource type | Count | Created by |
| --- | ---: | --- |
| AKS cluster | 1 | main |
| AKS node pool (system) | 1 pool, 3 VMs | main |
| Container Registry | 1 | main |
| Storage accounts | 10 | main 1 (ledger), customers 1, documents 1, finance 1, group 1, reinsurance 1 (SFTP), underwriting 1, unit-pricing 2 |
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
| Log alert rules | 5 | reinsurance 2, unit-pricing 3 |
| Action groups | up to 2 | reinsurance and unit-pricing, only when `alertEmails` is set |
| User-assigned managed identities | 13, each with one federated credential | main 5, the module templates 8 |
| Role assignments | about 50 | every template |

## Compute

### AKS cluster (`infra/main.bicep`)

| Setting | Value |
| --- | --- |
| Name | `<name>-aks` |
| Tier | Standard in prod (uptime SLA), Free in dev/test |
| Node pool | One pool, `system`, mode System. **3 × `Standard_D4ds_v5`** (4 vCPU, 16 GiB, 150 GiB local temp disk each), Azure Linux, zones 1, 2 and 3. The size and count are the parameters `nodeVmSize` and `nodeCount`. There is no autoscaler and no separate user pool. |
| Total | 12 vCPU and 48 GiB. After AKS's own reservations, about 3.8 vCPU and 11 to 12 GiB per node are left for pods. |
| Networking | Azure CNI overlay with Cilium as dataplane and network policy. Advanced Container Networking Services with **WireGuard** encrypts traffic between nodes on UDP 51871. |
| Identity | Entra ID with Azure RBAC and local accounts off. OIDC issuer and Workload Identity are on. |
| Add-ons | Key Vault Secrets Store CSI driver (rotation every 2 minutes) and the monitoring agent to the Log Analytics workspace. |
| Upgrades | Patch channel; node OS on NodeImage. |

### Container Registry (`infra/main.bicep`)

One registry on the **Premium** SKU, with the admin user and anonymous pull off. The kubelet identity has AcrPull. It holds one image, `insurance-core`, which contains every binary.

### Workloads on the cluster (`deploy/k8s`)

These are the requests and limits as written in the manifests. Memory has limits; CPU has requests only.

| Workload | Kind | Replicas | CPU request | Memory request / limit | Storage |
| --- | --- | ---: | --- | --- | --- |
| `ic-node` | StatefulSet, one per zone | 3 | 1 | 2 GiB / 4 GiB | 128 GiB `managed-csi-premium` each (Premium SSD P10, zonal LRS: 500 IOPS and 100 MB/s, bursting to 3,500 IOPS) |
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
| **Long-running total** | | **27 pods** | **6.8 vCPU** | **10.25 GiB / 30 GiB** | **384 GiB Premium SSD** |
| `ic-close` | CronJob, 00:05 NZ | 1 | 100m | 128 MiB / 512 MiB | none |
| `ic-unit-orders` | CronJob, every 15 minutes | 1 | 100m | 128 MiB / 512 MiB | none |
| `ic-payments-drill` | CronJob, Tuesdays at 09:30 NZ | 1 | 50m | 64 MiB / 256 MiB | none |
| `ic-admin` | CronJob, suspended, run by hand | 1 | 100m | 128 MiB / 512 MiB | none |
| `ic-bootstrap` | Job, once | 1 | 100m | 128 MiB / 512 MiB | none |

Each Deployment and the StatefulSet has a pod disruption budget. `ic-api`, `ic-portal`, `ic-customers`, `ic-documents` and `ic-group` spread their two replicas across zones; the other Deployments leave placement to the scheduler.

**Fit.** The requests (6.8 vCPU, 10.25 GiB) fit the three nodes, with about 4 vCPU to spare once the system pods are counted. The headroom is small when a zone is lost. The two remaining nodes have about 7.6 vCPU for pods, and they must take every stateless pod from the lost node beside their own: about 5.8 vCPU of requests plus the system pods. The lost zone's `ic-node` cannot move, because its disk is zonal. The memory limits add up to about 30 GiB against about 35 GiB allocatable, so the pods can burst together on three nodes but not on two. No load test has sized the cluster yet; `ic-simulate` is the tool for it (see Gaps).

## Storage

All accounts are StorageV2, TLS 1.2 minimum, with public blob access off. They are reached by Entra ID with no shared keys.

| Account | Template | Redundancy (prod / dev) | Containers and protection | Serves |
| --- | --- | --- | --- | --- |
| Ledger account `<name><unique>` | main | ZRS / LRS, Hot | `merkle-roots`: write-once for `retentionDays` (2,920 days, 8 years). `cold-segments`: write-once for 8 years; moves to Cool after 30 days and Cold after 180. `payments-evidence`: write-once for 8 years. `reinsurance-records` (added by `reinsurance.bicep`): write-once for 8 years. | Signed daily Merkle roots, sealed journal segments, bank evidence, statements and releases to reinsurers |
| Customer master register | customers | GZRS / LRS | `customers-erasures`: write-once for `registerYears` × 366 days (10 years). The lock is applied once in prod with `tools/lock-erasure-register.sh`. | The erasure register a restore is replayed against |
| Document vault | documents | GZRS / LRS | `documents-vault`: versioning with version-level immutability, each document locked to its own retention date. `print-outbox`: for the print house to collect. | Issued documents and print files |
| Finance extracts | finance | ZRS / LRS | `finance-extracts`: write-once for 8 years. Also reads `merkle-roots`. | Valuation and experience extracts |
| Group quarantine | group | ZRS / LRS | `group-quarantine`, deleted after 35 days. Scanned by Defender. | Employer membership files |
| Reinsurer SFTP drops | reinsurance | ZRS / LRS, hierarchical namespace, SFTP on | One `ri-<reinsurer>` container and one SFTP local user (SSH key) per entry of `sftpReinsurers` | Listings and statements that reinsurers collect |
| Clinical store | underwriting | ZRS / LRS | `clinical`: no soft delete, no versioning and no change feed, on purpose, so that deleting a person's wrapped key erases them | Health disclosures and evidence |
| Valuations in | unit-pricing | ZRS / LRS | `unit-pricing-inbound`, deleted after 90 days. Scanned by Defender. The fund administrator writes it under its own identity. | Valuation files |
| Holdings extract out | unit-pricing | GZRS / LRS | `unit-pricing-outbound`: write-once for 2,555 days (7 years) | Unit holdings extract for the fund administrator |

## Databases

| Server | Template | SKU (prod / dev) | Storage | Availability and backup | Serves |
| --- | --- | --- | --- | --- | --- |
| Customer master `<name>-<unique>-customers` | customers | `Standard_D4ds_v5` General Purpose (4 vCores, 16 GiB) / `Standard_B2ms` Burstable (2 vCores, 8 GiB) | 128 GiB, grows automatically | Zone-redundant HA in prod; 35-day backups, geo-redundant in prod | PostgreSQL 16, database `customers`. Entra sign-in only with TLS required. The firewall admits the AKS outbound IP. |
| Temporal store `<name>-<unique>-pg` (only with `deployPostgres`) | main | `Standard_D2ds_v5` General Purpose (2 vCores, 8 GiB) | 128 GiB | Zone-redundant HA in prod; 35-day backups | PostgreSQL 16 for a self-hosted Temporal, if Temporal Cloud is not chosen |

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
| Log Analytics workspace | main | PerGB2018, 90-day retention | Container logs (JSON lines on stdout) from the monitoring add-on |
| Application Insights | main | Workspace-based | Created and exported as an output, but no service sends to it (see Gaps) |
| Log alert rules | reinsurance (2), unit-pricing (3) | Every 5 minutes on the workspace; severity 1 or 2 | Reinsurance exception opened and delivery stopped; a price that needs a person, failing passes, a silent driver |
| Action groups | reinsurance, unit-pricing | Email to `alertEmails` | On-call |

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

Each identity is federated to exactly one service account in the namespace `insurance-core` and holds only its own roles and its own cluster token. The Entra app registrations and app roles are made by `deploy/entra/apps.sh` (workforce and External ID tenants) and `deploy/entra/group-app.sh`, with one registration per reinsurer that uses the API.

## Needed but not in the templates

| Need | What it is | Who decides or provides it |
| --- | --- | --- |
| **Temporal** | The close runs on Temporal. Either Temporal Cloud, or self-hosted Temporal on AKS (its own pods, not sized here) with the optional PostgreSQL server. `deploy/k8s/settings.yaml` holds a placeholder address. | The insurer (`decisions.md`) |
| **Ingress, TLS and DNS** | An ingress controller (for example Application Gateway for Containers, or an NGINX ingress with a public IP), TLS certificates and DNS names for `ic-api`, `ic-portal`, the reinsurer API and the documents delivery-report webhook. A web application firewall in front of the public pages. None of these are in the templates. | The insurer's platform team |
| **Email domain and SMS number** | An Email Communication Services resource with a verified sending domain linked to Communication Services, and a purchased SMS number or short code (`infra/documents.bicep`, header). | A person, once |
| **Bank connectivity** | The mutual TLS channel to the chosen bank, and its credentials in the payments vault. No bank is chosen. | The insurer and its bank |
| **Second and third region** | Further voters, their AKS clusters or node pools, network links between the regions, and storage for the region-loss plan. | The pending placement choice above |
| **Fund administrator's identity** | An Entra identity of the administrator's, granted write on the inbound valuations container (`administratorPrincipalId`). | The administrator |
| **AIA Vitality intake** | A small service and its identity, once the feed format is known. | AIA |

## Gaps between the templates and what the services need

1. **No alerts on the core.** The only alert rules are reinsurance's and unit pricing's. Nothing alerts when a close fails, a ledger node is down, a partition has no leader, housekeeping falls behind or `unit_orders_left` is not zero. These conditions are logged, and `ic-admin status` shows them, but no rule watches for them.
2. **Application Insights is unused.** It is created and its connection string is an output, but no manifest or service reads it. Logs reach Log Analytics through the monitoring add-on. Either wire the services to it or drop it.
3. **One node pool runs everything.** System pods and every workload share three D4ds_v5 nodes, with no autoscaler and little headroom after a zone loss (see Fit). For production, a separate user pool would be the usual layout. A best estimate until a load test says otherwise is three `Standard_D8ds_v5` (8 vCPU, 32 GiB) user nodes across the zones beside a small system pool. Size it with `ic-simulate` and a business-day load before the first prod deployment.
4. **Ledger disks may limit write rate.** Each `ic-node` has a 128 GiB Premium SSD (P10: 500 IOPS baseline), and Raft syncs every entry to disk before acknowledging it. The disks have not been measured under load. Premium SSD v2 or a larger tier may be needed.
5. **No private networking.** Every PaaS service (storage, Key Vault, Event Hubs, PostgreSQL, Communication Services) is reached over its public endpoint, with Entra ID and TLS. The customer master's PostgreSQL admits the AKS outbound IP through a firewall rule. No virtual network integration or private endpoints are written. Many insurers and regulators expect private endpoints for personal and health data.
6. **The ledger's write-once evidence is not geo-redundant.** The ledger account (Merkle roots, sealed segments, payment evidence, reinsurance records) is ZRS in prod, while the customer master register, document vault and holdings extract are GZRS. Losing the region would leave the proofs out of reach until it returns. This should follow the placement choice.
7. **No backup of the node disks.** The ledger relies on three Raft replicas and the sealed segments in Blob Storage. No Azure Backup or disk snapshot policy is written for the `ic-node` disks.
8. **Log retention is 90 days.** Container logs, which include the cluster access log and the reinsurer API log, are kept 90 days in the workspace. Records with longer retention needs are kept in write-once storage. Confirm 90 days meets the audit requirement.
9. **The Defender settings use a preview API version** (`2022-12-01-preview`) in `group.bicep` and `unit-pricing.bicep`.
10. **The module templates are separate deployments.** `main.bicep` does not reference them, so the deployment order in `operations.md` (main, tokens, the seven module templates, then the manifests) is the only thing that keeps them in step.
11. **"Losing a region" is missing from the runbook.** `deploy/k8s/portal.yaml` points to a section of that name in `operations.md`, and it does not exist yet.
