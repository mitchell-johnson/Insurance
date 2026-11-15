# Operations

## First deployment

What these steps create, with every resource's size and what is needed beyond the templates (Temporal, ingress, the email domain, the bank), is in [azure-resources.md](azure-resources.md).

1. **Create the platform.**

   ```sh
   az group create -n <rg> -l newzealandnorth
   az deployment group create -g <rg> -f infra/main.bicep -p infra/main.bicepparam
   az deployment group show -g <rg> -n main --query properties.outputs
   ```

   Keep the outputs: they fill in the manifests.

2. **Issue the cluster tokens.** Each workload that talks to the ledger nodes has its own token, the Key Vault secret `cluster-token-<workload>`, and the nodes hold each to its grant in `deploy/cluster-access/grants.json` (see "Cluster access" below). Nobody needs to know a token.

   ```sh
   deploy/cluster-access/issue.sh <keyVaultName>
   az deployment group create -g <rg> -f infra/main.bicep -p infra/main.bicepparam -p clusterTokensIssued=true
   ```

   The script makes any token that is missing and writes `deploy/cluster-access/access.json`, the map of their digests; it is applied with the manifests in step 6. Each identity is granted its own token's secret and no other secret, so the tokens must exist before the grants are made: hence the second deployment. The module templates (steps 9, 10 and the others' "Running it") are deployed after this step.

   **Then deploy each module's template** into the same resource group. Each one is deployed on its own (`main.bicep` does not reference them), after the tokens exist, and before step 6, because every module's manifest is in the kustomization and needs that template's outputs. Every template takes `name`, `keyVaultName` and `aksOidcIssuer` from the main deployment's outputs; the other parameters and what each creates are in the comment at the top of the file and in the module's document.

   | Template | Further parameters | Fills | Described in |
   | --- | --- | --- | --- |
   | `infra/customers.bicep` | `eventHubsNamespaceName`, `postgresAdminGroupObjectId`, `postgresAdminGroupName`, `indexKey` | `deploy/k8s/customers.yaml` | `docs/customers.md`; then create the service's database role with `pgaadauth_create_principal` (from a host inside the virtual network: the database has no public address), and in prod lock the erasure register once (`tools/lock-erasure-register.sh`) |
   | `infra/documents.bicep` | `eventHubsNamespaceName`, `webhookKey`, `emailDomain`, `emailDnsZoneName`, `printHouseIpRanges` | `deploy/k8s/documents.yaml` | `docs/documents.md`, "Deploying": deployed twice, the second time with `deliveryReportsUrl` once the service is reachable |
   | `infra/underwriting.bicep` | `eventHubsNamespaceName` | `deploy/k8s/underwriting-intake.yaml` | step 10 below |
   | `infra/reinsurance.bicep` | `storageAccountName`, `eventHubsNamespaceName`, `alertEmails`, `sftpReinsurers`, `reinsurerIpRanges` | `deploy/k8s/reinsurance.yaml` | `docs/reinsurance.md`, "Hosting and the driver" |
   | `infra/group.bicep` | `eventHubsNamespaceName` | `deploy/k8s/group.yaml` | `docs/group.md`, "On Azure" |
   | `infra/unit-pricing.bicep` | `administratorPrincipalId`, `alertEmails`, `fundAdministratorIpRanges` | `deploy/k8s/unit-pricing.yaml` | `docs/unit-pricing.md`, "On Azure" |
   | `infra/finance.bicep` | `ledgerStorageName` | `deploy/k8s/finance.yaml` | `docs/finance.md` |

   Every module template also takes `privateNetwork`, the main deployment's output of that name (pass it as JSON: `-p privateNetwork="$(az deployment group show -g <rg> -n main --query properties.outputs.privateNetwork.value -o json)"`). With it, each storage account and the customer database get a private endpoint and refuse everything else; without it, they stay public with Entra ID. See "Private networking".

   Payments needs no template of its own: `main.bicep` creates its vault and evidence container (step 9).

3. **Build and push the image.**

   ```sh
   az acr build -r <registry> -t insurance-core:$(git rev-parse --short HEAD) .
   ```

