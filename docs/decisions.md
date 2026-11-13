# Decisions and known gaps

This is the honest list: what is not production grade yet, and the decisions that are the insurer's to make. Everything not listed here is built, tested and meant to be relied on.

## Open decisions

### Where Temporal runs

The daily close needs a Temporal service. The choices are Temporal Cloud (least to operate; the workflow history, which holds dates and ledger hashes but no personal data, leaves New Zealand) or self-hosted Temporal on the same AKS cluster with Azure Database for PostgreSQL (everything stays in the region; more to run). The Bicep template can create the PostgreSQL server (`deployPostgres`), but does not deploy Temporal itself. Until this is decided, `deploy/k8s/settings.yaml` has a placeholder for the address.

### Encryption in transit inside the cluster

Traffic between the API, the workers and the ledger nodes is HTTP carrying the cluster token. Network policies restrict who can connect, but do not encrypt. Before production the cluster needs pod-to-pod encryption: a service mesh with mutual TLS (Istio add-on for AKS) or transparent encryption in the CNI (Cilium WireGuard). This is a platform choice, so it is not baked into the manifests.

### A second region

Three ledger nodes across the zones of New Zealand North survive the loss of a zone. Surviving the loss of the region needs voters in two more regions (five voters across three regions keeps a majority after losing any one). The software supports it: nodes are addressed by name and membership is changed with `ic-admin members`. The infrastructure for it is not written, and it means policy data leaving New Zealand, which is a decision for the insurer and its regulator.

### Questions the persona tests raised that the design does not answer

The persona test pack (`docs/testing/personas/`) holds a test open on purpose for each of these until the insurer decides:

1. How sex is captured as a rating factor for a life whose recorded sex is neither option a rate table holds (`PRD-011`).
2. Whether a payee may decline late-payment interest on a claim, and how that posts (`CLM-037`).
3. Whether a customer may set their own hold on refunds and surrenders, with a cooling-off before it can be removed (`SVC-022`).
4. How an insolvency official's claim on a policy's surrender value is recorded and enforced in a bankruptcy (`JNT-029`).
5. How imprisonment affects an income protection claim in payment (`CLM-040`).

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

Its gaps are listed in `docs/group.md` under "Known gaps". The ones that matter most today: a member has no way yet to pay a loading billed to them, and its Azure resources (`infra/group.bicep`) are compiled in CI but not yet referenced by `main.bicep`.

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
- **Reinstatement after a long lapse**: a product may set `reinstatement_evidence_free_days` on its arrears feature. Up to that many days after the lapse, paying the arrears is enough. After it, inside the window, `Reinstate` is refused as `underwriting_required` until underwriting's decision is on the lapse (`ApproveReinstatement`, caller `underwriting`) and still in date. The decision's terms are applied from the reinstatement day as a change of terms, so the missed periods are owed on the old terms. Not set, the whole window is free of evidence. Shipped term life allows 60 days, health plan 30, group life is not set; the reasons are in underwriting.md.
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

### Non-disclosure remedies in the core

The underwriting module decides the remedy on an approved case; the core carries it out with two commands from caller `underwriting`.

- **`AvoidPolicy { policy, case, premium }`** on a retail policy in force: status `Avoided`, every layer ends on the day it started, `PolicyAvoided` is emitted. With `refund`, every premium billed since inception is undone (earned, unearned, GST and levies, including receivables long since pruned from state, whose totals are kept in `policy.pruned`) and what was paid goes to policy suspense; it is paid out by the ordinary suspense refund, not automatically. With `retain`, nothing goes back: premium paid but not yet earned goes to the account role `avoidance_premium_retained`, tax collected stays collected and what was owed is written off. The design notes give no rule for which applies and ask for legal review, so the case carries the choice.
- **`VaryForNonDisclosure { policy, case, terms }`**: every layer the life has on the cover reads as written on the varied terms from its start; `policy.variations` keeps what each was written on; nothing already billed is billed again and later periods price on the new terms. `TermsVaried` names the layers and not the terms.
- **Limits**: neither applies to a scheme (`not_retail`). An avoidance is refused while a benefit is owed on the policy (`claim_in_payment`), while it holds units (`policy_invested`), or if any of its billing was closed earlier (`avoidance_needs_review`). Benefits already paid are not recovered by the core; that is a claims case. An avoidance cannot be set aside by a command: a court order restoring the policy would be worked by hand. Policies whose receivables were pruned before `policy.pruned` existed would under-refund; there are none, since nothing is in production.

