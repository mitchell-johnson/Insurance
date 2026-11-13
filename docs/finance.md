# The finance ledger

`crates/finance` (`ic-finance`) keeps the insurer's books. The core keeps every policy's money on its partitions. The finance ledger holds one consolidated ledger by account, statutory fund, product and posting type, built only from the events the core publishes, and proves every business day that it equals the core. It is hosted on the deployed node as the module `finance`, in its own replicated group.

Configuration is in `config/finance/`: the chart of accounts, the close configuration, the actuarial table schemas and the return definitions, all Starlark.

## How money reaches the books

- The core publishes each posted entry as lines by account role (`Event::EntryPosted.lines`), with its value date and the entry it reverses, and after each close the partition's balances by role (`Event::ControlTotals`).
- The chart (`config/finance/chart.star`) maps each role to one finance account. A test reads the role list from the core's source, so a role added to the core without an account here fails the build and not the feed. Accounts fed this way are control accounts and are locked against manual journals. A role the chart does not map stops the feed from that partition until a chart version that maps it is approved.
- The lines of one partition and business date are posted as one summarised journal when that day closes. Its detail names the partition, the date, the closing chain head and the range of core entries it carries. That is the lineage from a ledger figure to the core.
- The node shows the module each event once. An event the books cannot take yet (no chart active, a role not placed) is held in the replicated state, in order, and taken in when the cause is fixed. `FeedStalled` and `FeedResumed` say when. Nothing the core published is dropped.

## The daily control check

For every partition and business date the books built from the lines must equal the core's own control totals, taken at the chain head the day closed on. The day's signed Merkle root is recorded with `RecordMerkleRoot`, and a date is anchored when the chain heads checked here produce that root. A difference, a missing partition, or a missing or wrong root keeps the period's `control_check` red.

## Prior-period items

Nothing posted is ever edited. A backdated entry or a reversal is a new entry today with its original value date. Where that date falls in an earlier period, the line is kept apart as a prior-period item under the period it belongs to, so the current period's own activity reads on its own. A late-notified death that unwinds months of premium shows in the movement report in the prior-period columns, each reversal can be walked to the entry it reverses, and the closed months keep the trial balances they closed with.

The end-of-day batch for a date posts on the next business date with the closed date as its value date. That is the new date's own activity, at month end as on any other day. Only a value date before the previously closed business date makes a prior-period item.

## Manual journals

A journal is entered by a maker and posts only when a checker approves it; the two are different people whatever roles they hold. It needs a reason and evidence, balances in each currency and within each statutory fund, and cannot touch a control account. Only a fund transfer journal may name more than one fund, and only the finance lead approves one. An actuarial journal names the registered extract it was calculated from and carries the appointed actuary's sign-off. A journal dated into an earlier period is a prior-period item.

## The close

A period moves to closing when its last day has closed on every partition. `config/finance/close.star` declares the checks, their owners, the working day each is due, and which block sign-off.

