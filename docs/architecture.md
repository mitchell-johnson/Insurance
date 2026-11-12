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

1. **Claims staff view on the page** (in progress at hand-over): `claims::question` gains `work`, `/claims` renders `WorkView` (tasks, evidence, waiting decision, payments, benefit, overpayments, timers). Clinical detail must only show after the person sends `open_file`; the claims module has no clinical question yet, so ask the claims thread for one before showing any.
2. **Customer master self-service** (`crates/customers`, `docs/customers.md`): the app's profile, contact details and consents through the master's `POST /v1/parties/{id}/commands` with app role `portal` and header `x-ic-party` set to the signed-in party; subject access export (`?purpose=subject_access`); `GET /v1/parties/{id}/routing` to send vulnerable customers to a person. The relay of relationship changes from the master into the channel directory (`BindSubject`, `RecordOwnership`, `RecordAssignment`, `RecordDependant`) is not written; agree with the customer master thread who writes it.
3. **Adviser replacements and clawback attribution** (commission, `docs/distribution.md`): show `ReplacementMatched` to the adviser at submission and offer `DisputeReplacement`; a staff page over the `replacement_matches` question with `DecideReplacement` (staff with `case.manage`); when the portal submits a reduction or a cover given up on an adviser's behalf, send `AttributeAlteration {policy, engagement}` as `policy-servicing` on the same business date before the core command.
4. **Document download**: the documents thread will add an HTTP route for `Driver::pdf` on request; the portal must say how it proves who is asking (staff seat or the customer's own party ID).
5. **Screens still missing**: group employer membership file upload, commission staff commands, underwriting, payments, investment ledger (unit holdings for the customer and adviser).
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
2. **Nothing in the portal uses the replacement rule's answers yet.** `WriteLayer` can answer with `ReplacementMatched`; `DisputeReplacement`, `DecideReplacement` and the question `replacement_matches` exist. The adviser's notice at submission, the dispute button and the oversight page are portal work. The portal thread has been told.
3. **Limits of the replacement rule** (`replacement.rs`). It runs only when a layer is written, so cover given up after its replacement was written is not matched: to close this, run the same match from `layer_ended` and `policy_ended` in `commission.rs` against layers written in the window, and take back initial commission already paid. Where only part of a layer given up is matched, or cover was reduced and not removed, the clawback already raised stands; only a whole layer matched in full has its clawback reversed (`continue_exposure`). Trail already paid before a match is overturned is not moved. A policy reinstated after its cover was matched raises the task `reinstated_after_replacement` and is not reversed automatically. Scheme members are not matched: the module reads the person behind a life from `PolicyIssued.lives` only, not from `MemberIdentified`.
4. **Not run on a real multi-node cluster.** Hosting and the outbox are tested through the functions the host calls, in the simulated cluster. The core thread has been asked to include the module in its integrated Docker run on the Mac and to confirm three things: commission is posted after a receipt on any partition, the outbox drains after a leader change, and `status` answers on every node. No result yet.
5. **A release does not net against the claim ledger.** A release that would leave less held than claims is collecting needs legal's signature (`short_of_owed_elsewhere` in `workbench.rs`), but the offset itself is done in claims. Reinsurance recoveries and scheme arrears appear on a position only if their owners post them with `PostCounterparty` and describe them with `DescribePosition`; neither does yet.
6. **Commission is its own entry**, posted after the premium event and idempotent on it, not lines in the premium entry. Making the two atomic would mean the core calling out during a receipt, which the module contract in [modules.md](modules.md) does not allow.
7. **Rules and schedules are data, not Starlark.** They are versioned and go through the change set but ship no worked scenarios that run at load, as products and treaties do.
8. **One currency** (New Zealand dollars), and **nothing is pruned**: allocations, movements and statements are kept for the life of the state, so the snapshot grows with the book. Pruning needs a decision on how long statement detail must stay answerable from the module.
9. **No screens of its own.** Staff commands go through the generic module command route; pages are the portal's.

Volume-based overrides are not built on purpose: the design notes leave them out.

### Open questions for Mitchell

- The replacement rule's settings are my defaults and the design notes say to confirm them with compliance: a window of 12 months (`replacement_window_months`), an amount tolerance of 5% where the advisers are unrelated (`replacement_amount_tolerance_ppm`), and "related" meaning the same provider or any provider under the same group (`Structure::related`).
- Who decides a disputed replacement match. It is whoever holds `case.manage`: the conduct officer or compliance.
- Whether paying held money to a party claims is pursuing should be refused outright. It currently needs legal's signature.

### Known failing checks

None. Things that catch people out: local builds need `--exclude ic-workflows` (no `protoc`) and `IC_SKIP_TEMPORAL_TESTS=1`; a posting reference may hold only letters, digits and `- _ . :`, so a layer key (which has `/`) never goes in one unchanged; the core's life IDs are per policy, so lives are matched by the party behind them; and a test that injects a core event by hand must do it last, or on a partition of its own, because injected events use up sequence numbers.

## Underwriting and new business (`ic-underwriting`)

Owned by the thread "Build underwriting and new business". The full module description and its known gaps are in [underwriting.md](underwriting.md); this section is the summary someone needs to carry the work on.

**Built, on main.**

- `crates/underwriting`: a deterministic state machine hosted on every ledger node as module `underwriting`. Proposals from quote to policy for every entry point (new business, increase, alteration, reinstatement, group excess, migrated cover), disclosure with versioned question sets, rules with their own version and two-approver activation (`config.rs`, `prelude.star`, `shipped.rs`), referral queues, authority limits and countersigning (`authority.rs`, `casework.rs`), evidence requests and chasing (`evidence.rs`), offers signed by the applicant, interim cover, delegated underwriting (`special.rs`), non-disclosure review with two seniors for a remedy, peer review, the exclusion library shared with claims (`codes.rs`, `ExclusionLibrary::for_claims`), the addendum's existing-cover prefill with consent (entry 2) and adviser licence loss (entry 4), and the clinical store over Azure Key Vault wrap/unwrap and Blob Storage (`clinical.rs`).
- The effects driver, binary `ic-underwriting` (`driver.rs`, `service.rs`, thin `src/bin`, `deploy/k8s/underwriting.yaml`). It reads the module's numbered outbox, carries each effect under a key derived from the effect, acknowledges what was delivered, parks what cannot be delivered with a reason for a person, and runs the daily sweep (lapse, chase, escalate) as caller `timer`. Routing: capacity reserve, release, confirm and risk factors to module `reinsurance`; new business to the core as `IssueQuoted` (refused `product_changed` if the product moved under the quote); increases as `AddUnderwrittenLayer`, alterations as `AlterUnderwrittenTerms`, reinstatement decisions as `ApproveReinstatement`, all caller `underwriting` and answered back to the module as `PolicyIssued` or `IssueRejected` (caller `issuer`); group excess terms and endings to module `group` as `RecordExcessDecision`, key `uw:{effect key}`, with the member's signed consent to the employer seeing a loading.
- Core commands this thread added (additive, in `crates/core/src/underwritten.rs` unless noted): `IssueQuoted`, `IssuePolicy.migrated`, cancel by `underwriting` (ends interim cover), `AddUnderwrittenLayer` (a layer from today, the days left of a billed period billed as an adjustment), `AlterUnderwrittenTerms` (each layer on other terms ended and written again with the same sum, origin and rating date; the days left billed or credited; `LayerAdded.continues` names the layer carried on, which reinsurance uses to keep the cession), `ApproveReinstatement` (decision recorded on `LapseRecord.approval`; `Reinstate` in `lifecycle.rs` refuses `underwriting_required` past the product's `reinstatement_evidence_free_days` without an in-date decision, and applies its terms from the reinstatement day). Product feature field `reinstatement_evidence_free_days` in `crates/product`. Tests: `crates/sim/tests/underwriting_door.rs`, and in `crates/underwriting/tests/`.
- `infra/underwriting.bicep`: the clinical identity, storage account and wrapping key. Compiled by CI only; never deployed.

**Seams with other areas.** Reinsurance owns capacity and the treaty; alterations and reinstatements reserve none because the cover is already ceded (agreed with reinsurance, 2026-11-12). Claims owns cause coding and reads the exclusion library through `for_claims()`; `tests/libraries.rs` holds both matchers to the same answers. Group administration opens excess proposals as caller `group-admin` with id `grp-uw:{scheme}:{case}` and sends `policy_issued` back; its contract tests deserialize this crate's `Envelope` and `Effect` and include `tests/common/mod.rs` by path, so renames there break `crates/group/tests`. The core thread owns the ledger side of non-disclosure remedies (below).

**Not finished, in the order to do it.**

1. Non-disclosure remedies in the core. The core thread agreed (2026-11-12) to write `AvoidPolicy { policy, case, premium: Refund | Retain }` and a vary command `{ policy, case, terms: Vec<LayerTerms> }`, caller `underwriting`, with no back-billing on a variation. When they land, route `Effect::NonDisclosureRemedy` in `driver.rs` (it already carries `terms` and `premium`), key `{review}/remedy`, park any refusal; replace the parked case in `tests/driver_branches.rs`.
2. Evidence-free reinstatement days per product. Mitchell asked this thread to decide and document (2026-11-12). Not yet set in `products/*.star`; changing a product file changes its content hash, so it should go in as a new product version with its own `effective_from`, and the `underwriting_door.rs` test that relies on the shipped product asking for no evidence must be revisited.
3. Receivers for notices, evidence requests and delegate file requests: the driver parks them. Each needs a door in documents or an evidence provider and a `Forward` implementation in `driver.rs`.
4. A process that writes disclosures to the clinical store, and deploying `infra/underwriting.bicep` (bring it into `infra/main.bicep`).
5. Risk codes on capacity reservations (`ReserveCapacity` effect does not send them), a horizon for the module's idempotency keys (agreed with the drivers), peer review drawing without scanning every proposal, staff grants, publishing proposal progress to channels, and an interim cover product.
6. Retry of an increase or alteration refused for `billing_behind` is manual.

**Open questions for Mitchell.** Whether an avoided policy's premiums are refunded or kept is recorded per case by the two seniors; which the law allows needs legal review against the Contracts of Insurance Act 2024 regime. The design describes a Temporal workflow per proposal; the module uses its state machine and the daily sweep instead, so timers are accurate to the day.

**Checks.** At the last run on main (2026-11-12) fmt, clippy, the test-doc check and all `ic-underwriting`, `ic-core` and `ic-sim` tests passed; crate line coverage was last measured at 98.38% before the alteration and reinstatement work and has not been re-measured since.

## Payments and bank channels

This section is the handover for money in and money out: what exists, where it is, and everything that is unfinished, written so the work can be finished without the thread that built it. The fuller description and runbook are in [payments.md](payments.md). State as of 12 November 2026.

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

1. **Bank formats and transport (waits on the question above).**
   - Done: `iso20022::document(batch, ordering)` writes `pain.001.001.09` (payments) and `pain.008.001.08` (collections) deterministically, refuses identifiers over 35 characters, and is tested.
   - To do: an `OrderingAccount` (name, number, optional BIC) per bank account in the channel settings; a format setting per channel in `settings.rs` so `HttpBank` or a new file bank sends `document(...)` instead of `Batch::text()`; the hash and manifest must then be taken over the ISO bytes, which means `Batch::content_hash` in `crates/payments/src/file.rs` has to hash what is actually sent.
   - To do: reading the bank's answers, `pain.002` (acknowledgement and rejection per item) and `camt.053` or `camt.054` (statement, settlement and returns), into the existing `BankMessage` values. The workspace has no XML parser; one must be added to `Cargo.toml` (for example `roxmltree`) and `Cargo.lock` updated.
   - To do: SFTP for the file channel and mutual TLS for both. The certificate expiry is configured and checked but no client certificate is presented. The secrets belong in the payments Key Vault.
   - If a New Zealand domestic direct credit format is chosen instead, it is a second writer beside `iso20022.rs` with the same signature.
2. **Durable message cursors.** `Pump` keeps how far it has read each bank's messages in memory (`pump.rs`). Move the cursor into the module's state with a small command, so a restart does not re-read a bank's whole history. Replays are harmless today because every message is recorded under an idempotent key.
3. **Unit-linked payout refused before settlement.** `failed_before_settlement` in `intake.rs` tells claims but not the core's investment ledger. The core needs a command for it (ask the core owner); until then servicing must re-instruct, and the reference is spent.
4. **Scheme members.** `PaymentAuthorityCancelled { policy, claim }` names no life, so `payer_deceased` in `authority.rs` ends every authority on a scheme policy, the employer's included. The core owner was asked for an optional `life` on the event; when it exists, filter on it. A credit a member pays in themselves is not matched to the member, because no per-member payment reference is issued. `RefundMemberCredit` is settled by the core and is not paid through a run.
5. **Claims.** `RecordRepayment` is never sent: an inbound credit is not tied to an overpayment. The `account_ref` claims verified is carried on the waiting payment but not compared with the payee's account. `ConfirmRun` assumes a run's `claim_paid` events arrive together; if some arrived after the earlier ones had all settled, the confirmation would be early (claims was told and has not objected).
6. **Reinsurance.** A statement whose net is owed to the insurer has no command; only payments to a reinsurer are handled.
7. **Receipt matching.** A `Match` instruction hands an inbound credit to the core's cash matching with the bank reference. Resolving the payer from an authority first is not done.
8. **Drill payments.** A drill's token payments appear on the statement as unexpected lines and are cleared as exceptions by a person. They are not posted.
9. **Account details at rest.** Payee and authority account details are in the module's state in clear. They are not sealed per person, so retention shredding in the customer master does not reach them.
10. **Notices.** `Notify` instructions are written to the structured log (`payments.notice`) with a role. Delivery to a work queue or a person is not wired to the channels or documents modules.
11. **Egress addresses.** Recorded per region and checked in configuration, not enforced on the network.
12. **Screens.** The portal has no payments operations screens (run preparation, approval, exceptions). The commands and queries they would call exist.

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
| Data mart | `src/mart.rs` | In the module's replicated state, fed from the same events before the ledger posts them. In-force, movement and experience extracts, definition version 4. Rating factors are kept by the date recorded; date of birth never leaves the mart. Policy suspense by policy. Admitted claims from the claims module's facts. |
| Tables | `src/tables.rs`, `config/finance/tables.star` | Validation of a rate table against its schema and previous version; dry run of a proposed version against the in-force book. Pure functions plus the question `table_dry_run`. |
| Host | `src/host.rs` | Module `finance` on the ledger nodes (`module_spec()`), reading core events; every question the module answers. |
| Close driver | `src/driver.rs`, `src/service.rs`, `src/bin/ic-finance-close.rs` | Service `ic-finance-close`: a restartable pass on a timer. Records verified Merkle roots, writes extracts to immutable blob storage and registers them, asks each module for its close report and forwards it, reports its own check, closes signed-off periods. |

Tests are in `crates/finance/tests/`, one file per part; `common/mod.rs` runs a real in-memory core beside the books. `tests/service.rs` runs a real node with the fake Azure store.

### Seams with other areas

- **Core.** Any new account role in `crates/core/src/accounts.rs` needs a line in `config/finance/chart.star` with the same class; `tests/config.rs::the_shipped_chart_maps_every_core_role` fails the build otherwise, and in production the feed would stall. The investment ledger answers `{"waiting_cash":{"as_at"}}` per partition for the `pending_investment_cash` aged check (`core_sources()`).
- **Close reports** (`driver.rs`, one `*_sources()` function per module; the caller name must match `by` in `close.star`): distribution (`commission_sweep`, aged `held_balance`), payments (`fund_cash_settlement`, aged `unidentified_receipts`), reinsurance (`reinsurance_sweep`, quarter ends), unit pricing (`unit_pricing`), core (aged `pending_investment_cash`), finance itself (aged `policy_suspense`). A check answer is `{"green","items"}`; an aged answer is `{"as_at","items","total_minor","count"}`.
- **Claims.** The mart reads `Event::Module { module: "claims" }` facts by key: `claim_reserved` (`policy`, `cover`, `life`, `event_date`), `claim_admitted` (`admitted_minor`, `monthly_minor`), `admission_reversed`. If the claims module renames a key the mart counts nothing for it and does not fail; there is no test against the real claims module, only hand-built events in `tests/mart.rs`. Claims and complaints send `RecordConductFacts`.
- **Portal.** People act through `/finance` in the portal under caller `finance-console` ([channels.md](channels.md)).
- **Azure.** Merkle roots are read from, and extracts written to, immutable blob containers (`merkle-roots`, `finance-extracts`). Only the fake store has been exercised.

### Not finished or not started

1. **Temporal workflow for the close.** Not started. The driver is a timed pass whose whole state is in the ledger, so it is safe to restart, but a step is not visible or retryable on its own. To build: one workflow per period wrapping the steps of `Driver::pass` in `src/driver.rs` as activities; the commands are already idempotent by key.
2. **Mart state grows without bound.** `Mart.movements`, `claims`, `admissions` and ended policy rows are kept for ever inside the module's replicated state and its snapshots. Fine for tests; before production volumes it needs either pruning of anything older than the oldest period that may still be cut (an extract as at a closed date must stay reproducible until it is stored and registered), or moving the mart to its own store. Not started, no measurement taken.
3. **Scheme benefit category is not published to finance.** The extracts have no category column and `tables::dry_run` cannot price a category table (`rate_for` skips category rows). Needs `category` on `LifeInfo` or `MemberCover` from the core, then a column in `mart.rs` (bump `DEFINITION_VERSION`) and a field on `RatedCover`.
4. **Loadings are not published**, so the dry run prices before loadings, discounts, fees and tax. It shows movement, not a bill.
5. **Experience age bands.** Exposure is banded on age at the end of the period asked; a claim on age at the event date. Around a birthday a claim can fall in the band beside its exposure. An actuary should say which basis is wanted (`mart.rs`, `experience`).
6. **No test against the real claims module** for the admitted-claims feed (see the seam above). `crates/sim/tests/claims.rs` shows how to drive it.
7. **Azure.** `infra/finance.bicep` and `deploy/k8s/finance.yaml` have never been deployed, and the service has not run in the integrated Mac cycle.
8. **Tax and analytical review** are reported by a person at the console; nothing works them out.
9. **Table validation is a library and a CI test.** Activating a rate table in the core does not require a passed validation report or dry run; nothing enforces the gate at activation.

### Questions for Mitchell

- The holiday list in `close.star` is New Zealand national holidays for 2026 to 2028, with no regional anniversary day. Is New Zealand right, and which region's day applies? Someone must add each new year.
- The chart's account codes, the close's due days and aged thresholds, and the shipped return (`config/finance/returns/insurer_quarterly.star`) are placeholders for finance and compliance to replace with the real chart and the regulator's template.
- The rate tables under `products/rates/` are synthetic.
- Is a restartable timed pass acceptable for the close in production, or is the Temporal workflow required first?