4. **Register the applications with Entra ID.** One script makes every registration, its app roles (fixed IDs, so a rerun changes nothing that is assigned) and version 2 access tokens, and prints the values the manifests need:

   ```sh
   deploy/entra/apps.sh workforce <name> <portal host>      # in the insurer's tenant
   az login --tenant <external tenant> --allow-no-subscriptions
   deploy/entra/apps.sh customers <name> <portal host>      # in the External ID tenant
   ```

   The workforce run makes the core API, the portal service and the staff consoles' single-page application, the customer master, documents, the underwriting intake and the reinsurer API; the customers run makes the customer-facing API and the adviser and employer portals' single-page application. Group schemes has its own script, `deploy/entra/group-app.sh`. Then give each workload's managed identity the roles it calls with, and staff the roles they work under (to a group, not to people one by one):

   ```sh
   deploy/entra/apps.sh grant api://<name>-customers portal <portal identity principal id>
   ```

   The portal also does staff and operations work in the customer master for people who passed its own checks (docs/channels.md, "Staff work in other services"), so its identity holds `staff`, `privacy` and `operations` there too: repeat the grant with each of those role names.

   Each reinsurer that reads its statements by API sends a certificate and gets its own application: `deploy/entra/apps.sh reinsurer <name> <reinsurer id> <certificate.pem>` prints the entry for `IC_REINSURANCE_API_CLIENTS`.

5. **Fill in `deploy/k8s`.** Replace every `REPLACE-` value: in `kustomization.yaml`, `identities.yaml`, `settings.yaml` and `bootstrap.yaml` from the main deployment; in `api.yaml` with the customer master's address and app scope; in `portal.yaml` and `payments.yaml` from the main deployment and steps 4, 8 and 9; and in each module's manifest from its template above. `grep -l REPLACE- deploy/k8s/*.yaml` lists what is left; `kubectl kustomize deploy/k8s` must show none. The Temporal address in `settings.yaml` waits on the Temporal decision (`docs/decisions.md`). The first business date in `bootstrap.yaml` is set once and can never be changed.

6. **Apply.**

   ```sh
   az aks get-credentials -g <rg> -n <aksName>
   kubectl apply -f deploy/cluster.yaml
   kubectl label namespace app-routing-system insurance-core/ingress=true
   kubectl create namespace insurance-core --dry-run=client -o yaml | kubectl apply -f -
   kubectl -n insurance-core create configmap ic-cluster-access \
     --from-file=deploy/cluster-access/access.json --dry-run=client -o yaml | kubectl apply -f -
   kubectl apply -k deploy/k8s
   kubectl -n insurance-core wait --for=condition=complete job/ic-bootstrap --timeout=15m
   kubectl -n insurance-core logs job/ic-bootstrap -c status
   ```

   `deploy/cluster.yaml` holds the cluster-scoped add-on objects: the internal NGINX controller and the snapshot class. The bootstrap job forms the Raft groups, opens the first business date and activates the shipped products. Every step is safe to repeat. With private networking and no `adminIpRanges`, run these from a host inside the virtual network (see "Private networking").

7. **Open the way in.** Deploy `infra/edge.bicep` (Front Door Premium and its WAF) and approve its Private Link connection once; see "The way in".

8. **Set up the portal service** (`ic-portal`: the app's API, the portals, the consoles and the status page). Its app registrations, the values in `deploy/k8s/portal.yaml` and its public host are described in `docs/channels.md`, "Running it".

9. **Set up payments** (`ic-payments-pump`: the bank channels and the evacuation drill). The payments vault, the bank keys and credentials, and the values in `deploy/k8s/payments.yaml` are described in `docs/payments.md`, "Running it".

10. **Set up underwriting** (the driver `ic-underwriting` and the intake `ic-underwriting-intake`; `docs/underwriting.md`). Before step 6, with the other module templates after step 2, deploy its template into the same resource group, with the main deployment's Key Vault, AKS OIDC issuer and Event Hubs namespace:

    ```sh
    az deployment group create -g <rg> -f infra/underwriting.bicep \
      -p name=<name> keyVaultName=<keyVaultName> aksOidcIssuer=<aksOidcIssuer> \
      eventHubsNamespaceName=<eventHubsNamespace>
    az deployment group show -g <rg> -n underwriting --query properties.outputs
    ```

    It creates the clinical store (its own storage account with no soft delete or versioning, deliberately), the HSM wrapping key, the intake's identity, the driver's own identity (which may read only `cluster-token-ic-underwriting`), and the `underwriting-notices`, `underwriting-evidence` and `underwriting-delegates` hubs with the driver's right to send to them. Register an app for the intake with the app roles `new-business`, `underwriting-console`, `evidence` and `delegate-api`, and put its application ID URI and the outputs in `deploy/k8s/underwriting-intake.yaml`. Put `underwritingDriverIdentityClientId` in `deploy/k8s/underwriting.yaml`. The driver reads `IC_EVENT_HUBS_ENDPOINT` from `settings.yaml`.

11. **Apply the insurer's own configuration.** The shipped products are active after step 6; what each module needs before the first day is the insurer's to decide and is kept in one set-up file: finance's ledger shape, chart and close configuration, the claims rules and authority book, the banks, channels and routes of payments, the complaints timers, the unit pricing partition count. `ic-admin apply <setup.json>` sends it, from the admin pod (`deploy/k8s/admin.yaml`) or a job:

    ```json
    { "steps": [
      { "core":   { "key": "claims-rules-1", "command": { "claims": { "activate_rules": { "rules": { } } } } } },
      { "module": { "module": "finance", "envelope": { "key": "configure-1", "caller": "operator", "command": { "configure": { "partitions": 64, "year_end_month": 3 } } } } },
      { "module": { "module": "complaints", "envelope": { "key": "timers-1", "caller": "config", "actor": "config", "at": "2027-11-15T08:00:00Z", "command": { "activate_timers": { } } } } }
    ] }
    ```

    A `core` step is a configuration command, sent as the caller `config` to every partition under its key; any other command is refused when the file is read. A `module` step is the module's own envelope, sent as written (each module's document gives its envelope and commands). Steps go in order and the run stops at the first one refused, naming it. Applying the file again changes nothing, so a run that died is simply repeated, and a later version of the file with steps added sends only what is new. A step may list `accept` words: a refusal containing one means the step is already as wanted. Keep the file under change control: it is the record of what was configured and who approved it. Where a module asks for a second person (finance's `approve_config`, the claims rules' `approved_by`), the file names the people who did approve; the tool does not stand in for them. Staff roles and the first administrator are set up through the portal service (`docs/channels.md`).

