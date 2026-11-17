# Decisions and known gaps

This is the honest list: what is not production grade yet, and the decisions that are the insurer's to make. Everything not listed here is built, tested and meant to be relied on.

## Open decisions

### Where Temporal runs

The daily close needs a Temporal service. The choices are Temporal Cloud (least to operate; the workflow history, which holds dates and ledger hashes but no personal data, leaves New Zealand) or self-hosted Temporal on the same AKS cluster with Azure Database for PostgreSQL (everything stays in the region; more to run). Both are templated. For Temporal Cloud, set `TEMPORAL_ADDRESS` and `TEMPORAL_NAMESPACE` in `deploy/k8s/settings.yaml` and add `TEMPORAL_API_KEY` from a Key Vault secret. For self-hosted, `infra/main.bicep` with `deployPostgres` and `selfHostedTemporal` makes the server, its two databases, Temporal's one password login (Temporal cannot sign in with Entra ID) in Key Vault and an identity that alone reads it, and `deploy/temporal` holds the Helm values and secrets for the official chart. Until this is decided, `settings.yaml` has a placeholder for the address.

### Encryption in transit inside the cluster

Traffic between the services inside the cluster is HTTP carrying bearer tokens (the cluster token, Entra tokens). Network policies restrict who can connect; WireGuard encrypts. `infra/main.bicep` turns on Advanced Container Networking Services with `transitEncryption: WireGuard` on the Cilium dataplane, so every pod-to-pod packet that leaves a node is encrypted. Traffic between two pods on the same node never leaves it and is not encrypted; traffic from host-network pods and the nodes themselves is not covered (none of ours run on the host network). WireGuard needs UDP 51871 open between nodes. Turning it on for a running cluster restarts the Cilium agent on every node: do it in a quiet hour. Check it with `kubectl -n kube-system exec ds/cilium -- cilium-dbg encrypt status` (WireGuard, peers = nodes - 1). WireGuard was chosen over a service mesh because it needs nothing in the manifests and adds no sidecar to the ledger's latency path.

### Log retention (placeholder)

Container logs and the traces in the workspace are kept 90 days for querying and seven years in all (`logInteractiveDays`, `logTotalRetentionDays` = 2,556 in `infra/main.bicep`), the rest in the archive tier, where a search job or restore brings them back. Seven years is a best estimate of the audit need: the cluster access log and the reinsurer API log are in these tables, and seven years matches the other records the system keeps. Confirm with compliance; the cost is the archive tier's per-GB rate on what is ingested. Records with a fixed legal period are not left to this: they are in write-once storage.

### Five questions the persona tests raised: decided

Mitchell delegated these to the build on 13 Nov 2026 ("You decide those questions"). Each is decided below with its reasoning, so that he, compliance or legal can revisit it. Where a reasonable insurer might choose differently, the choice is a product or system setting and the default here is the one that protects the customer. The persona test that holds each one is named in brackets.

**1. Sex as a rating factor for a life who is neither male nor female (`PRD-011`).** The life record keeps gender (what the person tells us, any value, never used in pricing) apart from a rating sex factor. Where a table rates by sex and the life's recorded sex is not one the table holds, the life is rated at the lower of the table's male and female rates. Nobody is asked to pick a category that is not theirs, and nobody is charged more for not doing so. The basis is a product setting (`unmatched_sex_rating`: `lower_of` by default, or `blended` at a stated mix) so an actuary can change it with the usual approvals. Reinsurance rates the same life on the same basis, so the treaty rate is found and never guessed. Why: sex-based pricing is lawful only where it rests on data it is reasonable to rely on, and no table holds data for this group. The lower rate is the one that cannot be unfair to the customer, and the cost is small and measurable. Built: the setting is on each cover's rating in the product file (`stepped("life-base", unmatched_sex_rating = blended("0.5"))`; left out, it is `lower_of`), the customer master keeps gender, and reinsurance carries the same setting on its rate tables.

**2. A payee who declines late-payment interest (`CLM-037`).** Interest is always worked out and offered; the payee is told the amount. A payee may decline it, but only after being told, never as a condition of the claim, and never by a default on a form. A declined amount is recorded with the payee's signed or recorded decision and reversed by its own posting with the reason `declined_by_payee`, so it stays visible in the claims and conduct reports. A payee may change their mind until the claim closes. Why: the interest is the insurer's obligation for its own delay. Letting a payee refuse money owed to them respects their values; making it the payee's informed choice keeps the insurer from benefiting from a waiver it suggested.

**3. A hold the customer sets on their own money going out (`SVC-022`).** Exists. A customer may place a hold on refunds, surrenders, withdrawals, policy loans and changes to their payout account. Placing it takes effect at once. Removing it needs fresh identity verification and then waits out a cooling-off period before it ends (system setting, default 7 days); the customer can cancel the removal in that time. Staff cannot lift it on the customer's behalf; only a court order or a legal direction can override it, and that is recorded. Premium collection and claim payments to third parties the customer has already nominated are not held. Why: it is a protection a vulnerable customer asks for themselves (gambling, a scam, a bad night), it costs the insurer nothing, and the cooling-off is what makes it worth having. Built in three places: the customer master owns the hold, its verification and its cooling-off; payments holds the money where it leaves; and the core refuses the request itself (`customer_hold` on a withdrawal, a surrender and a refund of a credit) on each policy the master has told it about with `SetOwnerHold`, which only the caller `customer-master` may send.

**4. A policyholder's bankruptcy (`JNT-029`).** Recorded as an interest, not an ownership change: when the Official Assignee gives notice of adjudication, an insolvency interest is recorded on each policy the bankrupt owns, ranked by when the notice arrived, like a lender's. While it stands, nothing the owner asks for that would release value goes ahead (surrender, withdrawal, loan, change of ownership, change of payout account, reduction) without the Official Assignee's consent. Cover, premiums and claims carry on. Money that becomes payable while the interest stands (a surrender value or a claim) is held in benefit payable and paid only on a legal direction saying to whom. The interest ends on discharge or on the Official Assignee's release. Why: whether a policy's value vests in the Official Assignee depends on the kind of policy and on statutory protections that legal must apply case by case. The system's job is to stop value leaving until that is decided, without ending cover the person may still need. Built: `InterestCommand::Record` with `kind: insolvency` (whole proceeds only), the usual consent and release, payouts raised while it stands held (`PayoutHeld`, nothing sent to payments), and `InvestCommand::DirectPayout` from policy-servicing naming the payee and the direction's vault reference (`PayoutDirected`, which payments takes in like any payout). Claims sees the interest in its cover facts with `insolvency: true`; holding claim money on it is for the claims module to build. A change of payout account is the customer master's and payments'; they must check the policy's interests before taking one.