## The unit-linked product and its fund menu (13 Nov 2026)

- **A unit-linked policy is a policy with an investment account cover.** The shipped one is `invest-plan`. Its cover is rated from a table whose rate is zero, so the billing, earning and arrears features run and find nothing to do. What the owner pays in is a contribution: a receipt into suspense, then cash set aside for investment, then buy orders. It is never premium and is never earned.
- **Money goes only into a fund on the product's menu.** Setting cash aside and buying are refused on a policy whose product has no investment account, and a buy or a switch into a fund off the menu is refused. Before this a term life policy could hold units.
- **Selling is never held to the menu**, so a fund taken off a later product version can still be left. A fee rebate may go back into the fund the fee came from, on the menu or not.
- **A full surrender is two steps**, because units are sold at a price that does not exist yet. The first sells everything and shuts the policy to new money; the second, once every sell has executed and no units are left, raises all the cash to benefit payable in one payout and closes the policy as surrendered. A sell that was cancelled or failed is placed again before the second step is accepted. Interested parties consent once, to the first step. No surrender charge is taken: the product language does not yet declare one.
- **The menu is the product version the policy is on.** A new version with a different menu applies to a policy when the policy moves to that version, like any other product term.

## Arrangements to clear overdue premium (13 Nov 2026)

- **Collections agrees an arrangement; the core keeps and watches it.** `MakeArrangement` (caller `collections`) records instalments that add up exactly to the premium overdue that day. The dunning sequence, the conversation and the decision are the collections module's.
- **While it is kept, standing is held where it stood and the policy does not lapse.** No new standing was added: a policy that was suspended when the arrangement was made stays suspended, so cover for new events comes back when the overdue premium is cleared and not when the promise is made. This is a choice, and the one to revisit if the business wants cover restored on the first instalment.
- **Kept means** every instalment whose day has closed has been paid, and premium billed since the arrangement was made is not past the product's grace period. Money is set against the oldest premium first, so what counts is the net: being ahead on the old premium offsets being behind on the new by the same amount.
- **Complete** when the premium the arrangement was for is cleared, early or on time. Standing is then derived in the ordinary way.
- **Broken** on the day after a missed instalment, or the day collections ends it (`EndArrangement`). The lapse clock then reads today less the days the arrangement ran, for as long as premium from before it is unpaid. A second broken arrangement adds its days to the first.
- **Not handled**: an arrangement left open on a policy that is then cancelled stays recorded as active and is never looked at again.
- **`ReceiptApplied` carries the statutory fund** the receipt's postings were stamped with, so payments can add receipts up by fund and move cash between the funds' bank accounts.

## Hardship overrides (13 Nov 2026)

- **A hardship override is a share of the premium for a dated period**: `GrantHardship { from, to, share_bps }` (caller `collections`), where a share of zero is a premium holiday. It starts today or later, never in the past, and runs at most two years. The application, assessment and decision are the collections module's; the core applies what was decided.
- **Billed, and earned, at the share.** A period is billed with the days the override covers at its share and the rest in full. The days left of the period already billed are credited when it is granted. Because the receivable is what is earned, a paused premium earns nothing, and commission on premium received follows without a rule of its own.
- **Cover carries on in full.** The notes allow a product to reduce cover during a holiday; no product declares that yet, so none does.
- **Ending early charges forward only**: `EndHardship` brings the end to today, charges the rest of the period already billed back at the full premium, and bills in full from then on. Days behind it stay as billed. One ended before its first day never applies.
- **One at a time**, and the latest is kept on the policy. Retail policies only: a scheme's premium is refused (`not_retail`).
- **Not handled**: a cover reduced or increased during an override has its adjustment for the days left worked out at the full premium, so that one adjustment is too large by the override's reduction. A policy fee is reduced with the premium.