## Day to day

**Is it healthy?** `GET /readyz` on the API answers 200 with the business date when every partition has a leader. For detail:

```sh
kubectl -n insurance-core create job status-now --from=cronjob/ic-admin
kubectl -n insurance-core logs job/status-now
```

`ic-admin` is a suspended CronJob that exists to be run by hand like this. To run another `ic-admin` command, write the job out with `--dry-run=client -o yaml`, change its `command`, and apply it.

`ic-admin status` prints, for each node and partition: the leader, the last log index, the state hash and the housekeeping backlog. All nodes should agree on the state hash at the same index.

`ic-admin watch [limit]` asks the same of every node and logs only what is wrong, one JSON line each (target `ledger.watch`), then a summary line `ledger watch`. The `ic-watch` CronJob (`deploy/k8s/admin.yaml`) runs it every five minutes with a backlog limit of 5,000 entries per partition, and the alerts below read its lines. It runs under its own service account and managed identity (`ic-watch`, output `watchClientId`) with its own cluster token, `cluster-token-ic-watch`, whose grant lets it ask a node's status and nothing else.

**The daily close** runs from the `ic-close` CronJob at 00:05 New Zealand time. It closes every business day whose midnight has passed, oldest first, so a missed night is caught up by the next run. Check it with `kubectl -n insurance-core get jobs` and read the report in the job's log.

To run a close by hand: `kubectl -n insurance-core create job close-now --from=cronjob/ic-close`.

**If a close fails with reconciliation breaks**, the day stays open and the next day is not started. The job log lists each break (the check, the partition, the amounts). Business continues on the open date. Do not force the date forward: find the cause, and once the books reconcile, run the close again.

**Unit orders** execute from the `ic-unit-orders` CronJob every fifteen minutes, once the unit pricing module has activated the price for their date, and the same job replays orders standing on a corrected price. The close does the same work as one of its steps, so a missed run is caught up at midnight. The job's log prints how many orders it could not move on each partition (`left`), and the close's report carries the same figure as `unit_orders_left`; anything other than zero there is for a person to look at. To run it now: `kubectl -n insurance-core create job unit-orders-now --from=cronjob/ic-unit-orders`. A line `unit orders left` (per partition, with the count) raises the alert of that name.

**Reconciliation at any time:** `GET /v1/admin/reconciliation` (role `operations`).

**Is every partition on the same configuration?** `ic-admin register` lists each product and rate table version with its effective date, hash and whether a policy holds it, on every partition, and reports drift: a global version missing or different on some partition. Run it after any activation and after `ic-admin apply`; anything under `drift` is for a person to look at.

**Taking a bad product or rate table version out of use (kill switch).** Send `withdraw_config` as caller `config` (a `core` step in the set-up file, or through the API): the kind (`product` or `rate_table`), the ID and version, the reason, the change reference and who approved it. From the business date the version before it prices the next bill and new business; what it already priced stays posted, for remediation by replay. With `retire` set, a product version is also retired, which is refused (`version_in_use`) while any policy on a partition still holds it. Event `ConfigWithdrawn`; `docs/architecture.md`, core item 6j, has the detail.

