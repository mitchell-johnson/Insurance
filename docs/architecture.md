# Architecture

## The shape of it

Everything the insurer does to a policy is a **command**: issue, apply a receipt, change scheme membership, close the day. A command goes into an ordered, replicated log, and a deterministic state machine applies the log. The state machine holds the policies and a double-entry ledger, and it is the only thing that ever writes either. Given the same log, every replica reaches the same state and the same ledger, byte for byte, and each replica proves it with a state hash.

That one idea gives the properties an insurer's core needs:

- **Nothing is lost.** A command is acknowledged only after a majority of replicas have it on disk.
- **Nothing happens twice.** Every command carries an idempotency key. A retry of a command that was applied returns the original result.
- **Everything is explainable.** The ledger is append-only and hash-chained. Policy state can be rebuilt by replaying the log.
- **Tampering shows.** Each day's ledger heads are folded into a Merkle root, signed with a Key Vault key and written to immutable storage.

## Partitions

Policies are grouped (a household, an employer's scheme), and a group lives on one of 64 partitions, chosen by a hash of the group's ID that never changes. Each partition is its own Raft group with its own log, ledger and business date. Work on one partition never waits for another, and one very large scheme cannot stall the rest: no command may do more than a bounded amount of work, so a 50,000-member scheme is billed in chunks.

A policy can move between groups, and so between partitions. The move is a two-step transfer with money held in an in-transit account between the steps, delivered by messages that are retried until acknowledged. Reconciliation checks every day that nothing is left in transit.

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

## Modules beside the core

A module whose state spans partitions runs as its own Raft group on the ledger nodes (`ic_raft::module`), with the same durable log, snapshots, membership changes and failover as a partition. A module that reads the core's events is fed them by the leader of its group: every member is shown each event of each partition once, in sequence, and the checkpoint is part of the group's replicated state. Each partition keeps its events until they are on the stream and every registered reader has them. `docs/modules.md` is the contract.

## Privacy

Command payloads carry personal and health information. They never go to logs: a log line names a command, its sender and its key only. Ledger entries carry salted commitments to their payloads, so the chain can be verified without the payloads being readable from it.

## Area: documents, correspondence and complaints

Owner thread: "Build documents and correspondence". Detail lives in [documents.md](documents.md) and [complaints.md](complaints.md); this section is the map and the list of what is left.

### What is built, and where

- **`crates/documents` (module `documents`)**: templates versioned by content hash and approved by legal, product and compliance; rendering (`render.rs`, `template.rs`); the encrypted vault on Azure Blob with a Key Vault key per owner (`vault.rs`, `crypto.rs`); issue, supersede and correct; mandatory notices with proof of delivery and notice clocks that the core waits on (`state.rs`); channel fallback email, SMS, in-app, post (`channels.rs`, Azure Communication Services and a print outbox container); delivery reports from Event Grid (`POST /v1/delivery-reports`); notice defects and reopen windows (addendum entry 3, document side); retention, legal holds and erasure by key shredding; triggers from the core's events (`triggers.rs`); the report.
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
- **Document download route.** `Driver::pdf` exists; no HTTP route serves it. Needs the portal thread to say who is asking (staff seat or customer party id) so the access check can sit in `ic-documents`.
- **Brand typeface.** `Fonts::new` / `AzureChannels::with_fonts` accept TrueType faces; there is no setting yet. Scripts outside Latin go as text without a PDF. PDFs are not tagged (PDF/UA).
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

- `crates/customers/src/`: `model.rs` (party, fields and classes, relationship kinds), `party.rs` (the decision logic: commands to events, bitemporal facts, merges), `event.rs` (event bodies, the public event, the hash chain), `crypto.rs` (AES-256-GCM per party, commitments, HMAC blind indexes), `keys.rs` (data keys wrapped by Key Vault, cache, rewrap), `matching.rs` (probe keys, Jaro-Winkler and Soundex scoring, bands 80/40), `validate.rs` (normal forms, IRD check digit), `store.rs` (PostgreSQL: streams, projection, idempotency, reviews, outbox, audit, shredding, death relay rows), `db.rs` (TLS plus Entra token for Azure, loopback-only plain connections, pool, migrations under an advisory lock), `access.rs` with `config/cedar/customers.cedar` (purpose-based access), `auth.rs` (Entra JWT), `service.rs` (axum routes), `retention.rs` (schedule and write-once erasure register), `relay.rs` (deaths to claims), `work.rs` (outbox publishing, retention sweep, death relay, process start), `config.rs`. Migrations in `crates/customers/migrations/`.
- Contracts in use: group schemes' `POST /v1/parties/match` (tested by group against the real service in `crates/group/tests/driver.rs`, which includes `crates/customers/tests/common/mod.rs` by path, so keep `world()`, `World.router`, `person`, `token`, `get` and `AUDIENCE` stable); documents' `GET /v1/parties/{id}/contact-points` returning `{name, destination, language, format, deceased}` or 404; claims' `mark_deceased` / `retract_death`; portal self-service through `x-ic-party`.
- Deaths to claims: every death recorded here, whoever recorded it, goes to every core partition as `ic_claims::Command::RecordDeath` from caller `customer-master` through the ledger cluster (`IC_CLUSTER_NODES`, `IC_CLUSTER_TOKEN_FILE`). Pending deaths sit in `death_relay`; each attempt has its own idempotency key because the core remembers a refusal (for example `business_date` before the day is opened) under its key. Retractions are not sent: claims has no reversal command yet.
- Deployment: `infra/customers.bicep` (own PostgreSQL Flexible Server with Entra-only sign-in and TLS required, `customers-kek` in Key Vault, `customers-index-key` secret, hubs `customer-events` and `customer-priority`, write-once container `customers-erasures`, managed identity with read on `cluster-token`), `deploy/k8s/customers.yaml` (in `kustomization.yaml`). After the first deployment someone in the database admin group creates the service's role with `pgaadauth_create_principal` (commands at the top of the Bicep file).
- Tests: `crates/customers/tests/` (api, lifecycle, party, db, process, relay) against a real PostgreSQL named by `IC_TEST_POSTGRES_URL`; `relay.rs` runs a real ledger node. CI starts `postgres:16` in `ci.yml`. Crate line coverage is 98.0%, at the floor: new code here needs its own tests.

**Not built yet.**

- Relationship changes into the channels directory. `docs/channels.md` expects the customer master to send `BindSubject`, `RecordOwnership`, `RecordAssignment` and `RecordDependant` (children with their age of control). The events are on `customer-events` with IDs and kinds but not values; a relay like `relay.rs` reading relationship rows (or a `channels_relay` table written in `store.change`) and proposing those commands is the plan. Seam owner: "Build portal and app services" (channels crate).
- Changing the blind-index key: needs a command that decrypts every party's facts and rebuilds `blind_index`. `Store::reindex` exists for one party; a batch over all parties under the new key, then swapping the key, is the missing part.
- Locking the erasure register's immutability policy in prod: a deliberate, irreversible step for a person once the retention period (default 10 years) is agreed.
- Group schemes sends no mobile phone (its file mapping has no column), so matching on mobile is unused for joiners.
- Open for Mitchell: the retention periods in `retention.rs` (7 years for most classes, 5 for verification evidence, 2 for prospects) are this thread's reading of the Tax Administration Act and the Privacy Act, not a confirmed schedule.
- Known check status: `ci.yml` on main passed at `ea9de28` with the PostgreSQL step, the customer master's tests, the coverage floor and `customers.bicep` compiled. The death relay (`relay.rs`, migration `002_death_relay.sql`) landed after that run.
