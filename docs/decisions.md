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

### Claims stay on the partition they were made on

A policy's claims move with it when it is transferred to another group: they travel in the transfer record and are registered again on the target (changed 12 November 2026; before that a policy with an open claim could not move). Only a claim in a prepared payment run holds a transfer back. The other gaps in claims operations are listed in `docs/claims.md`.

### Fail-back is built; the second region is not provisioned

The fail-back controller runs on every node and is tested with three nodes in one process. The shipped infrastructure is still one region, so `IC_PREFERRED_NODES` is unset in the manifests and nothing fails back in a deployed cluster until voters exist in a second and third region. Pacing is per node: after a region loss the leaders sit on a few nodes, each of which moves its own share per round, so the share moved across the cluster in one round can be up to that share times the number of leading nodes. A pause is not persisted across a node restart. The operations console and status page showing the degraded state belong to the channels module and are not driven from this status yet.

### Group scheme administration

Its gaps are listed in `docs/group.md` under "Known gaps". The ones that matter most today: no test takes a member through a whole underwriting decision with both services up, a member has no way yet to pay a loading billed to them, and its Azure resources (`infra/group.bicep`) are compiled in CI but not yet referenced by `main.bicep`.

### Payments are built against a fake bank

No bank is connected. The payments module, the pump and the drill are complete and tested against a fake bank that checks what a bank checks, in the module's own batch format over HTTPS. A real bank's file layout, transport and mutual TLS are one trait per bank and are not built. `docs/payments.md`, "What is not built", lists this and the module's other gaps.

### A lender's interest in a policy is recorded; some of what it should restrict does not exist yet

`Command::Interest` records an assignee's interest from a notice of assignment (party, date notice arrived, extent, effective date, vault reference), a consent to one restricted action, and a release on the party's discharge. `ic_core::interest::ranked(policy, on)` gives the interests standing on a date, first notice first, for claims to pay in that order. While an interest stands the core refuses a unit-linked withdrawal or surrender payout and a cancellation at the owner's request (caller `policy-servicing`) until every interested party has a recorded consent to that action; the consent is used up by it. Not built: a policy loan and a change of owner do not exist as commands, so there is nothing yet to restrict; an owner's reduction of cover (`ReduceCover`) needs consent, but ending a group excess layer (sent by group administration or underwriting) is not treated as the owner's act; and the claims module does not read the interests yet. The documents module copies lapse, suspension and policy-ended notices to interested parties.

### Unit-linked business is part built

The core's investment ledger holds funds, unit prices, unit orders and unit holdings: a fund and a price are activated through the log, an order waits with its cash in pending investment cash until the price for its date is active, executes once in an entry that balances in dollars and in units, and a switch places its buy when its sell executes. The reconciliation checks each fund's units on issue against what policies hold. What is not built, so no unit-linked product can be sold yet:

- **Price correction after execution** is built in the core and not yet run by anything. A corrected price is taken at any time. `ReplayOrders { superseded, limit, materiality_minor }` (caller `unit-orders`) then puts a chunk of the orders executed at the old price on the new one: the side the customer fixed stays fixed, a policy that is owed gets units (cash if it is no longer in force), a policy given too much has units taken back as far as it holds them and never cash, and what cannot be taken back the insurer pays the fund through the pricing error account (`pricing_error`). Entries are `unit_price_correction` with the pricing date as value date. The `ToReplay` question lists superseded prices with orders still on them. Two limits: cash owed to a policy that has left sits in its pending investment cash until servicing raises a payout for it; and an order of a policy that has since moved partition is not replayed and stays on the `ToReplay` list.
- **Money out** is built as far as the payable. `RaisePayout { policy, payout, amount_minor, reason }` (callers `policy-servicing`, `claims`) moves pending investment cash that no buy order is counting on to the policy's benefit payable, and the payments module settles it with the payout's ID as the payment reference, the same path as a claim. A payment the bank sends back makes the payout owed again until `ReinstructPayout` gives it a new reference. Not built: the servicing workflow that turns a withdrawal or surrender request into the sell orders and the payout, exit charges and tax (they need the unit-linked product), and the change of status to surrendered.
- **Charges in units.** Policy fees and insurance charges as sell orders on a cycle are not written, and there is no unit-linked product with a fund menu and contribution rules. Cash reaches pending investment cash only by an explicit allocation from suspense.
- **The run** is built. One pass asks each partition what is executable (`Executable`) and what stands on a superseded price (`ToReplay`), and sends `ExecuteOrders` and `ReplayOrders` in chunks until a pass changes nothing; the replay threshold is the fund's `compensation_minimum_minor` pricing term. The close does it as a Temporal step before reconciliation, and `ic-worker unit-orders` does the same work between closes on a fifteen-minute schedule (`deploy/k8s/worker.yaml`). The scheduled run is a plain idempotent command, not a Temporal workflow: both send the same keys, so a run that dies is made good by the next one or by the close. Orders a run cannot move are reported in the close's report (`unit_orders_left`) and do not stop the day; nothing alerts on that figure yet.
- **Fund files** are built: a fund is a Starlark file under `products/funds/` loaded by `ic_core::invest::load_fund`, with a hash of its source, and `ic-admin` activates the shipped ones with the rest of the shipped configuration. The three shipped funds (cash, balanced, growth) are synthetic: their spreads, tolerances and compensation minimum are placeholders nobody has approved.
- **Moving partition.** A policy with units, pending cash or pending orders is refused a transfer between groups (`policy_invested`).
- **Opening loads** for migrated holdings.