### Alerts

`infra/main.bicep` makes these log alert rules on the workspace, sent to the action group of `alertEmails` (with no addresses they still fire and show in Azure Monitor, but notify nobody). The module templates add their own (reinsurance 2, unit pricing 3; see their documents).

| Alert | Severity | Fires when | What to do |
| --- | --- | --- | --- |
| Partition has no leader | 0 | The watch logged `partition has no leader` | That partition takes no commands. `ic-admin status`: if two nodes are down, bring either back ("Losing things"). If all are up, look for a network policy or disk fault on the nodes' logs. |
| Ledger state hashes differ | 0 | The watch logged `state hashes differ` | Stop. Do not move leaders or resume fail-back. Find which voter differs (`ic-admin status`), replace it as a new member ("Replacing a node that lost its disk") and keep its disk for the investigation. |
| Ledger node down | 1 | Fewer `ic-node` pods running than `ledgerVoters`, or the watch could not reach one | One more loss stops writes. `kubectl -n insurance-core get pods -l app=ic-node -o wide`: a pod pending on its zone's disk waits for that zone; a pod crashing has the reason in its log. |
| Daily close failed | 1 | An `ic-close` pod failed, or ic-worker logged `ic-worker command failed` | Read the job's log. Reconciliation breaks: see "If a close fails". Anything else: fix and run the close by hand; the next run catches up every day due. |
| Housekeeping backlog | 2 | A partition's backlog above the watch's limit three checks running | Sealing, root publication or event delivery is behind. Check Blob Storage, Key Vault and Event Hubs (and their private endpoints); the backlog falls once they answer. |
| Unit orders left unpriced | 2 | The close or the unit-orders run logged `unit orders left` | See "Unit orders". Usually a price not yet activated by unit pricing. |
| Ledger watch silent | 2 | No `ledger watch` line for fifteen minutes | The rules above are blind. Check the `ic-watch` CronJob and its pod's log. |

## Upgrades

Roll the image tag in `kustomization.yaml` and apply. The StatefulSet replaces one ledger node at a time and the disruption budget keeps two of three up, so every partition stays writable throughout. The API and workers roll without dropping requests: they stop accepting work on SIGTERM and finish what is in flight.

A new version must be able to replay the log the old version wrote. Changes to how a command is applied are therefore additive, and are covered by the replay tests.

AKS node pool upgrades are safe for the same reason: drains respect the disruption budgets.

## Losing things

| What is lost | What happens | What to do |
| --- | --- | --- |
| A ledger node's pod or VM | Its partitions elect new leaders within a second or two. Callers retry and lose nothing. | Nothing. The pod is rescheduled, reattaches its disk and catches up. |
| An availability zone | One node is gone; two remain and every partition stays writable. | Nothing until the zone returns. |
| Two ledger nodes at once | Partitions stop accepting writes (no majority). The API answers 503. Nothing acknowledged is lost. | Bring either node back. |
| A worker during the close | Temporal hands the unfinished step to the other worker. | Nothing. |
| Temporal | The close cannot run. Business continues on the open date. | Restore Temporal; the next run closes every day that is due. |
| Blob Storage, Key Vault or Event Hubs | Sealing, root publication or event publication waits and retries. Business continues. | Nothing; watch the housekeeping backlog in `ic-admin status` fall once the service returns. |
| A ledger node's disk | See below. | Replace the node under a new number. |
| The region | See "Losing a region". | Depends on where the voters are. |

### Replacing a node that lost its disk

A node whose disk is gone must not come back under its old number: it has forgotten its votes. Its peers enforce this. A node that starts with an empty disk under a number the group already knows is refused, and logs `knows this node by another incarnation`.

Replace it with a new member under a number that has never been used:

1. Add the new number to `IC_NODE_PEERS` and `IC_NODES` in `settings.yaml` (for example `4=ic-node-4.ic-node:7000`) and set the StatefulSet to four replicas. Apply. `ic-node-4` starts empty.
2. Change the membership of every partition to the nodes that should now vote:

   ```sh
   kubectl -n insurance-core create job members --from=cronjob/ic-admin --dry-run=client -o yaml > members.yaml
   # in members.yaml set the command to:
   #   ["/usr/local/bin/ic-admin", "members", "all",
   #    "1=ic-node-1.ic-node:7000,2=ic-node-2.ic-node:7000,4=ic-node-4.ic-node:7000"]
   kubectl apply -f members.yaml
   ```

   The new node is caught up from a snapshot and then starts voting. The lost node is dropped.
3. Check `ic-admin status`: three voters on every partition, equal state hashes.

