# Operations

## First deployment

1. **Create the platform.**

   ```sh
   az group create -n <rg> -l newzealandnorth
   az deployment group create -g <rg> -f infra/main.bicep -p infra/main.bicepparam
   az deployment group show -g <rg> -n main --query properties.outputs
   ```

   Keep the outputs: they fill in the manifests.

2. **Set the cluster token.** It is the shared secret between the API, the workers and the ledger nodes. Nobody needs to know it.

   ```sh
   az keyvault secret set --vault-name <keyVaultName> --name cluster-token --value "$(openssl rand -hex 32)"
   ```

3. **Build and push the image.**

   ```sh
   az acr build -r <registry> -t insurance-core:$(git rev-parse --short HEAD) .
   ```

4. **Register the API with Entra ID.** Create an app registration for the API, define the app roles callers will hold (`new-business`, `policy-servicing`, `cash-matching`, `group-admin`, `claims`, `collections`, `config`, `operations`), and note its application ID URI.

5. **Fill in `deploy/k8s`.** Replace every `REPLACE-` value in `kustomization.yaml`, `identities.yaml`, `settings.yaml` and `bootstrap.yaml`. The first business date in `bootstrap.yaml` is set once and can never be changed.

6. **Apply.**

   ```sh
   az aks get-credentials -g <rg> -n <aksName>
   kubectl apply -k deploy/k8s
   kubectl -n insurance-core wait --for=condition=complete job/ic-bootstrap --timeout=15m
   kubectl -n insurance-core logs job/ic-bootstrap -c status
   ```

   The bootstrap job forms the Raft groups, opens the first business date and activates the shipped products. Every step is safe to repeat.

7. **Expose the API** through your ingress controller, and label its namespace `insurance-core/ingress=true` so the network policy admits it.

8. **Set up the portal service** (`ic-portal`: the app's API, the portals, the consoles and the status page). Its app registrations, the values in `deploy/k8s/portal.yaml` and its public host are described in `docs/channels.md`, "Running it".

9. **Set up payments** (`ic-payments-pump`: the bank channels and the evacuation drill). The payments vault, the bank keys and credentials, and the values in `deploy/k8s/payments.yaml` are described in `docs/payments.md`, "Running it".

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

## Rotating the cluster token

Nodes accept any token in a comma-separated list; callers send the first.

1. Set the Key Vault secret to `new,old` and restart the ledger nodes (`kubectl rollout restart statefulset/ic-node`). Nodes now accept both.
2. Restart the API and the workers. They now send `new`.
3. Set the secret to `new` and restart the ledger nodes again.

Each restart is a rolling one and the cluster stays writable throughout.

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

