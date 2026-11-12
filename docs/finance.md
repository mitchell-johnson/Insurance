# The finance ledger

`crates/finance` (`ic-finance`) keeps the insurer's books. The core keeps every policy's money on its partitions. The finance ledger holds one consolidated ledger by account, statutory fund, product and posting type, built only from the events the core publishes, and proves every business day that it equals the core. It is hosted on the deployed node as the module `finance`, in its own replicated group.

Configuration is in `config/finance/`: the chart of accounts, the close configuration, the actuarial table schemas and the return definitions, all Starlark.

## How money reaches the books

- The core publishes each posted entry as lines by account role (`Event::EntryPosted.lines`), with its value date and the entry it reverses, and after each close the partition's balances by role (`Event::ControlTotals`).
- The chart (`config/finance/chart.star`) maps each role to one finance account. Accounts fed this way are control accounts and are locked against manual journals. A role the chart does not map stops the feed from that partition until a chart version that maps it is approved.
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
| Worked out by the ledger | `control_check`, `valuation_extracts`, `unapproved_journals`, `actuarial_journals` (quarter ends), `trial_balance`, `fund_balance` |
| Reported by a named caller | `commission_sweep` (commission), `reinsurance_sweep` (reinsurance, quarter ends), `fund_cash_settlement` (payments), `subledger_reconciliations` (close-workflow), `actuarial_handover` (reporting, quarter ends), `tax_reconciliation` and `analytical_review` (finance-console) |
| Aged items reported by a named caller | `aged:held_balance` (commission), `aged:policy_suspense` (collections), `aged:pending_investment_cash` (investment) |

- A blocking check that is red stops sign-off. The finance lead may waive it on the record with a reason, a remediation owner who is someone else, and a date. A waiver never clears the item: the next period's check starts red again and the earlier waiver stays on the dashboard until it is green.
- Held balances are a blocking month-end item. An item older or larger than the threshold with no documented action turns `aged:held_balance` red and names the party.
- The finance lead signs off every period, and the appointed actuary also signs a quarter end. A posting after sign-off clears it.
- Months close in order. A closed period is frozen with its trial balance and a hash, and takes no journal, report, waiver or sign-off afterwards.
- After the last month of the financial year closes, `ProposeYearEndClose` builds the journal that closes income and expense to retained earnings, fund by fund, in the year's adjustment period (`YYYY-MM-ADJ`), which is signed and closed on its own.

## Reports

All are questions to the module (`{"q": "..."}`): `status`, `dashboard` (steps by status, owner and due date, blockers, waivers, sign-offs still needed, control days, prior-period items, aged items by party, extracts, journals), `checks`, `trial_balance` (optionally one fund), `movements` (opening, activity, prior-period columns, closing, year to date), `sources` (the partition days behind a period), `backdated`, `journal`, `conduct`, `return`.

## Regulatory returns and conduct

- A return definition (`config/finance/returns/`) is a list of lines, each a sum of account balances, account movements or a conduct measure. `FileReturn` works the return out from a closed period and stores it. Each line shows the accounts behind it.
- A filed return never changes. When a later posting restates its period, `returns_needing_restatement` lists it, and `RestateReturn` files a new version with a reason, showing the restating amount by the period it was posted in.
- Conduct facts are reported by claims and complaints with an opaque ID, a date and a product, and counted once. A decline that is later overturned stays a decline in the period it was made. The overturn is counted on its own in the period it happened, against the period of the decline, and a reversed admission likewise. The two are never netted.

The shipped return is illustrative. Its lines and account mappings are for finance and compliance to replace with the regulator's template.

## Actuarial tables

`ic_finance::tables::validate` is the gate a rate table passes before activation, against the schema declared for it in `config/finance/tables.star`:

- manifest (model, model version, basis report, author), schema, shape (every rating cell present, age bands without gap or overlap), range and precision;
- the rate never falls from one age band to the next, never jumps by more than the schema's largest step, and a smoker is never rated below a non-smoker. The adjacent-age checks are the control that catches a cell mis-keyed by a factor of ten, which a range check passes;
- against the previous version: a small uniform change passes, a larger move in a cell is listed for the actuary, and a cell that moves by more than the blocking factor blocks;
- `cross_check` for tables that must line up, such as a premium table and the reinsurer's.

A blocking finding stops activation. The test suite runs every shipped table through its schema, so a table edited in the repository that breaks a rule fails CI.

## Who may send what

Commands are `FinEnvelope { key, caller, actor, roles, command }`. The caller is the sending service; the actor and roles are the person and the roles the gateway verified.

| Caller | Commands |
| --- | --- |
| `operator` | `Configure`, `RetryFeed` |
| `feed` | `CoreEvent`, `Ingest`, `RecordMerkleRoot` |
| `reporting` | `RegisterExtract`, and the checks it reports |
| `claims`, `complaints` | `RecordConductFacts` |
| `finance-console` | everything a person does: propose and approve configuration, journals, waivers, sign-off, closing, returns |
| `close-workflow` | `ClosePeriod`, and the checks it reports |
| the caller a check or aged kind names | `ReportCheck`, `ReportAgedItems` |

Roles: `finance_operator` enters, `finance_approver` approves journals, `finance_lead` approves configuration and fund transfers, waives, signs off and restates, `appointed_actuary` signs actuarial journals and quarter ends.

## Setting it up

1. `Configure { partitions, year_end_month }` from `operator`.
2. Propose and approve the chart, then the close configuration. Until the chart is active the events the node delivers are held, not lost.
3. Record each day's Merkle root as it is published.

## Not built yet

- The data mart and the valuation and experience extracts. The ledger registers extracts (immutable, tied to the Merkle root of their date) and the close requires the in-force and movement extracts, but nothing cuts them yet. They need de-identified rating facts on the core's events, which the core thread has noted.
- The drivers that connect the other modules to the close: the Merkle root publisher, the callers of `ReportCheck` and `ReportAgedItems`, and the close workflow in Temporal.
- The dry-run compare tool for a new table against the in-force book.
- Public holidays in step due dates. Weekends are skipped.