The old pod (`ic-node-3` here) stays in the StatefulSet, shut out and idle, because a StatefulSet cannot remove a pod from the middle. It does no harm. Remove it at the next planned maintenance by re-founding the StatefulSet with the surviving numbers.

Azure managed disks are replicated three times within their zone, so this is a rare event. The failure drill (`deploy/compose/chaos.sh`) rehearses it on every push to main, in its own workflow (`.github/workflows/drill.yml`).

### Backups and restore

Azure Backup for AKS (`infra/main.bicep`, `backupLedgerDisks`, on by default) snapshots the `ic-node` disks every four hours, keeps the snapshots 7 days, and copies one a day to the backup vault for 35 days (geo-redundant and immutable in prod). Only the `app=ic-node` volumes in `insurance-core` are included.

A backup is the last line, not the first. The ledger is held three times by Raft, every sealed segment is in write-once storage, and a node that loses its disk is replaced from its peers ("Replacing a node that lost its disk"). Restore from a backup only when no voter's disk survives, or to inspect an old state:

- **A restored disk is behind its peers and has forgotten votes it cast since the snapshot.** It must never rejoin under its old number. Restore it under a new number (a new pod and claim) and add it with `ic-admin members` as a new member, exactly as for a lost disk.
- **If every voter is lost**, restore each node's latest vault copy into a new cluster, start the nodes under their old numbers together (they hold the same snapshot moment, so no vote is contradicted), and replay the days since from the sealed segments in `cold-segments`. The Merkle roots in `merkle-roots` prove the result: each closed day's recomputed root must match its signed blob ("Proving a closed day").

To take a snapshot by hand before risky work: `kubectl -n insurance-core apply -f -` with a `VolumeSnapshot` of `data-ic-node-<n>` in class `ic-ledger-snapshots`.

## The way in

The public names are on Azure Front Door Premium (`infra/edge.bicep`): managed TLS certificates, the WAF (Microsoft Default Rule Set 2.1 and the bot manager, in Prevention, with a per-client rate limit), and HTTPS redirect. Front Door reaches the cluster over Private Link to the internal NGINX controller (`deploy/cluster.yaml`), whose rules are in `deploy/k8s/ingress.yaml`. The cluster has no public address.

1. After step 6, the controller has made the Private Link Service `insurance-core-ingress` in the cluster's node resource group.
2. Deploy the edge:

   ```sh
   az deployment group create -g <rg> -f infra/edge.bicep \
     -p name=<name> privateLinkServiceResourceGroup=<MC_ group> dnsZoneName=<zone> \
        portalHost=<host> apiHost=<host> reinsurersHost=<host> hooksHost=<host>
   ```

   With the zone in Azure DNS here, the CNAME and validation records are written; otherwise give the outputs `endpointHostName` and `validationTokens` to whoever runs DNS.
3. Approve Front Door's connection, once: `az network private-endpoint-connection list --id <pls id>` shows it pending; `az network private-endpoint-connection approve --id <connection id>`. Nothing else is ever approved, so only Front Door can reach the controller.
4. Put the same host names in `deploy/k8s/ingress.yaml` and apply.

The rate limit (`rateLimitPerMinute`, 600) and the country list (`allowedCountries`, empty) are placeholders. In a new environment, set the WAF to Detection for the first week (`mode`) and read `AzureDiagnostics | where Category == "FrontDoorWebApplicationFirewallLog"` for false positives before turning Prevention back on.

## Private networking

With `privateNetworking` (on by default), every PaaS service the system uses is reached only through a private endpoint in the `private-endpoints` subnet, and its name resolves there through the private DNS zones `main.bicep` links to the virtual network: the ledger storage, both Key Vaults, Event Hubs, the registry, the Temporal and customer PostgreSQL servers, every module's storage account, the backup account, and Azure Monitor (through a private link scope, for both the monitoring agent and the collector). The AKS API server is private.

Three things stay public on purpose, each with Entra ID required:

- **Accounts an outside party reaches:** the print outbox (the print house), the unit pricing upload and extract accounts (the fund administrator), and the reinsurers' SFTP account. Give their address ranges (`printHouseIpRanges`, `fundAdministratorIpRanges`, `reinsurerIpRanges`) to admit only those.
- **Communication Services** has no private endpoint. The documents service reaches it over TLS with its own identity.
- **Front Door** is the one way in, by design.

Every connection out of the cluster leaves from one public range, `egressPrefix` (a /30). Give it to the bank and any provider that allow-lists callers.