| Kind | Checks |
| --- | --- |
| Worked out by the ledger | `control_check`, `valuation_extracts`, `unapproved_journals`, `actuarial_journals` (quarter ends), `trial_balance`, `fund_balance`, `migration_control` |
| Reported by a named caller | `commission_sweep` (commission), `unit_pricing` (unit-pricing), `reinsurance_sweep` (reinsurance, quarter ends), `fund_cash_settlement` (payments), `subledger_reconciliations` (close-workflow), `actuarial_handover` (reporting, quarter ends), `tax_reconciliation` and `analytical_review` (finance-console) |
| Aged items reported by a named caller | `aged:held_balance` (commission), `aged:unidentified_receipts` (payments), `aged:policy_suspense` (close-workflow, from the ledger's own feed), `aged:pending_investment_cash` (investment) |

- A step is due a number of working days after period end. Weekends and the `holidays` the close configuration lists are skipped. The list holds national public holidays on the day they are observed, a weekend date is refused, and each new year has to be added before it starts (a year with none listed counts weekdays only).
- `migration_control` watches the accounts a migration posts its other side to (listed in the close configuration). The first period one is posted to is its cutover; anything posted through it in a later period turns the check red, even where a load and its reversal net to nothing.
- A blocking check that is red stops sign-off. The finance lead may waive it on the record with a reason, a remediation owner who is someone else, and a date. A waiver never clears the item: the next period's check starts red again and the earlier waiver stays on the dashboard until it is green.
- Policy suspense is aged by the ledger itself. It follows every posting to the `policy_suspense` role by policy as the feed arrives, so the list is the control account split by policy, and `since` is the day the present holding began (a holding that was used up and paid again starts afresh). The question `{"q":"aged_suspense","as_at"}` answers with the items oldest first, as at the latest day closed on every partition.
- A person records what is being done about an aged item with `DocumentAgedItem { kind, party, since, action, owner }` at the console (operator or lead). The action stays with the item while the same holding is reported again, and goes when the holding ends.
- Held balances are a blocking month-end item. An item older or larger than the threshold with no documented action turns `aged:held_balance` red and names the party.
- The finance lead signs off every period, and the appointed actuary also signs a quarter end. A posting after sign-off clears it.
- Months close in order. A closed period is frozen with its trial balance and a hash, and takes no journal, report, waiver or sign-off afterwards.
- After the last month of the financial year closes, `ProposeYearEndClose` builds the journal that closes income and expense to retained earnings, fund by fund, in the year's adjustment period (`YYYY-MM-ADJ`), which is signed and closed on its own.

## Reports

All are questions to the module (`{"q": "..."}`): `status`, `dashboard` (steps by status, owner and due date, blockers, waivers, sign-offs still needed, control days, prior-period items, aged items by party, extracts, journals), `checks`, `trial_balance` (optionally one fund), `movements` (opening, activity, prior-period columns, closing, year to date), `sources` (the partition days behind a period), `backdated`, `journal`, `conduct`, `return`, `work`, `extract`.

## Regulatory returns and conduct

- A return definition (`config/finance/returns/`) is a list of lines, each a sum of account balances, account movements or a conduct measure. `FileReturn` works the return out from a closed period and stores it. Each line shows the accounts behind it.
- A filed return never changes. When a later posting restates its period, `returns_needing_restatement` lists it, and `RestateReturn` files a new version with a reason, showing the restating amount by the period it was posted in.
- Conduct facts are reported by claims and complaints with an opaque ID, a date and a product, and counted once. A decline that is later overturned stays a decline in the period it was made. The overturn is counted on its own in the period it happened, against the period of the decline, and a reversed admission likewise. The two are never netted.

The shipped return is illustrative. Its lines and account mappings are for finance and compliance to replace with the regulator's template.

## The data mart and extracts

`ic_finance::mart::Mart` is the book of policies and covers as the core's events describe it. It is a read model: it holds nothing the events do not carry, knows a policy and a life only by opaque ID, and is rebuilt from the first event of each partition. Events may arrive in any order between partitions; within a partition a gap is refused.

| Extract | Rows | Control totals |
| --- | --- | --- |
| `in_force(as_at)` | every cover in force at the close of the date | policies, covers, sum insured |
| `movement(from, to)` | every change to the book and every premium billed, with the business date it was recorded on and the date it took effect | opening and closing policies, covers and sum insured, premium |
| `experience(from, to)` | days of exposure and the claims that ended a policy, by product and cover type | cover days, sum insured days, ending claims |

- An extract is cut only as at a date every partition has closed.
- Something recorded after a date closed never changes that date's extract. A backdated change is a movement of the period it was recorded in, with its own effective date. The same extract cut a month later is the same file to the byte.
- The movement rows are the difference each event makes to its policy; the in-force rows come from the dates each cover was known between. An extract is `reconciled` when the two agree: opening plus movements equals closing.
- `Extract::bytes` is the file (CSV), `name` the path it is stored under in immutable storage, and `record` the `ExtractRecord` to register with `RegisterExtract`: content hash, row count, control totals and the Merkle root of its date.

Each cover row carries the life's age last birthday on the extract date (on the day of the movement, in the movement extract), sex, smoker status and occupation class, where the core published them; the experience extract groups by five-year age band at the period end, sex, smoker status and occupation class. The date of birth stays in the mart and is never in a file. A member who joins or changes after issue carries the factors the core published with that notice, from that business date on: factors are kept by the date they were recorded, so a correction never changes an extract as at an earlier date. The experience extract counts claims two ways. `ending_claims` are the claims that ended a policy, from the core's termination. `admitted_claims`, `admitted_minor` and `admitted_monthly_minor` come from the facts the claims module publishes through the core (`claim_reserved`, `claim_admitted`, `admission_reversed`) and cover every admitted claim, including trauma, income and health claims that leave the policy in force. A claim is placed in its rating cell when it is registered, on the life's age at the event date, and counted once on the business date it is admitted; a further admission adds money only, and a reversal takes the claim and the money back on the date it is decided, so an extract already cut does not change. A death claim is in both counts. A policy avoided for non-disclosure leaves the book on the day it is avoided as an `avoided` movement, and a refund of its premiums takes what was billed back as `premium_refunded_on_avoidance`; where the insurer keeps the premium, the premium billed stands. The layouts are definition version 4.

## The close driver

The mart is part of the ledger's replicated state: it takes each core event as it arrives and needs no chart. `ic_finance::driver::Driver::pass` carries everything between the ledger and the outside, and keeps nothing between runs:

- asks the ledger what the close is waiting for (`{"q": "work"}`);
- reads each closed date's published Merkle root from immutable storage, verifies it, and records it;
- for a period past its last day, reads each extract a page at a time (`{"q": "extract", "kind", "period", "offset"}`), checks the pages hash to the whole, writes the file once to immutable storage and registers it. A file already stored with the same content is registered; one with other content is never overwritten or registered;
- asks each module that owes the close a report and passes it on under that module's caller name. `distribution_sources()` wires the commission sweep and held balances, `payments_sources()` the fund cash settlement and unidentified bank receipts, `reinsurance_sources()` the quarterly reinsurer sweep, `unit_pricing_sources()` the unit pricing check, `core_sources()` the pending investment cash on every partition of the core's investment ledger, and `finance_sources()` the ledger's own aged policy suspense;
- reports `subledger_reconciliations` green when every module answered and the three extracts are registered, and red with the reasons otherwise;
- closes a period that is signed off with nothing blocking, oldest first.

Anything that cannot be done yet is listed in `Pass::waiting` and tried on the next pass. Every command carries a key made from what it is about, so a driver stopped anywhere is started again.

### Running it

`ic-finance-close` is the driver as a process: the ledger and the other modules through the cluster client, roots and extracts on Azure Blob Storage with a workload identity. `infra/finance.bicep` creates its identity, the extracts account with time-based retention, and read access to the core's `merkle-roots` container.

| Setting | Meaning |
| --- | --- |
| `IC_CLUSTER_NODES` | the ledger nodes, `host:port` separated by commas |
| `IC_CLUSTER_TOKEN` or `IC_CLUSTER_TOKEN_FILE` | the cluster token |
| `IC_FINANCE_ROOTS_ENDPOINT`, `IC_FINANCE_ROOTS_CONTAINER` | where the core publishes Merkle roots (container defaults to `merkle-roots`) |
| `IC_FINANCE_EXTRACTS_ENDPOINT`, `IC_FINANCE_EXTRACTS_CONTAINER` | where extracts are written (container defaults to `finance-extracts`) |
| `IC_FINANCE_PASS_SECONDS` | seconds between passes, 60 by default |

`deploy/k8s/finance.yaml` runs two replicas under their own service account. It refuses to start without the cluster or either storage endpoint, runs a pass at once after one that did something, and stops on SIGTERM.

## Actuarial tables

`ic_finance::tables::validate` is the gate a rate table passes before activation, against the schema declared for it in `config/finance/tables.star`:

- manifest (model, model version, basis report, author), schema, shape (every rating cell present, age bands without gap or overlap), range and precision;
- a schema that declares `occupation_classes` needs every cell once for each class, and each class's rows are checked and compared with the previous version on their own;
- the rate never falls from one age band to the next, never jumps by more than the schema's largest step, and a smoker is never rated below a non-smoker. The adjacent-age checks are the control that catches a cell mis-keyed by a factor of ten, which a range check passes;
- against the previous version: a small uniform change passes, a larger move in a cell is listed for the actuary, and a cell that moves by more than the blocking factor blocks;
- `cross_check` for tables that must line up, such as a premium table and the reinsurer's.

A blocking finding stops activation. After validation a proposed version is priced against the book: the question `{"q":"table_dry_run","as_at","request":{covers, per_life, age_at_start, current, proposed}}` prices every cover in force on a closed date under both versions and answers with the annual premium under each, the movement by age band, sex and smoker status, the twenty covers that move most, and the covers the proposed version leaves without a rate (which fails the run). Premiums are before loadings, discounts, fees and tax, so the figures show the movement and are not a bill. A row for one occupation class prices only lives of that class. A scheme's benefit category is not published to finance, so category tables are not priced. The test suite runs every shipped table through its schema, so a table edited in the repository that breaks a rule fails CI.

## Who may send what

Commands are `FinEnvelope { key, caller, actor, roles, command }`. The caller is the sending service; the actor and roles are the person and the roles the gateway verified.

| Caller | Commands |
| --- | --- |
| `operator` | `Configure`, `RetryFeed` |
| `feed` | `CoreEvent`, `Ingest`, `RecordMerkleRoot` |
| `reporting` | `RegisterExtract`, and the checks it reports |
| `claims`, `complaints` | `RecordConductFacts` |
| `finance-console` | everything a person does: propose and approve configuration, journals, waivers, documented actions on aged items, sign-off, closing, returns |
| `close-workflow` | `ClosePeriod`, and the checks it reports |
| the caller a check or aged kind names | `ReportCheck`, `ReportAgedItems` |

The console people use is `/finance` in the portal (`docs/channels.md`, "The finance console"). It sends under `finance-console` with the roles below and refuses the commands that belong to services.

Roles: `finance_operator` enters, `finance_approver` approves journals, `finance_lead` approves configuration and fund transfers, waives, signs off and restates, `appointed_actuary` signs actuarial journals and quarter ends.

## Setting it up

1. `Configure { partitions, year_end_month }` from `operator`.
2. Propose and approve the chart, then the close configuration. Until the chart is active the events the node delivers are held, not lost.
3. Record each day's Merkle root as it is published.

## Not built yet

- A Temporal workflow for the close. The driver is a restartable pass on a timer, with all its state in the ledger.
