# Decisions and known gaps

This is the honest list: what is not production grade yet, and the decisions that are the insurer's to make. Everything not listed here is built, tested and meant to be relied on.

## Open decisions

### Where Temporal runs

The daily close needs a Temporal service. The choices are Temporal Cloud (least to operate; the workflow history, which holds dates and ledger hashes but no personal data, leaves New Zealand) or self-hosted Temporal on the same AKS cluster with Azure Database for PostgreSQL (everything stays in the region; more to run). The Bicep template can create the PostgreSQL server (`deployPostgres`), but does not deploy Temporal itself. Until this is decided, `deploy/k8s/settings.yaml` has a placeholder for the address.

### Encryption in transit inside the cluster

Traffic between the API, the workers and the ledger nodes is HTTP carrying the cluster token. Network policies restrict who can connect, but do not encrypt. Before production the cluster needs pod-to-pod encryption: a service mesh with mutual TLS (Istio add-on for AKS) or transparent encryption in the CNI (Cilium WireGuard). This is a platform choice, so it is not baked into the manifests.

### A second region

Three ledger nodes across the zones of New Zealand North survive the loss of a zone. Surviving the loss of the region needs voters in two more regions (five voters across three regions keeps a majority after losing any one). The software supports it: nodes are addressed by name and membership is changed with `ic-admin members`. The infrastructure for it is not written, and it means policy data leaving New Zealand, which is a decision for the insurer and its regulator.

## Known gaps

### Callers have roles but no relationships

The API takes the caller's roles from the Entra ID token. What a customer or adviser may see depends also on their relationship to a policy (policyholder, life insured, adviser of record), which belongs to a customer master that is not part of this system. Until one is connected, a caller with no staff role sees nothing. Staff roles work fully.

### Rotating the cluster token needs a rolling restart

Nodes accept a list of tokens, so rotation is safe: add the new token, restart, switch callers, remove the old one. But processes read the token when they start, so each step is a rolling restart. See the runbook.

### Policy-number lookups ask every partition

Finding a policy by its number, and matching a payment by reference, asks all 64 partitions in parallel. That is correct and fast enough at the scale tested, and it is the first thing to replace (with a small directory service fed from Event Hubs) if lookup volume grows.

### Single-process mode is for development

`ic-api` without `IC_NODES` runs everything in one process with no replication. It keeps its ledger on disk and re-sends unpublished events after a restart, but a Merkle root that could not be published at the close is reported and not retried, and there is one copy of the data. Do not run it in production.

### Built beside the core

Claims, underwriting, reinsurance, group administration, commission and held balances, payments, documents and correspondence, the finance ledger and the portal services are modules in their own crates. `docs/modules.md` is the contract between them and the core. Each module records its own gaps.

### Unwinding has limits

A retail policy can be unwound at most 365 days back, because settled receivables older than 400 days are no longer in the policy's state (they are in the journal). An older correction is a remediation case worked from the journal. A scheme policy is never unwound as a whole; it is corrected through its membership file by effective date.

### Reopening covers the choices the core holds

Two choices can be made again under a reopen reference: a decline of indexation and a cancellation. Reductions of cover, frequency changes and premium type switches are not servicing commands in the core yet, so there is nothing of theirs to reopen; when they are added they take the same path. Only the latest anniversary's decline is put right retroactively, and only within 365 days. The policy records which anniversaries passed with indexation declined, but no product rule yet takes the right to indexation away after repeated declines, so that record is not acted on.

### A module added to a running cluster starts from what the partitions still hold

A module hosted from the first start is shown every event. One added later starts at the oldest event each partition still holds. Loading a new module from Event Hubs or the cold tier is not built.

### Fail-back is built; the second region is not provisioned

The fail-back controller runs on every node and is tested with three nodes in one process. The shipped infrastructure is still one region, so `IC_PREFERRED_NODES` is unset in the manifests and nothing fails back in a deployed cluster until voters exist in a second and third region. Pacing is per node: after a region loss the leaders sit on a few nodes, each of which moves its own share per round, so the share moved across the cluster in one round can be up to that share times the number of leading nodes. A pause is not persisted across a node restart. The operations console and status page showing the degraded state belong to the channels module and are not driven from this status yet.

### Group scheme administration

Its gaps are listed in `docs/group.md` under "Known gaps". The ones that stop a scheme working end to end today: underwriting does not yet receive requests for cover above the automatic amount, the reinsurance module is not hosted so its notices wait, no customer master exists to match joiners on named schemes, and the group service's Azure resources (`infra/group.bicep`) are not yet referenced by `main.bicep` or compiled in CI.