**5. Imprisonment during an income protection claim (`CLM-040`).** Decided by the product's wording through a product setting (`benefit_while_imprisoned`: `suspend` or `pay`). The shipped income protection product suspends the monthly benefit for whole days in custody after sentence, with remand counted only if the person is later convicted. While suspended, the claim stays open, premium waiver continues, and the review timers pause. On release the benefit resumes from the day after, with no new waiting period, if the person is still disabled. A suspension needs evidence and a recorded conversation (with the person's representative where they have one) and is a decision with its own reason code, never a decline. Why: the benefit replaces income lost to disability, and in custody income is lost to imprisonment instead. Keeping the claim, the waiver and the cover whole means the person leaves custody insured. Built: `monthly_benefit(waiting_days, benefit_months, benefit_while_imprisoned = "suspend")` in a product file, the shipped `products/income_protection.star` (28 days' wait, 60 months, suspend), and the cover check passing the waiting period and the setting to claims, which enforces them.

### Withholding tax: the core posts it, the payer says how much

The design posts tax on claim payments and investment earnings at the event, at the rate the customer master holds. The core does not know rates or people, so the module that pays says how much to withhold and the core posts it: out of benefit payable to `withholding_tax_payable/{kind}` in its own entry (`tax_withheld`, posting type `withholding_{kind}`), and the bank is asked for the rest. Withholding on commission is built in the commission module as a placeholder (schedular payments, the payee's declared rate, 45% for a person with no declaration, businesses not withheld from; see docs/distribution.md), posting type `withholding_schedular`. Built for the two payments that exist: late-payment interest on a claim (`Instruction::Pay.interest_withheld_minor`, kind `rwt`) and an income benefit period (`Instruction::PayBenefit.tax_withheld_minor`, kind `benefit`). Finance's chart has account 2220 and the tax reconciliation lists its posting types. Not built, each waiting on the tax review the design calls for: claims working out the amount from the payee's rate (claims), tax deducted from units on investment-linked policies (core, order kind `tax_deduction` exists), and annuities (not built at all). The remittance to the revenue authority is built (14 Nov 2026): payments prices it from finance's `withholding_due` question and each partition's share by fund (`crates/payments/src/tax.rs`), and the core discharges it per partition with `RemitWithholding`, put back by `ReturnRemittance` if the bank returns it (`crates/core/src/remittance.rs`, posting types `withholding_remittance_{kind}`, under 2220 in `config/finance/close.star`). The split by partition is a best estimate until the core can answer by month.

## Known gaps

### The core API is for staff and services; people come through the portal

Decided 14 Nov 2026, replacing "Callers have roles but no relationships", which was written before the customer master and the portal existed.

What a customer, adviser or employer may see depends on their relationship to a policy (policyholder, life insured, member, adviser of record, administrator). Those relationships are held by the customer master (`crates/customers`) and relayed to the portal's directory (`crates/channels/src/directory.rs`), which also knows which sign-in is which party, follows the core's own events, and applies a revocation ahead of everything else. The portal checks every row it shows against the same audience policy the core API uses (`ic_readmodel::scope::AudienceScope`).

The core API (`crates/api`) therefore does not ask the customer master about anybody. It takes roles from the Entra ID token and serves staff and services. A token with no staff role sees nothing there, and a policy it may not see answers exactly as one that does not exist. That is the intended behaviour in production and not a gap to fill: a second place deciding who is related to whom would have to be kept in step with the directory, and its revocations would lag the portal's.

Development mode (`Auth::Dev`) can assert relationships in a header, so the audience policy can be exercised through the core API without a portal. Entra mode never reads that header.

### Each workload has its own cluster token

Every caller of a ledger node used to present one shared token, which let any of a dozen workloads send any command to any module under any caller name; the core's check of who may send a command trusted that name. Now each workload has its own token and the nodes hold it to a grant (`crates/raft/src/access.rs`, docs/operations.md "Cluster access"): the caller names it may use on core commands, the modules it may write and read, partition reads, administration. Within a module, the person a command names (an approver, an actor) is still the calling workload's word; the module's own rules (two different people, authority limits) are applied to it. Tokens, not Entra workload identity, because the nodes must keep running when Entra cannot be reached, and a token check needs nothing outside the node. The shipped grants start in report mode and are enforced once a day's logs show nothing they would refuse.

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

### What losing a region still costs

The ledger carries on through the loss of any one region ("Where the ledger's voters run"), but three things it depends on live in Sydney alone and wait for Azure's own failover: the ledger storage account (sealing old log segments and publishing the day's root pause until its RA-GZRS copy in Melbourne is made writable; nothing is lost, because the outputs wait in the replicated state), Event Hubs (events wait the same way until the geo-replicated namespace is promoted; prod only), and the PostgreSQL databases behind the customer master and Temporal (zone redundant, with geo-redundant backups, not a standby in Melbourne). Fail-back pacing is per node: after a region loss the leaders sit on two nodes, each moving its own share per round. A pause is not persisted across a node restart. Raft traffic between regions travels Microsoft's backbone, which Azure encrypts between its datacentres; it is not additionally encrypted by the nodes (see "Encryption in transit inside the cluster").

### Group scheme administration

Its gaps are listed in `docs/group.md` under "Known gaps". The ones that matter most today: a member has no way yet to pay a loading billed to them, and its Azure resources (`infra/group.bicep`) are compiled in CI but not yet referenced by `main.bicep`.

### Payments are built against a fake bank

No bank is connected. The payments module, the pump and the drill are complete and tested against a fake bank that checks what a bank checks, in the module's own batch format over HTTPS. A real bank's file layout, transport and mutual TLS are one trait per bank and are not built. `docs/payments.md`, "What is not built", lists this and the module's other gaps.

### A lender's interest in a policy is recorded; some of what it should restrict does not exist yet

`Command::Interest` records an assignee's interest from a notice of assignment (party, date notice arrived, extent, effective date, vault reference), a consent to one restricted action, and a release on the party's discharge. `ic_core::interest::ranked(policy, on)` gives the interests standing on a date, first notice first, for claims to pay in that order. While an interest stands the core refuses a unit-linked withdrawal or surrender payout and a cancellation at the owner's request (caller `policy-servicing`) until every interested party has a recorded consent to that action; the consent is used up by it. A policy loan needs every interested party's consent (`RestrictedAction::Loan`, `crates/core/src/loan.rs`). Not built: a change of owner does not exist as a command, so `ChangeOwner` has nothing yet to restrict; an owner's reduction of cover (`ReduceCover`) needs consent, but ending a group excess layer (sent by group administration or underwriting) is not treated as the owner's act; and the claims module does not read the interests yet. The documents module copies lapse, suspension and policy-ended notices to interested parties.

### Unit-linked business is part built

The core's investment ledger holds funds, unit prices, unit orders and unit holdings: a fund and a price are activated through the log, an order waits with its cash in pending investment cash until the price for its date is active, executes once in an entry that balances in dollars and in units, and a switch places its buy when its sell executes. The reconciliation checks each fund's units on issue against what policies hold. What is not built, so no unit-linked product can be sold yet:

- **Price correction after execution** is built in the core and run by the unit order job (`ic-unit-orders` every fifteen minutes, and the close's unit order step, `crates/workflows/src/steps.rs`). A corrected price is taken at any time. `ReplayOrders { superseded, limit, materiality_minor }` (caller `unit-orders`) then puts a chunk of the orders executed at the old price on the new one: the side the customer fixed stays fixed, a policy that is owed gets units (cash if it is no longer in force), a policy given too much has units taken back as far as it holds them and never cash, and what cannot be taken back the insurer pays the fund through the pricing error account (`pricing_error`). Entries are `unit_price_correction` with the pricing date as value date. The `ToReplay` question lists superseded prices with orders still on them. Two limits: cash owed to a policy that has left sits in its pending investment cash until servicing raises a payout for it; and an order of a policy that has since moved partition is not replayed and stays on the `ToReplay` list.
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
- **Built since**: adding a cover that was not on the policy at issue (`AddCover`) and future insurability options (`ExerciseOption`), both from callers `underwriting` and `policy-servicing`. **Not built**: conversion to another product.

### Cover a scheme member pays for is built for underwritten layers only

The design gives voluntary cover its own receivable per member and its own standing, and bills a loading to the member where a total would identify the few members who have one.

- **Built**: an underwritten layer on a scheme can be paid for by the member, in full (`member_pays: all`) or for its loading only (`loading`). That part never reaches the scheme invoice, its loading total, the scheme's balance or its standing. It is a receivable on the scheme policy with the member as payer (`MemberPremiumBilled`). `ApplyMemberReceipt` sets a member's money only against what that member owes; the rest is the member's credit in an account of its own, which pays later bills and is paid back with `RefundMemberCredit`. Each such member has a standing derived from their oldest unpaid bill on the scheme product's arrears rules (`MemberStandingChanged`). Past the lapse period the layers the member pays for end, both payers are credited for the days left, what the member owed is written off and `MemberCoverLapsed` is emitted; the member's employer-paid cover and the scheme are untouched. When the scheme itself lapses or ends, what members paid ahead goes back to their credit, not to the employer.
- **Changing who pays** is ending the layer and adding it again from the same date.
- **Not built**: automatic (formula) cover cannot be member-paid (a contributory scheme is not in the design notes); dependants' cover paid by the employee is built as member-paid menu cover on the dependant, which is what the notes describe. A member's lapse waits for a proven lapse notice recorded under purpose `member_lapse:<life>` (14 Nov), as a policy's lapse does; no notice to the member is raised by the core; the documents module issues the member's own lapse notice and reinstatement letter from `MemberStandingChanged` (`crates/documents/src/triggers.rs`). A member's lapsed cover can be reinstated inside the product's window by paying what was written off and the periods since (`ReinstateMemberCover`, 14 Nov); after the evidence-free period it is added again. A member's credit moves with the scheme on a partition transfer (14 Nov). When a lapsed scheme is reinstated, members' bills closed at the lapse stay closed. Nothing routes a member's payment to `ApplyMemberReceipt` yet: the payments module matches receipts to policies, not to members.
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
- **A cover can be added after issue** (`AddCover`, callers `policy-servicing` and `underwriting`, `underwritten.rs::add_cover`). The life gets its own layer from today; a cover nobody on the policy had joins the policy today with wording versions bound as at today; the combination rules are checked on what the life would hold; the days left of a billed period are billed at once as the whole policy's price with the cover less its price without it, both over every layer in force today, so a bundle discount the cover brings in is in the difference and is not counted twice. The cover check for a claim now gives the life's own start on the cover, so waiting periods run from there. Not handled: a life joining a cover someone else on the policy already had gets that cover's original wording versions, not today's; the core does not decide whether an addition needs underwriting (both callers may send it, and the product does not yet say which covers need evidence when added later); `AddUnderwrittenLayer` still prices its difference from the layers at the period's start, which is right for sums insured and would miss a count discount.
- **Excess levels** (13 Nov). A cover may declare `excess_options` (`excess_option(code, excess, premium_factor)`); `health-flex` hospital offers nil, standard and high. Where a cover has them, every layer carries one (`Layer.excess_option`), chosen at issue or when the cover is added; a cover without them refuses one (code `excess_option`). The level multiplies that cover's premium, loadings included. `ChangeExcess` (caller `policy-servicing`) ends the life's layers on the cover today and carries them on as new layers at the new level, billing or crediting the days left; the old layers keep their level, and the cover check for a claim reports the level held on the event date (`CoverFacts.chosen_excess_minor`), which claims is to use in place of the schedule's excess. More of the same cover (`AddUnderwrittenLayer`) and indexation steps take the level the life already holds. Not handled: a rule that a move to a lower excess needs underwriting or a waiting period (any offered level is accepted at once); one excess per policy as an exclusive choice across covers; the placeholder levels and factors are not anyone's approved prices. Adding the options changed the prelude, so every product and rate table hash changed.
- **Not built, in the order intended**: other per-cover choices as rating inputs and other per-cover choices as rating inputs; waiting periods and counters per cover per life, and shared counters (claims holds the counters); the discount chain as pricing components with entitlements and overrides (today the one discount is `multi_cover_discount`, counted across the policy and not per life); a child pricing cap, dependants added and aged off; premium lines per cover on the notice (the billing event groups by cover type, and every health cover has the same type); canonical bundles as named presets.
- **Questions for Mitchell, none blocking this slice**: the real menu, limits and combination rules are product decisions and the shipped ones are placeholders; whether the bundle discount counts covers per life or across the policy.

## Nominations and the continuation link (13 Nov 2026)

- A nomination for a death benefit is kept on the policy with the date it takes effect and the business date it was recorded. Nothing is edited or deleted: a change is a new nomination, and a removal is a nomination that names nobody. `nomination::in_force` answers who was nominated on a date, as the record stands now or as it stood on an earlier day. A nomination for one life wins over a policy-wide one for that life.
- Command `Nominate`, caller `policy-servicing`. Shares are whole percent and add up to 100, at most 20 people, effective between the policy's start and today. The event `NominationRecorded` carries the party IDs and shares.
- The conversation the notes ask for before a vulnerable customer changes a beneficiary is decided by servicing, which knows the customer. The core cannot see the marker, so it does not enforce the pause; it keeps the conversation's reference where one was held. Not built: the servicing pause itself (hand-over item 10).
- Claims reads the nominees for the event date through `cover_facts`, after interests.
- `IssuePolicy.continuation_of { policy, life }` records the scheme membership a continuation policy carries on from. Only caller `group-admin` may set it. The scheme may be on another partition, so the core records the link and does not check the scheme; scheme administration is trusted for it.

## Loadings are read by question, not published (13 Nov 2026)

- Loadings stay off events. Finance reads them with the partition query `Loadings { after, limit, as_at }` (`ClusterClient::loadings`), which walks policies in ID order, at most 500 a page, and returns each loaded layer with its dates and its loadings as stored. Loadings add, so a layer's total is their sum.
- Not built: a caller check on the query. Partition queries carry no caller today (a policy read by number already returns its loadings), so who may ask is decided by network policy and whatever stands in front of the node. A per-caller check on queries is an open item for the core.

## The erasure register is kept 10 years and locked (13 Nov 2026)

- Mitchell agreed the customer master's erasure register (`customers-erasures`) is kept 10 years (`registerYears`, 3,660 days) and is locked. The register must outlive every database backup, because a restore brings erased keys back and the register is how they are erased again (`POST /v1/admin/reshred`).
- Bicep cannot lock an immutability policy. `tools/lock-erasure-register.sh` does it once in prod after the first deployment: it refuses unless the policy is the agreed period and unlocked, and asks for the account name. No subscription has been deployed to yet, so it has not been run. Once locked the period can only be lengthened, and the account cannot be deleted while it holds a page inside its period.

## Group schemes: continuation, delegated underwriting, split and health menu (14 Nov 2026, NZ time)

Mitchell answered the four group questions in the group thread.

- **Continuing underwritten cover on leaving:** allowed as a scheme setting, off by default (`continuation.underwritten_excess`). With it on, underwriting carries the terms it accepted before onto the continuation policy, as a layer, with no new evidence. The layer starts on the policy's first day, so a later take-up is backdated with no gap.
- **Delegated underwriting for an employer's broker:** to be built. The scheme names the underwriting module's delegated authority. Only cover we can retain goes to the delegate: not a facultative scheme, and not a member reinsurance asked us to underwrite. Underwriting checks every delegated decision against the authority.
- **Splitting a scheme (sale or demerger):** to be built. Members move with their history and underwriting terms, from the first day of a billing period, with no leaver events, using the core's `SplitScheme` (being built in the core thread). Moving the new policy to a buyer's group uses the existing transfer protocol.
- **Health plan menu:** to be built. Plans are sets of the product's covers. A member chooses a plan and a tier, and the employer sees the tier but never the dependants. The menu and members' choices are built; putting a choice into force needs per-member covers and dependants on a scheme in the core, which the core thread agreed to in principle.

## A cover's options are axes of choices, kept in the layer's option code (14 Nov 2026)

- The AIA products need many per-cover choices at once. An income cover takes a waiting period, a benefit period, a premium type, Premier or base, the mental health limit, claim indexation and redundancy. A cover may now declare `option_axes`: each axis is one question, and each choice sets a premium factor, terms (excess, waiting days, benefit months or age, expiry age or term, premium basis) and features. A life's selection is one code, `axis=choice,...` in axis order. It is stored in the layer's existing `excess_option` field, so a change of option is a new layer from the day it applies, as an excess change already was.
- Rejected: one variant cover per combination. AIA's income menu alone is about 1,120 combinations, and the claims, reinsurance and finance mappings would multiply with it. Also rejected: a new field on the layer. That would change about 80 literal sites in other areas' crates for no gain, since the field already carries the choice and its history.
- A cover offers excess levels or axes, never both. The premium factor is the product of the chosen factors. Two choices that set the same term are refused when the selection is resolved. A choice can be restricted to named choices on other axes (`only_with`).
- Riders that only switch a benefit on (waiver of premium, buy-backs, Premier) are axes. Benefits with their own sum insured (accidental death, SAIC, Retirement Protection) are separate covers.
- Built-in benefits are declared on the cover with their kind and limits, so claims reads them from the product, not from its own copy.
- The Multi-Benefit Discount is a discount condition judged per life on each life's cover amounts. The core now passes those amounts to the pricing chain.
- The premium basis can come from the option: stepped, level to an age, level for blocks of years, or limited pay. Limited pay stops billing at its age and keeps the cover in force.
- Entry ages are checked at issue and when a cover is added (reject code `entry_age`).
- Two cover types were added for underwriting and claims: `tpd` and `accidental_injury`.

## AIA material may be used (14 Nov 2026)

- Mitchell confirmed the system is built for AIA ("Yes it's for Aia so can use everything from them"). The AIA products keep AIA's product and benefit names, and they may use AIA's published wordings and material.

## AIA covers are bound to AIA's full published wordings (14 Nov 2026)

- Mitchell asked for the full published wordings to be loaded for claims to assess against ("Yes do that"). Each published document is one definition, `aia-wording-<code>`, and its text is the wording in full. A cover is bound to its umbrella wording, its own appendix and any wording that applies to it, beside the existing summaries.
- **Why:** a cover holds the hash of each definition it is bound to, so the exact words in force when the cover started are fixed, and a later AIA version does not change an existing contract.
- **Source of the text:** the files in `products/wordings/aia/` are the source; `tools/aia_wordings.py` generates the definitions from them, so the text is edited in one place.
- **Not loaded:** Essentials Life, whose online edition has no text layer, and Corporate Solutions, whose schemes each have their own policy.

## Reinsurance: open questions set as best estimates (14 Nov 2026)

Mitchell asked on 13 Nov 2026 for open items to carry best-estimate placeholders. Each of these is a placeholder for a reinsurance manager to confirm against the signed treaties; where a treaty could reasonably say otherwise, the choice is a treaty setting.

- **Money got back on an overpaid claim is shared with the reinsurers.** A provider that billed twice and repays, a claimant who repays, or an offset taken on another partition, reduces the loss the reinsurers share, and the recovery due from each is worked out again on the net (`overpayment_recovered` and `overpayment_offset_received`). Setting: `claim_terms(share_recoveries = True)` per treaty; `False` keeps recoveries for the insurer. Why: treaties are written on the net loss after salvage and recoveries, and reinsurers that paid their share of an overpayment expect their share of what comes back.
- **On avoidance, recoveries already billed are credited back without a disputed item.** A policy avoided for non-disclosure was never on risk, so the reinsurer was never liable and has nothing to dispute. Not a setting yet: a treaty whose errors-and-omissions clause asks for these to go through the disputed item route needs that built.
- **On avoidance, ceded premium is returned to the reinsurers in full, even when the insurer keeps the customer's premium.** Why: the reinsurer carried no risk, and keeping the customer's premium is the insurer's remedy for the non-disclosure, not the reinsurer's. Not a setting yet: a treaty that shares retained premium is not modelled.
- **A change of underwriting terms on cover with facultative reinsurance carries on and tells the reinsurer.** The cession continues on the new terms and the exception `facultative_terms_altered:<key>` asks operations to tell the reinsurer; it closes with `facultative_reinsurer_told`. Why: holding the customer's change until a reinsurer answers would leave the customer waiting on something they cannot see, and most facultative acceptances allow a change notified promptly. A reinsurer that declines the new terms is handled as a change of cession by operations.
- **A continuation policy is ceded as new cover.** A policy that continues a scheme membership is ceded on its own product's treaty from its issue date, and its claims recover on that cession; the scheme's treaty and the member's history are not carried. Why: most group treaties end the reinsurer's liability when the member leaves the scheme, and the continuation option is the insurer's promise to the member, written as new individual cover. A treaty that carries continuations needs that said in its terms and built.
- **Who gets reinsurance alerts.** The alerts in `infra/reinsurance.bicep` fire whatever the recipients, and show in Azure Monitor. Who is mailed is the deployment parameter `alertEmails`, which is empty until the insurer names its reinsurance operations mailbox; no address is guessed.

## AIA Vitality is its own module that only tells the core a tier (14 Nov 2026)

- The wellness module holds the programme's data (membership, statuses) and works out the AIA Vitality Discount under AIA's Premium Adjustment Rules. The core sees only `SetEntitlement` for `aia-vitality` with the discount as its tier.
- **Why:** the core's entitlement design (13 Nov) keeps outside data out of the policy record. The rules (initial discount, first-ten-months protection, which status is read, the baseline) are AIA's programme rules, not product pricing, so they belong outside the product files.
- **Choices made where the rules are silent:**
  - A member with no status reported counts as Bronze, the status every member starts and resets to.
  - An anniversary status more than a year before the decision is not read again.
  - The Initial Discount runs from the join date, and the core adjusts a bill already raised.

## Cover held on a life's other policies counts towards the AIA Multi-Benefit Discount (14 Nov 2026)

- AIA's terms count a life's cover on its other AIA Living, Cover for Life and Start-Up policies towards the minimums. A benefit category now names that cover as `product/cover` (`elsewhere`). The core keeps, for each named life, what the same party holds in force on their other retail policies in the same group, and counts it towards the category. Only this policy's covers are discounted.
- **Why `product/cover`:** cover codes are a product's own. Qualifying them keeps another product's `life` from being taken for this policy's `life`, and lets a product say which other products count.
- **Choices:**
  - The same benefit held on two other policies counts as one sum: the sums are added. AIA's terms say the minimum "can be on" those policies.
  - Only policies in the same group count, because a group's policies share a partition.
  - A change counts from the next bill, as AIA's terms say.

## AIA special events totals and Life Future Insurability (14 Nov 2026)

- An insurability option may now cap all its uses together (`max_total_percent_of_original`, `max_total_increase`; the lower applies) and keep increases apart (`once_in_days`). AIA Living's special events use the wording's limits: the lower of 100% of the original cover and $750,000 in all, one increase in any 12 months.
- A lump sum cover may declare `anniversary_option(every_years, max_percent_of_original, at_least, max_total_percent_of_original, max_total_increase, within_days, to_age, requires, once_in_days)`. It is used through the same `ExerciseOption` command with the reserved event `anniversary` and the anniversary as the event date. AIA Living Life Cover declares it for lives that chose Optional Life Future Insurability: each third anniversary, the greater of 20% and $100,000, within 60 days after, before 60, the lower of twice the original and $1,000,000 in all.
- **Why one command:** the anniversary option is the same act as a special events increase (a layer on the original terms, no evidence), and the wording makes the two share the 12-month rule, so one record of uses serves both.
- **Choices made where the wording is silent:**
  - Anniversaries count from the day the life's original cover started.
  - "In any 12 month period" is read as 365 days between the days increases were made.
  - The special events totals and the anniversary totals are counted separately, as the wording gives each its own total.
  - The wording sets no number of special events increases, so the count is set high (99) and the money and the 12 months bound them.
- **Not built:** the wording's per-event caps (the home loan amount, five times a salary increase) are for the sender to check against the evidence; the totals across all of a life's AIA policies count only this policy; the notice AIA sends before each third anniversary.

## Where the ledger's voters run (15 Nov 2026)

Decided by Mitchell on 15 Nov 2026, after asking why the Raft groups could not run across two regions with a quorum in both, active-active, failing over by themselves.

**Why two regions alone cannot do it.** A Raft group commits a write only when a majority of its voters have it. With voters split between two regions, one region always holds the majority or the two hold equal halves. If Sydney holds three of five, losing Sydney leaves Melbourne two of five, which is not a majority, so Melbourne cannot carry on by itself; it must not, because from Melbourne a lost Sydney and a cut link look the same, and a Sydney that is only cut off would still be taking writes. Equal halves (two and two) have the same problem both ways. Running more machines does not change it: whatever the count, a region that can carry on without the other must hold a majority, and two regions cannot both hold one. Automatic failover between two regions needs a third place to break the tie.

**What was chosen.** Five voters in every partition's group: two in Australia East (Sydney), two in Australia Southeast (Melbourne), and a witness in Australia Central (Canberra). Losing any one region leaves three of five.

- **Active-active.** Both main regions lead: even partitions and half the modules lead from Sydney, odd ones from Melbourne (`IC_LEADER_REGIONS`, `IC_REGION_NODES`). Both clusters run the services, and Front Door sends to both.
- **Automatic failover and fail-back.** When a region is lost, the voters elsewhere elect leaders within seconds; nothing acknowledged is lost and nothing is re-sent. When it returns, its voters catch up and fail-back hands each group back to its home region, paced and checked (`crates/raft/src/failback.rs`).
- **The witness holds no data.** It votes and keeps the log until every data voter holds it, but applies nothing: no policies, no money, no personal information (`crates/raft/src/witness.rs`). It never keeps a lead; if it wins an election it hands the group to a data voter that holds the whole log. It is one small VM (`infra/ledger-witness.bicep`), not a cluster.

**Why Australia Central for the witness.** It is about 4 ms from Sydney and 8 ms from Melbourne, so it adds nothing to the time a write takes when a main region is down. It is in Australia, so the log it holds stays in the same country as the ledger. It is a separate region from both, on its own power and network. Its limits are capacity and access: it is a smaller region, and some subscriptions must ask for it to be enabled, so check that the subscription can deploy there before the first deployment. If it cannot, New Zealand North is the fallback (about 25 to 30 ms from both; the template takes `location=newzealandnorth`), at the cost of the witness's copy of the log being held in New Zealand.

**Why Sydney is the home of the stateful services.** The databases, Key Vault, storage and Event Hubs are not run in both regions; they are in Sydney with Azure's own replication to Melbourne. Running them in both would mean a second copy of every database kept consistent across regions, which is what the ledger already does for the books. The services in Melbourne reach them over the peered networks.

**What it costs.** A second AKS cluster with a ledger pool in Melbourne, a VM in Canberra, peering between three networks, and a cross-region wait on every write: a majority is three, and a region holds two, so each write needs one voter outside the leader's region. The witness is usually that voter, about 4 ms away from Sydney and 8 ms from Melbourne (round trip); Melbourne and Sydney are 10 to 15 ms apart. Policy data is held in Australia, which is a decision for the insurer and its regulator.

Built in: `crates/raft` (witness, shared leadership), `infra/second-region.bicep`, `infra/ledger-witness.bicep`, `infra/regions.bicep`, `deploy/regions`, `tools/check_regions.py`, and the region drill (`deploy/compose/regions.sh`). Runbook: `docs/operations.md`, "Losing a region".


## AIA facilities made to work (15 Nov 2026)

Mitchell asked for every declared AIA benefit to work. The calls made, and why:

- **Conversions are a core command, not new business.** Premium and standalone conversion move cover the life already holds, so `ConvertCover` cuts the old layers and writes the new ones on a draft, re-bills the year already billed once, and keeps nothing if any step fails. Underwriting is not involved, because the wordings ask for no evidence.
- **The newest layers convert first, and only as much as is held at the same choices.** This keeps a partial conversion from mixing two premium types in one request.
- **Business wordings have no personal special events or Future Insurability.** These were removed from AIA Living Business. Business Safeguard replaces them, as in 1151 ALB-LIFE.
- **Income Future Insurability shares one 100% total across anniversary and pay-rise increases** (`shared_total`). The wording gives one total for the benefit.
- **The once-a-year rule for pay-rise increases counts anniversary increases too.** This is the cautious reading of "once per year".
- **MIRC's 115% of the repayment rise is checked on the lender's evidence.** The core enforces only the $750 limit, because the repayment figure is evidence, not policy data.
- **No increase or conversion while a claim is open on the cover.** The wordings refuse an increase "if you are entitled to or are receiving payment of a claim". Open claims are not tracked per life, so on a policy with several lives, another life's claim on the same cover also blocks.
- **Guaranteed rates are chosen per layer.** Each layer keeps the rate table version in force when it was priced. An increase is priced on the table of its own day, which is what the wording says.
- **Cover for Life is made paid up instead of lapsing** only where every paying cover qualifies. Otherwise it lapses as before. Premium for the days ahead is credited first, then what is unpaid for the days already covered is written off as at a lapse. This posts under its own kind (`paid_up_arrears`), so the conduct return does not count it as a lapse.
- **Medical Advancements and Enhancement Pass Back are shown in the cover check.** The facts list the later definition version beside the bound one, and the assessor applies whichever is more favourable. The claims decision records both.

## A claim payment above the item limit (17 Nov 2026)

The scenario run (SCN-12, F-12-4) released a $499,883.13 death benefit to an estate. The claims authority book let a senior assessor release it, and the lump sum claims route's limit for one item is $100,000. Payments could only hold it, and there was no honest way through: claims cannot split a payment, and the hold was hidden behind "payee cooling off".

**What was chosen.** The item limit stays a hold, not a refusal, and treasury lets an item above it through once the money is there (`release_item` by someone holding `payments.treasury` who did not prepare the run). The run is then approved by its approvers as any other, under the run and day limits and the available-funds check. Each hold is recorded on the item (`limit_released_by`), and the check shows every hold an item has at once.

**Why not raise the limit for claims, or split the payment.** A higher limit for one run type moves the line without adding a person at the moment a large sum leaves. Treasury already hears of large payments in advance (the pre-advice) and is the role that funds the account, so it is the natural second person. Splitting one benefit into items under the limit would defeat the limit and the duplicate check (several equal amounts to one account), and would put several payment references on one claim payment. The claims side is unchanged: what an assessor may release is the authority book's, and how money leaves is payments'.

**What it costs.** A large claim waits for treasury as well as an approver. Treasury needs the payments console, with the release on its Act list.

## Back-office fixes from the scenario run (17 Nov 2026)

The scenario run (SCN-17 to SCN-23, SCN-03) found the back-office consoles hard to use and some rules in the wrong order. The calls made, and why:

- **A complaint is deadlocked by hand only after a decision or after its time.** A handler may record deadlock once a delivered outcome is rejected (not a front-line resolution or a withdrawal), or once the decide-by date has passed with no outcome. Before either, the complainant has not been given the insurer's final answer, so the scheme's deadlock letter would be premature. A complaint is decided once. (F-21-8; e2e E2E-049 and E2E-050 follow this order.)
- **A bundle's reviewer is a colleague seated in complaints or legal.** The portal refuses any other reviewer and offers only those colleagues. This is not a full maker-checker: the person who releases the bundle still must differ from the reviewer, as before. (F-21-7)
- **Open conduct case counts stay with conduct and legal.** The complaints report drops `open_conduct_cases` for other seats. (F-21-12)
- **A vulnerability marker takes the complaint from an untrained handler.** It goes back to the queue rather than staying with someone not trained for it. (F-21-4)
- **A complainant is picked from the customer master.** A typed name is refused, so every complaint is about a known person. (F-21-1)
- **Staff pages take their dates from the books.** `portal.today()` is the books' business date: the one the page was served with (a `business-date` meta tag), or for staff the one `/staff/v1/me` gives, so month-ends and defaults match the ledger. Only while the core has reported no date is it New Zealand's calendar date. (F-20-6, F-SCN22-2)
- **Compliance holds a payee's payments.** A new Cedar action `payee.hold` (distribution administrator or compliance) covers `set_payment_hold`, so an adviser under investigation is not paid without recording a licence event that would misstate the licence and lock their book. (F-20-3)
- **The commission console offers first what a person may do.** The rest is set apart under "Done by other roles", naming who does each, rather than hidden: the module still decides and records every attempt. (F-SCN03-5)
- **A price for a fund held by an open price error is not offered for approval** until the error's correction is approved; the corrections themselves are. (F-SCN18)
- **One open countersign item per command.** A second identical prepare is refused, naming who prepared the first. (F-SCN22-3)

## What the portal offers and asks of people (17 Nov 2026)

Decided while fixing the scenario run's portal findings (SCN-01, SCN-02, SCN-08, SCN-13, SCN-16):

- **An assistant is offered only the tools it could use.** The MCP tool list is filtered by what the caller holds, not by which sign-in they used. Staff see a tool when one of their templates, or an active one-off or break-glass grant, gives an action the route checks. Customer-realm callers see adviser tools only with an engagement or practice, and employer tools only as a scheme administrator. A profile uplift changes limits, not actions, so it offers nothing new. The route still decides each call; the list is only so that an assistant is not shown tools that can only refuse it.
- **A phone number written with a leading 0 is a New Zealand number.** `021 123 4567` is stored as `+64211234567`; spaces, dashes, brackets and dots are dropped. A number for another country must start with `+`. Customers are in New Zealand, and asking them to type `+64` was refused as an error they could not understand.
- **Lifting a hold needs a person.** A customer cannot lift a hold on their record from the page: removing it needs an identity check passed in the last 24 hours, which a person on the phone does. The page says to call, rather than asking for a code the customer has never been given.
- **A policy is reached by its number in any case, or by the last three digits in a search.** Callers read out numbers, often only the end. A search by number lists a policy closed to the searcher (their own, or a declared connection's) only as closed, with nothing of it shown.

## Employer changes to members, pools and second administrators (17 Nov 2026)

The scenario run (op-b, SCN-05, SCN-07, SCN-09, SCN-19) found the employer's "Ask for a change" form stored requests nothing acted on, the second-administrator rule was never applied, and a mistyped pool name or salary became insured. The calls made:

- **A hand change is a change file on the group service.** The employer portal's "Change a member" form sends `enter_changes`: a leaver or a new salary, checked as a change file's rows are, refused whole and naming the employee when it does not fit (a wrong employee ID, a date before they joined, the same salary). It is approved like any file. Joiners are not entered by hand: a joiner needs the identity fields the membership file carries, so the form says to send the file. A request to `/app/v1/requests` under the module `group-admin` is refused, since nothing reads one.
- **The second-administrator rule is the portal's, per kind of change.** The employer chooses which kinds need a second administrator (`add_member`, `remove_member`, `change_salary`). A hand change of such a kind is entered with `second_approver`, which the group service enforces; a file holding such a change cannot be approved by the person who sent it (409 `second_administrator`, checked by the portal from the file's counts). A scheme's own terms can still ask for a second approver on every file.
- **A colleague is named only once they have signed in.** The reference a person shows on the employer page is the one to give; a reference nobody holds is refused (`unknown_person`). Group administration's naming of a first administrator still takes the reference it finds, because the person may not have signed in yet.
- **Only staff add pools.** An employer's declaration for a pool the scheme does not have is refused, naming the pools (`unknown_pool`). A declaration of 0 people and $0.00 ends a pool from its date; the scheme's last pool cannot be ended so (`last_pool`): the scheme is ended instead.
- **A pool total above $1,000,000.00 a head is refused** (`implausible_salary`) as a slip of units; a real scheme above it is set by staff in the terms, not declared.
- **A scheme is pools or named members, not both** (F-SCN07-5). A pool and named staff for one employer are two schemes on two policies, each billed and renewed on its own terms. Mixing them would make every file and every declaration answer for members the other counts.

Built in: `crates/group/src/state.rs` (`enter_changes`, `employer_limits`), `crates/group/src/work.rs` (pools), `crates/channels/src/http.rs` (`group_member_change`, `scheme_admins`, `second_administrator`), `crates/channels/src/directory.rs`, `crates/channels/ui/employer.html`. Docs: `docs/group.md`, `docs/channels.md`.

## Staff access and the customer master (17 Nov 2026, scenario round 2)

Decided while fixing SCN2-12, SCN2-13, SCN2-14, SCN2-23 and SCN2-24:

- **A sign-in bound to a merged record sees nothing until it is re-linked.** After a merge the absorbed record resolves to the survivor for staff and services, but the person signed in on the absorbed record is refused `record_combined` for their record, history, hold and copy, and cannot change it. The identity service binds the sign-in to the survivor once the person's identity is checked again. The alternative, following the merge for the customer, shows one person another's details whenever a merge is wrong (F2-13-1). The survivor's own copy of everything held is their own record only; the privacy officer's copy holds both.
- **A merge needs a reason; a pair judged apart needs a second look.** Someone who dismissed a duplicate review as two people cannot merge the same pair; someone else may, after looking again, and is shown who judged them apart, when and why (F2-13-5, F2-13-6). A blanket second-person approval on every merge was not chosen: most duplicates are clear and the merge can be undone; the risk sits where someone has already said "not the same person".
- **Staff configuration and authority profiles take effect only on a second person's approval.** `approved_by` on the configuration commands is accepted as the name of the person asked (`approver`), never as an approval (F2-24-1). Giving someone an administering template on employment or assignment is still one person's act: a platform with one administrator could otherwise never appoint the second. That gap is left for the owner (see the round 2 fixes list).
- **A role for a period is a grant kind** (`role`), approved like any grant, at most 31 days by default, so cover for leave ends by itself (F2-24-3).
- **No manager named means an administrator recertifies**, other than the person themselves (F2-24-6).
- **An access approver may remove a declared connection** as well as a staff administrator, with a reason, so the only administrator's own declaration is not stuck (F2-24-7, F2-24-9).
- **Legal settles capacity through the portal as legal.** `settle_capacity` is asked of the customer master in the `legal` role under the staff action `customers.settle_capacity` (F2-23-6). The portal's identity needs the `legal` app role on the customer master.
- **A record is not closed while the person has cover.** `exit` from the staff console is refused `cover_in_force`, naming the policies the core holds for the person that have not ended (F2-14-2).

Built in: `crates/customers/src/service.rs` (`ask_about`, `merging`, `dismiss`), `crates/customers/src/store.rs` (`merge`, `dismiss_review`, `judged_apart`), `crates/channels/src/staff.rs` (`propose`, `decide_change`, `check_duties`, `SetManager`), `crates/channels/src/customers.rs` (`settles_capacity`), `crates/channels/src/http.rs` (`refuse_exit_with_cover`), `crates/channels/ui/records.html`.

## New business and underwriting from scenario round 2 (17 Nov 2026)

The second scenario run (op2-a: quoting, disclosure, consent, underwriting) found a customer could not answer the questions, a conflicted underwriter could read the case they were closed to, and underwriting dated work by the wall clock. The calls made:

- **A conflicted underwriter reads nothing of the case.** The proposal question and the queue take the asking underwriter; a case closed to them answers only that it is closed (403 `conflict`). Declaring a conflict needs a reason, which is kept on the `ConflictDeclared` event. (F2-03-1)
- **Underwriting dates work on the books' business date.** The driver, the disclosure intake and the portal ask the core (partition 0) for its date; the wall clock gives only the time of day. Evidence deadlines, chase days, interview days and offer expiry follow the books, so a test or catch-up day ages work as the ledger does. Only while the core reports no date is the clock's date used. (F2-02-8)
- **The question wording is held by channels.** The module checks answers by id and version; the portal words each question for the customer and for the underwriter's screen (`crates/channels/src/questions.rs`), so wording can be read without changing what was asked.
- **The reasonable-care questions add medication, blood pressure, lungs and anything else.** They are a new rule set (`individual-reasonable-care`, from 15 Nov 2027) on the v2 question set, so the in-force v1 set and its rules are unchanged. A yes, or any free text, refers the life for medical review.
- **Refusing or withdrawing a consent needs no device; giving one does.** A refusal binds nobody, and asking for a signature to say no stopped people refusing.
- **A customer may ask us to look again once.** A second counter is refused, saying to accept, decline or talk to their adviser. The offer shows its terms and the premium on the books' date before the customer answers.
- **A walk-in quote names no one and needs no purpose.** The officer gives the product, covers in cents, frequency and the life's birth date, sex and smoking; the product's eligibility is asked of underwriting first (each cover's type taken as its code), so nobody is quoted cover the product would not take.
- **A queue lead or manager may move a case another underwriter holds**, under the Reassign action, recorded as the holder's move. Anyone else is told who can.
- **A peer-review period is drawn once, and never empty.** A second draw for the same period or an overlapping window is refused (`already_drawn`); when the rate draws nothing but decisions were eligible, the nearest is drawn, so a period is never signed off unreviewed.
- **An authority refusal says why and who can act** (over a limit, in dollars; a loading beyond the profile; a second signature needing a senior), rather than "not authorised".
- **A sum insured outside a cover's range is said in dollars**, with a hint when it looks typed in dollars rather than cents.
- **Device-only MCP tools stay listed.** `submit_disclosure` and the signing of consents are described as done on the customer's own device, so an assistant can explain the step rather than not knowing it exists.

## Documents: authoring, corrections and the console (17 Nov 2026)

Decided while fixing the scenario run's documents findings (op2-d, F2-10, F2-11):

- **A sentence is at most 40 words.** `Settings.max_sentence_words`, 40 by default, is checked at submission beside the reading grade: one long sentence can pass a document's average grade and still lose a reader. (F2-10-5)
- **A product's notice uses only the fields a policy's documents are given.** A template bound to a product, of a kind the core's events ask for, is refused at submission for any other field. A template bound to no product is used only where asked for by its id, as the shipped placeholders are. (F2-10-4)
- **Activation may be backdated.** `effective_from` may lie before today, so documents that waited for a template issue once it is active from on or before their business date. The page says so beside Activate. (F2-10-10)
- **A draft is withdrawn by disabling it.** `DisableTemplate` takes a draft too, and nothing can approve or activate it after. (F2-10-9)
- **Corrected notices are rendered from the original's data with the newer version of the same template.** The service issues them, but only once remediation asks (`CorrectDefectNotices`), and only from a version newer than the wrong one that is in force: the correction says the same thing about the same facts, in the wording put right. (F2-11-2)
- **Whoever raised a defect does not close it.** Unchanged: a second person who works notice defects checks and closes. The refusal says so, and the page names colleagues who can. (F2-11-6)
- **A legal hold says why.** The console requires a reason to place or release one, and keeps who and when. The module takes a hold without one, so the log already written replays. (F2-11-11)
- **Returned post is recorded against the recipient** (`RecordReturnedPost`), which fails that delivery and lists the document as undeliverable until it is sent again. (F2-11-4)

Built in: `crates/documents/src/supplied.rs`, `state.rs`, `module.rs` (`RenderSample`, `Authoring`, `Undeliverable`, `CorrectionsDue`), `required_documents.rs` (corrections, why a trigger waits), `crates/channels/src/correspondence.rs`, `crates/channels/ui/documents.html`.

## A large claim's second signature (17 Nov 2026)

The scenario run (SCN2-08, F2-08-2) admitted a $900,000 trauma claim. The claims manager, the only person with authority above $600,000, proposed it; it then waited for a second signature from someone whose own limit also covered $900,000. Nobody held that, so the claim could never be admitted.

**What was chosen.** Authority and the four-eyes check are separate. When the proposer's own profile covers the decision, the second signature needed because of its size is a peer's: another person accredited for the line who may themselves sign that kind of decision at the two-signature level (an admission at `two_signature_admission_minor`, an ex gratia payment at `two_signature_ex_gratia_minor`, a write-off at `two_signature_write_off_minor`; for a decline, the power to decline; for a reversal, a senior with the power to reverse). When the proposer's authority does not cover it, the second signer must cover the whole decision, as before. Conflicts and the different-person rule apply to both.

**Why.** Requiring two people who each hold the full limit makes the top of the authority book a single point of failure: with one claims manager, every claim above everyone else's limit stops. The four-eyes control is that a second competent person checks the decision, not that the insurer has two people able to make it alone; the person accountable for the amount is the proposer, whose authority is recorded with the decision. A peer still has to be trusted with large decisions in their own right (the two-signature level), so a junior cannot wave a large claim through.

**What it costs.** A senior assessor can countersign a decision above their own limit. A business that wants two full-limit signatures above some amount should add a second person to the authority book at that level; the refusal says when nobody else can sign.

Built in: `crates/claims/src/decide.rs` (`Need::Peer`, `peers`, `signers`, `cannot_sign`), `crates/claims/src/report.rs` (`WorkDecision.signers`), `crates/channels/ui/claims.html`. Docs: `docs/claims.md`.

## Direct debit authorities, hardship and refunds to a payee (17 Nov 2026)

The scenario run (SCN2-15, SCN2-16) found an authority with a digit missing accepted and active on one person's word, a payer in hardship collected again, a suspended authority added to a run and reinstated by one click, and a customer's refund requested to another customer's account. The calls made:

- **An authority's account is checked by its check digits.** Direct debits are collected from New Zealand accounts only, so the bank's published check-digit algorithms apply (A to G and X by bank). Payee accounts keep the looser shape check, because a payee can be abroad or be given by a file in another form.
- **A second payee maintainer confirms an authority**, as a payee is confirmed: on creation, on reinstatement after suspension, and on a change of account, which also waits out the cooling-off period. Reinstatement still resets the dishonour count (the payer has told us the money is there); the second person is the control, not a new account. Authorities recorded before this change count as confirmed.
- **A hardship mark stops new collections.** Collecting from someone the insurer knows cannot pay only adds dishonour fees. The mark can carry a last day; without one it stands until collections takes it off. A collection already in a run when the mark is made is held at the check; a presentation again that a recorded conversation agreed goes ahead.
- **A refund to a payee goes only to the owner's own payee.** The owner is the one the core reports (`PolicyOwner`); until it has, a refund to a payee is refused rather than guessed. A refund of money paid by direct credit from someone else's account is paid to the owner, not back to that account: the channels have no rail to return a credit, and the third-party payer case is compliance's.

Built in: `crates/payments/src/authority.rs`, `model.rs` (`Authority::collection_stop`), `run.rs`, `refund.rs`, `console.rs`; `crates/channels/ui/payments.html`. Docs: `docs/payments.md`.
