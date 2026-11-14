# Unit pricing

`crates/pricing` (`ic-pricing`) strikes each fund's unit price from the fund administrator's valuation, checks it, has a second person approve it, and carries it to every partition of the core's investment ledger as a logged command. It also corrects a price found wrong after it was used and sends the administrator the unit holdings it needs. It is hosted on the deployed node as the module `unit-pricing`, in its own replicated group, because a price is one fact for every partition.

The core owns what a price does: the investment ledger holds funds, prices, orders and holdings, executes orders once the price for their date is active, and replays orders onto a corrected price (`docs/decisions.md`, "Unit-linked business is part built"). This module owns which price is right.

## A price, start to finish

1. **Fund.** The module follows the core: each `FundActivated` event carries the core's `ic_core::invest::FundDefinition`, and the module takes it in as its own `ActivateFund`, keyed by fund, version and content hash, so it holds exactly the versions the core holds whoever activated them, and the event from each partition is taken once. `ic_pricing::command::activation` builds the same command under the caller `config` for a fund sent directly. The shipped funds are Starlark files in `products/funds/`, and a test here checks every one is accepted. The module's terms are in its `pricing` map and are checked when the fund is activated. Unknown keys are refused.

   | Term | Meaning |
   | --- | --- |
   | `previous_move` | the largest move from the previous price that passes without a hold, as a fraction (`0.05`) |
   | `benchmark_gap` | optional: the largest gap between the fund's move and its benchmark's |
   | `stale_asset_days` | an asset priced more than this many days before the pricing date is stale |
   | `unexplained_move` | the largest move in net assets that applications and redemptions do not explain |
   | `error_threshold` | a correction smaller than this, as a fraction of the price, is immaterial |
   | `launch_price` | the first price's previous, for the move check |
   | `pricing_lag_days` | business days after the pricing date by which the price is due |
   | `time_zone` | optional, default `Pacific/Auckland`: where the cut-off is read |
   | `compensation_minimum_minor` | optional, 0 to 100000: the core's per-order replay threshold |

2. **Valuation.** The driver reads the administrator's file and proposes `ReceiveValuations`. A file it cannot read is recorded with `RefuseValuationFile` and the reason, and shows in the `work` question. A valuation sent again for the same fund and date is a new revision; a price struck from an earlier revision can no longer be approved.

3. **Prepare.** A preparer (`unit_pricing_preparer`) sends `PreparePrice { fund, pricing_date }`. The module strikes mid, buy (rounded up) and sell (rounded down) at the fund's decimals and spreads, hashes every input, and runs the checks:
   - **Units on issue** (blocking): the administrator's units must equal the sum of the core's units on every partition once every partition reports the previous price's date as executed through (nothing pending for it or any earlier date, including a switch buy whose sell has not yet run). A difference makes the price `Blocked`; nobody can approve it.
   - **Previous move**, **benchmark**, **stale assets**, **unexplained move**: a failure makes the price `Held`.

4. **Approve.** An approver (`unit_pricing_approver`), never the preparer, sends `ApprovePrice`. A held check must be accepted by name with a reason. Approval is refused if the valuation, the core's units or the previous price have changed since the price was prepared, if another price for the slot is already on its way, if the fund is suspended, if an open price error on the fund is not yet corrected, or if an earlier pricing day has no price and was not suspended. `RejectPrice` ends it.

5. **Publish.** An approved price is queued for each partition as `InvestCommand::ActivateUnitPrice` under the caller `unit-pricing`, keyed by `{price}/{partition}/{attempt}`. The node's host carries the outbox and reports each outcome back with `RecordDelivery`. The price is `Active` once every partition has applied it, and `PriceActivated` is published. A partition that refuses it leaves the publication refused for the lead to `Republish` after the cause is fixed.

A fund lead (`unit_pricing_lead`) can `SuspendFund` and `ResumeFund`; a suspended fund takes no approvals.

## Price errors

