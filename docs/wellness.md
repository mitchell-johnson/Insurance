# Wellness: AIA Vitality

The wellness module (`crates/wellness`, module `wellness` on the ledger nodes) works out each member's AIA Vitality Discount. It tells the core what the discount is, and the core takes it off the bill. It follows AIA's Premium Adjustment Rules (September 2023).

## What it does

- **Membership.** The AIA Vitality programme tells the module three things: a life on a policy has joined (`Join`), a member's status (`ReportStatus`: the status earned this Vitality year, and the status reached at a Vitality anniversary), and a member has left (`Leave`). These arrive under caller `vitality`.
- **Initial Discount.** From the day a member joins, their eligible cover is discounted 10%.
- **Premium Flex.** The module decides two weeks after each policy anniversary that follows the join. The new discount applies from the anniversary itself, and the core adjusts any bill already raised for it.
  - In the first 10 months of membership, the discount only rises: Platinum adds 2 points, and any other status keeps it.
  - In months 11 and 12, the status earned so far flexes it.
  - After that, the status reached at the member's Vitality anniversary in the year to the decision flexes the discount from the previous anniversary.
  - Each status moves the discount by a fixed step: Platinum +2 points (up to 20%), Gold no change, Silver −1 and Bronze −2 (never below 0%). A member with no status reported counts as Bronze.
- **Leaving.** The discount comes off from the policy's next premium due date. The module learns that date from the core's bills, including a bill raised before it is due.
- **Eligible products:** `aia-living`, `aia-living-business`, `aia-start-up-income`, `aia-private-health` and `aia-cancer-care`. These are the products whose discount chains name `aia-vitality`, and a test holds the list to the products. Cover for Life and Essentials Life are excluded by AIA.

## How it reaches the core

Each change is one `SetEntitlement` (caller `wellness`) for entitlement `aia-vitality` on the member's life. Its tier is the discount in whole percent. The product's `by_tier` table turns the tier into the discount line `vitality` on the bill. The module reads the core's `PolicyIssued`, `PremiumDue`, `PolicyStatusChanged` and `BusinessDateAdvanced` events. The node host carries its outbox to the core, and each refusal is kept for operations (question `refused`).

## Questions

`member`, `outbox`, `refused` and `state_hash`.

## Not built

- **The programme's feed.** AIA has not said how the Vitality programme reports joins, statuses and leavers, so there is no intake service and no workload grant for it in `deploy/cluster-access/grants.json`. When the format is known, a small service under its own Entra identity sends the three commands to the module.
- **The notice to the policy owner** before a discount change. The core's `EntitlementChanged` event is the trigger; documents has no template for it yet.

## Tests

- `crates/wellness/tests/flex.rs`: the rules.
- `crates/wellness/tests/hosting.rs`: events read, questions, outbox, keys, and the eligible list.
- `crates/wellness/tests/on_the_core.rs`: the discount on real bills, the flex at the anniversary, and the removal after leaving.
