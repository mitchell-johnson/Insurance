# The ledger's two main regions

The ledger's five voters (docs/decisions.md, "Where the ledger's voters run"):

| Voter | Where | Address |
|---|---|---|
| ic-node-1, ic-node-2 | Australia East (Sydney), the home cluster | 10.40.6.4, 10.40.6.5 |
| ic-node-3, ic-node-4 | Australia Southeast (Melbourne), the second cluster | 10.41.6.4, 10.41.6.5 |
| ic-witness (5) | Australia Central, a VM with no ledger data | 10.42.0.10 |

Each cluster is applied from its own overlay:

    kubectl --context <home cluster>   apply -k deploy/regions/australiaeast
    kubectl --context <second cluster> apply -k deploy/regions/australiasoutheast

Both are `deploy/k8s` plus one internal load balancer per voter, at a fixed
address in the region's `ledger-peers` subnet, so that the voters in the
other cluster and the witness reach each pod directly (the clusters use an
overlay pod network, so a pod's own address is not routable from outside).
`infra/regions.bicep` names those addresses in the `ledger.internal` zone,
and `tools/check_regions.py` (run in CI) checks that the addresses here, the
zone and the ledger settings agree.

The second cluster runs everything the home cluster runs except the one-off
`ic-bootstrap` Job, which forms the Raft groups once, from home.