**Running deployment steps.** With `adminIpRanges` empty, nothing is reachable from outside the virtual network: run `kubectl`, `az keyvault`, `deploy/cluster-access/issue.sh` and `psql` from a host inside it (a jump host in its own subnet with Bastion, or a self-hosted build agent there). Or list the administrators' or build agents' ranges in `adminIpRanges`, which admits them to the API server, the vaults, the registry and the storage accounts beside the endpoints.

## Telemetry

Every process sends its traces and log records over OTLP (HTTP) to the OpenTelemetry Collector in the namespace (`deploy/k8s/telemetry.yaml`), which sends them to Application Insights by default; its data is in the Log Analytics workspace (`AppRequests`, `AppDependencies`, `AppTraces`). The same log lines also go to stdout as JSON, each with `trace_id` and `span_id`, and reach `ContainerLogV2` through the monitoring agent, so a log line and its trace are found from either side. One trace follows a request from the browser (the portal pages send `traceparent`) through the portal, the services, the ledger node, the command's envelope, the event on Event Hubs (its `traceparent` property) and Temporal's workflow and activities. Personal data is scrubbed in each process before anything leaves it (`crates/telemetry/src/scrub.rs`), and the collector drops the same kinds of attribute again.

**The settings** are the standard OpenTelemetry variables, in the ConfigMap `ic-telemetry`, which every workload takes:

| Variable | Default here | Meaning |
| --- | --- | --- |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://ic-otel-collector:4318` | Base URL; `/v1/traces` and `/v1/logs` are added |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `..._LOGS_ENDPOINT` | unset | A full URL for one signal, taking precedence |
| `OTEL_EXPORTER_OTLP_PROTOCOL` (and per signal) | `http/protobuf` | Or `http/json`. `grpc` is refused: put the collector in the path |
| `OTEL_EXPORTER_OTLP_HEADERS` (and per signal) | unset | `key=value,key2=value2`, percent-encoded; the per-signal list is added to it |
| `OTEL_EXPORTER_OTLP_COMPRESSION` (and per signal) | `gzip` | Or `none` |
| `OTEL_EXPORTER_OTLP_TIMEOUT` (and per signal) | `10000` | Milliseconds |
| `OTEL_TRACES_EXPORTER`, `OTEL_LOGS_EXPORTER` | `otlp` | `none` turns that signal's export off |
| `OTEL_SDK_DISABLED` | unset | `true` turns both off; trace context is still made and handed on |
| `OTEL_TRACES_SAMPLER_ARG` | `1.0` | Share of new traces kept; a caller's decision is kept |
| `OTEL_RESOURCE_ATTRIBUTES` | `deployment.environment.name=<env>` | Added to every span and record |
| `OTEL_SERVICE_NAME` | each process's own name | |

With no endpoint set at all (the compose cycle, the tests), nothing is exported and the log lines on stdout carry the trace IDs as before.

**Sending somewhere else.** Keep the processes pointed at the collector and change the collector's exporters in its ConfigMap: credentials then live in one place, under one identity, and more than one backend can be fed at once. The ConfigMap has each of these written out and commented:

- **Grafana Cloud:** exporter `otlphttp/grafana` to the stack's OTLP gateway, with `Authorization: Basic <instance id:token, base64>`.
- **Datadog:** the `datadog` exporter with the site (`ap1.datadoghq.com` for Australia) and the API key.
- **Honeycomb:** exporter `otlphttp/honeycomb` to `https://api.honeycomb.io` with header `x-honeycomb-team: <key>`.

Put the key in Key Vault, add it as an object to `ic-otel-collector-secrets` (it appears under `/mnt/secrets`), grant the collector's identity (`collectorClientId`) `Key Vault Secrets User` on that secret, and add the exporter to the `traces` and `logs` pipelines. With private networking the collector reaches outside services out through `egressPrefix`; Azure Monitor through its private link scope.

To skip the collector and send straight from the processes, set `OTEL_EXPORTER_OTLP_ENDPOINT` (and `OTEL_EXPORTER_OTLP_HEADERS` from a secret, not the ConfigMap) to the backend's OTLP HTTP endpoint. The collector's image is mirrored to the registry: `az acr import` as in `kustomization.yaml`.

## Losing a region

What happens depends on where the voters are, which is still to be chosen (`decisions.md`, "A second region"). The templates support either; this is what to do in each.

**Today and option A: three voters across the zones of one region, a standby region.** Losing the region stops the ledger: no voter is left. Nothing acknowledged is lost, because a write is acknowledged only once a majority has it on disk, and those disks are in the region. What is outside it:

- The proofs (Merkle roots, sealed segments, payment evidence, reinsurance records): the ledger account is RA-GZRS in prod, so a copy is readable in the paired region within about 15 minutes of each write (Azure's stated RPO).
- The customer master and Temporal databases: geo-redundant backups (`geoRedundantBackup`).
- The ledger disks' daily backup copies: the backup vault is geo-redundant in prod.
- The images: replicate the registry (`az acr replication create`) to the standby region before it is needed.

Decide first whether to wait. A region outage usually ends within hours, and waiting loses nothing. Failing over means running from a copy behind the last acknowledged write by up to the last sealed segment or disk backup, so every command since must be re-sent by its caller: the commands' idempotency keys make that safe.

To fail over:

1. Declare the incident and stop callers: the portal's status page shows the region lost when `IC_REGIONS` and `IC_HOME_REGION` are set (`deploy/k8s/portal.yaml`).
2. Deploy `infra/main.bicep` and the module templates in the standby region under a new `name` (Melbourne is Australia Southeast).
3. Restore each ledger node's last vault backup there ("Backups and restore", every voter lost), restore the customer master from its geo backup and replay its erasure register over it (`docs/customers.md`, "Restoring a backup").
4. Initiate the storage account failover (`az storage account failover`) so the proofs are writable in the standby region, then check the last closed day's root against its blob.
5. Point Front Door at the new cluster's Private Link Service (`infra/edge.bicep` with the new `privateLinkServiceResourceGroup`), and approve the connection.
6. Tell the callers to re-send everything since the restored point; reconcile (`GET /v1/admin/reconciliation`) before the next close.

Fail back the same way once the home region is healthy, in a planned window.

**Option B: a voter in a third region as a tiebreaker (five voters: two, two and one).** Losing any one region keeps a majority. The voters elsewhere take over within seconds; nothing is lost and nothing needs re-sending. The API keeps answering, from further away. Set `IC_PREFERRED_NODES` to the home region's nodes, and when it returns each node hands its groups back (`ic_raft::failback`, "Fail-back to the preferred region"). What to do:

1. Nothing for the ledger. Watch the alerts: "Ledger node down" fires for the lost voters; "Partition has no leader" should not.
2. Front Door has an origin per region (add the second to `infra/edge.bicep` with a health probe on `/healthz`); it stops sending to the lost one once the probe fails.
3. Sealing and publication carry on against the surviving region's storage. If the lost region held the storage account, its RA-GZRS copy is read-only: fail it over as in option A, step 4.
4. When the region returns, its voters rejoin and catch up; fail-back moves leadership home outside the frozen periods.

Option B needs, beyond these templates: a cluster or node pool in each further region on a peered virtual network (Raft port 7000 between them, latency between Sydney and Melbourne about 10 to 15 ms), the extra voters added with `ic-admin members`, and a second origin in `infra/edge.bicep`.

## Cluster access

Every caller of a ledger node presents its own token: the secret `cluster-token-<workload>` in Key Vault, mounted only into that workload's pods. The nodes read the access map (`IC_CLUSTER_ACCESS_FILE`, the ConfigMap `ic-cluster-access`) and hold each workload to its grant: the caller names it may put on a core command, the modules it may write and read, whether it may read the partitions, and whether it may administer the cluster. Only the nodes themselves are peers, reaching Raft and everything else. The map holds the tokens' SHA-256 digests, not the tokens. The code is `crates/raft/src/access.rs`; the grants are `deploy/cluster-access/grants.json`. Every process that calls the nodes has its own service account, identity, token and grant; no two share one. The admin pod and the bootstrap job are the exception by design: they administer the cluster, so they use the nodes' own token. A test (`crates/raft/tests/access.rs`) fails the build if a token in `deploy/k8s` has no grant, a grant has no token, or one token is mounted by two workloads.

The nodes read the map again every 30 seconds; a map that does not parse is logged and the last good one stays. A node does not start if its map is unreadable or its own token is not a peer in it.

**Report, then enforce.** The shipped grants are in `report` mode: nothing is refused, and every call a grant does not cover is logged by the nodes as `would be refused` (target `cluster.access`, with the workload, the path and what it lacked). After the first deployment, run the failure drill and a business day, and read those lines:

```sh
kubectl -n insurance-core logs statefulset/ic-node --since=24h | grep '"cluster.access"' | grep 'would be refused'
```

Add each missing right to `grants.json` (or fix the caller, if it should not need it), run `issue.sh` and apply the ConfigMap. When a full day passes with none, set `"mode": "enforce"` and apply it. From then on such a call is answered 403 and logged as `refused`.

**Rotating a workload's token.** No restart of a node.

1. `deploy/cluster-access/issue.sh <vault> rotate <workload>` puts a new token first in the workload's secret and keeps the old second, and writes the map with both digests. Apply the ConfigMap; within a minute the nodes accept both.
2. Restart the workload (`kubectl rollout restart deployment/<workload>`) once the CSI driver has the new secret (two minutes). It now sends the new token.
3. `issue.sh <vault> retire <workload>` drops the old token. Apply the ConfigMap.

**A workload that may no longer call.** Remove it from `grants.json`, run `issue.sh` and apply: its token is refused within a minute.

**Moving a live cluster from the shared token.** No cluster has run on the shared token in production, so this is only for one that did. With `"shared_token": "accept"` in the map, a token the node accepts that is in no grant works with full access, with a warning on each use. Run `issue.sh` first, then add the old shared token as the last line of `cluster-token-ic-node` by hand (were `issue.sh` run again it would list that line as the node's own, making it a peer, so do not run it until the move is done). Move the workloads to their own secrets one by one, then remove the line, set `refuse`, delete the `cluster-token` secret, and delete the old vault-wide `Key Vault Secrets User` assignments, which an incremental deployment leaves in place (`az role assignment list --scope <vault id>`).

Without an access map (the compose cycle, the drill and the tests), every holder of the one cluster token may do everything, as before.

## Proving a closed day

Each closed day has a blob `merkle-roots/<year>/<date>.json` in the immutable `merkle-roots` container: the ledger head of every partition, their Merkle root, and a Key Vault signature over it. Sealed journal segments are in `cold-segments`. To prove a day's books are unchanged, recompute the root from the ledger heads at that date and verify the signature against the key version named in the blob. Neither container allows a blob to be changed or deleted inside the retention period (eight years by default).

## Fail-back to the preferred region

Set on every ledger node:

| Variable | Meaning | Default |
| --- | --- | --- |
| `IC_PREFERRED_NODES` | Node numbers in the preferred region, for example `1,2`. Unset turns fail-back off. | off |
| `IC_FAILBACK_SHARE_PERCENT` | Share of a node's groups handed back per round. | 20 |
| `IC_FAILBACK_LATENCY_MS` | How long a moved group may take to answer a majority read. | 2000 |
| `IC_FAILBACK_SETTLE_SECONDS` | How long a preferred voter must have been level before it is handed a group. | 120 |
| `IC_FAILBACK_FROZEN` | Periods of the day with no moves, for example `23:30-02:30,06:00-08:00`. | none |
| `IC_FAILBACK_TIMEZONE` | Time zone of those periods. | `Pacific/Auckland` |

Set the frozen periods from the freeze calendar: end-of-day and the close, the billing peak, and the bank cutoffs.

- **See what it is doing:** `GET /v1/failback` on each node, with the cluster token. `waiting` says why nothing moved; `halted` says why it stopped itself.
- **Pause or resume:** `POST /v1/failback/pause` or `/resume` on each node that leads groups. Pause is held in memory: a restarted node starts unpaused. Resume also clears a stop.
- **Move a leader by hand, or reverse a fail-back:** pause first, then `POST /v1/partitions/{n}/leader` with `{"to": <node>}` to any node.
- **When it stops itself:** the log line `fail-back stopped on this node` carries the reason. Too slow: check the link to the preferred region before resuming. A different state hash at the same log entry: do not resume; that replica must be replaced as a new member and the difference investigated.

## Moving a cohort of policies from another system

1. The mapping code writes the cohort file: `cohort`, `currency`, `cutover` (the business date of the cutover), `control` (the old system's totals, taken independently of the rows) and `policies`, each with `issue` (marked `migrated`), `opening` and `legacy` (that policy's own figures). See `crates/api/src/migrate.rs`.
2. On the cutover day, after the close and before any billing: `ic-admin migrate load cohort.json`. The report lists refusals by reason with the first 200 exceptions, and each partition's figures beside its control totals. The business date will not advance while the cohort is open.
3. Clear what does not reconcile, or abandon: `ic-admin migrate abandon <cohort>` takes the load out on every partition; correct the file and load it again under a new cohort name.
4. Sign off once it reconciles: `ic-admin migrate sign-off <cohort> <finance> <business-owner> <evidence> [actuary]`. Nothing is signed unless every partition reconciles. Sign-off releases the policies, and only then do they appear to other modules.
5. Send the commission books: `ic-admin migrate commission cohort.json` sends each policy's `commission` (agent code, option, annual premiums, what was last paid each payee) to the commission module, once the cohort is released everywhere. Map legacy agent codes first (`map_legacy_agent`, see `docs/distribution.md`); refusals are listed by policy and the step can be run again.