`RaisePriceError` (lead) opens an error over one or more active prices of a fund, with a reason. Each is re-prepared and approved as a new version that supersedes the old one, and the core replays the orders executed at the old price (its order run sends `ReplayOrders` with the fund's `compensation_minimum_minor` as `materiality_minor`). A correction smaller than `error_threshold` is recorded as `Immaterial` and is not sent to the core. While an error is open and not yet corrected, no new date of that fund can be approved, so a wrong price does not become the previous of the next one.

The module reads the core's `UnitOrderReplayed` events and keeps a remediation record per error: the policies affected, units and cash moved, what the insurer bore, and orders below the threshold that were not compensated. `CloseError` closes it once every correction is active.

## The holdings extract

The module reads the core's `UnitOrderExecuted` events and keeps each policy's holding and each date's applications and redemptions. When every partition reports that a fund's orders for a date have all executed, it takes a snapshot, and the driver writes it to the outbound container as `extracts/<fund>/<date>.csv`:

```
H,unit-pricing,<extract id>,<fund>,<pricing date>,<price id>
D,<policy>,<units>
F,<currency>,<applications>,<redemptions>,<net>,<orders>
T,<holders>,<units on issue>
```

The file is written once; a second write of the same name is success. `RecordExtractWritten` records its name and SHA-256 digest.

## The valuation file

Plain comma-separated text, at most 4 MiB, at `valuations/<YYYY-MM-DD>/<any name>` in the inbound container:

```
H,<administrator>,<file id>
V,<fund>,<pricing date>,<net assets>,<units on issue>,<applications>,<redemptions>,<benchmark return or empty>,<restated Y|N>
A,<fund>,<pricing date>,<asset>,<priced on>
T,<number of V lines>,<number of A lines>
```

Amounts are in the fund's currency in major units and units carry up to six decimals. A file with a bad trailer, a record it does not know or a malformed field is refused whole, with the line and the reason.

## The driver

`ic-unit-pricing` runs a pass every 30 seconds (`crates/pricing/src/driver.rs`):

1. lists the inbound folders of the last seven days, reads each file once Defender for Storage has tagged it clean, and proposes its valuations; a file tagged with a threat is refused unread, and an untagged one waits;
2. asks every partition for each fund's units on issue (`Question::UnitsOnIssue`) and proposes each figure that changed;
3. writes each extract that is due;
4. raises each alert that is new (see Alerts).

A fault in one step is reported in the pass's log line and the others still run. Every proposal carries a key made from what it is about (`valuation-file:<name>`, `units:<fund>:<partition>:<executed through>:<units>`, `extract-written:<id>`), so a pass repeated after a crash, or two drivers at once, propose nothing new.

| Setting | Meaning |
| --- | --- |
| `IC_NODES` | the ledger nodes |
| `IC_UNIT_PRICING_INBOX_ENDPOINT` | the inbound account's blob endpoint |
| `IC_UNIT_PRICING_INBOX_CONTAINER` | default `unit-pricing-inbound` |
| `IC_UNIT_PRICING_OUTBOUND_ENDPOINT` | the outbound account's blob endpoint |
| `IC_UNIT_PRICING_OUTBOUND_CONTAINER` | default `unit-pricing-outbound` |
| `IC_UNIT_PRICING_CYCLE_MS` | default 30000 |
| `IC_UNIT_PRICING_PATIENCE_MS` | how long one command looks for a leader, default 10000 |
| `IC_CLUSTER_TOKEN_FILE` | the cluster token, mounted from Key Vault |

## Alerts

The `alerts { as_at }` question lists what operations must be told of, each item with a stable `id`. The driver logs each new item once as an error line with the target `unit_pricing.alert`, and `infra/unit-pricing.bicep` turns those lines into Azure Monitor alerts mailed to `alertEmails`.

| Kind | Raised when | Clears when |
| --- | --- | --- |
| `late_price` | a fund's price for a date is past its lag with no active price and no suspension | the price is active, or the date is suspended |
| `publication_refused` | a partition refused an approved price | the lead republishes and every partition applies it |
| `file_refused` | a valuation file could not be read (malformed, or the scan found a threat) | not: the file stays refused; the administrator sends a new one |
| `definition_refused` | a fund definition the core activated has terms this module refuses | a version of that fund this module accepts arrives |

A restarted driver raises late and refused prices again, since they still stand, but not files refused before it started. Two more alerts watch the driver itself: **Unit pricing driver failing** when ten or more passes in fifteen minutes have errors, and **Unit pricing driver silent** when no pass is logged for fifteen minutes (every pass logs a line at info).

## On Azure

`infra/unit-pricing.bicep` creates:

- the **inbound** account, Entra ID only, with Defender for Storage malware scanning on upload and a lifecycle rule that deletes valuation files after 90 days. The administrator writes over HTTPS (AzCopy or any Blob client) under its own Entra service principal, which a role condition allows to write only under `valuations/` and never to delete or set tags, so it cannot forge a clean scan result. SFTP is not used: Blob Storage's SFTP needs a hierarchical namespace, and such an account has no index tags, which is where the scan result is written.
- the **outbound** account, with a time-based immutability policy (seven years by default) on the extracts container; the administrator may only read it;
- the driver's managed identity, federated to the `ic-unit-pricing` service account, with read on inbound, write on outbound and the cluster token.

`deploy/k8s/unit-pricing.yaml` runs two replicas of the driver with that identity.

## The console

Staff act through the unit pricing console in the portal (`/pricing` in `ic-portal`, `docs/channels.md`, "The unit pricing console"). It sends the console commands under the caller `unit-pricing-console` with the staff member's party ID as actor and their roles, and reads the questions below.

## Questions

All are `{"q": "..."}` to the module: `status`, `funds`, `price`, `prices` (a fund between two dates), `valuations`, `work` (prices waiting for a person and refused files), `late` (prices overdue as at a date), `errors`, `error`, `outbox`, `events`, `extracts_due`, `holding`, `close_check` and `alerts`.

`close_check { as_at }` is the finance close's check: `green` when no price is late, none is waiting for a person, no price error is open and every extract due is written, otherwise the `items` that stop it.

## Known gaps

- **The finance close does not ask yet.** `close_check` is built here, but the close driver has no source for it; the finance thread wires it beside `core_sources()`.
- **Not deployed.** `infra/unit-pricing.bicep` is compiled in CI but not referenced by `main.bicep`, and its Defender resource uses a preview API version. `deploy/k8s/unit-pricing.yaml` has not been applied to a cluster.
