# Architecture

## The shape of it

Everything the insurer does to a policy is a **command**: issue, apply a receipt, change scheme membership, close the day. A command goes into an ordered, replicated log, and a deterministic state machine applies the log. The state machine holds the policies and a double-entry ledger, and it is the only thing that ever writes either. Given the same log, every replica reaches the same state and the same ledger, byte for byte, and each replica proves it with a state hash.

That one idea gives the properties an insurer's core needs:

- **Nothing is lost.** A command is acknowledged only after a majority of replicas have it on disk.
- **Nothing happens twice.** Every command carries an idempotency key. A retry of a command that was applied returns the original result.
- **Everything is explainable.** The ledger is append-only and hash-chained. Policy state can be rebuilt by replaying the log.
- **Tampering shows.** Each day's ledger heads are folded into a Merkle root, signed with a Key Vault key and written to immutable storage.

## State of the build and hand-over

This document is shared by every area of the build. The sections above the line "Modules beside the core" describe how the core works. Below it, each area has one section of its own, headed with the area's name, that says what is built and where, what is not, and what a person would need to finish it. An area edits only its own section. `docs/decisions.md` is the list of known gaps and open decisions, one entry per subject, and wins where the two disagree on detail.

| Area | Crates | Its own document | Hand-over section |
|---|---|---|---|
| Core (ledger, policy model, billing, nodes, daily close, Azure adapters, CI) | `core`, `ledger`, `log`, `money`, `product`, `raft`, `node`, `api`, `workflows`, `azure`, `readmodel`, `cashmatch`, `sim` | this file, `operations.md`, `modules.md` | "Core: hand-over" below |
| Claims | `claims` | `claims.md` | its own section |
| Reinsurance | `reinsurance` | `reinsurance.md` | its own section |
| Group schemes | `group` | `group.md` | its own section |
| Adviser commission and held balances | `distribution` | `distribution.md` | its own section |
| Payments and bank channels | `payments`, `payments-service` | `payments.md` | its own section |
| Underwriting and new business | `underwriting` | `underwriting.md` | its own section |
| Documents, complaints | `documents`, `complaints` | `documents.md`, `complaints.md` | its own section |
| Finance ledger and reporting | `finance` | `finance.md` | its own section |
| Portal and app services | `channels` | `channels.md` | "Portal and app services" below |
| Unit pricing | `pricing` | `unit-pricing.md` | its own section |
| Customer master | `customers` | `customers.md` | its own section |
| Persona test pack (matrix, stories, traceability) | none; `tools/persona_matrix.py` | `testing/personas/README.md` | "How far the build reaches" in that README |

### Checks every area shares

- **`ci.yml`** on every push to `main`: format, `cargo clippy --workspace --all-targets -D warnings`, `tools/check_test_docs.py` (every test has `Purpose:` and `Why it matters:`), `tools/persona_matrix.py --check` (the persona test matrix holds together and every built test its traceability names exists; when you rename or remove a test it names, update `docs/testing/personas/traceability.md`), the whole workspace's tests under `cargo llvm-cov` with a real Temporal server and a PostgreSQL container, a 98% line floor by llvm-cov's own table (`tools/coverage_floor.py`), Bicep compile of `infra/*.bicep` and `kubectl kustomize deploy/k8s`. About 15 minutes.
- **`drill.yml`** on every push: builds the image with the `drill` profile, starts the Compose cluster (three ledger nodes, Temporal, two workers, the API) and runs `deploy/compose/chaos.sh`, which kills a ledger node and a worker during a close and checks the books. 30 to 55 minutes, limit 90.
- **Reading the results.** Each workflow runs one at a time on `main` and never cancels a run that has started (`cancel-in-progress: false`), so a `ci.yml` run completes about every 16 minutes however many pushes land. A run still waiting is replaced by the next push and shows as cancelled within seconds, which with eleven areas pushing is most of the list: on 13 Nov 2026 the last 15 listed were cancelled while one was running and the one before it had passed. That was looked at again on 13 Nov and left as it is: cancelling nothing would queue a 16-minute run for every push and the results would fall hours behind. To judge your own commit, skip the cancelled runs, take the latest run with conclusion success or failure, and check that its head contains your commit (`git merge-base --is-ancestor <your commit> <run head>`). If it does not, the run now in progress or the one waiting will. A red run is everyone's to read: the failing job names the crate. Last read, 13 Nov 2026 04:50 UTC: `ci.yml` passed in full at `5017b57` (04:05 UTC). The run at `d1ac971` (04:22 UTC) was red on the coverage floor alone: every test passed and line coverage by cargo llvm-cov's table was 97.98% (1,376 of 68,267 lines uncovered, 12 lines short). The step named "Uncovered lines by file" prints a second figure from `lcov.info` (98.74% in that run) that counts lines differently and is not the gate; read the `TOTAL` row of "Coverage by file". The files with most uncovered lines in that run: `core/src/invest.rs` 56 (about 35 since covered), `raft/src/server.rs` 39, `sim/src/year.rs` 35, `documents/src/service.rs` 28, `distribution/src/commission.rs`, `group/src/work.rs` and `distribution/src/replacement.rs` 26 each, `core/src/claims.rs` 23. `drill.yml` passed at `8b3cdb8` (12 Nov) in 33 minutes with the drill profile.
- **Before pushing a change to a public type or a core event**, build the whole merged workspace with all targets: other crates' tests construct those types by hand.
- **In a small container** a workspace test build fills the disk. Test crate by crate, with `IC_SKIP_TEMPORAL_TESTS=1` where no Temporal server is reachable; CI runs everything.

### The integrated cycle on one machine

The whole arrangement runs on one machine with `docker compose -f deploy/compose/docker-compose.yml up --build` and is broken on purpose by `deploy/compose/chaos.sh` (see the README and `operations.md`). CI runs exactly this in `drill.yml`. It has been run by hand on the development Mac for the core alone. **It has not been run by hand since the modules were hosted on the ledger nodes**, so on a real multi-node cluster nobody has yet watched: a module's outbox drain after a leader change, commission posted after a receipt on any partition, each module's `status` question answering on every node, or the unit pricing and group drivers against the cluster. The commission thread asked for exactly those three checks. The Mac ran out of memory once building the test image; build the image with `CARGO_PROFILE=drill` there too.

## Partitions