Unit pricing itself (valuation intake, checks, maker and checker, suspension, price error correction and the holdings extract) is a separate module, `crates/pricing`, with its own driver `ic-unit-pricing`; its gaps are in `docs/unit-pricing.md`.

### Alterations to retail cover are part built

The design lists increase, add a cover, decrease or remove, change of terms, future insurability options and conversion.

- **Built**: an increase accepted by underwriting (`AddUnderwrittenLayer`), a change of underwriting terms on cover held (`AlterUnderwrittenTerms`, caller `underwriting`: each layer on other terms ends and is written again from today with its sum insured, origin and rating date, and the days left of a billed period are billed or credited), and a decrease or removal at the owner's request (`ReduceCover`, caller `policy-servicing`). A reduction takes the newest layer first, ends each layer it touches from today with `LayerEnded`, and replaces a part-cut layer with a smaller one on the same terms and rating date. The premium billed for the days left is a credit adjustment; where the period was paid it becomes a credit in policy suspense, which is refunded by the existing suspense refund and is not paid out automatically. A standing lender's interest must have consented.
- **Reinstatement after a long lapse**: a product may set `reinstatement_evidence_free_days` on its arrears feature. Up to that many days after the lapse, paying the arrears is enough. After it, inside the window, `Reinstate` is refused as `underwriting_required` until underwriting's decision is on the lapse (`ApproveReinstatement`, caller `underwriting`) and still in date. The decision's terms are applied from the reinstatement day as a change of terms, so the missed periods are owed on the old terms. Not set, the whole window is free of evidence, which is how every shipped product is today.
- **Level premium** is priced on each layer's rating date (`Layer.rated_from`, otherwise its start) until the cover's `level_to_age`, then on attained age. No shipped product uses a level premium; it is tested on a copy of term life.
- **Order of reduction** is always newest first. The design lets a product declare another order; no product setting for that exists.
- **A cover given up for every life** stays on the policy with no layer in force, so it prices at nothing and can be taken up again by an underwritten increase. It is not marked ended.
- **Not built**: adding a cover that was not on the policy at issue, future insurability options, and conversion to another product.

### Cover a scheme member pays for is built for underwritten layers only

The design gives voluntary cover its own receivable per member and its own standing, and bills a loading to the member where a total would identify the few members who have one.

- **Built**: an underwritten layer on a scheme can be paid for by the member, in full (`member_pays: all`) or for its loading only (`loading`). That part never reaches the scheme invoice, its loading total, the scheme's balance or its standing. It is a receivable on the scheme policy with the member as payer (`MemberPremiumBilled`). `ApplyMemberReceipt` sets a member's money only against what that member owes; the rest is the member's credit in an account of its own, which pays later bills and is paid back with `RefundMemberCredit`. Each such member has a standing derived from their oldest unpaid bill on the scheme product's arrears rules (`MemberStandingChanged`). Past the lapse period the layers the member pays for end, both payers are credited for the days left, what the member owed is written off and `MemberCoverLapsed` is emitted; the member's employer-paid cover and the scheme are untouched. When the scheme itself lapses or ends, what members paid ahead goes back to their credit, not to the employer.
- **Changing who pays** is ending the layer and adding it again from the same date.
- **Not built**: automatic (formula) cover cannot be member-paid, so dependants' cover paid by the employee is not modelled. A member's lapse does not wait for a proven lapse notice as a policy's lapse does; no notice to the member is raised by the core, and the documents module does not yet act on `MemberStandingChanged`. A member's lapsed cover is not reinstated; it is added again. A scheme holding a member's credit is refused a partition transfer (`member_credit_held`). When a lapsed scheme is reinstated, members' bills closed at the lapse stay closed. Nothing routes a member's payment to `ApplyMemberReceipt` yet: the payments module matches receipts to policies, not to members.
- **Known rounding**: cancelling a policy on which nothing was paid can leave one cent in policy suspense, from the fraction of a cent of unearned premium. It was there before this work and is not fixed.
