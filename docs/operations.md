# Operations

## First deployment

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

3. **Build and push the image.**

   ```sh
   az acr build -r <registry> -t insurance-core:$(git rev-parse --short HEAD) .
   ```

4. **Register the API with Entra ID.** Create an app registration for the API, define the app roles callers will hold (`new-business`, `policy-servicing`, `cash-matching`, `group-admin`, `claims`, `collections`, `config`, `operations`), and note its application ID URI.

5. **Fill in `deploy/k8s`.** Deploy underwriting's resources first (step 10): its manifests are in the kustomization and need that template's outputs. Replace every `REPLACE-` value in `kustomization.yaml`, `identities.yaml`, `settings.yaml` and `bootstrap.yaml`, and in `underwriting-intake.yaml` from step 10. The first business date in `bootstrap.yaml` is set once and can never be changed.

6. **Apply.**

   ```sh
   az aks get-credentials -g <rg> -n <aksName>
   kubectl create namespace insurance-core --dry-run=client -o yaml | kubectl apply -f -
   kubectl -n insurance-core create configmap ic-cluster-access \
     --from-file=deploy/cluster-access/access.json --dry-run=client -o yaml | kubectl apply -f -
   kubectl apply -k deploy/k8s
   kubectl -n insurance-core wait --for=condition=complete job/ic-bootstrap --timeout=15m
   kubectl -n insurance-core logs job/ic-bootstrap -c status
   ```

   The bootstrap job forms the Raft groups, opens the first business date and activates the shipped products. Every step is safe to repeat.

7. **Expose the API** through your ingress controller, and label its namespace `insurance-core/ingress=true` so the network policy admits it.

8. **Set up the portal service** (`ic-portal`: the app's API, the portals, the consoles and the status page). Its app registrations, the values in `deploy/k8s/portal.yaml` and its public host are described in `docs/channels.md`, "Running it".

9. **Set up payments** (`ic-payments-pump`: the bank channels and the evacuation drill). The payments vault, the bank keys and credentials, and the values in `deploy/k8s/payments.yaml` are described in `docs/payments.md`, "Running it".

10. **Set up underwriting** (the driver `ic-underwriting` and the intake `ic-underwriting-intake`; `docs/underwriting.md`). Before step 6, deploy its template into the same resource group, with the main deployment's Key Vault, AKS OIDC issuer and Event Hubs namespace:

    ```sh
    az deployment group create -g <rg> -f infra/underwriting.bicep \
      -p name=<name> keyVaultName=<keyVaultName> aksOidcIssuer=<aksOidcIssuer> \
      eventHubsNamespaceName=<eventHubsNamespace>
    az deployment group show -g <rg> -n underwriting --query properties.outputs
    ```

    It creates the clinical store (its own storage account with no soft delete or versioning, deliberately), the HSM wrapping key, the intake's identity, and the `underwriting-notices`, `underwriting-evidence` and `underwriting-delegates` hubs with the workers' right to send to them. Register an app for the intake with the app roles `new-business`, `underwriting-console`, `evidence` and `delegate-api`, and put its application ID URI and the outputs in `deploy/k8s/underwriting-intake.yaml`. The driver reads `IC_EVENT_HUBS_ENDPOINT` from `settings.yaml`.

## Day to day

**Is it healthy?** `GET /readyz` on the API answers 200 with the business date when every partition has a leader. For detail:

```sh
kubectl -n insurance-core create job status-now --from=cronjob/ic-admin
kubectl -n insurance-core logs job/status-now
```

`ic-admin` is a suspended CronJob that exists to be run by hand like this. To run another `ic-admin` command, write the job out with `--dry-run=client -o yaml`, change its `command`, and apply it.

`ic-admin status` prints, for each node and partition: the leader, the last log index, the state hash and the housekeeping backlog. All nodes should agree on the state hash at the same index.

**The daily close** runs from the `ic-close` CronJob at 00:05 New Zealand time. It closes every business day whose midnight has passed, oldest first, so a missed night is caught up by the next run. Check it with `kubectl -n insurance-core get jobs` and read the report in the job's log.

To run a close by hand: `kubectl -n insurance-core create job close-now --from=cronjob/ic-close`.

**If a close fails with reconciliation breaks**, the day stays open and the next day is not started. The job log lists each break (the check, the partition, the amounts). Business continues on the open date. Do not force the date forward: find the cause, and once the books reconcile, run the close again.

**Unit orders** execute from the `ic-unit-orders` CronJob every fifteen minutes, once the unit pricing module has activated the price for their date, and the same job replays orders standing on a corrected price. The close does the same work as one of its steps, so a missed run is caught up at midnight. The job's log prints how many orders it could not move on each partition (`left`), and the close's report carries the same figure as `unit_orders_left`; anything other than zero there is for a person to look at. To run it now: `kubectl -n insurance-core create job unit-orders-now --from=cronjob/ic-unit-orders`.

**Reconciliation at any time:** `GET /v1/admin/reconciliation` (role `operations`).

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

## Cluster access

Every caller of a ledger node presents its own token: the secret `cluster-token-<workload>` in Key Vault, mounted only into that workload's pods. The nodes read the access map (`IC_CLUSTER_ACCESS_FILE`, the ConfigMap `ic-cluster-access`) and hold each workload to its grant: the caller names it may put on a core command, the modules it may write and read, whether it may read the partitions, and whether it may administer the cluster. Only the nodes themselves are peers, reaching Raft and everything else. The map holds the tokens' SHA-256 digests, not the tokens. The code is `crates/raft/src/access.rs`; the grants are `deploy/cluster-access/grants.json`.

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