Policies are grouped (a household, an employer's scheme), and a group lives on one of 64 partitions, chosen by a hash of the group's ID that never changes. Each partition is its own Raft group with its own log, ledger and business date. Work on one partition never waits for another, and one very large scheme cannot stall the rest: no command may do more than a bounded amount of work, so a 50,000-member scheme is billed in chunks.

A policy can move between groups, and so between partitions. The move is a two-step transfer with money held in an in-transit account between the steps, delivered by messages that are retried until acknowledged. Reconciliation checks every day that nothing is left in transit.

One other kind of message crosses partitions: a claims command, for one person whose claims sit on two partitions (an overpayment on one recovered from a benefit on the other). The claims module queues it through its port (`send`), it leaves in the same step as the sender's postings, and the relay delivers it at least once as caller `partition`, so only a claims command that allows that caller is applied. `MessageBody::into_delivery` is the one place that says what each message becomes at the receiver. Money on its way sits in `claims_offset_in_transit` (chart account 1210). The core does not know how many partitions there are, so whoever asks names each target. Not built: a daily check that this account nets to zero across partitions when no offset is on its way, to add beside the transfer check once claims posts to it.

## Ledger nodes (`ic-node`)

A node holds a replica of every partition. Three nodes, one per availability zone, tolerate the loss of any one.

- **Raft** (openraft) orders the log. Each node's log is an append-only journal, `fsync`ed before an entry is acknowledged. Members ask before they campaign (pre-vote), so a member returning from a network fault does not force an election on a healthy group.
- **Snapshots** bound the log. The state machine is snapshotted as the log grows, and a new or lagging node is caught up from a snapshot.
- **Identity.** A node's number is bound to an identity created on its disk the first time it starts. A node that comes back with an empty disk under its old number is refused by its peers, because it might vote again in a term it already voted in. It is replaced under a new number instead.
- **Housekeeping.** The leader of each partition seals finished stretches of the journal to immutable Blob Storage, publishes events to Event Hubs and delivers cross-partition messages. Each is at-least-once, recorded in the log when done, and resumed by the next leader if the leader dies.
- **Read models.** Each node keeps its partitions' read models (the policy directory, the life index used for sum-at-risk, the billing view) inside the replicated state, so a query answered by a node is consistent with the ledger it sits beside.
- **Authentication.** Every caller of a node presents the cluster token. The token lives in Key Vault.

## The API (`ic-api`)

A stateless gateway. It authenticates the caller's Entra ID token, checks the caller's role against the command, finds the partition (by group, by policy, or by asking every partition for a policy number), and proposes the command to that partition's leader, retrying across leader changes with the same idempotency key. Reads are linearizable: they are answered by the leader after it confirms it still leads.

When the ledger cannot be reached the API answers `503` with `Retry-After`, and a retry with the same idempotency key is always safe.

The same binary, started without `IC_NODES`, is the "insurance in a box": every partition in one process, for development and demonstrations. It is not the production arrangement.

## The daily close (`ic-worker`)

At midnight in New Zealand each business day is closed: the cutoff, billing, arrears and lapses, earning, reconciliation, then the signed Merkle root. The close is a Temporal workflow whose steps are each safe to repeat any number of times, by any worker. Temporal records each finished step, so when a worker dies another continues from the step after the last recorded one. A day whose books do not reconcile fails the close, and the next day is not opened over it.

The business date moves only by the close. Nothing in the core reads the wall clock.

## Money and products

Money is exact decimal arithmetic with explicit rounding. Rounding differences are posted, never dropped. Products, rate tables and policy definitions are data: Starlark files under `products/`, activated through the log like any other command, so the product version a policy was sold under is on record and replayable. An active version is immutable: the ledger accepts the same content again, and refuses other content under a product, rate table or definition version already in use, or another value for an index rate already in effect (`config_conflict`). A product changes by activating a new version.

## Events notified late

An insured event can be notified weeks after it happened, and by then the policy has carried on: periods were billed, an anniversary applied indexation, a cover may have expired by age. `TerminateOnClaim` ends the policy as at the day of the event. It first unwinds everything effective after that day, then earns premium up to and including the day, and returns the premium for every later day to policy suspense.

`UnwindAfter` is the same reversal on its own, for a claim or for a remediation case correcting the insurer's error. Nothing is removed or rewritten:

- Each period billed after the date is reversed by a new entry with the period's own value date, naming the entry it reverses (`detail["reverses"]`) and the claim or case (`detail["reference"]`). The hash chain extends over the reversals and every published root still verifies.
- A layer that started after the date is ended on its start date, so it existed for no days and its history shows it was created and unwound. A cover that expired after the date is in force again.
- The policy is then held as at the date: end-of-day earns what is still billed and does nothing else, so the anniversary just reversed is not applied again that night. A claim ends the hold by terminating the policy. A remediation case ends it with `ResumeAfterUnwind`, and end-of-day replays the anniversaries and billing from the date on the terms then in force.

By default an indexation step applied after an event nobody yet knew of is treated as never having started, and `PolicyTerminated` reports the sum insured as at the event. A product whose wording pays the indexed amount says so (`indexation(..., pays_step_applied_after_event = True)`), and the step then stands.

Refunds go back to the account the money came from. Where the payer is the person the claim is for (`payer_deceased`), the core cancels the payment authority (`PaymentAuthorityCancelled`) and refuses any refund that is not paid with that claim (`RefundSuspense.with_claim`).

## Choices made on a defective notice

A choice made in reliance on a wrong mandatory notice is not a valid choice. The documents module finds the defect, sends a corrected notice, and once that has proof of delivery it opens a reopen window on the policy (`OpenReopenWindow`, idempotent by reference). Inside the window the policyholder may make the choice again with `ReopenChoice`, and it applies as if made correctly the first time: no fee, no underwriting, no waiting period, nothing asked of the policyholder first.

- **A decline of indexation set aside.** If the latest anniversary passed with indexation declined, the step it would have added is added as a layer that starts on the anniversary. The periods billed since are reversed by new entries with their own value dates and billed again at the premium that includes the step. The difference is due on the last day of the window, so making the choice again never puts the policy into arrears. The decline is removed from the policy's record of declined anniversaries. If the anniversary has not come yet, the choice is recorded and the anniversary indexes the cover in the ordinary way.
- **A cancellation set aside.** The cancellation's own entry is reversed at its own value date, and the covers, receivables and standing are put back as they were, so cover is unbroken. If the refund has already been paid out, the policy is in force all the same and the refund is owed again as a receivable due at the end of the window.
- **The notice clock.** `RecordNoticeDelivery` records proof that a mandatory notice was delivered and the first day its effect is allowed. A policy does not lapse before the earliest effect of its latest lapse notice, so a corrected notice restarts the clock. A policy with no lapse notice on record lapses by the product's arrears rules alone.

## Returning leaders to the preferred region

When the region that normally leads is lost, the voters elsewhere take over within seconds. When it returns, its voters rejoin as followers and catch up, and each node then hands the groups it leads back to them (`ic_raft::failback`), with no operator involved. A node hands a group over only when:

- a preferred voter has held the log to within a few entries for the settling period, holds every entry at that moment, and reports the same state hash at the same log entry (a voter that reports a different one stops fail-back and is logged as an error);
- the time of day is outside the frozen periods of the freeze calendar, no end-of-day run or policy transfer is in progress on the node, and no module answers `false` to the question `{"failback_quiet": true}`.

It moves a set share of its groups per round and does not start another round until each moved group has answered a read that needs a majority, under its new leader, inside the latency budget. A group that is too slow, or has no leader five seconds after the handover, stops fail-back on that node until an operator resumes it. `GET /v1/failback` on a node says whether it is paused or stopped and what it is waiting for. The same mechanism returns leaders after any move, planned or not. The handover itself uses Raft's leadership transfer: the leader stops accepting writes, tells every member who is next, and that member stands at once.

## Units and funds

A unit-linked policy's savings are units of funds, and units are ledger balances like money: an account per policy per fund, held in micro-units, in a balance family of its own so that every entry balances in each fund's units and in currency separately. A policy's dollar value is never a balance. It is units times a price, worked out where it is shown and labelled with the price date.

Pricing is forward. An order is replicated state, not a posting: it names a pricing date, and waits with its cash in the policy's pending investment cash until the unit pricing module activates the price for that fund and date through the log. The core never looks a price up or works one out. Execution is a command that works through a fund and date's pending orders in chunks; an executed order's entry carries the price ID, the price and the pricing date. Buying NZD 1,000 at 2.0000 debits the policy's pending cash and credits the fund's cash position by 1,000, and debits the fund's unit position and credits the policy's unit account by 500 units. A switch is a sell that places a buy in the other fund when it executes. An order can be cancelled until its price is active and not after, and a new order cannot take a date whose price is already struck, since that would be dealing at a known price.

A price is never edited. A correction is a new price that names the one it replaces, and a replay command then moves each order executed at the old price onto the new one, posting the difference today with the original pricing date as its value date. The customer is put where the right price would have put them, is never asked for cash back, and what cannot be recovered the insurer pays the fund through a pricing error account, so the fund's other investors do not carry it.

Money leaves the investment ledger the way a claim is paid. A sell puts its proceeds in pending investment cash; a payout command raises them to the policy's benefit payable, and the payments module pays from there under the payout's ID, so a withdrawal passes the same payee checks and bank reconciliation as every other payment.

What is and is not built is in [decisions.md](decisions.md).

## Core: hand-over

Owned by the thread "Implement insurance core system". What the core is and how it works is the first half of this document; this section is the state of it on 12 Nov 2026 and what is left.

### What is built, and where

- **Ledger, log, money, products**: `crates/ledger` (double entry, hash chain, account roles), `crates/log` (the state machine trait and idempotent apply), `crates/money`, `crates/product` (Starlark products, rate tables, definitions, funds; `prelude.star` is part of every content hash).
- **Policy state machine**: `crates/core/src/machine.rs` dispatches every `Command` (`command.rs`; each has `allowed_callers()` and a `name()` equal to its wire tag, checked by `crates/sim/tests/integrity.rs`, which must list every command). By file: `lifecycle.rs` (issue, anniversaries, indexation, merge, cancel, reinstate), `billing.rs`, `cash.rs`, `earning.rs`, `arrears.rs`, `premium.rs`, `group.rs` (scheme membership, chunked scheme billing, underwritten layers), `voluntary.rs` (cover a member pays for), `arrangement.rs` (an arrangement to clear overdue premium by instalments, watched from `arrears.rs`; tests in `sim/tests/arrangement.rs`), `underwritten.rs` (underwriting's increase and alteration; written by the underwriting thread), `reduce.rs` (owner's reduction), `nondisclosure.rs` (avoid, vary), `interest.rs` (lender's interest and consents), `invest.rs` (unit-linked ledger), `unwind.rs` and `reopen.rs` (late-notified events and defective notices), `transfer.rs` (moving partition), `payments.rs`, `counterparty.rs`, `reinsurance.rs`, `claims.rs` (the port the claims module uses), `recon.rs` (the reconciliation every close runs).
- **Nodes and close**: `crates/raft` (openraft groups, the module host), `crates/node` (`ic-node`, hosts the partitions and every module), `crates/api` (`ic-api`, `ic-admin`), `crates/workflows` (`ic-worker`: the Temporal end of day, the unit order run), `crates/azure` (Key Vault signing, Blob, Service Bus, Entra ID), `crates/readmodel`, `crates/cashmatch`.
- **Simulation and tests**: `crates/sim` runs real partitions in memory; `crates/sim/tests/*.rs` are the core's behaviour tests, one file per subject.
- **Deployment**: `infra/main.bicep` and one Bicep file per module, `deploy/k8s` (kustomize), `deploy/compose` (one machine and the failure drill), `Dockerfile` (`ARG CARGO_PROFILE`).

### Added on 12 Nov 2026, with the area that asked

| What | Where | Tests | For |
|---|---|---|---|
| Unit-linked ledger: orders, switches, price correction by replay, payouts, the order run, fund files | `invest.rs`, `crates/workflows/src/eod.rs` and `steps.rs`, `products/funds/` | `sim/tests/investment.rs`, `workflows/tests/end_of_day.rs` | unit pricing, finance |
| Lender's interest, consent, release | `interest.rs` | `sim/tests/interests.rs` | claims, documents |
| `Layer.rated_from`; level premium to `level_to_age` | `policy.rs`, `premium.rs` | `sim/tests/edges.rs` | underwriting |
| `ReduceCover` | `reduce.rs` | `sim/tests/reduce_cover.rs` | commission |
| Member factors on `MemberJoined` and `MemberChanged`; occupation class as a rating factor | `event.rs`, `group.rs`, `crates/product` | `sim/tests/group_cover.rs`, `edges.rs` | finance |
| Cover a scheme member pays for: bills, receipts, credit, standing, lapse | `voluntary.rs`, `group.rs`, `billing.rs` | `sim/tests/voluntary_cover.rs` | group schemes, payments |
| `AvoidPolicy`, `VaryForNonDisclosure` | `nondisclosure.rs` | `sim/tests/nondisclosure.rs` | underwriting |

The last two rows were pushed on the evening of 12 Nov and no completed CI run covering them had been read when this was written. They passed format, clippy on the whole workspace, the test-doc check and the tests of `ic-core`, `ic-sim`, `ic-finance`, `ic-group`, `ic-underwriting` and `ic-workflows` locally. Whether the workspace is still at 98% with them is not known until that run completes.

### Not built: the plan, in the order I would do it

Each item has an entry in `docs/decisions.md` with the detail of what exists.

1. **Have the other areas handle `PolicyStatus::Avoided` and `PolicyAvoided`.** They were told through the coordinator on 12 Nov; none had acted when this was written. Commission must claw back everything on an avoided policy, reinsurance must reverse cessions from inception, finance must map the status and the `avoidance_premium_retained` role (already in `config/finance/chart.star`), documents needs the letter, and underwriting must route its `NonDisclosureRemedy` effect to the two commands. Every other crate compiles because they match statuses with a wildcard, which is exactly why none of them handles it yet.
2. **Unit-linked product.** Built (13 Nov 2026): `products/invest_plan.star` (code `invest-plan`, one `account` cover with a three-fund menu, rate table `no-premium` so nothing is billed) and the fund menu enforced in the core (`invest::menu` and `invest::offered`, called from `machine.rs`): cash is set aside and units are bought only on a policy whose product has an investment account, and only in funds on its menu (codes `not_invested`, `fund_not_offered`); selling is never held to the menu and a fee rebate may return to a fund taken off it. Tests: `money_goes_only_into_funds_on_the_products_menu` in `sim/tests/investment.rs`. Full surrender is built too: `InvestCommand::Surrender` (caller policy-servicing; lender's consent as for any surrender; sells everything in each fund held at the pricing date given for that fund, orders named `{surrender}-{fund}`; no new money while it waits) and `CompleteSurrender` (once every sell has executed and no units are left: the whole pending cash raised to benefit payable under one payout, cover ended, status `Surrendered`); test `a_full_surrender_sells_everything_pays_the_value_and_closes_the_policy`. Not in it: a surrender charge (the product language has no fee design yet), and any contribution left in suspense is refunded by servicing as after a cancellation. Still needed: contribution rules that send a receipt to `AllocateCash` and buy orders by a standing split (today servicing sends both); fees and insurance charges taken as sell orders each month; the servicing commands for a withdrawal and a surrender (sell orders, exit charge, `RaisePayout`, status `Surrendered`); carrying units when a policy moves partition (today refused `policy_invested`); opening loads for migration; an alert when `EndOfDayReport.unit_orders_left` is not zero. Seams: unit pricing (`crates/pricing`, prices and corrections), finance (the close check on pending investment cash), payments (payouts are paid as benefit payables).
3. **Joint life and linked covers.** Agreed with claims that these are policy facts the core holds. Nothing exists. Needed: a cover on two lives that ends on the first event, covers linked so that a claim on one reduces another, and both published in `claims::cover_facts`.
4. **Policy loans and change of owner.** `RestrictedAction::Loan` and `ChangeOwner` exist in `interest.rs` with nothing to restrict. A loan needs a surrender value basis, which traditional products do not have here. Change of owner is the transfer protocol with a new policyholder; the account role `policy_loan` exists unused.
5. **Interests in claims.** `ic_core::interest::ranked(policy, on)` gives the lenders to pay first. The claims thread was to add `CoverFacts.interests` and fill it in `crates/core/src/claims.rs`; check whether that landed.
6. **Other alterations in the design**: adding a cover that was not on the policy at issue, a product-declared order of reduction, future insurability options, conversion to another product.
7. **Voluntary cover, the rest**: automatic (formula) cover paid by the member, so dependants paid for by the employee; a proven notice before a member's cover lapses, as a policy's lapse waits for one; reinstating a member's lapsed cover; carrying member credit on a partition transfer (refused `member_credit_held`); an optional life on `PaymentAuthorityCancelled` so payments can end one member's authority (asked by payments).
8. **Smaller known defects**: cancelling a policy on which nothing was paid can leave one cent in policy suspense; `infra/group.bicep` and the customer master's Bicep file compile in CI but are not referenced from `main.bicep`; finance's rate table checks index cells without occupation class (`crates/finance/src/tables.rs`).
9. **The integrated cycle** described above, by hand, with the modules hosted.
10. **Taken on 13 Nov 2026 from other areas and the persona matrix** (`docs/testing/personas/coverage.md`, "Built so far"), in the order intended:
    - *Hardship override* (COL-014) and *arrangement* (COL-015): built, `hardship.rs` and `arrangement.rs`, sent by payments' `ic-collections` module as caller `collections`; rules and what is not handled are in `decisions.md`. Tests `sim/tests/hardship.rs` and `sim/tests/arrangement.rs`.
    - *Automatic premium loan and the non-forfeiture order* (COL-023): waits on policy loans and surrender values for traditional products (item 4).
    - *Composable health product and the discount chain* (HLT, DSC; GRP-017 waits on it): first slice built, `products/health_flex.star` with the `composition` feature (`ProductDefinition::check_composition`, checked in `lifecycle.rs` at issue and `reduce.rs` when a cover is given up), tests `sim/tests/health_flex.rs`. What is not built is listed in order in `decisions.md` under "The mix-and-match health product": adding a cover after issue comes first, then excess options, then the discount chain as pricing components. `docs/group.md` records that the health plan menu was not designed enough to build; read the design notes' health sections first and record any question that needs Mitchell.
    - *Vulnerable customers* (VUL-003, 005, 007, 010, 013): the customer master answers `GET /v1/parties/{id}/routing` and `/authority` (`docs/customers.md`). Pausing a large withdrawal or a surrender for a conversation, and the abuse indicators on a beneficiary change, are servicing rules on this side and are not built. The core has no servicing edge that calls the customer master: decide whether that check sits in the API layer (`crates/api`) or in the portal before building.
    - *Joint lives and ownership* (JNT, 15 priority 1 rows), *surrender, maturity and loans* (SVC), *annuities* (ANN), *migration cohorts* (MIG) and *incident runbooks* (INC): mostly items 3 and 4 above; annuities and migration cohorts have nothing built in the core.
    - *Claims in-transit check*: once claims posts to `claims_offset_in_transit`, add a daily check beside the transfer one that it nets to zero across partitions when no offset is on its way.

### Open questions for Mitchell

- **Screens.** Whether adviser, employer, staff and operations screens belong in this repository was answered only with "Continue"; the portal thread took that as yes.
- **Premium on avoidance.** The core takes refund or retain from the approved case because the design notes ask for legal review and give no rule. If there should be a rule by regime (before and after the Contracts of Insurance Act 2024), it belongs in underwriting's effective-dated rules.
- **GST on premium kept on avoidance** is left as collected. That is a tax question nobody has answered.
- **Temporal hosting, encryption inside the cluster and the second region** are the three open decisions at the top of `docs/decisions.md`.

## Modules beside the core

A module whose state spans partitions runs as its own Raft group on the ledger nodes (`ic_raft::module`), with the same durable log, snapshots, membership changes and failover as a partition. A module that reads the core's events is fed them by the leader of its group: every member is shown each event of each partition once, in sequence, and the checkpoint is part of the group's replicated state. Each partition keeps its events until they are on the stream and every registered reader has them. `docs/modules.md` is the contract.

## Portal and app services (`ic-channels`, `ic-portal`)

Owned by the thread "Build portal and app services". Full description: [channels.md](channels.md); this section is the hand-over.

### What is built (on main)

- **Channel module** (`crates/channels`, library `ic-channels`): a deterministic state machine (`src/state.rs`, `ChannelState`) hosted on the ledger nodes as the module `channels` in its own Raft group, fed the core's events. It holds the directory of who relates to which policy (`src/directory.rs`), identity bindings, devices and signed approvals (`src/identity.rs`), staff authorisation with Cedar (`src/staff.rs`, `config/cedar/staff.cedar`), audience-scoped read models and per-party feeds for offline sync (`src/feed.rs`, scoping in `crates/readmodel/src/scope.rs` and `config/cedar/audience.cedar`, which the core thread owns; the channels thread made additive changes only), servicing requests that wait for the customer's signature, the held balance workbench (`src/workbench.rs`, addendum entry 4), and countersigned commands for other modules (`src/countersign.rs`).
- **Gateway** (`src/http.rs`, binary `ic-portal`, `src/main.rs`, settings in `src/settings.rs`): `/app/v1` (customer app API and sync protocol), `/adviser/v1`, `/employer/v1`, `/staff/v1`, `/ops/v1`, `/internal/v1` (module-to-channel commands), RFC 9457 problems, `Idempotency-Key` on every write, Entra ID for staff and modules, Entra External ID for customers, advisers and employers, dev identity headers only with `Identity::Dev`.
- **Web pages** (`ui/`, compiled in by `src/ui.rs`, run under a strict content security policy): `/adviser`, `/employer`, `/staff` (servicing console and workbench), `/ops` and `/status` (addendum entry 6), and staff consoles for other modules: `/pricing` (unit pricing), `/complaints` (complaints and conduct), `/finance` (month-end close, journals, books), `/claims` (queues, a claim, every claims action), `/reinsurance` (exceptions). The customer app is an API only; the native app belongs with the bank's app (the thread's own decision, not yet confirmed by Mitchell).
- **Console pattern for other modules.** No crate dependency on the module. `ic-portal` passes whitelisted questions and people's commands to the module on the nodes as JSON (`forward::Source`, `forward::Hosted`), under a console caller, with the person as actor and roles or seat derived from channels staff actions (`unit_pricing.*`, `complaints.*`, `finance.*`, `claims.work`, `reinsurance.*`). Commands only a module's own services send are refused at the portal. `tests/http.rs` runs each console against the module's real state machine through dev-dependencies and doubles in `tests/common/`. Claims are different: they live inside the core, so `src/claims.rs` asks every partition (`claims::Partitions`, needs `IC_PARTITIONS`) and sends `Command::Claims` under the caller `claims` to the partition holding the claim, replacing every `actor` with the signed-in staff ID and purpose.
- **Commission forwarder** (`src/forward.rs`): reads the commission module (`distribution`) and records engagements, licences, adviser structure, party status, held money, memo lines (claim overpayments, with their recovery status) and open conduct cases in the channel module; replays approved workbench actions there as each signer's own command.
- **Deployment**: `deploy/k8s/portal.yaml` (AKS, workload identity `portalClientId` in Bicep), compose service on port 8090. Line coverage of the crate 98.50% (2026-11-12); every test has a documented purpose.

### Seams with other areas

- **Core**: event stream to the channel module; `crates/readmodel` scope and the audience Cedar policy are the core thread's files.
- **Commission (`ic-distribution`)**: JSON contract tested in `tests/forward.rs`; the workbench and that module each count signatures (both ask legal above the threshold, for a new account of a party in liquidation or deceased, and for a release that leaves less held than claims is collecting). A refusal at replay closes the workbench action as failed with that module's code.
- **Claims**: questions `Queue`, `Claim`, `Work` (staff view), `Book`, `Report`, `Remittance`; StaffId in the claims authority book must equal the workforce directory ID.
- **Reinsurance**: reads exceptions from the module (not the event hub); abandonments need two people here, sent with `approved_by`.
- **Finance, unit pricing, complaints**: callers `finance-console`, `unit-pricing-console`, the complaints seats; details in channels.md.

### Not finished, in the order to take them

1. **Clinical detail in the claims console**: the staff view (`work`) is on the page; the cause, reason codes and evidence documents are not, because the claims module has no question for them. Ask the claims thread for one answered only after `open_file`, then show it.
2. **Customer master: routing and acting for another.** Self-service is built (`/app/v1/profile`, `/profile/commands`, `/profile/export`, `src/customers.rs`). The relay of ownership, assignments and dependants from the master into the channel directory is the master's and is on main (its 154f885); an owner or dependant ending is not relayed, because the directory has no command for it (adding `EndOwnership` and telling the customer master thread would close that). Routing is built: each submitted request asks the master and carries `route_to_person` (true also when the master cannot answer); owning modules must honour it (policy servicing, claims, group admin). Gap: a request prepared by an adviser or support person is routed by the submitter, not the approving customer. Not built: `GET /v1/parties/{id}/authority` before letting an attorney or guardian act in the app (the channel directory has no attorney relationship yet). Deployment must give the portal's identity the master's app role `portal`.
3. **Clawback attribution** (commission, `docs/distribution.md`): replacements are built end to end (staff page `/replacements` with `DecideReplacement` by the conduct officer; the adviser's own matches and dispute, carried as `DisputeReplacement` under caller `adviser-portal`, in `src/replacements.rs`). Not built: `AttributeAlteration {policy, engagement}`, which belongs with whoever carries out a reduction (policy servicing), since the portal only records the adviser's request. A request names the party who asked but not the engagement they acted under; policy servicing would look that up, or the request could carry it.
4. **Document download**: built (`src/documents.rs`, `/app/v1/documents/{id}/pdf` and `/staff/v1/documents/{id}/pdf` with staff action `documents.download`). Not built: links to documents on the pages, since no page lists document IDs yet; and the documents app registration, which the documents thread says is not created.
5. **Screens still missing**: group employer membership file upload, commission staff commands, underwriting, payments, investment ledger (unit holdings for the customer and adviser). Underwriting now publishes `proposal` and `proposal_life` rows (`docs/underwriting.md`); they reach devices through sync, and the adviser page shows each policy's proposals (status, whose move, received and expiry). Disclosure signing is built as an API (`src/disclosures.rs`, `/app/v1/proposals/{proposal}/lives/{life}/disclosures[/to-sign]`), agreed with underwriting (their intake verifies `ecdsa-p256`). The native app renders the questions; the portal has no web page for it, since the customer app is API only. Deployment: `deploy/k8s/portal.yaml` sets `IC_UNDERWRITING_INTAKE_URL` and `_SCOPE` (and `IC_CUSTOMERS_URL` and `_SCOPE`); the portal's identity needs the intake's app role `new-business` and the master's `portal`. Plain HTTP is allowed only to localhost and in-cluster service names.
6. **Workbench positions from other modules**: finance, reinsurance, group administration and claims do not yet publish their held or owed money to the workbench through `/internal/v1/commands`.

### Open questions for Mitchell

- Confirm the interface decision: web pages in this repo for advisers, employers, staff and operations; the customer app as an API and sync protocol only.
- Commission balances are assumed to be in one currency (`IC_PORTAL_CURRENCY`).

### Checks

- `cargo test -p ic-channels`; coverage `cargo llvm-cov -p ic-channels --summary-only` (CI floor 98% workspace-wide).
- Browser check, not run by CI: `cargo test -p ic-channels --test http serve_for_a_browser -- --ignored &` then `POLICY=pol-00000001 node crates/channels/tests/browser/check.js`. All pages passed on 2026-11-12.
- No known failing checks in this crate at hand-over.

## Unit pricing (`ic-pricing`)

Unit pricing decides which price is right. The core's investment ledger (section "Units and funds") decides what a price does. The module is the crate `crates/pricing`. It is hosted on every ledger node as the module group `unit-pricing` (one line in `ic_node::modules()`), because a price is one fact for every partition. Its driver is the binary `ic-unit-pricing`. The full reference is [unit-pricing.md](unit-pricing.md): file formats, commands, roles, questions and settings.

**Where the code is.**

- `fund.rs`: the module's terms, read from the `pricing` map of the core's own `ic_core::invest::FundDefinition`.
  - The terms are `previous_move`, `benchmark_gap`, `stale_asset_days`, `unexplained_move`, `error_threshold`, `launch_price`, `pricing_lag_days`, `time_zone` and `compensation_minimum_minor`. Unknown keys are refused.
  - It also holds the pricing calendar: weekends and the fund's non-pricing days, the cut-off and when a price is due.
- `valuation.rs`: the administrator's CSV file (`H`/`V`/`A`/`T` records, at most 4 MiB, refused whole if malformed).
- `strike.rs`:
  - strikes mid, buy (rounded up) and sell (rounded down);
  - runs the checks: units on issue (blocking), previous move, benchmark, stale assets and unexplained move (each holds the price);
  - hashes every input.
- `state.rs`: the deterministic state machine `PricingState`. It covers funds, valuations, prices and their statuses, suspensions, the per-partition publication outbox, price errors with their remediation record, holdings and flows from the core's events, extracts, idempotency keys (kept for 35 days) and a bounded event log.
- `command.rs`: commands, callers and roles.
- `host.rs`: the module's questions, `react()` and the core outbox. `react()` turns core events into module commands.
- `driver.rs`, `adapters.rs`, `service.rs`, `bin/ic-unit-pricing.rs`: the driver process.
- `extract.rs`: the holdings extract file.

The tests are in `crates/pricing/tests/`:

- `prices`, `errors` and `host`;
- `driver`, which uses fakes;
- `adapters`, which uses a stand-in for the Blob List API;
- `funds`, which checks that every shipped fund in `products/funds/` is accepted;
- `cluster`, an end-to-end test against a real node. It runs file intake, prepare, approve, delivery to the core, order execution, the holding and the extract written, and then the shipped binary under SIGTERM.

Line coverage was 98.3% at hand-over.

**The contract with the core.** All of it is on main.

- **Funds.** The core emits `InvestEvent::FundActivated` with `definition: Some(FundDefinition)` on each partition. `react()` turns it into the module's `ActivateFund`, keyed `fund:{id}:{version}:{content_hash}`. The same key comes from each partition and from `ic_pricing::command::activation()` (caller `config`), so the module holds exactly the versions the core holds.
- **Prices.**
  - An approved price is queued for each partition and carried by the node's host outbox as `InvestCommand::ActivateUnitPrice(UnitPrice { price_id, fund, pricing_date, version, buy, sell, mid, inputs_hash, supersedes })`. It is sent under caller `unit-pricing` with key `unit-pricing:{price}/{partition}/{attempt}`.
  - The core requires plain identifiers (letters, digits, `- _ . :`, at most 120 characters). So a slot is `{fund}:{date}` and a price ID is `{fund}:{date}.{version}`.
  - The host reports each outcome back as `RecordDelivery`. The price is `Active` once every partition has applied it. A refusal waits for the lead's `Republish`.
- **Orders.** The core runs `ExecuteOrders` and `ReplayOrders` itself, as a step of the close and from `ic-worker unit-orders` every fifteen minutes. This module never sends them. The replay threshold is the fund's `compensation_minimum_minor`, which the core sends as `materiality_minor`.
- **Events read.**
  - `UnitOrderExecuted` becomes a signed Buy or Sell movement.
  - `UnitOrderReplayed` becomes a Correction, keyed `{order}/replay/{to_price}`, that carries the amount borne and whether it was compensated.
  - From these the module keeps each policy's holding, each date's flows and each price error's remediation record.
- **Units on issue.**
  - The driver asks each partition `Question::UnitsOnIssue { fund }`. The reply is `units_micro`, `executed_through` and `pending`. The driver proposes `RecordCoreUnits` when a figure changes.
  - `executed_through` is the latest struck date with nothing pending for it or any earlier date, counting a switch buy whose sell has not run (the core changed it to this at 88c7474).
  - The units check for date D adds the partitions' units once every partition reports executed through the previous price's date. The holdings snapshot for D is taken once every partition reports executed through D.

**Maker and checker.**

- The roles are `unit_pricing_preparer`, `unit_pricing_approver` and `unit_pricing_lead`. The approver is never the preparer.
- Each held check is accepted by name with a reason. A blocked price cannot be approved.
- Approval re-checks four things: the valuation revision, the core's units, the previous price, and that no price for that slot is in flight. It is also refused when the fund is suspended, when an uncorrected price error is open on the fund, or when an earlier pricing day has no price and was not suspended.

**Price errors.**

- `RaisePriceError` names active prices. Each one is re-prepared as version n+1, which supersedes the old price.
- A correction below `error_threshold` becomes `Immaterial` and is never sent to the core.
- `CloseError` needs every correction active.

**On Azure.** The template is `infra/unit-pricing.bicep` and the Kubernetes file is `deploy/k8s/unit-pricing.yaml` (two replicas, workload identity, cluster token from Key Vault).

- **Inbound account.** It has a flat namespace and uses Entra ID only. Defender for Storage malware scanning writes its result as an index tag; the driver reads a file only once the tag says clean, refuses a file with a threat unread, and waits for an untagged one.
- **Administrator access.** The administrator writes over HTTPS as its own service principal. An ABAC role condition allows writes only under `valuations/` and no deletes or tag writes, so it cannot forge a clean tag. SFTP was dropped because it needs a hierarchical namespace, which has no index tags; Mitchell approved this on 12 Nov 2026.
- **Outbound container.** It is immutable for seven years. The driver writes `extracts/<fund>/<date>.csv` once.
- **Driver pass.** It runs every 30 seconds and has three steps: intake over the last seven days of folders, units on issue, and extracts. A fault in one step does not stop the others. Every proposal is keyed (`valuation-file:<name>`, `units:<fund>:<p>:<through>:<units>`, `extract-written:<id>`), so two drivers or a restart propose nothing new.

**Handed to other areas, and their state at hand-over.**

- **Finance close.** Done by the finance thread. `ic_finance::driver::unit_pricing_sources()` asks `{"q":"close_check","as_at":…}`. The answer is `{green, items}`, and the item kinds are `late_price`, `price_awaiting_action`, `open_price_error` and `extract_not_written`. `ic-finance-close` reports it as the close check `unit_pricing`. Aged pending investment cash comes from the core, not from this module.
- **Staff screens.** Done by the portal thread.
  - The page `/pricing` in `ic-portal` uses the routes `/staff/v1/unit-pricing/*` and sends the eight console commands as caller `unit-pricing-console`.
  - Roles come from the staff actions `unit_pricing.prepare`, `.approve` and `.lead`.
  - `crates/channels/tests/http.rs` (`the_unit_pricing_console`) runs the console against `PricingState`, so renaming a command or field here fails that test.

**Not finished.**

- `infra/unit-pricing.bicep` compiles in CI but is not referenced by `main.bicep`. It has not been deployed, and its Defender resource uses a preview API. The ABAC condition string has not been tested against a live account.
- `deploy/k8s/unit-pricing.yaml` has not been applied to a cluster.
- No real administrator is onboarded. That needs its service principal's object ID (`administratorPrincipalId`) and an agreed file layout; the current layout is our own.
- A definition the core accepts but this module refuses (bad pricing terms) is recorded as a refusal on the event's key, and the fund cannot be priced until a corrected version is activated. Nothing alerts on this yet.
- A movement that arrives for a date whose extract was already written still updates holdings. Its flow entry is never consumed, so the next extract's holdings are right but the earlier date's flows stay short. The core's `executed_through` rule should prevent this.
- Nothing alerts on a price that is late or a publication that was refused. Both show only in the `late`, `work` and `close_check` questions and in the driver's warn log.

## Privacy

Command payloads carry personal and health information. They never go to logs: a log line names a command, its sender and its key only. Ledger entries carry salted commitments to their payloads, so the chain can be verified without the payloads being readable from it.

## Area: documents, correspondence and complaints

Owner thread: "Build documents and correspondence". Detail lives in [documents.md](documents.md) and [complaints.md](complaints.md); this section is the map and the list of what is left.

### What is built, and where

- **`crates/documents` (module `documents`)**: templates versioned by content hash and approved by legal, product and compliance; rendering (`render.rs`, `template.rs`); the encrypted vault on Azure Blob with a Key Vault key per owner (`vault.rs`, `crypto.rs`); issue, supersede and correct; mandatory notices with proof of delivery and notice clocks that the core waits on (`state.rs`); channel fallback email, SMS, in-app, post (`channels.rs`, Azure Communication Services and a print outbox container); delivery reports from Event Grid (`POST /v1/delivery-reports`); notice defects and reopen windows (addendum entry 3, document side); retention, legal holds and erasure by key shredding; triggers from the core's events (`triggers.rs`); the report.
- **Avoidance letter**: the core's `PolicyAvoided` raises a trigger for `AvoidanceNotice` (a mandatory notice, copied to any lender), and an avoided policy counts as ended for retention. The wording (template of kind `avoidance_notice`) is not written.
- **Lender copies**: interests from the core's `Event::Interest` are tracked, and lapse (on entering grace), suspension and policy-ended letters must be copied to every party whose interest stands (`interested_party_missing`). Question `interested_parties`.
- **PDF** (`pdf.rs`): the stored text set on A4 with bundled Liberation Sans (`crates/documents/fonts/`, OFL), attached to emails that may carry text, written beside each print instruction, and `Driver::pdf` for download. Never stored.
- **Customer master seam**: `HttpDirectory` calls `crates/customers` (`GET /v1/parties/{id}/contact-points?purpose=notices&channel=…`), which answers name, destination, language, format and deceased. Nothing is sent to a person recorded as deceased.
- **`crates/complaints` (module `complaints`)**: register, versioned timers with escalation and automatic deadlock, referral to the scheme, courts and privacy regulator, remediation references, conduct cases including licence events (addendum entry 4) with sampled file review and regulator timers, the report. Idempotency keys in both modules are forgotten after 90 days.
- **The `ic-documents` service** (`service.rs`, binary is a thin `run_from`): the documents cycle, the delivery-report webhook, and the complaints relay (`complaints_relay.rs`: `Tick`, publish `cmp-*` to hub `complaints-events`, and pull licence cases from module `distribution` into `complaints` as caller `distribution`).
- **Deployment manifests**: `infra/documents.bicep`, `deploy/k8s/documents.yaml`. Never deployed.
- The complaints and conduct workbench is the portal thread's (`crates/channels`, page `/complaints`); its test runs against `ComplaintsState`, so renaming a complaints command, question or field breaks it.

### In progress when this was written (2026-11-12)

Letters the complaints register owes, on the branch `claude/project-thread-3jubfu` (uncommitted when this section was written; look for it on main before redoing it):
- `ic-complaints`: question `letters_due` (`ComplaintsState::letters_due`, `LetterDue`, `LetterKind`) lists acknowledgements not issued, deadlock notices not issued, and letters owed to each customer of a completed licence case. Caller `documents` may send `RecordAcknowledgement` and `RecordDeadlockNotice`.
- `ic-documents`: `complaint_letters.rs` (`LetterIssuer`, `run_letters`) asks for that list, issues each letter from templates `complaint-acknowledgement`, `complaint-deadlock-notice`, `licence-event-notice` with ids `complaint-ack:<c>`, `complaint-deadlock:<c>`, `licence-notice:<case>:<party>`, and records it. `Driver::issue` keys its command on the document id plus a digest of the request, so a refused request that is corrected is judged again.
- Remaining: a clippy failure in `crates/documents/tests/complaint_letters.rs` (a mutex guard held across an await in `the_step_runs_until_stopped`), then the full checks, coverage and merge.

### Not built

- **Letter wording.** The three templates above, and every other template, are content for legal, product and compliance to write and approve; until a template is active its letters are left for people and the pass logs it.
- **Letters other modules ask for.** Built 2026-11-13 (`requested_letters.rs`, `Command::RequestLetter`, agreed with underwriting and reinsurance): kinds and templates in `model::REQUESTED_KINDS`, contract in `docs/documents.md`. Not done: the wording of every template, and the callers' driver sides (theirs). Decided by Mitchell: listings go by SFTP and an API in the reinsurance service, never by email.
- **Document download.** Built 2026-11-13: `GET /v1/documents/{id}/pdf` (`service.rs`, `auth.rs`), Entra roles `portal` (with `x-ic-party`, owner or recipient only, else 404) and `staff` (with `x-ic-actor`, `x-ic-purpose`, never clinical). Not done: the Entra app registration, and the portal calling it.
- **Typeface.** `IC_DOCUMENTS_FONT_REGULAR` / `IC_DOCUMENTS_FONT_BOLD` set the insurer's TrueType faces (checked at start); the faces themselves are the insurer's to supply. Scripts outside the face go as text without a PDF. PDFs are not tagged (PDF/UA).
- **Audio and braille** are recorded and passed to the print house; not produced here.
- **Outcome, update and other complaint letters** (`RecordUpdate`, outcome letters, forum bundles) are still issued by handlers through the workbench; only acknowledgement, deadlock and licence letters are automatic.
- **Loading history.** A module added to a running cluster starts at the oldest event each partition still holds.

### Needs Mitchell or a named person

- Retention periods per document kind (legal); real complaint timer values (compliance; the ones in tests are short).
- Email domain verification and the SMS number for Azure Communication Services.
- The Communication Services role ID in `infra/documents.bicep` is a parameter and unverified (`az role definition list`). The template is deployed twice because Event Grid validates the webhook.

### Working notes

`protoc` cannot be downloaded in the cloud environment, so local checks run with `--exclude ic-workflows` and CI covers that crate. Coverage for these two crates was 98.2 to 98.6 percent before the letters work.

## Customer master (`ic-customers`)

The one place the insurer knows who a person or organisation is. The core and every module know people only by an opaque party ID (`pty_…`). It is a stateless HTTP service with its own Azure PostgreSQL server, not a module on the ledger, because personal data must be erasable and the ledger is append-only. Full description and API: [customers.md](customers.md).

**Built, on main.**

- `crates/customers/src/`: `model.rs` (party, fields and classes, relationship kinds), `party.rs` (the decision logic: commands to events, bitemporal facts, merges), `event.rs` (event bodies, the public event, the hash chain), `crypto.rs` (AES-256-GCM per party, commitments, HMAC blind indexes), `keys.rs` (data keys wrapped by Key Vault, cache, rewrap), `matching.rs` (probe keys, Jaro-Winkler and Soundex scoring, bands 80/40), `validate.rs` (normal forms, IRD check digit), `store.rs` (PostgreSQL: streams, projection, idempotency, reviews, outbox, audit, shredding, death relay rows), `db.rs` (TLS plus Entra token for Azure, loopback-only plain connections, pool, migrations under an advisory lock), `access.rs` with `config/cedar/customers.cedar` (purpose-based access), `auth.rs` (Entra JWT), `service.rs` (axum routes), `retention.rs` (schedule and write-once erasure register), `relay.rs` (deaths to claims, relationships to the channels directory), `work.rs` (outbox publishing, retention sweep, death and directory relays, marker prompts, process start), `config.rs`. Migrations in `crates/customers/migrations/`.
- Contracts in use: group schemes' `POST /v1/parties/match` (tested by group against the real service in `crates/group/tests/driver.rs`, which includes `crates/customers/tests/common/mod.rs` by path, so keep `world()`, `World.router`, `person`, `token`, `get` and `AUDIENCE` stable); documents' `GET /v1/parties/{id}/contact-points` returning `{name, destination, language, format, deceased}` or 404; claims' `mark_deceased` / `retract_death`; portal self-service through `x-ic-party`.
- Deaths to claims: every death recorded here, whoever recorded it, goes to every core partition as `ic_claims::Command::RecordDeath` from caller `customer-master` through the ledger cluster (`IC_CLUSTER_NODES`, `IC_CLUSTER_TOKEN_FILE`). Pending deaths sit in `death_relay`; each attempt has its own idempotency key because the core remembers a refusal (for example `business_date` before the day is opened) under its key. Retractions go the same way as `RetractDeath` for the recorded date (migration `003_retractions.sql`).
- Relationships to the channels directory: owners, assignees (added and ended) and dependants with their age of control on a policy go to the `channels` module as `RecordOwnership`, `RecordAssignment` and `RecordDependant` from `Actor::Module("customer-master")`, through `directory_relay` (migration `006_directory_relay.sql`), in order per party. `relay.rs` tests this against a real node running the channels module. Not sent: an owner or dependant ending (the directory has no command for it); `BindSubject` is the identity service's.
- Deployment: `infra/customers.bicep` (own PostgreSQL Flexible Server with Entra-only sign-in and TLS required, `customers-kek` in Key Vault, `customers-index-key` secret, hubs `customer-events` and `customer-priority`, write-once container `customers-erasures`, managed identity with read on `cluster-token`), `deploy/k8s/customers.yaml` (in `kustomization.yaml`). After the first deployment someone in the database admin group creates the service's role with `pgaadauth_create_principal` (commands at the top of the Bicep file).
- Tests: `crates/customers/tests/` (api, lifecycle, party, db, process, relay) against a real PostgreSQL named by `IC_TEST_POSTGRES_URL`; `relay.rs` runs a real ledger node. CI starts `postgres:16` in `ci.yml`. Crate line coverage is 98.0% (98.01% with the directory relay), at the floor: new code here needs its own tests.

- Vulnerable customers (persona matrix VUL): structured markers `{kind, review_on, adjustments}` with routing that shows adjustments to anyone serving and the kind only to `vulnerability_support`; third-party concerns (`record_concern`) opening `consider_marker` prompts in `/v1/reviews`; representatives with basis, document, scope and dates, and `GET /v1/parties/{id}/authority` answering allow, refuse or route. Callers (portal, policy servicing, payments) must ask `authority` before acting for someone else; that wiring is theirs.

**Not built yet.**

- Marker review dates (VUL-017) are built: `marker_reviews` holds only the date, and each sweep opens one `consider_marker` prompt (reason `review_date`). The outcome screen (renew, change, remove) is the portal's. Bereavement prompts on a joint policy (VUL-015) need claims or the death relay to open a `consider_marker` prompt for the surviving owner.
- Changing the blind-index key: needs a command that decrypts every party's facts and rebuilds `blind_index`. `Store::reindex` exists for one party; a batch over all parties under the new key, then swapping the key, is the missing part.
- Locking the erasure register's immutability policy in prod: a deliberate, irreversible step for a person once the retention period (default 10 years) is agreed.
- Group schemes now sends `mobile_phone` (optional column, +64 form). Its test against the real service (`crates/group/tests/driver.rs`) expects a mobile with matching given name and birth date but a different family name to score 55 (possible, not linked); changing the weights or the 80/40 bands in `matching.rs` means changing that test too.
- Retention periods in `retention.rs` (7 years for most classes, 5 for verification evidence, 2 for prospects) were confirmed by Mitchell on 13 Nov 2026.
- Known check status: `ci.yml` on main passed at `ea9de28` with the PostgreSQL step, the customer master's tests, the coverage floor and `customers.bicep` compiled. The death and retraction relay, marker reviews and the directory relay (migrations 002 to 006) landed after that run.

## Area: adviser commission and held balances (`ic-distribution`)

State as at 12 November 2026. The full description is [distribution.md](distribution.md); this section is the handover: what is built and where, what is not, and what somebody finishing it needs to know.

### What is built and where

Everything is in `crates/distribution`, hosted on the ledger node as the module `distribution` (`host.rs`, `module_spec()`, listed in `crates/node/src/lib.rs`). Staff policy is `config/cedar/distribution.cedar`.

| File | What it holds |
| --- | --- |
| `state.rs` | The state machine: `Envelope` (key, actor, command), every `Command` and `Event`, `Settings`, tasks and cases, which module or staff action may send each command. |
| `structure.rs` | Parties, dated relations and engagements, licence records and how a licence problem cascades. |
| `rules.rs`, `model.rs`, `config.rs` | Distribution rules and fragments, schedules, books, layer and exposure records; change sets (propose, approve twice, dry run); writing a layer (`write_layer`); change of adviser; book moves and merges. |
| `commission.rs` | Reading the core's events (`ingest`): earning, clawback, reversals, premium handed back, waived premium, layers ended, reduced or rewritten, claims overpayment memo lines, backdated rules, licence events moving paid money to held. |
| `replacement.rs` | The rule for replacements nobody declared, the adviser's dispute and its decision. |
| `statement.rs` | Statements, payment instructions, sweeps, payment settled and returned. |
| `workbench.rs` | The held balance workbench: positions, proof of debt, typed actions with signatures, the daily aged control, month-end blocks. |
| `authority.rs` | Roles, grants, limits and the Cedar check, with every decision recorded. |
| `host.rs` | Hosting, the questions other areas ask (`Question`), and the outbox that carries postings to the core (`OUTBOX`). |

Tests are in `crates/distribution/tests/` (`commission`, `config`, `edges`, `host`, `lifecycle`, `replacement`, `statements`, `units`, `workbench`), all against the real core in the simulated cluster (`tests/common/mod.rs`, `World`). Module line coverage was 98.7% when last measured. CI and the failure drill were green on main at `ea9de28`, which includes everything described here.

### Seams with other areas

- **Core.** The module reads events and moves money only through `PostCounterparty`, building balances only from `CounterpartyPosted`. Core additions made for it, all additive: `Command::PostCounterparty`, `Event::CounterpartyPosted`, `net_by_cover_type` on `ReceiptApplied` and `SuspenseApplied`, `Event::PremiumUnapplied` (from `unwind.rs` and from `billing.rs` `settle_credits`), `Event::LayerReduced` (from `reduce.rs`), and `net:<cover_type>` keys on the `premium_waived` detail (`claims.rs`). It also reads `LayerAdded.continues`, which the core thread added.
- **Portal** (`crates/channels/src/forward.rs`) publishes the workbench and adviser directory from the questions and replays approved workbench actions one signature at a time.
- **Finance** (`crates/finance/src/driver.rs`, `distribution_sources()`) reports `sweep_check` and `aged_held` to month-end.
- **Complaints** (`crates/documents/src/complaints_relay.rs`) opens the conduct case from `conduct_cases`.
- **Claims** publishes overpayment facts as module events; the workbench shows them as memo lines and they stay in the claim ledger.
- None of these readers depends on the crate, so the shape of an answer must be agreed with its reader before it changes. Adding a field to a public type breaks other crates' tests, so run `cargo clippy --workspace --all-targets --locked --exclude ic-workflows -- -D warnings` before pushing such a change.

### Not finished, with what is needed to finish it

1. **Nothing sends `AttributeAlteration`.** Clawback by cause (rule switch `clawback_by_cause`) only applies when the channel that submits a reduction or a cover given up on an adviser's behalf first tells the module which engagement it is, as `policy-servicing`, on the same business date and before the core command. The portal thread has been told. Until it does, clawback falls on the original liable party, which is the design's default.
2. **Nothing in the portal uses the replacement rule's answers yet.** `WriteLayer` can answer with `ReplacementMatched`; `DisputeReplacement`, `DecideReplacement` and the question `replacement_matches` exist. The adviser's notice at submission and the oversight page are portal work. The portal sends `DisputeReplacement` itself as `adviser-portal`, naming the adviser, which the module checks against the layer's writing engagements.
3. **Limits of the replacement rule** (`replacement.rs`). It matches in both orders: when new cover is written (`match_replacement`) and when cover is given up after new cover was written, as when an adviser moves a customer who is still in force (`match_given_up`, which takes back initial commission already paid on the matched amount). Where part of a layer given up is matched, that share of its clawback is given back and that share of its exposure carries on (`continue_exposure`); where cover was reduced and not removed, the clawback on the part cut stands even if it is matched later, because the reduction's exposure items have already moved to the layer that carries on. Trail already paid before a match is overturned is not moved. A policy reinstated after its cover was matched raises the task `reinstated_after_replacement` and is not reversed automatically. The person behind a life is read from `PolicyIssued.lives`, `MemberJoined` and `MemberIdentified`.
4. **Not run on a real multi-node cluster.** Hosting and the outbox are tested through the functions the host calls, in the simulated cluster. The core thread has been asked to include the module in its integrated Docker run on the Mac and to confirm three things: commission is posted after a receipt on any partition, the outbox drains after a leader change, and `status` answers on every node. No result yet.
5. **A release does not net against the claim ledger.** A release that would leave less held than claims is collecting needs legal's signature (`short_of_owed_elsewhere` in `workbench.rs`), but the offset itself is done in claims. Reinsurance recoveries and scheme arrears appear on a position only if their owners post them with `PostCounterparty` and describe them with `DescribePosition`; neither does yet.
6. **Commission is its own entry**, posted after the premium event and idempotent on it, not lines in the premium entry. Making the two atomic would mean the core calling out during a receipt, which the module contract in [modules.md](modules.md) does not allow.
7. **Rules and schedules are data, not Starlark.** Rules now carry worked examples that must come out right before a rule activates (`rule_examples_required`, on by default; see docs/distribution.md). Schedules carry none of their own and are checked through the rules' examples. Rules shipped from an older system need examples written before they load, or the setting turned off for the migration.
8. **One currency** (New Zealand dollars). History is now bounded: detail older than `history_days` (seven years by default) that nothing can reach is let go of monthly, with balances brought forward (see docs/distribution.md, "History is bounded"). Adjustment references are kept for ever so none can be reused, and a policy with money still held is never let go of. If the business wants statement detail answerable from the module for longer or shorter than seven years, it is one setting.
9. **No screens of its own.** Staff commands go through the generic module command route; pages are the portal's.

Volume-based overrides are not built on purpose: the design notes leave them out.

Also built since this section was first written: a policy avoided for non-disclosure (`PolicyStatus::Avoided`, from the core's `AvoidPolicy`) has every line of commission on it reversed and any clawback given back (`policy_avoided` in `commission.rs`).

### Open questions for Mitchell

- The replacement rule's settings are my defaults and the design notes say to confirm them with compliance: a window of 12 months (`replacement_window_months`), an amount tolerance of 5% where the advisers are unrelated (`replacement_amount_tolerance_ppm`), and "related" meaning the same provider or any provider under the same group (`Structure::related`). Mitchell confirmed the 12-month window and the 5% tolerance on 13 November 2026.
- Answered by Mitchell, 13 November 2026: a disputed match is decided by whoever holds `case.manage` (the conduct officer or compliance). On paying held money to a party claims is pursuing, Mitchell left it to Claude, provided it can be changed later: the default is legal's signature, and an approved change set (`Change::ReleaseOwedElsewhere(Refused)`) switches it to refusal.

### Known failing checks

None. Things that catch people out: local builds need `--exclude ic-workflows` (no `protoc`) and `IC_SKIP_TEMPORAL_TESTS=1`; a posting reference may hold only letters, digits and `- _ . :`, so a layer key (which has `/`) never goes in one unchanged; the core's life IDs are per policy, so lives are matched by the party behind them; and a test that injects a core event by hand must do it last, or on a partition of its own, because injected events use up sequence numbers.

## Area: reinsurance (`ic-reinsurance`)

Written 12 November 2026 so the area can be finished without the thread that built it. The detail of how each part behaves is in [reinsurance.md](reinsurance.md); this section is the map, the seams and what is left.

### What is built, and where

Everything below is on `main` and passed CI at `ea9de28` (run 192, 12 November). The crate is `crates/reinsurance`; treaties and reinsurer rate tables are Starlark files in `treaties/`.

| Part | File | What it does |
|---|---|---|
| State machine | `src/machine.rs` | `Reinsurance`: every command (`Command`), who may send each (`allowed_callers`), idempotency by envelope key, exceptions (`set_exception`), the notice types and which audience each is for (`Notice::audience`). |
| Treaties as code | `src/treaty.rs`, `src/loader.rs`, `src/treaty.star` | Typed treaty terms, aggregate treaties (catastrophe, stop loss) and rate tables loaded from Starlark. Each treaty file carries scenarios that are checked when it is loaded. |
| Event feed | `src/feed.rs` | Reads every partition's core events (issue, layers, membership, status, transfer, unwind, billing, claims facts), de-duplicated by sequence, refusing a gap. `continue_layer` carries a cession across a change of underwriting terms. |
| Cession calculation | `src/cession.rs`, `src/math.rs` | A pure function of the covers on one life: retention filled in a fixed order, surplus and quota share, facultative shares, unplaced cover. Produces a timeline per cover. |
| Capacity step | `src/capacity.rs` | Reservations for proposals, the answer underwriting reads before offering terms, facultative referrals and their terms. |
| Ceded premium | `src/premium.rs` | Per billing period from the timeline, posted as differences and reversed with the billing it followed. |
| Recoveries | `src/claims.rs` | Claims facts read from the core's claims events (`fact_from_event`), expected and actual recoveries, consent and large-claim notices, disputed items, aggregate treaties. |
| Reinsurer accounts | `src/account.rs` | Statements, listings, supplementary listings, agreement, hold, settlement instruction (maker and checker), settlement recorded or returned, profit commission, migrated cessions and opening balances. |
| Views | `src/views.rs` | Exposure, scheme concentration, reconciliation of the module against itself, treaty comparison, and `sweep_check` for the finance close. |
| Outbox | `src/outbox.rs` | Numbered instructions for the core and notices for everyone else, kept in replicated state until acknowledged; refused ones set aside with an exception; retry and abandon with a reason and an approver. |
| Host | `src/host.rs` | The module as `reinsurance` on the ledger nodes (registered in `ic_node::modules()`), its questions (`Query`) and how core events become commands (`react`). |
| To the core | `src/delivery.rs` | Instructions as the core commands `record_cession` and `post_reinsurance` (`crates/core/src/reinsurance.rs`), and notices for claims as `Command::Claims(Reinsurance(..))`. |
| Driver | `src/driver.rs`, `src/carriers.rs`, `src/service.rs`, `src/bin/ic-reinsurance.rs` | The stateless process `ic-reinsurance`: carries instructions to the partitions and each notice to its audience. `ClusterAudiences` reaches the group, underwriting and payments modules; `Carriers` adds write-once Blob Storage for records and the event hub for staff. |
| Azure and deployment | `infra/reinsurance.bicep`, `deploy/k8s/reinsurance.yaml` | The driver's own managed identity, the write-once container `reinsurance-records`, the event hub `reinsurance-events`, the reinsurers' SFTP account, and the deployment with its service account, cluster token and the reinsurer API's Service and network policy. |
| Reinsurer API | `src/reinsurer_api.rs` | Reinsurers read their own statements and listings, signed in with their own Entra clients; served by the driver process. |

Tests are in `crates/reinsurance/tests/` (one file per part, `common/mod.rs` holds the `Bench`), with the module against the real core in `tests/core.rs`, against a real node and the shipped binary in `tests/cluster.rs`, and the wire formats against the other real modules in `crates/node/tests/between_modules.rs`. The core's side of the two commands is tested in `crates/sim/tests/reinsurance.rs`. The crate had no uncovered lines by `cargo llvm-cov -p ic-reinsurance -p ic-node --show-missing-lines` on 12 November.

### Seams with other areas

Each is JSON in the other module's wire format with no crate dependency, unless said.

- **Core.** In: the event feed. Out: `record_cession` and `post_reinsurance` from caller `reinsurance`. The core keeps a refusal against the idempotency key, so a refused instruction is never retried under the same key; the module re-sends a cover's whole record after a transfer. `Event::LayerAdded.continues` (added by underwriting) names the layer a rewritten one replaces.
- **Claims** (in the core). In: `Event::Module { module: "claims" }` kinds listed in `reinsurance.md`. Out: `ReinsuranceNotice` commands (large claim, consent required, consent, disputed item raised and resolved). An unreadable fact opens `claim_fact:<claim>`.
- **Underwriting.** It reserves capacity, confirms it at issue and reads the answer (`Query::Capacity`, `Query::FacultativeTerms`); the driver sends it `facultative_terms_recorded`. Agreed 12 November: an alteration or a reinstatement reserves no capacity, because a lapse suspends a cession and does not release it.
- **Group administration.** The driver sends `require_underwritten_excess`; group sends scheme terms (`SetSchemeTerms`).
- **Payments.** The driver offers a net due to a reinsurer as `offer_payment` (payment `ri-stmt-<n>`, source `reinsurance:statement:<n>`, payee the reinsurer's ID). The payments pump sends back `record_settlement` and `settlement_returned` as caller `payments`. Pinned on their side in `crates/payments/tests/intake.rs`.
- **Finance.** Its driver asks `{"sweep_check": {"period_end": ..}}` for the close check `reinsurance_sweep`. The reinsurance account roles are mapped in `config/finance/chart.star`.
- **Portal.** The staff console `/reinsurance` in `ic-portal` (`crates/channels/src/reinsurance.rs`) reads `exceptions`, staff notices, `abandoned`, `abandoned_notices` and `sweep_check`, and sends `retry_notice`, `record_settlement`, `facultative_reinsurer_told`, and the two abandon commands after a second person signs. Its tests run against this crate as a dev-dependency.

### Not finished, with what is needed to finish it

None of these is started unless it says so. They are ordered by how much they matter in production.

1. **Alerts are built but go to nobody yet.** `infra/reinsurance.bicep` has two scheduled-query alerts on the Log Analytics workspace (`logAnalyticsWorkspaceName`, default `<name>-logs`): `reinsurance-exception-opened` (each `exception_opened` notice in the driver's log under target `reinsurance.notice`, split by code) and `reinsurance-delivery-stopped` (three failed or stopped delivery passes in fifteen minutes). They notify an action group mailing `alertEmails`, which is empty until someone is named on call (open question below). The query reads both `ContainerLogV2` and `ContainerLog` because the cluster's table depends on its monitoring settings. Not yet proven against a deployed workspace.
2. **`infra/reinsurance.bicep` has never been deployed, and could not be compiled where it was written.** The cloud container may not run the bicep CLI, so CI is the only compile check; it passed in run 192. To finish: deploy it into the resource group (parameters are in the file's header), put `reinsuranceIdentityClientId` into `deploy/k8s/reinsurance.yaml`, and in production lock the retention policy on `reinsurance-records` by hand.
3. **Built: a refused instruction holds only its lane.** Each cover's cession record is a lane and money on a partition is another (`Instruction::lane`); the driver delivers the other lanes past a held one and acknowledges them with `AcknowledgeDelivered` (`driver.rs`, test `a_refused_instruction_holds_its_lane_until_a_person_decides`). Left: a refused posting still holds all money on its partition; split by treaty and reinsurer if that proves too blunt.
4. **A module added to a running cluster misses earlier events** and raises `events_not_seen`. Hosted from the cluster's first start, as now, it misses nothing. To finish it needs the host (`ic_raft::module`, the core thread's) to replay from the cold tier or Event Hubs; nothing in this crate changes except removing the exception.
5. **Built: money due from a reinsurer is matched by payments.** Statements name `payment_reference` `ri-stmt-<n>`; payments' reconciler (main at a8cd252) sends `record_settlement` with a negative `amount_minor` for a credit line quoting it, under key `payments:received:<account>-<date>-<line>`; a refusal leaves the receipt unidentified for a person. Left: money a reinsurer owes is still not requested from it; the statement document is the request, and sending it is item 9.
6. **Built: settlement clearing is cleared against the bank by this module.** Decided here because payments' docs put such entries with the requesting module. Core (additive): posting kind `settlement_cleared` with `bank_account` on `post_reinsurance`. The module issues it per partition on `record_settlement` with `bank_account`, reverses it on a return, and opens `settlement_not_cleared:<n>` when no account was given (`clear_settlement` names it). Tests: `a_cleared_settlement_moves_clearing_to_the_bank` (sim), `a_settlement_recorded_without_its_bank_account_is_cleared_when_it_is_named`, and the end-to-end sweep test in `crates/reinsurance/tests/core.rs`. Payments sends `bank_account` with settlements and receipts since 13f751a.
7. **Built: a returned settlement is offered again by a person's decision.** `reoffer_settlement` (caller `reinsurance-ops`) offers it to payments as `ri-stmt-<n>-<try>` with the same source; payments needed no change because it reads the statement from the source (`account.rs`, test `a_returned_settlement_is_offered_again_as_a_new_payment`, and the payments format in `crates/node/tests/between_modules.rs`).
8. **Built: recoveries on a claim reopened after a reversed admission.** `ClaimFact::Reopened` on a reversed claim opens `reopened_after_reversal:<claim>`; `resume_recoveries` (caller `reinsurance-ops`) reopens it once its disputed items are resolved (`claims.rs`, test `recoveries_resume_on_a_reopened_claim_only_when_its_disputes_are_settled`). Left: nobody is alerted to the exception other than through the staff feed and the portal console.
    Also built since this list was written: **reversal from inception when a policy is avoided for non-disclosure** (`Event::PolicyAvoided`, `feed.rs`, test `an_avoided_policy_is_reversed_from_inception`). Covers end on their start day; ceded premium, commission and billed recoveries come back, and the life's other policies get the retention. Left for a reinsurance manager to confirm: recoveries are taken back on the account without a disputed item, and premium goes back in full even when the customer's premium is kept (see `docs/reinsurance.md`).
9. **Built: statements reach reinsurers.** Statements and listings are left over SFTP (`carriers.rs` `Outbound`, `infra/reinsurance.bicep`, test `statements_and_listings_are_left_for_their_reinsurer_to_collect`), the recommended option of the open question below, reversible if Mitchell chooses otherwise. The covering letter (totals only) goes through documents' `request_letter` (`driver.rs` `documents_command`, tests `a_statement_asks_documents_for_a_covering_letter_to_the_reinsurers_party` and `crates/node/tests/between_modules.rs`). Mitchell decided on 13 November: SFTP and an API as well. The API is built (`reinsurer_api.rs`, tests in `tests/reinsurer_api.rs`, deployment in `deploy/k8s/reinsurance.yaml`): each reinsurer signs in with its own Entra client and reads only its own statements and files. Left: each reinsurer's organisation party on the panel, SSH key in `sftpReinsurers`, and Entra app registration (a certificate it holds, the role `Reinsurer.Statements.Read`, its client ID in `IC_REINSURANCE_API_CLIENTS`), which need the real reinsurers; the API's own app registration and an ingress route for it; the module does not read back whether the letter was delivered (staff see it in documents).
10. **Listings are filed in parts.** A listing longer than the treaty's `listing_part_rows` (default 2,000; `statements(..., listing_part_rows = n)`) is filed as `statement-<n>-listing-<k>-of-<m>.json`, and the statement names each part with its hash and row count (test `a_long_listing_is_filed_in_parts_the_statement_names`). Left: `cut_statement` still walks the whole ceded book in one command; at a few hundred thousand covers that command should be split per partition.
11. **Original premium before discounts.** The core's billing event carries premium after discounts only, so a treaty priced on original premium uses the discounted figure (`premium.rs`). Needs the core to send both.
12. **Claims on continuation policies and provider recoveries.** Claims' `claim_reserved` now carries `continued_from_policy` and `continued_from_life` (13 November), naming the scheme policy and member a continuation policy carries on from, so such a claim could be ceded under the scheme's treaty with the member's history. The core does not fill them yet; the module does not read them. Also undecided here: whether a reinsurer shares a provider recovery (claims' `claim_offset` entries and `overpayment_raised` with cause ProviderBilling). Today the module does not pass either to reinsurers.
13. **Not built because the core has nothing to cede:** joint-life cover (the design counts it in full against each life) and longevity reinsurance for annuities.

### Open questions for Mitchell

- Settled without Mitchell, on payments' rule that the requesting module posts its own bank entries: reinsurance clears `int/reinsurance_settlement_clearing` against the bank on `record_settlement`. Say if finance should own it instead.
- Should a change of underwriting terms on facultatively placed cover hold the change until the reinsurer answers? Today the cession carries on and an exception asks a person to tell the reinsurer.
- Who is on call for reinsurance exceptions, and by what channel, so the alert in item 1 has somewhere to go?
- Decided by Mitchell (13 November): listings reach reinsurers by SFTP and by an API. Both are built (item 9). Documents does not email listings.

### Known failing checks

None known. CI run 192 at `ea9de28` passed with all of this area's commits in it. Locally the crate is checked with `--exclude ic-workflows` and `IC_SKIP_TEMPORAL_TESTS=1`; `ic-node` now needs `protoc` (through `ic-finance`), which `apt-get install protobuf-compiler` provides in the cloud container. CI runs the whole workspace.

## Underwriting and new business (`ic-underwriting`)

Owned by the thread "Build underwriting and new business". The full module description and its known gaps are in [underwriting.md](underwriting.md); this section is the summary someone needs to carry the work on.

**Built, on main.**

- `crates/underwriting`: a deterministic state machine hosted on every ledger node as module `underwriting`. Proposals from quote to policy for every entry point (new business, increase, alteration, reinstatement, group excess, migrated cover), disclosure with versioned question sets, rules with their own version and two-approver activation (`config.rs`, `prelude.star`, `shipped.rs`), referral queues, authority limits and countersigning (`authority.rs`, `casework.rs`), evidence requests and chasing (`evidence.rs`), offers signed by the applicant, interim cover, delegated underwriting (`special.rs`), non-disclosure review with two seniors for a remedy, peer review, the exclusion library shared with claims (`codes.rs`, `ExclusionLibrary::for_claims`), the addendum's existing-cover prefill with consent (entry 2) and adviser licence loss (entry 4), and the clinical store over Azure Key Vault wrap/unwrap and Blob Storage (`clinical.rs`).
- The effects driver, binary `ic-underwriting` (`driver.rs`, `service.rs`, thin `src/bin`, `deploy/k8s/underwriting.yaml`). It reads the module's numbered outbox, carries each effect under a key derived from the effect, acknowledges what was delivered, parks what cannot be delivered with a reason for a person, and runs the daily sweep (lapse, chase, escalate) as caller `timer`. Routing: capacity reserve, release, confirm and risk factors to module `reinsurance`; new business to the core as `IssueQuoted` (refused `product_changed` if the product moved under the quote); increases as `AddUnderwrittenLayer`, alterations as `AlterUnderwrittenTerms`, reinstatement decisions as `ApproveReinstatement`, all caller `underwriting` and answered back to the module as `PolicyIssued` or `IssueRejected` (caller `issuer`); group excess terms and endings to module `group` as `RecordExcessDecision`, key `uw:{effect key}`, with the member's signed consent to the employer seeing a loading.
- Core commands this thread added (additive, in `crates/core/src/underwritten.rs` unless noted): `IssueQuoted`, `IssuePolicy.migrated`, cancel by `underwriting` (ends interim cover), `AddUnderwrittenLayer` (a layer from today, the days left of a billed period billed as an adjustment), `AlterUnderwrittenTerms` (each layer on other terms ended and written again with the same sum, origin and rating date; the days left billed or credited; `LayerAdded.continues` names the layer carried on, which reinsurance uses to keep the cession), `ApproveReinstatement` (decision recorded on `LapseRecord.approval`; `Reinstate` in `lifecycle.rs` refuses `underwriting_required` past the product's `reinstatement_evidence_free_days` without an in-date decision, and applies its terms from the reinstatement day). Product feature field `reinstatement_evidence_free_days` in `crates/product`. Tests: `crates/sim/tests/underwriting_door.rs`, and in `crates/underwriting/tests/`.
- `infra/underwriting.bicep`: the clinical identity, storage account and wrapping key. Compiled by CI only; never deployed.

**Seams with other areas.** Reinsurance owns capacity and the treaty; alterations and reinstatements reserve none because the cover is already ceded (agreed with reinsurance, 2026-11-12). Claims owns cause coding and reads the exclusion library through `for_claims()`; `tests/libraries.rs` holds both matchers to the same answers. Group administration opens excess proposals as caller `group-admin` with id `grp-uw:{scheme}:{case}` and sends `policy_issued` back; its contract tests deserialize this crate's `Envelope` and `Effect` and include `tests/common/mod.rs` by path, so renames there break `crates/group/tests`. The core thread owns the ledger side of non-disclosure remedies (below).

**Not finished, in the order to do it.**

1. Non-disclosure remedies are routed: the core's `AvoidPolicy` and `VaryForNonDisclosure` (core thread, 13ce2de) are sent by the driver under `{review}/remedy`; refusals are parked for a person. Paying out a refund held in policy suspense is the existing suspense refund, started by a person.
2. Evidence-free reinstatement days are set (term life 60, health plan 30, group life none; reasons in underwriting.md). Still to build: collections opening a reinstatement proposal by itself when `Reinstate` is refused `underwriting_required`.
3. Staff notices go to the `underwriting-notices` event hub; letters go to documents as `request_letter` (agreed 13 Nov; both sides on main, pinned by a contract test against the real documents state machine in `tests/notices.rs`). Still parked: evidence requests and delegate file requests.
4. The disclosure intake `ic-underwriting-intake` is built (seals signed answers in the clinical store and submits them; `deploy/k8s/underwriting-intake.yaml`). Still to do: deploy `infra/underwriting.bicep` (bring it into `infra/main.bicep`; the driver needs its notices hub), have the portal call the intake with the life's device signature, and a staff console page for `POST /v1/disclosures/open` (an underwriter reading a disclosure, logged by the module first).
5. Profiles naming each underwriter's party (supported, none set yet), portal pages that render the progress rows underwriting now publishes (kinds `proposal`, `proposal_life`; portal work), and an interim cover product.
6. A scheme member's loading can move to them (`reoffer_excess_terms`, agreed with group 13 Nov, both sides on main). Not yet: reinsurance is not told the layer number changed when the replacement layer is written. Takeover amounts and reinsurance's reference on the excess entry are read (an excess within the takeover amount is accepted without evidence); the reference does not yet travel on the capacity request, which needs a field from reinsurance.
7. Terms refused for `billing_behind` are retried by the daily sweep, up to five attempts, then left to a person.

**Open questions for Mitchell.** Whether an avoided policy's premiums are refunded or kept is recorded per case by the two seniors; which the law allows needs legal review against the Contracts of Insurance Act 2024 regime. The design describes a Temporal workflow per proposal; the module uses its state machine and the daily sweep instead, so timers are accurate to the day.

**Checks.** At the last run on main (2026-11-12) fmt, clippy, the test-doc check and all `ic-underwriting`, `ic-core` and `ic-sim` tests passed; crate line coverage was 98.37% when re-measured on 2026-11-12 after the staff notices and peer review index.

## Payments and bank channels

This section is the handover for money in and money out: what exists, where it is, and everything that is unfinished, written so the work can be finished without the thread that built it. The fuller description and runbook are in [payments.md](payments.md). This thread also owns collections (`ic-collections`), described at the end of this section. State as of 13 November 2026.

### What is built and where

- **`crates/payments` (`ic-payments`), module name `payments`.** A deterministic state machine hosted on the ledger nodes as its own Raft group (`ic_node::modules()`, `ic_payments_service::module_spec()`). Commands arrive as `Envelope { key, actor { id, roles }, at, command }`; commands are internally tagged (`"type"`, snake case). Files:
  - `machine.rs`: the command and event enums, the state (`PaymentsState`), roles and dispatch.
  - `config.rs`: banks, channels (file or API, cutoff, an endpoint per region with credential name, signing key name, certificate expiry and egress address), accounts by purpose and statutory fund, routes and limits. A configuration that leaves a voter region without a working endpoint is refused (`region_not_provisioned`, addendum entry 6).
  - `authority.rs`: direct debit authorities, with an optional scheme member (`life`). A payer's death cancels the authority and blocks a sole-name account (addendum entry 1).
  - `payee.rs`: payee accounts with verification, cooling-off, screening and legal sign-off for estates and liquidators.
  - `run.rs`: runs of twelve types from prepare to close: checks, maker and approver separation, submission exactly once, fallback and redirection, acknowledgement, settlement, returns, re-presentment.
  - `intake.rs`: waiting payments released by other areas (`offer_payment`, `take_waiting`) and the reports back to them.
  - `refund.rs`: refunds to source, and the deceased-payer exception that holds a refund for the claim.
  - `recon.rs`: statements, expected against actual, exceptions, and the two financial close questions.
  - `drill.rs`: the evacuation drill's record of readiness per region.
  - `file.rs`: the batch, its text form, hash and signed manifest, and the bank's messages.
  - `query.rs`: the questions the module answers, and `react`, which turns core events into module commands (key `core-{event id}`, actor `core-events`).
  - `model.rs`: shared types, including the outbox `Instruction` variants `Core`, `CoreAt`, `Module`, `Match` and `Notify`.
- **`crates/payments-service` (`ic-payments-service`).** The drivers. `pump.rs` (`Pump`, `DeployedPump`, `run_from`) works submissions and the outbox in order; `bank.rs` holds the `Bank` trait and `HttpBank`; `fake.rs` is the fake bank used by every test; `ports.rs` holds the `Module` and `Core` traits; `settings.rs` reads the environment; `iso20022.rs` writes ISO 20022 files; `bin/ic-payments-pump.rs` is the binary, with modes loop, `once` and `drill [<id>]`.
- **Deployment.** `deploy/k8s/payments.yaml` (service account `ic-payments`, config, two secret provider classes, a two-replica pump Deployment, a disruption budget, the weekly drill CronJob), with entries in `kustomization.yaml` and `network.yaml`. `infra/main.bicep` has the payments identity, its own Key Vault and the immutable `payments-evidence` container. The Bicep was not compiled locally, because the build container has no `az`; CI validates it.
- **Tests.** `crates/payments/tests/` (`runs.rs`, `controls.rs`, `intake.rs`, `recon.rs`, `refunds.rs` and others, sharing `common/mod.rs`) and `crates/payments-service/tests/` (`pump.rs`, `cluster.rs`, `iso20022.rs`). `crates/node/tests/between_modules.rs` proves the reinsurance seam on real nodes. Line coverage of both crates was about 98.7% when last measured.

### Seams with other areas

- **Core.** Settlement sends `SettlePayable` (caller `payments`) or `ApplyReceipt` (caller `cash-matching`); a member's collection sends `ApplyMemberReceipt`; a return sends the reversal. The item ID is the bank's end-to-end reference and the core's payment or receipt ID. The module reads `PaymentAuthorityCancelled`, `PayoutRaised` and `PayoutReinstructed`.
- **Claims.** The claims events `claim_paid` and `claim_cost_paid` become waiting payments under the claims payment reference (sources `claim:{claim}:{ref}` and `claim-cost:{claim}:{ref}`). Claims is told `PaymentReturned` when the bank refuses or returns a payment before settlement, and `ConfirmRun { run, settled_minor }` when every payment seen for a claims run has settled or failed. Both go through the claim's policy as `Instruction::CoreAt`.
- **Reinsurance.** Its driver offers a net settlement (source `reinsurance:statement:{n}`). The pump tells the reinsurance module `record_settlement` or `settlement_returned` with keys `payments:settled:{item}` and `payments:returned:{item}:{date}`.
- **Finance.** The close driver asks `{"q":"fund_cash_settlement","as_at"}` and `{"q":"aged_unidentified_receipts","as_at"}`.
- **Anyone else.** `prepare_run` then `add_run_items`, or `request_refund`. Results come back as `item_settled` and `item_returned` events carrying the caller's `source`.

### Open question for Mitchell

**Which bank file format do the channels speak?** The design notes ("Money in and money out: payment runs and bank connectivity") settle a file channel with a signed manifest, an API channel, two banks, idempotency and cutoffs, but name no bank and no format. The question was put in the payments thread on 12 November 2026 with three options: ISO 20022 XML (recommended), a named bank's own format, or wait until a bank is chosen. It was unanswered when this was written. The standing rule is that nothing is ever sent to a real bank endpoint; channels are tested against the fake bank only.

### Unfinished, in the order I would do it

1. **Bank formats and transport.** Mitchell left the choice to this thread on 13 November 2026, and it is ISO 20022.
   - Done: `crates/payments/src/iso20022.rs` writes `pain.001.001.09` and `pain.008.001.08`, reads initiations (for the test bank), and reads `pain.002`, `camt.053` and `camt.054` into `BankMessage`s. A channel's `format` is `own` (default) or `iso20022`; every account at an ISO 20022 bank needs `at_bank` (holder, number, BIC), checked at activation. The XML is what is hashed, signed and kept (`payments/<submission>/file.xml`). Run IDs are held to 31 characters and item IDs to 35 when made. The fake bank speaks it when `iso` is set. Parser dependency: `roxmltree` 0.20.
   - Done: mutual TLS. Endpoints name `client_certificate` and `bank_authority` secrets (Key Vault certificates fetched as PEM through the CSI driver; placeholders in `deploy/k8s/payments.yaml`); an ISO 20022 channel needs a client certificate in every voter region. Test certificates live in `crates/payments-service/tests/tls/` and the fake bank can serve HTTPS requiring them (`FakeBank::start_tls`).
   - Judged not needed: SFTP. The notes ask for a secure transfer, which HTTPS with mutual TLS is. If the bank chosen offers only SFTP, write a second `Bank` implementation (for example on `russh` and `russh-sftp`) beside `HttpBank`; nothing above the trait changes. The ISO 20022 versions chosen are common ones; a real bank's implementation guide may want another version or extra elements (for example `PmtTpInf`, a mandate block in `pain.008`), which are changes inside `iso20022.rs` alone.
   - The open question for Mitchell above is answered; the "Open question" paragraph can be read as history.
2. **Durable message cursors: done.** The pump records its place per channel in the module (`note_messages_read`, question `messages_read`), and holds it before a message the module refused. A message the module refuses for good is therefore offered again on every restart until a person deals with it; there is no command to skip one.
3. **Unit-linked payout refused before settlement.** `failed_before_settlement` in `intake.rs` tells claims but not the core's investment ledger. The core needs a command for it (ask the core owner); until then servicing must re-instruct, and the reference is spent.
4. **Scheme members.** `PaymentAuthorityCancelled { policy, claim }` names no life, so `payer_deceased` in `authority.rs` ends every authority on a scheme policy, the employer's included. The core owner was asked for an optional `life` on the event; when it exists, filter on it. A credit a member pays in themselves is not matched to the member, because no per-member payment reference is issued. `RefundMemberCredit` is settled by the core and is not paid through a run.
5. **Claims.** `RecordRepayment` is never sent: an inbound credit is not tied to an overpayment. The `account_ref` claims verified is carried on the waiting payment but not compared with the payee's account. `ConfirmRun` assumes a run's `claim_paid` events arrive together; if some arrived after the earlier ones had all settled, the confirmation would be early (claims was told and has not objected).
6. **Reinsurance.** Done: money a reinsurer pays in, quoting `ri-stmt-<n>`, is sent to the reinsurance module as `record_settlement` (amount negated) and resolved when it records it. A reinsurer who pays without quoting the statement still lands in unidentified receipts for a person.
7. **Receipt matching.** A `Match` instruction hands an inbound credit to the core's cash matching with the bank reference. Resolving the payer from an authority first is not done.
8. **Drill payments.** A drill's token payments appear on the statement as unexpected lines and are cleared as exceptions by a person. They are not posted.
9. **Account details at rest.** Payee and authority account details are in the module's state in clear. They are not sealed per person, so retention shredding in the customer master does not reach them.
10. **Notices.** `Notify` instructions are written to the structured log (`payments.notice`) with a role. Delivery to a work queue or a person is not wired to the channels or documents modules.
11. **Egress addresses.** Recorded per region and checked in configuration, not enforced on the network.
12. **Screens.** The portal has no payments operations screens (run preparation, approval, exceptions). The commands and queries they would call exist.

### Collections (`crates/collections`, module `collections`)

Built 13 November 2026 by agreement with the core thread: the core keeps standing, lapse, arrangements and hardship overrides; this module decides who is told what and when, and when a person steps in. Hosted on the ledger nodes (`ic_collections::module_spec()`), reading core events and posting to the core through the host's outbox (caller `collections`).

- `config.rs`: the dunning sequence per product (`SetSequence`, role configuration). A sequence without exactly one suspension and one final notice, with the final notice closer to lapse than the product's minimum, or a minimum under `NOTICE_FLOOR_DAYS` (14, to be confirmed by legal and conduct), is refused. Mandatory notices go by post or email with proof of delivery. An optional early call day for a high annual premium; a call task escalation age; an officer's arrangement limit.
- `dunning.rs`: follows `PolicyIssued`, `PremiumDue` (scaled to a year for the queue), `StandingChanged`, `PolicyStatusChanged` and the arrangement events. A partition's `BusinessDateAdvanced { from }` takes the steps for `from` for that partition's policies, after its end-of-day has settled standing. A late arrival gets its latest reminder and every mandatory notice reached; earlier reminders are `StepPassed`. One notice per party, step and day (policies join it until the documents service takes it) and one open task per party and kind. A broken arrangement pauses the clock by the days it ran, as the core does.
- `cases.rs`: outcomes as reason codes, each driving the next step; arrangements proposed to the core as `MakeArrangement` and ended as `EndArrangement`; hardship applied for by an officer or the hardship team, decided by the hardship team by someone else, granted to the core as `GrantHardship` (share in basis points) and ended early today as `EndHardship`. A core refusal becomes a task.
- `query.rs`: the queue (days to lapse, then annual premium; an officer's answer lists what it withheld), policy, notices due, tasks per team, hardship case (a health-disclosing note is withheld from officers), outbox, unsequenced products; `react` and the outbox adapters.
- Tests: `crates/collections/tests/` (`sequence.rs`, `dunning.rs`, `cases.rs`, `queue.rs`); line coverage about 99%. Matrix rows COL-012, 013, 014, 015, 018, 019 and 026 are in `traceability.md`.

Not done, in order:
1. **Arrears age between standing changes.** The module learns the oldest unpaid due date only from `StandingChanged`. A payment that clears the oldest premium while a later one is still overdue leaves standing unchanged, so the module's age is too old until the next change. Ask the core for the oldest unpaid due date on `ReceiptApplied` and `ReceiptReversed`, or an event when it moves.
2. **Notice delivery.** Notices wait in the state (`notices_due`) for the documents service, which reports `record_notice_sent` and `record_notice_delivery` as caller `delivery`. The documents thread has not wired that relay. An undeliverable mandatory notice raises a task, but the core does not yet pause the lapse step for it (COL-009 is the core's).
3. **Sequences for the real products** are not written; until one is set, a product is listed by `unsequenced` and nothing is sent.
4. **Advisers and lenders** are not told at the call step; vulnerability, recent claims and tenure do not bring the call earlier (premium only). Winback tasks, retention offers, scheme employer arrears and reporting measures are not built.
5. **Screens.** The portal has no collections screens; the questions and commands exist.

### Checks

Nothing in this area is known to fail. Before a push: `cargo fmt --all`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `python3 tools/check_test_docs.py`; `IC_SKIP_TEMPORAL_TESTS=1 cargo test --workspace --locked`. Chain them with `&&`, so a failure stops the push. Most CI runs on main are superseded by the next push before they finish, so judge main by the latest completed run. The coverage table counts a closure that never ran as a missed line: use a named helper that one test exercises. The build container's disk fills from `target/`; delete `target/debug` and build with `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0`.

## Finance ledger and reporting (`ic-finance`): state and handover

Written 2026-11-12 so the area can be finished without its build thread. The working description is [finance.md](finance.md); this section says what exists, where, and what is left.

### What is built and where

Everything is in `crates/finance`, configuration in `config/finance/`, with `infra/finance.bicep` and `deploy/k8s/finance.yaml`. CI run 192 on main (commit ea9de28, 2026-11-12) passed with all of it. No check in this area is known to fail.

| Part | File | What it does |
| --- | --- | --- |
| Books | `src/books.rs` | The replicated state machine. Commands are `FinEnvelope { key, caller, actor, roles, command }`. Core events arrive as `CoreEvent`, are held per partition and posted in order by `Ingest`; a posting that cannot be mapped stalls that partition's feed until `RetryFeed`. Maker-checker journals, waivers, sign-off, aged items and their documented actions (`DocumentAgedItem`). |
| Chart | `src/chart.rs`, `config/finance/chart.star` | Maps every core account role to a general ledger account of the same class. |
| Close | `src/close.rs`, `src/period.rs`, `config/finance/close.star` | Periods, checks (automatic, reported, aged), waivers, sign-off, frozen periods, year-end adjustment period, public holidays. |
| Reports | `src/report.rs` | Trial balance, movements, sources (lineage back to core entries), backdated and prior-period items, the close dashboard with due dates, the work list the driver reads, aged policy suspense. |
| Returns and conduct | `src/returns.rs`, `config/finance/returns/` | Return definitions, filing, restatement; conduct facts with overturns counted apart from declines. |
| Data mart | `src/mart.rs` | In the module's replicated state, fed from the same events before the ledger posts them. In-force, movement and experience extracts, definition version 5 (with scheme benefit category). Rating factors are kept by the date recorded; date of birth never leaves the mart. Policy suspense by policy. Admitted claims from the claims module's facts. |
| Tables | `src/tables.rs`, `config/finance/tables.star` | Validation of a rate table against its schema and previous version; dry run of a proposed version against the in-force book. Pure functions plus the question `table_dry_run`. |
| Host | `src/host.rs` | Module `finance` on the ledger nodes (`module_spec()`), reading core events; every question the module answers. |
| Close driver | `src/driver.rs`, `src/temporal.rs`, `src/service.rs`, `src/bin/ic-finance-close.rs` | Service `ic-finance-close`: the close as a Temporal workflow where Temporal is configured, otherwise a restartable pass on a timer. Records verified Merkle roots, writes extracts to immutable blob storage and registers them, asks each module for its close report and forwards it, reports its own check, closes signed-off periods. |

Tests are in `crates/finance/tests/`, one file per part; `common/mod.rs` runs a real in-memory core beside the books. `tests/service.rs` runs a real node with the fake Azure store.

### Seams with other areas

- **Core.** Any new account role in `crates/core/src/accounts.rs` needs a line in `config/finance/chart.star` with the same class; `tests/config.rs::the_shipped_chart_maps_every_core_role` fails the build otherwise, and in production the feed would stall. The investment ledger answers `{"waiting_cash":{"as_at"}}` per partition for the `pending_investment_cash` aged check (`core_sources()`).
- **Close reports** (`driver.rs`, one `*_sources()` function per module; the caller name must match `by` in `close.star`): distribution (`commission_sweep`, aged `held_balance`), payments (`fund_cash_settlement`, aged `unidentified_receipts`), reinsurance (`reinsurance_sweep`, quarter ends), unit pricing (`unit_pricing`), core (aged `pending_investment_cash`), finance itself (aged `policy_suspense`). A check answer is `{"green","items"}`; an aged answer is `{"as_at","items","total_minor","count"}`.
- **Claims.** The mart reads `Event::Module { module: "claims" }` facts by key: `claim_reserved` (`policy`, `cover`, `life`, `event_date`), `claim_admitted` (`admitted_minor`, `monthly_minor`), `admission_reversed`. If the claims module renames a key the mart counts nothing for it and does not fail in production; `tests/mart.rs::the_real_claims_module_feeds_the_experience_extract` runs the real claims module through a straight-through health claim and fails the build if those facts change. Reversal and benefits in payment are tested with hand-built events only. Claims and complaints send `RecordConductFacts`.
- **Portal.** People act through `/finance` in the portal under caller `finance-console` ([channels.md](channels.md)).
- **Azure.** Merkle roots are read from, and extracts written to, immutable blob containers (`merkle-roots`, `finance-extracts`). Only the fake store has been exercised.

### Not finished or not started

1. **Temporal workflow for the close.** Built: `src/temporal.rs` (`CloseRunWorkflow`, activities `record_roots`, `closing`, `work_period`), `service::run_on_temporal`, and the binary chooses it when `TEMPORAL_ADDRESS` is set (`deploy/k8s/finance.yaml` reads the `ic-temporal` settings). Tests: `tests/temporal.rs` (scripted steps on a real server: retry, non-retryable failure, two starters) and `tests/service.rs::the_close_service_runs_on_temporal_against_a_real_node` (two replicas of the binary). They need a Temporal server, so they run in CI, not on the build machine; all of them passed on their first run there (ci run 319, 2026-11-13). That run failed only on the workspace coverage gate (97.96% of lines against 98% by llvm-cov's table, short across many crates). Not built: a workflow per period that waits on a signal for a person's sign-off (today a run ends reporting what it waits for, and the next run looks again); a Temporal Schedule in place of the service's own timer.
2. **Mart history.** Built: when a period closes, the mart lets go of everything more than twelve months before it (`Mart::trim` in `src/mart.rs`, called from `close_period` in `src/books.rs`, `MART_KEEPS_MONTHS`). Extracts from the kept date on are cut byte for byte as before; earlier ones are refused and the stored files are the record. Not built: a policy's row, its premiums by receivable and each registered claim are kept for ever, so the state still grows with the number of policies and claims ever written (not with their activity). `FinanceState.backdated`, `days` and `idempotency` in `books.rs` are not trimmed either. No measurement at production volume has been taken.
3. **Scheme benefit category.** Built (2026-11-13): the core publishes `LifeInfo.category`; the mart keeps it with a life's dated factors, the three extracts carry a `category` column (definition version 5) and `tables::dry_run` prices a category row against members in that category. Not built: the dry run's cells are still by age band, sex and smoker only.
4. **Loadings are not published**, so the dry run prices before loadings, discounts, fees and tax. It shows movement, not a bill. Agreed with the core (2026-11-13): loadings stay off the event feed because they imply health; the core will add a question on the partition, answered to caller "finance" only, giving each layer's total loading for a page of policies after a cursor. When it lands, the dry run should ask it and hold the answer only for the run, never in an extract. Also agreed: the core will add `RecordRateTableValidation` and publish `RateTableValidated {id, version, content_hash, report_hash, schema_version, by}`; finance then needs a close check that is red for any `RateTableActivated` with no matching record (no hard gate in the core).
5. **Experience age bands.** Exposure is banded on age at the end of the period asked; a claim on age at the event date. Around a birthday a claim can fall in the band beside its exposure. An actuary should say which basis is wanted (`mart.rs`, `experience`).
6. **Claims facts tested end to end for one path only.** A straight-through health admission runs through the real claims module. For `admission_reversed` and a benefit in payment (`monthly_minor`) the behaviour is tested with hand-built events, and `tests/mart.rs::the_claims_module_still_publishes_the_facts_the_mart_reads` reads the claims source and fails the build if a fact or key is renamed. A full assessed claim through the real module (`crates/sim/tests/claims.rs` shows how) is not built.
7. **Azure.** `infra/finance.bicep` and `deploy/k8s/finance.yaml` have never been deployed, and the service has not run in the integrated Mac cycle.
8. **Tax and analytical review** are reported by a person at the console; nothing works them out.
9. **Table validation is a library and a CI test.** Activating a rate table in the core does not require a passed validation report or dry run; nothing enforces the gate at activation.
10. **Avoided policies (non-disclosure).** Built: the policy leaves the in-force book on the day it is avoided (movement kind `avoided`), and where every premium is refunded the premium billed comes off as `premium_refunded_on_avoidance` (`mart.rs`, `Event::PolicyAvoided`). Not built: where the insurer keeps the premium, the mart cannot tell how much of what was billed was paid and kept, so the movement extract's premium stands; the kept amount is in the ledger under `avoidance_premium_retained`. The experience extract keeps the exposure of an avoided policy up to the day it was avoided, because extracts already cut cannot change; whether a study should leave avoided policies out altogether is a question for the actuary.
11. **Migration control accounts.** Built: automatic check `migration_control` (`src/close.rs::migration_moves`, accounts listed under `migration_control` in `close.star`: 1420 reinsurance, 1430 claims). The first period an account has anything posted through it is taken as the cutover; anything posted through it in a later period turns the check red, even if it nets to nothing (`FinanceState.turnover` keeps gross postings per account and period). Not built: comparing the balance at cutover with the old system's total, which needs that total supplied by the migration.

### Questions for Mitchell

- The holiday list in `close.star` is New Zealand national holidays for 2026 to 2028, with no regional anniversary day. Is New Zealand right, and which region's day applies? Someone must add each new year.
- The chart's account codes, the close's due days and aged thresholds, and the shipped return (`config/finance/returns/insurer_quarterly.star`) are placeholders for finance and compliance to replace with the real chart and the regulator's template.
- The rate tables under `products/rates/` are synthetic.

## Area: claims operations (`ic-claims`): state and handover

Written 12 November 2026 by the thread "Build claims operations". The full description of behaviour, every limit and the configuration is `docs/claims.md`; this section is what someone needs to pick the work up without that thread.

### What is built and where (all on main)

- **The crate.** `crates/claims` is pure and deterministic and does not depend on `ic-core`. `port.rs` defines the `Core` trait (business date, cover facts, `execute(Instruction)`, notify, work units), the `Instruction` enum and `CoverFacts`. `command.rs` is every command and who may send it. `state.rs` holds `ClaimsState`, the dispatch, the indexes rebuilt by `commit`, the daily due work (`drain_due`, `fall_due`) and moving a policy's claims (`moving`, `detach`, `attach`, `reattach`, `release`). `intake.rs` is notification, triage, tasks, evidence, health pricing and the file-opening log. `decide.rs` is decisions and signatures. `book.rs` is paying: lump sums, handling costs, interested parties, benefit schedules, runs, returns, waiver, death. `overpay.rs` is overpayments and recovery. `control.rs` is the exclusion-match control and reinsurance notices. `report.rs` is the views and `Question`/`Reply`. `config.rs` is `Rules` and `AuthorityBook`. `testing.rs` is the fake core for the suite.
- **Hosting.** `ic-core` depends on `ic-claims`: `CoreState.claims`, `Command::Claims(Box<ic_claims::Command>)`, the port in `crates/core/src/claims.rs` (every instruction's postings are there), re-exported as `ic_core::claims_module`. Facts leave as `Event::Module { module: "claims", kind, subject: <claim id>, detail }`. Claims is therefore inside each partition's state and its hash, not a separate service. Because `ic-underwriting` depends on `ic-core`, claims cannot import underwriting; the shared exclusion library is proved equal by a test in `crates/underwriting/tests/libraries.rs`.
- **Money.** Claims never posts. Reserve at notification, admission to benefit payable, release (`Pay`, `PayBenefit`), ex gratia, handling costs (`claims_handling` account), late payment interest (`claim_interest`, entry `claim_late_interest`), overpayment raise, recovery and write-off, premium waiver (`premium_waived`), all in `crates/core/src/claims.rs`. The payments module settles a released payment with core `SettlePayable { payable: Benefit }` for exactly the released amount.
- **Reads.** `Query::Claims` in `ic-raft`; gateway routes in `crates/api/src/routes.rs`: `/v1/claims/{claim}`, `/v1/claims/{claim}/work`, `/v1/claims/queues/{queue}`, `/v1/claims/book`, `/v1/claims/report`, `/v1/claims/runs/{run}/remittance`.
- **Design addendum.** Entry 5 (reversal of admission, held overpayment, recovery by decision only) is in `decide.rs` and `overpay.rs`; entry 1's claims points (late-notified death, cover as at the event date, `Receivable.settled_on`) are in `book.rs` and the core's `cover_facts`.
- **Built on 13 November**: actuarial valuation reserves (`ApplyValuation`), withdrawn deaths (`RetractDeath`), policy status for advisers and employers (`Question::PolicyStatus`), restricted investigations with the duplicate provider billing rule (`investigate.rs`), recovery of a provider's billing from its next provider run (core posts `claim_offset`), and persona tests CLM-005, 015, 018 and 027.
- **Built on 12 November**, each with tests in `crates/claims/tests/suite` and on the real ledger in `crates/sim/tests/claims.rs`: provider fees (`PayCost`), late payment interest (`Rules.late_interest`), premium waiver (`StartWaiver`, waiver-only claims), lenders and assignees paid first (`PayInterest`, facts from `ic_core::interest::ranked`), proof of life (`Rules.proof_of_life_months`), a policy's claims moving with it between partitions (`TransferRecord.claims`), the staff working view (`Question::Work`), `run` on `claim_paid`.

### Seams with other areas

- **Payments**: reads `claim_paid` and `claim_cost_paid`; sends `PaymentReturned` for a refusal before settlement and `ConfirmRun` from the `run` field. Not sent: `RecordRepayment` (an inbound credit is not tied to an overpayment ID). Payments does not compare `account_ref` with its own payee's account. Lenders and providers must be verified payees in payments.
- **Reinsurance**: reads the claim events; its driver sends the five reinsurance notices as caller `reinsurance`. `payment_returned` carries `ex_gratia`, `cost`, `interest_minor`.
- **Distribution**: shows overpayments as memo lines from the four overpayment events; earns renewal commission from `premium_waived`; sends recovery status and write-off decisions as `held-balances`.
- **Core**: owns the assignment record, the transfer protocol and billing. Policy loans, joint life first-event and linked covers are on the core thread's list; claims has nothing for them yet.
- **Customer master**: relays every death as `RecordDeath { party, date_of_death }` and every withdrawn death as `RetractDeath` (same date) to every partition, caller `customer-master` (on main since 096ed9f).
- **Compliance**: no compliance module exists yet. Claims runs the duplicate provider billing rule itself and accepts `ReferForInvestigation { case, rule, claims }` from caller `compliance` for the patterns the design gives that module (claims soon after inception, altered documents). Restricted cases are worked by the `investigator` role on the claims console (`/staff/v1/claims/cases`).
- **Portal**: building the claims screen on the queues and `Question::Work`; people's commands go in as caller `claims` with the signed-in staff ID and a typed purpose. `OpenFile` must stay a command so views of the clinical file are logged.
- **Finance**: every core account role must be mapped in `config/finance/chart.star` or `crates/finance/tests/config.rs` fails. Claims roles mapped: 5030 claims handling, 5040 claim interest.

### In progress when this was written

- **Migration loader for claims**: finished after this section was first written and on main. `Command::LoadMigrated` from caller `migration`, `ClaimsState::load_migrated` in `intake.rs`, `Instruction::OpenMigrated`, core `open_migrated` posting `claim_migrated` against `claims_migration_control` (chart account 1430). Tests in the claims suite (`intake.rs`) and `a_migrated_claim_opens_against_the_migration_control_account` in the sim. Not done: mapping a legacy extract into the claim shape, and carrying payments released but unsettled in the old system.

### Not built, with what each needs

- **Offsets across partitions.** An overpayment is offset only against benefits on the same partition (`offset_sources` in `book.rs`). The core's transport is on main (357c969: `MessageBody::Claims`, `Core::partition` and `Core::send`, account `claims_offset_in_transit` 1210); the claims commands over it are not built. Each must be a no-op on repeat by its own ID, a receiver that cannot act replies rather than refusing, and the sender names the target partitions. Once claims posts to the in-transit account the core adds a daily check that it nets to zero. Also: when a policy moves, an offset against another policy's benefit silently stops, and the duplicate billing rule sees one partition's claims only.
- **Actuarial reserve table**: built after this section was first written. `Command::ApplyValuation` (caller `config`, `config::Valuation`, handler `apply_valuation` in `state.rs`) sets the reserve of claims in payment from the actuary's approved figures. Not built: the path from the actuarial model to the activation (design gap noted in the notes).
- **Claim status for advisers and employers.** Mitchell decided on 13 November 2026: status only, no cause. Claims answers `Question::PolicyStatus { policy }` (ID, status, notified and closed dates per claim). The portal thread has been asked to serve it on the adviser and employer routes after its own relationship check; until then nobody outside staff sees it.
- **Nominations in the policy record.** Claims pays a death benefit to `CoverFacts.nominations` (built and tested with the stand-in core). The core holds no nomination record yet; the core thread has been asked for one on both timelines, filled into the cover facts as at the date of death. Until then the field is empty and nothing changes.
- **Continuation link in the policy record.** Claims reads `CoverFacts.continuation_of` (scheme policy and member) and passes it to reinsurance and the assessor (built). The core does not record the link; the core thread is asked to carry it on `IssuePolicy` and in the cover facts, and group administration to set it when it issues a continuation policy. Reinsurance is asked to read `continued_from_policy` on `claim_reserved`.
- **Non-disclosure remedy.** Underwriting's outcome is recorded on the claim; there is no core command to vary or avoid the policy from it.
- **Undoing a recorded death** is built (`RetractDeath`, reopen basis `death_retracted`); the customer master has been asked to send it on its own retraction. An overpayment recorded for the withdrawn death is not written off automatically.
- **Death register or app confirmation satisfying proof of life without a person**, **tax withheld from late payment interest**, **a waiver checked against a product fact** (today the owner judges from the wording), **reversal billing back waived premium**: none built; each is one command or one rule and is described under its feature in `docs/claims.md`.
- **Health limits** are held in claims (`health_used`) and nowhere else; the standing check reads receivables the core prunes after 400 days, so a very late claim reads standing as unknown.
- **Run records stay on the partition that paid them** when a policy moves; a run confirmed after the move is confirmed there.

### Open questions for Mitchell

1. Answered 13 November 2026: advisers and employers see status only, no cause.
2. Claims rules and authority limits are not shipped as configuration: the insurer's own staff IDs and limits have to be activated by two people before any claim can be worked. Is a sample book wanted for the demonstration environment?
3. Late payment interest: the rate, the free days, and whether tax is withheld are the insurer's and the law's to set. The rule is off unless configured.

### Checks

- The claims suite alone covers the crate at 98.18% of lines (`cargo llvm-cov -p ic-claims`); `port.rs` and `report.rs` are covered from the sim and API tests.
- The whole workspace suite passed locally before every push from this thread, the last at commit 71af93a. CI on main rarely completes because pushes arrive faster than a run; judge by the latest completed run.
- Known weak spot outside claims' crates: `AbortTransfer` on the source is accepted while the transfer record may still be in flight to the target (reported to the core thread).

## Area: group scheme administration (`ic-group`)

State as at 12 November 2026. The full description (what it does, routes, settings, operating it) is [group.md](group.md); this section is the handover: what is built and where, what is not, and what somebody finishing it needs to know.

### What is built and where

Everything is in `crates/group`. It has three parts: a deterministic state machine hosted on the ledger nodes as the module `group` (`host.rs`, `module_spec()`, listed in `ic_node::modules()`), a driver that carries the module's outbox to the outside world, and a stateless service (binary `ic-group`) that runs the driver on a timer and serves the routes the portals use. Deployment is `infra/group.bicep` and `deploy/k8s/group.yaml`.

| File | What it holds |
| --- | --- |
| `terms.rs` | Scheme terms as data, checked whole and versioned: basis (named, unnamed, pools), eligibility, automatic acceptance limit, leaver and absence rules, backdating, the safety share, billing groups, continuation, renewal, how loadings are billed (`EmployerTotal`, itemised, `MemberPays`, and `excess_loading_min_count`), short-payment suspension. |
| `csv.rs`, `mapping.rs`, `record.rs`, `diff.rs` | Reading an employer's file: delimited text, the scheme's sealed versioned column mapping, the canonical member record, and the comparison that gives joiners, leavers and changes. |
| `benefit.rs` | The benefit formula and eligibility on the day cover would start. |
| `command.rs`, `event.rs`, `model.rs` | The `Envelope` (`key`, `caller`, `actor`, `today`, `command`), every command and which caller may send it, the events, the outbox entries (`Outbound::{Core, Reinsurance, Underwriting, WithdrawUnderwriting, UnderwritingOutcome, ResolveParties, Publish}`) and what is held per scheme. |
| `state.rs` | `GroupState`: handling of each command, files from receipt to approval, holds, freezes, pools, renewal and ending, the invoice as the employer sees it, idempotency. |
| `work.rs` | The scheme's work queue: chunks of `ApplyMembership` to the core, halving a refused chunk to find the row, blocking on a policy refusal, matching results, cover above the automatic limit (request, withdrawal, decision, layer, payer of the loading, reaction to `MemberCoverLapsed`), leavers and continuation, reinstatement. |
| `host.rs` | Hosting: how core events become commands (`react`), and the questions (`Question`) other areas and the service ask. |
| `driver.rs` | The driver and its ports (`Peers`, quarantine, customer master, events). One pass (`run_once`) carries the outbox, takes stored files one step, and collects underwriting's answers. |
| `adapters.rs` | The ports in production: module and core on the cluster (`ClusterPeers`, `follow_transfer`), Blob Storage quarantine with the Defender scan tag, Event Hubs, and the exact message shapes sent to underwriting and reinsurance. |
| `service.rs`, `bin/ic-group.rs` | Configuration (`Config::from_lookup`, secrets by value or `_FILE`), the HTTP routes with `/readyz` and `/metrics`, the pass loop, the customer master client (`HttpCustomerMaster`), and stopping on SIGTERM. |
| `auth.rs` | Callers' Entra ID tokens: signature, issuer, audience, and the app roles that are the caller names a service may use. |

Tests are in `crates/group/tests/`: `admin`, `apply`, `billing`, `excess`, `intake`, `leavers`, `year` (state machine against the real core in `ic-sim`, harness in `tests/common/mod.rs`); `driver` (every outside party failing in turn, and the real customer master service over HTTP and PostgreSQL when `IC_TEST_POSTGRES_URL` is set); `underwriting` (a whole decision with the real underwriting state machine: accepted, withdrawn, lapsed); `cluster` (Azure adapters against the protocol stand-in, the message contracts against the other modules' own types, and a real node hosting group, reinsurance and underwriting with the `ic-group` binary spawned and stopped). 82 tests. Crate line coverage was 98.48% when last measured, which was before the member-paid loading, lapse reaction and the two integration tests were added; it has not been re-measured since.

Both design addendum items for this area are built: entry 2 (one life on two partitions) as matching each joiner with the customer master, linking only on one high-confidence match, telling reinsurance of every candidate, and reinsurance's power to cap a member's automatic cover (`work.rs`, `tests/intake.rs`, `tests/excess.rs`).

### Persona tests (13 November 2026)

Of the 40 group rows in `docs/testing/personas/`, 19 are carried out in full and 9 in part (`traceability.md`). Added on 13 November: a year of monthly files on 5,000 members (GRP-001, `tests/intake.rs`), cover following hours across the minimum (GRP-008, a new rule in `diff.rs`), a short payment brought forward on the next invoice (GRP-013, `brought_forward_minor` on the invoice), a new site with its own billing group (GRP-030), and a member paying their own loading through the core's `ApplyMemberReceipt` (`tests/excess.rs`). Priority 1 rows still without a full test: GRP-013 (the order a part payment is allocated in is the core's and is not checked here), GRP-017 (dependants as voluntary cover on a group health scheme: needs the composable health product, which is not built), GRP-028 (an administrator's sessions closed when they leave: the portal's) and GRP-032 (what the employer sees of a member: the portal's and the read model's). Rows this module could still take: GRP-033 (occupation classes on named directors), GRP-035 (takeover terms; the waiting-period waiver is unit tested in `benefit.rs`, the reason code and people already on claim are not), GRP-019 (a payroll API sending changes: the service's command route takes them but no test names it), GRP-024 and GRP-038.

### Seams with other areas

- **Core.** Commands go as caller `group-admin`: `ApplyMembership`, `AddExcessLayer` (with `member_pays`), ending layers, issuing a continuation policy (with `continuation_of` naming the scheme policy and the leaver's life, which claims reads), cancelling. The module reads the business date, scheme invoices, adjustments, `PolicyBalance`, `LayerAdded`, refused layer writes and `MemberCoverLapsed`. The scheme's `outstanding_minor` here comes from `PolicyBalance` and leaves out what members owe; the core's `policy.outstanding_minor()` includes it.
- **Underwriting** (`crates/underwriting`). A request is `open_proposal` to the module `underwriting`, proposal and key `grp-uw:{scheme}:{case}`, entry `{entry:"group_excess", policy}`, one life, one cover for the excess. Withdrawal is `{withdraw:{proposal}}` under `{proposal}/withdraw`. Answers are read from underwriting's outbox (`{query:"outbox",after,max}`, never acknowledged by group): `apply_terms` and `proposal_ended` become `record_excess_decision` under key `uw:{effect key}`, the same key underwriting's own driver uses, so each is recorded once. When the core writes or refuses the layer, group reports `policy_issued` (key `{proposal}/excess-issued`) or `issue_rejected` (`{proposal}/excess-rejected`) as caller `issuer`. Shapes are in `adapters.rs` and pinned in `tests/cluster.rs`.
- **Reinsurance** (`crates/reinsurance`, hosted as `reinsurance`). Group sends scheme terms, risk factors and possible matches as notices; reinsurance sends `require_underwriting` to cap automatic cover.
- **Customer master** (`crates/customers`, `http://ic-customers`). `POST /v1/parties/match` with role `group-admin` and header `x-ic-purpose: scheme_membership`; answers `linked`, `created`, `possible`, `none`.
- **Portal** (`crates/channels`) and any other caller of the service routes. Each presents its own Entra ID token issued for the group service (`IC_GROUP_AUDIENCE`) whose app roles include the caller name it uses (`employer-portal`, `member-portal`, `group-admin`, ...), and names the person in `actor`. No code in the portal calls these routes yet.
- **Tests borrow two harnesses by path**: `crates/underwriting/tests/common/mod.rs` and `crates/customers/tests/common/mod.rs`. Both owners know; a change to those files can break group's tests.

### Not finished, with what is needed to finish it

1. **The member cannot pay a loading yet.** Group decides who pays when terms are accepted (`record_excess_decision` in `work.rs`) and writes the layer with `MemberPays::Loading`; the core bills and lapses it. A way for the member to pay, and the notices before a lapse, are core and portal work and are not built. Nothing is needed in this crate once they are, beyond a test that follows a payment through.
2. **Moving a loading to the member after the count drops: built on both sides and tested together.** Decided 13 November 2026 (Mitchell left it to this thread): who pays is what the member was told in the offer, so the module never changes it by itself. `check_loading_total` in `state.rs` raises `loading_total_too_small` for staff. Staff then send underwriting `reoffer_excess_terms {proposal, reason}` (underwriting's command, from caller `group-admin`; no staff screen sends it yet, which is the portal's); the member consents to paying; underwriting emits a fresh `apply_terms` with `member_pays_loading: true`. Group takes that as `ExcessDecision::Accepted { member_pays_loading: true }` on a case whose cover is in place (`replace_with_member_paying` in `work.rs`): a new case and layer with the member paying from today, the old layer ended from the same day after the new one is written, the outcome reported under the original proposal with key `{proposal}/excess-issued/{new case}`. A refusal by the member, or an offer that ran out, is underwriting's `reoffer_ended` effect, read by `excess_answer` in `adapters.rs` as `ExcessDecision::ReofferDeclined`. Tested in `tests/excess.rs` with the decision given by hand, and in `tests/underwriting.rs` (`a_loading_moves_to_the_member_through_underwritings_reoffer`) with underwriting's real state machine: decline, then acceptance, the replacement layer and the second issue without a second capacity confirmation. The employer sees the old cover ended and the new one in place on the same day, as two events.
3. **A lapsed member-paid layer is not reinstated.** The case ends (`ExcessCoverEnded`); the member's next salary change raises a new request. Reinstating on payment would be a new command from the core's `MemberStandingChanged` in `host.rs` `react`.
4. **Consent to itemising has no screen.** On a scheme whose terms itemise loadings, an accepted answer with a loading and without the member's consent is refused here (`consent_required`), logged, and the case stays open. Underwriting records the consent by a signed step; the portal has no page that collects it. Portal work.
5. **Not tested with all the processes together.** No test runs the group service, underwriting's driver and reinsurance's capacity answer at once; in `tests/underwriting.rs` the capacity answer is given by hand. Reinsurance notices are followed on a real node as far as its list of exceptions (a possible match from a file appears there and its books reconcile), not into the retained amounts. The place to add the rest is `tests/cluster.rs`, which already hosts the three modules on one node. The core thread's integrated Docker run on the Mac has not been reported as including this module.
6. **The proposal sent to underwriting is thin.** It carries no bound quote (premium 0, the scheme's rates apply), gives the policy ID as its number, and does not carry the facultative path. The amount a previous insurer had accepted and reinsurance's reference for a capped member are sent on the entry as `takeover_accepted_minor` and `reinsurance_reference` (agreed with underwriting 13 November and read there: at or under the takeover amount is accepted on standard terms without disclosure, above it or with a reference is underwritten in full; an amount of nothing or a blank reference is left off, because underwriting refuses it).
7. **Matching depends on the employer's file.** The customer master links only on IRD number, email or mobile. A mapping can name email, IRD number and mobile phone columns (`mapping.rs`); a file with none of them only creates people or raises possible matches, and a mobile without an email or IRD number finds a possible match but scores too low to link (75 against the customer master's 80). Mobile numbers written for dialling at home are assumed to be New Zealand numbers (`international` in `mapping.rs`); a scheme with members overseas would need a country setting on the mapping.
8. **Continuation carries automatic cover only.** Underwritten excess and its terms are not carried to the retail policy. The design notes do not say they should be.
9. **One file layout per scheme**, full or change files, not both at once.
10. **Deployment is written but never applied.** `infra/group.bicep` compiles in CI but `main.bicep` does not reference it (the core thread said it would add it); it does not assign the customer master's `group-admin` app role to the service's identity; its Defender for Storage resource uses a preview API version. `deploy/k8s/group.yaml` is in the kustomization and has not been applied to a cluster.
11. **The service's app registration is a manual step.** The shared key is gone: callers present Entra ID tokens and may use only the caller names their token carries as app roles (`auth.rs`, tested in `tests/driver.rs` and with the binary in `tests/cluster.rs`). What is not automated is the app registration itself, its six app roles and assigning them to the calling services' identities; `infra/group.bicep` does not do it. `deploy/k8s/group.yaml` needs `REPLACE-group-audience` filled in.
12. **Not built because the design notes do not say enough**: splitting a scheme in two, delegated underwriting authority for an employer's broker, and a menu of health plans for members to choose from.

### Open questions for Mitchell

- Should underwritten excess be offered on continuation, and on what terms?
- Scheme split, delegated underwriting for brokers and the health plan menu each need a design before they can be built.

### Known failing checks

None known. When last run locally, format, clippy for the crate, the test documentation check and all 82 tests passed. Things that catch people out: local builds need `--exclude ic-workflows` (no `protoc`); the customer master test in `tests/driver.rs` skips unless `IC_TEST_POSTGRES_URL` is set (CI sets it); the crate's `clippy.toml` forbids `Instant::now`, `SystemTime::now`, `HashMap` and `HashSet`, in tests too, so waits are counted loops; the `ic-group` binary must install its signal handlers before its first pass, or the SIGTERM test in `tests/cluster.rs` fails; and underwriting does not store a refusal under its key, so a refused `open_proposal` (for example `no_rules`) leaves the request waiting and is sent again.