## Rate table validation is recorded, not enforced (13 Nov 2026)

- **The core records that a rate table version passed validation and does not gate activation on it.** `RecordRateTableValidation` (caller `config` or `finance`) stores the report hash, schema version and who ran it against the table's content hash, and publishes `RateTableValidated`. Finance, which runs the checks, turns a close check red for any table activated without a matching record. Agreed with finance: the core does not depend on the finance crate, and the shipped test configuration activates tables with no report.
- **A record is tied to content.** One for a version that is active under another content hash is refused. A version can be validated before it is activated.
- **Loadings stay off the event feed.** Finance asked for a layer's total loading on the published layer facts so its table dry run can price what is billed. Loadings imply health, so they are not published; a question answered to finance alone will give them instead (not built yet).

## The mix-and-match health product, first slice (13 Nov 2026)

- **A shell product declares its menu and its combination rules.** The product feature `composition(core, requires, exclusive)` says which sets of covers one life may hold: at least one core cover, every cover its other covers require, at most one of each exclusive set. `products/health_flex.star` (code `health-flex`) is the shipped one: hospital, specialist, dental and optical, each with its own schedule and rate table, specialist only with hospital, and a bundle discount from three covers. `health-plan` is unchanged.
- **The rules hold per life**, so a family policy gives each person their own set, and one person's hospital cover does not stand in for another's. They are checked at issue and when a cover is given up (`ReduceCover` to nothing, code `composition`). Giving up a cover priced per life, which has no sum insured, now ends its layers; before, it was refused.
- **Premium is the sum of the chosen covers** under the product's discount, each cover and its tax rounded on its own line. A test issues every subset of the menu and checks it.
- **Not built, in the order intended**: adding a cover after issue (with its own start date and waiting periods); excess options and other per-cover choices as rating inputs; waiting periods and counters per cover per life, and shared counters (claims holds the counters); the discount chain as pricing components with entitlements and overrides (today the one discount is `multi_cover_discount`, counted across the policy and not per life); a child pricing cap, dependants added and aged off; premium lines per cover on the notice (the billing event groups by cover type, and every health cover has the same type); canonical bundles as named presets.
- **Questions for Mitchell, none blocking this slice**: the real menu, limits and combination rules are product decisions and the shipped ones are placeholders; whether the bundle discount counts covers per life or across the policy.

## Nominations and the continuation link (13 Nov 2026)

- A nomination for a death benefit is kept on the policy with the date it takes effect and the business date it was recorded. Nothing is edited or deleted: a change is a new nomination, and a removal is a nomination that names nobody. `nomination::in_force` answers who was nominated on a date, as the record stands now or as it stood on an earlier day. A nomination for one life wins over a policy-wide one for that life.
- Command `Nominate`, caller `policy-servicing`. Shares are whole percent and add up to 100, at most 20 people, effective between the policy's start and today. The event `NominationRecorded` carries the party IDs and shares.
- The conversation the notes ask for before a vulnerable customer changes a beneficiary is decided by servicing, which knows the customer. The core cannot see the marker, so it does not enforce the pause; it keeps the conversation's reference where one was held. Not built: the servicing pause itself (hand-over item 10).
- Claims reads the nominees for the event date through `cover_facts`, after interests.
- `IssuePolicy.continuation_of { policy, life }` records the scheme membership a continuation policy carries on from. Only caller `group-admin` may set it. The scheme may be on another partition, so the core records the link and does not check the scheme; scheme administration is trusted for it.
