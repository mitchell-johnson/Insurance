# Test matrix, part 1: the spine

Policy model, ledger, products and rating, billing and earning, collection and standing, policy transfer, audience scoping and the customer app, resilience and capacity. Most of Phase A lives here.

Columns: **Story** is the user story the test proves. **Personas** are the fixtures it runs on. **Type** is J scripted journey, N negative or refusal, P property test, S simulation invariant, R reconciliation, X access and privacy, F fault injection, L load, O operational. **Ph** is the build phase from the roadmap (A spine, B slices, C widening, D migration and launch). **Pri** is 1 for money, cover or privacy being wrong, 2 for important behaviour, 3 for edges.

## POL Policy model and lifecycle

Design: Policy model; Design review (contradictions fixed).

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| POL-001 | US-P01-3 | P1 | Issue TERM0 for $100,000 on 4 Jan 2027, fortnightly | Policy in force, standing paid, one cover with one layer, accounts created in her group, events emitted with opaque IDs only | J | A | 1 |
| POL-002 | US-P05-1 | P5, P4 | One LIFE1 policy with two single covers, one per life | Two covers each with own sum insured, own layer and own claim path. No joint basis recorded | J | A | 1 |
| POL-003 | US-P29-1 | P29 | Cover with original layer, an increase and two indexation steps | Sum insured on any date equals the sum of layers in force on that date, on both timelines | P | A | 1 |
| POL-004 | US-ST16-2 | P10 | Two indexation layers with identical terms, book, cession basis and no open clawback are merged by command | One layer, same sum insured, same premium, history retained. Merge refused if any distinguishing attribute differs | J | C | 3 |
| POL-005 | US-E03-1 | E3, N1 | Unnamed member record created with age band, salary band and occupation class | Life record exists with no party in the customer master. Rates and bills | J | C | 1 |
| POL-006 | US-B16-1 | B16 | Pool basis: headcount 40 and declared total sum insured | No individuals recorded. Cover expressed in aggregate | J | C | 2 |
| POL-007 | US-P04-2 | P4, P5 | Policy moves in force to grace to suspended to in force across a winter | Status stays in force throughout. Standing derives paid, in grace, suspended, paid. Nobody sets standing by hand, and a command that tries is refused | J | A | 1 |
| POL-008 | US-P17-3 | P17 | Policy lapses, then is reinstated inside the window | Status lapsed then in force. Reinstatement is an event, never a status | J | A | 1 |
| POL-009 | US-P30-1 | P30 | Trauma cover ends on a full claim while life cover stays in force | Cover status differs from policy status. Policy remains in force | J | B | 2 |
| POL-010 | US-ST03-1 | P26, E3 | Leaver notified 40 days late with true effective date | Record shows what was true on the date and when the insurer learned it. A query as at the earlier knowledge date still shows the member in force | J | B | 1 |
| POL-011 | US-P02-3 | P2 | Date of birth corrected with a past effective date | Correction is a new event with a reason. Earlier state visible on the second timeline | J | A | 2 |
| POL-012 | US-ST16-2 | P1 | Send a transition the product rules do not allow (surrendered to in force) | Refused by the product's rules with a reason. No state change, no event | N | A | 2 |
| POL-013 | US-B18-1 | B18 | Quote then proposal, no evidence supplied before the proposal's life ends | Proposal lapses with notice to applicant, reason recorded, no accounts left with a balance | J | B | 3 |
| POL-014 | US-P25-2 | P25, B12 | Formula-driven group cover at two times salary, salary changes | One layer whose amount moves with the member's data. No new layer per salary change | J | B | 2 |
| POL-015 | US-ST16-2 | all | Seeded population of 100,000 synthetic policies drawn from the persona shapes across four lines | Every policy has a valid status, at most one standing, and covers that reference existing lives | S | A | 1 |

## LED Ledger mapping and invariants

Design: Platform foundations; Ledger mapping.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| LED-001 | US-ST16-2 | all | Every journal entry in every scenario and simulation | Postings sum to zero per currency and per unit type separately | S | A | 1 |
| LED-002 | US-P17-1 | P17 | Same receipt command sent three times with one idempotency key | One entry. Second and third calls return the original result | N | A | 1 |
| LED-003 | US-ST16-2 | all | Record a simulated year's log, replay it on a fresh node | Identical state hash and identical head hash per partition | S | A | 1 |
| LED-004 | US-ST16-2 | all | Alter one byte of a stored entry on one replica | Independent verifier detects the break, the monitor stops the line, the replica rebuilds from peers | F | A | 1 |
| LED-005 | US-P05-1 | P5 | Premium falls due | One entry: debit premium receivable, credit unearned premium for the whole period, with fee, GST and levy as separate postings | J | A | 1 |
| LED-006 | US-P25-1 | P25, B12 | Yearly premium billed 1 Mar 2027, earned to 29 Feb 2028 | Daily release in fine units over 366 days including the leap day. Unearned is exactly zero at period end and the rounding account holds only the declared residual | J | A | 1 |
| LED-007 | US-P14-3 | P14 | Receipt with an unmatched reference | Credit to policy suspense, aged daily with an owner. Never to income | J | A | 1 |
| LED-008 | US-P03-1 | P3 | Contribution of NZD 1,000 received before the price exists | Cash sits in pending investment cash, not suspense. Nothing in units yet | J | B | 1 |
| LED-009 | US-P57-2 | P57 | Claim admitted for less than the reserve | Reserve released, difference to claims expense, benefit payable credited. Cause of claim appears on no posting | J | B | 1 |
| LED-010 | US-ST11-1 | all | Daily control between the core and the finance ledger by account class | Equal for every closed date, anchored to the published Merkle root | R | A | 1 |
| LED-011 | US-ST10-1 | all | Nightly reconciliation set | Receivable equals outstanding invoices, unearned equals per-policy balances, benefit payable equals admitted unpaid claims plus surrenders and maturities, suspense aged with owners | R | A | 1 |
| LED-012 | US-ST16-2 | all | Two commands race to apply the same $200 credit balance | Applied one at a time in log order. The second sees the reduced balance and is refused or applies the remainder | F | A | 1 |
| LED-013 | US-ST16-2 | P1 | A correction is needed to a posted premium | Reversal and new entry. Any attempt to edit or post into a closed business date is refused | N | A | 1 |
| LED-014 | US-ST11-1 | P57, P3 | Entries on a life policy and a unit-linked policy | Statutory fund present as a dimension on every posting, taken from the policy | P | B | 1 |
| LED-015 | US-P54-1 | P54 | Receipt logged at 23:59:58 and another at 00:00:02 around the cutoff | First carries the closing business date, second the next. Real-time processing does not pause at end-of-day | J | A | 2 |
| LED-016 | US-ST16-2 | all | Daylight saving ends on 4 Apr 2027 and starts on 26 Sep 2027 | Business date advances exactly once per day. No double or missed accrual on the 25-hour and 23-hour days | S | A | 1 |
| LED-017 | US-B09-1 | B9, P41 | Premium received in EUR for an NZD policy | Entry balances in EUR and in NZD separately through position accounts. Receipt shows the rate | J | C | 2 |
| LED-018 | US-ST16-2 | all | Build with a function in the state machine that reads the clock | Determinism lint fails the build | N | A | 2 |

## PRD Products, rating and quotes

Design: Products and covers in Starlark; Capacity model (quotes and dry runs).

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PRD-001 | US-P03-2 | P3 | Quote, then issue with unchanged facts under the same product and rate versions | Issued premium equals quoted premium to the cent | P | A | 1 |
| PRD-002 | US-P05-1 | P5 | Premium for given age, sex, smoker status, sum insured and table version | Exact expected premium, calculated in D256 and rounded once by the product's mode | J | A | 1 |
| PRD-003 | US-ST15-1 | ST15, ST12 | Activate product version 2 by content hash at a log index | Every replica switches at the same index. Postings after carry the new hash, postings before keep the old | J | A | 1 |
| PRD-004 | US-ST15-1 | ST15 | Load a Starlark product with a mistyped feature parameter | Rejected at load by validation into the typed definition. Nothing activates | N | A | 2 |
| PRD-005 | US-P03-3 | P3 | Trauma definition is revised for new business after her cover started | Her cover stays bound to the version in force at its start date | J | B | 1 |
| PRD-006 | US-P20-1 | P20, P13 | Level premium policies pass an anniversary | Premium unchanged by age. Stepped policies on the same date re-rate | J | B | 2 |
| PRD-007 | US-ST15-1 | ST15, ST12 | Compare tool runs a new rate table over the in-force sample | Report lists who is affected and by how much. No log entries are written by the dry run | J | B | 2 |
| PRD-008 | US-P34-1 | P34 | Adviser generates 5,000 quotes in an hour | Ledger write volume is unchanged. Quotes served by the stateless dry-run service | L | B | 3 |
| PRD-009 | US-P08-3 | P8, P7 | Family health rating with five children where the product charges for the first three | Premium lines per life, children four and five at zero, notice shows each | J | B | 2 |
| PRD-010 | US-P12-3 | P12 | GST attribute: health covers taxable, life covers not, on one customer | GST posted on health premium lines only, to the tax payable account | J | B | 1 |
| PRD-011 | US-P49-2 | P49 | Rating a life whose recorded sex is neither option the table holds | Rated at the lower of the table's male and female rates by the product's `unmatched_sex_rating` setting. Gender kept apart and never used in pricing. Reinsurance rates on the same basis (decision 1 in `docs/decisions.md`) | N | B | 2 |
| PRD-012 | US-ST12-1 | ST12 | Rate table referenced by a product is missing for an anniversary date | Offer cannot be computed, exception item raised, register refuses the dangling reference at activation | N | B | 2 |

## BIL Billing and earning

Design: Premium billing and collection (billing); Ledger mapping (premium flow).

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| BIL-001 | US-P01-1 | P1 | Fortnightly billing over a simulated year | 26 receivables (27 where the cycle demands), each carrying the period it covers. Billing happens whether or not anyone pays | J | A | 1 |
| BIL-002 | US-P05-1 | P5 | Premium notice rendered after the billing run | Deterministic rendering equal to the receivable, with base, discount lines, fee, tax and net | J | A | 1 |
| BIL-003 | US-P35-1 | P35 | Billing day changed from the 1st to the benefit day mid-term where the product allows | Current period pro-rated by value date. Nothing already billed is edited | J | A | 1 |
| BIL-004 | US-P53-1 | P53 | Frequency changed from monthly to yearly | Takes effect at the next anniversary where the product does not allow mid-term | J | A | 2 |
| BIL-005 | US-P29-1 | P29 | Cover increased part way through a billed period | Adjustment receivable for the remainder, value-dated from the change date | J | A | 1 |
| BIL-006 | US-P51-2 | P51 | Three policies in one group billed on the same day | Three separate receivables in the ledger, one consolidated presentation in the read model whose total equals their sum | J | A | 2 |
| BIL-007 | US-P11-1 | P11, B8 | Pays $3,000 ahead when $250 is due | $250 applied, $2,750 held as a credit against the policy in suspense and consumed by the next eleven billings. Refund on request returns the unconsumed balance | J | A | 1 |
| BIL-008 | US-P22-1 | P22, D1 | Cancels inside the free-look period after one premium | All premium refunded, full receivable reversed, all commission reversed from whoever received it | J | B | 1 |
| BIL-009 | US-P37-1 | P37 | Cancels mid-term on day 10 of a 30-day paid period | Cover ends on the effective date. Unearned premium for 20 days refunded by value date | J | A | 1 |
| BIL-010 | US-P24-1 | P24 | Premium holiday for three months under an approved override | Nothing billed and nothing earned in the holiday. Override is dated and evented. Cover follows the product's rule | J | C | 1 |
| BIL-011 | US-P43-2 | P43 | Waiver of premium while on claim | Premium billed as usual and paid by a second posting from claims expense. Earned premium and claims cost both recognised | J | C | 1 |
| BIL-012 | US-ST16-2 | all | Anniversary on 29 Feb 2028 for a policy issued 28 Feb 2027 and one issued 29 Feb 2028 | Product's anniversary rule decides the date in non-leap years. No policy skips or doubles an anniversary | P | A | 2 |
| BIL-013 | US-ST10-1 | all | Sum of daily earned postings for any fully earned period | Equals the net premium billed for that period to the cent, with residual in the rounding account | P | A | 1 |
| BIL-014 | US-P46-1 | P46 | Pays the same premium three times in a month | First applied. Second and third become a policy credit. Prompt raised for a person to consider a marker. Refund available | J | A | 2 |
| BIL-015 | US-E02-1 | all | 150,000 billings on a popular billing day across 64 partitions | Completes inside the end-of-day budget. No command exceeds the apply budget | L | A | 2 |

## COL Collection, cash matching, standing and dunning

Design: Premium billing and collection; Collections operations.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| COL-001 | US-P01-1 | P1 | Direct debit dishonoured for insufficient funds | Retried once per the rail rule, customer told before re-presentment, then a dishonour event. Standing in grace, cover in full | J | A | 1 |
| COL-002 | US-P02-1 | P2 | Premium paid on day 20 then charged back on day 50 | Allocation reversed by a new entry. Receivable age recalculated from the original due date, so the lapse clock was never reset | J | A | 1 |
| COL-003 | US-B01-1 | B1 | Direct credit with reference "HEMI DAIRY" and no policy number | Match by payer account against the remitter on file, rule and confidence recorded | J | A | 1 |
| COL-004 | US-B01-1 | B1 | Same payment from a new account, amount within tolerance | Low confidence. Exception item on the case platform, money in suspense with an owner | J | A | 1 |
| COL-005 | US-P14-3 | P14, P1 | S14: Chloe quotes another customer's policy number | If applied to the other policy, a correction reverses it by new entry and applies it to hers, and both standings are recalculated from original due dates | J | A | 1 |
| COL-006 | US-P05-3 | P5 | Payment covers a policy fee, a reinstatement charge and part of two premiums | Allocation order by product rule: fee and charge first, then oldest receivable | J | A | 2 |
| COL-007 | US-P04-2 | P4, P5 | Unpaid past grace on a product that uses suspension | Standing suspended at end-of-day. Event in the suspended period is not covered. Paying all arrears restores cover from payment date with no underwriting | J | A | 1 |
| COL-008 | US-P17-3 | P17 | Unpaid to the lapse day in January 2029 | Status lapsed. Unearned part of open receivables reversed, earned-but-unpaid written off to the lapse write-off account, lapse event emitted | J | A | 1 |
| COL-009 | US-P16-1 | P16 | Lapse notice by post is returned undeliverable | Exception pauses the lapse step and raises a task. The policy does not lapse | N | A | 1 |
| COL-010 | US-P17-3 | P17 | Reinstates in the window: pays arrears and gives a declaration of health | Receipts posted, status in force, earned premium for the gap recognised at that point | J | A | 1 |
| COL-011 | US-P17-3 | P17 | Asks to reinstate after the window has closed | Refused. Treated as a new application | N | A | 2 |
| COL-012 | US-P13-1 | P13 | S3: dunning sequence configured as day 3, 10, 17, 31, 45, 60 | Each step is an event producing the notification or task named. Mandatory notices cannot be skipped by configuration | J | C | 1 |
| COL-013 | US-P13-1 | P13, ST5 | S3: call task on day 17 reaches Kath, the other owner | Conversation recorded with a reason code. Alternatives offered as typed commands. Lender also receives arrears notices | J | C | 2 |
| COL-014 | US-P13-2 | P13, ST6 | S3: hardship application approved for six months reduced premium, ended early in month four | Override dated and evented. Normal billing resumes from the early end date. No retrospective charge | J | C | 1 |
| COL-015 | US-P43-1 | P43 | Arrangement to clear two months of arrears over four, then one instalment missed | Standing held within arrangement while kept. On the break the lapse clock resumes from where it stood | J | C | 1 |
| COL-016 | US-P35-2 | P35, ST6 | Dishonour on a customer with a financial hardship marker | No re-presentment until a conversation is recorded. Hardship offered proactively | J | C | 1 |
| COL-017 | US-P47-1 | P47 | Third consecutive dishonour on one authority | Task to confirm the payment method. Authority suspended after the configured count | J | C | 2 |
| COL-018 | US-P20-1 | P20 | Pays by card in the app on day 44, between final notice and lapse | Standing moves to paid at once. Sequence stops | J | A | 1 |
| COL-019 | US-ST05-1 | ST5 | Collections queue for the day | Ordered by days to lapse then annual premium. Officer sees ordinary and financial classes only. Insurance-sensitive fields masked and the response says so | X | C | 1 |
| COL-020 | US-ST05-1 | ST5, ST10 | Officer proposes a suspense match above the threshold | Needs a second person. The proposer cannot approve | N | C | 2 |
| COL-021 | US-B01-1 | B1 | Unmatched money reaches the return period | Returned by the rail it came on with a notification | J | C | 3 |
| COL-022 | US-P56-1 | P56 | Pays one transfer covering four relatives' policies | Receipt split across policies by the officer's tool. Payer-not-owner rule checks recorded relationships | J | C | 2 |
| COL-023 | US-P13-3 | P13 | WOL0 premium overdue with surrender value available | Automatic premium loan pays it before lapse. Owner told. Non-forfeiture options tried in declared order before any lapse | J | C | 1 |
| COL-024 | US-P44-1 | P44, B11 | S5: card tokens invalidated for 300 customers before a billing day | Failures tagged to the incident. No dishonour fee. Customers asked for new details. Remediation case corrects any standing that advanced | J | C | 1 |
| COL-025 | US-P35-3 | P35, P1 | S15: collection channel down four hours on a collection day | Collections queue and resubmit in the next window. Any fee or standing change caused by the outage is found by population query and reversed | J | C | 1 |
| COL-026 | US-ST05-1 | all | Multi-policy customer in arrears on three policies | One message and one task covering all three | J | C | 3 |
| COL-027 | US-P07-1 | P7, P8 | Joint policy with a lender's interest reaches suspension | Lender receives the suspension and lapse notices with proof of delivery | J | C | 1 |
| COL-028 | US-P17-3 | P17, ST5 | Lapsed policy inside the reinstatement window | Winback task with reinstatement conditions and cost shown to the customer | O | C | 3 |

## XFR Policy transfer between groups

Design: Design review (policy transfer protocol); Policy model: ownership.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| XFR-001 | US-P18-2 | P18 | S11: policy moves from the joint group to Kiri's group on another partition | Freeze, record with hash, move through in-transit accounts per currency, land, close. Policy number, terms, standing and open receivables unchanged | J | A | 1 |
| XFR-002 | US-P18-2 | P18 | Transfer with the target partition rejecting | Source unfreezes. Nothing has moved. Queued commands apply | F | A | 1 |
| XFR-003 | US-ST16-2 | P18 | Kill the source leader after the record is written and before the outbox relays | Transfer completes exactly once after recovery. In-transit accounts net to zero | F | A | 1 |
| XFR-004 | US-P29-2 | P29 | Unit-linked policy with a pending order is asked to transfer | Transfer waits until pending orders execute or are cancelled. Units move through in-transit accounts per unit type | J | C | 1 |
| XFR-005 | US-P57-3 | P57 | Policy with an open claim and a benefit schedule transfers | Claim, schedule and time triggers are re-registered on the target. No payment missed or doubled | J | C | 1 |
| XFR-006 | US-P18-2 | P18 | Customer views history after the transfer | One continuous history. Source chain keeps old entries and the policy links to them | J | A | 2 |
| XFR-007 | US-P18-2 | P18 | Directory lookup during and after transfer | Policy directory resolves to the source until close, then the target. Cash received mid-transfer is matched and applied on the right side | J | A | 1 |
| XFR-008 | US-B21-1 | B21 | Trustee changes on a trust-owned policy | Relationship change inside the same group. No transfer runs | N | C | 2 |
| XFR-009 | US-P18-2 | P18 | Transfer leaves payment authority behind | New owner is asked to set up payment. Previous owner's direction and authority do not carry | J | C | 1 |
| XFR-010 | US-E01-1 | E1 | Scheme with a scoped rate table moves a member's continuation policy out | Scoped configuration needed by the policy is carried in the transfer record | J | C | 2 |

## AUD Audience scoping, customer app and identity

Design: Design review (who sees what); Customer app; Policy model (privacy inside a family).

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| AUD-001 | US-P14-1 | P14, E2 | Member registers in the app | Device holds her own cover and nothing else from a 50,000-member scheme | X | A | 1 |
| AUD-002 | US-P04-1 | P4, P5 | Tama has a health claim. Mere opens the policy | Mere sees premium and the family counter moved by an amount, with no person or service. Tama sees his claim | X | B | 1 |
| AUD-003 | US-P04-1 | P4, P5 | Tama gives consent, then withdraws it | Detail appears for Mere, then a purge removes it from her device | X | B | 1 |
| AUD-004 | US-P09-1 | P9, P8, P7 | S4: Losa turns 18 at end-of-day on a Tuesday | From the next business date her claim detail leaves her parents' feeds by purge, she can enrol her own device, and all three are notified | J | B | 1 |
| AUD-005 | US-P32-1 | P32 | Toby reaches the product's configured age for controlling health information | Cedar policy hides his claim detail from the policyholder from that date. Counters still move | X | B | 1 |
| AUD-006 | US-P40-2 | P40, B4 | One device, three roles | Scheme cover shown as life insured, family and individual policies as owner, each with its slice. Employer portal shows him no health or claim detail for any director | X | C | 1 |
| AUD-007 | US-P15-1 | P15 | Opens the app with no signal | Every screen reads from the device database. A payment attempt is refused until connected and signed | J | A | 2 |
| AUD-008 | US-P53-1 | P53 | Connection drops after submit and before the outcome event | Screen shows pending. Retry with the same key creates nothing new. Outcome appears when the event lands | F | A | 1 |
| AUD-009 | US-P55-2 | P55 | Phone lost | Recovery by fresh verification binds a new device. Old device is purged and its tokens revoked | J | B | 1 |
| AUD-010 | US-P55-1 | P55 | Her parents pay her premium | Payer has no relationship that grants a view. No policy data reaches their devices | X | B | 1 |
| AUD-011 | US-P38-2 | P38 | S7: eldest daughter added as view-only | Read scope as the parents allow. Every command from her is refused | X | C | 1 |
| AUD-012 | US-P16-3 | P16 | Son Peter phones asking for her policy details | Refused. Nothing confirmed or denied beyond a generic response. Contact recorded | N | B | 1 |
| AUD-013 | US-P27-3 | P27, B7 | Accountant holds a read-only relationship to the company | Sees the company-owned policy's premiums and documents. Sees none of her personal policies and no claim detail | X | C | 2 |
| AUD-014 | US-P08-2 | P8 | Notification for a premium reminder | Pointer to the in-app inbox. No sign-in link. Contact details fetched at send time | X | A | 1 |
| AUD-015 | US-P01-3 | P1 | Receipt event lands while the app is open | Standing and next payment update by the sync feed, read-your-own-writes by sequence number | J | A | 2 |
| AUD-016 | US-N07-1 | N7 | Relationship ends: a member leaves a scheme | Feed sends a purge for the scheme cover slice | X | B | 2 |
| AUD-017 | US-ST16-2 | all | Inspect every event and every telemetry span from a full scenario run | No personal data, no clinical data, no loading or exclusion codes in the public event set or telemetry | X | A | 1 |
| AUD-018 | US-N04-2 | N4 | Completes quote, payment, claim and consent journeys with a screen reader | Every control is labelled and reachable. Documents available in an accessible format | J | C | 2 |
| AUD-019 | US-P28-1 | P28 | Mokopuna helps with the app | Support person relationship recorded with consent. No shared sign-in. Signed steps still need Wiremu | J | C | 2 |

## RES Resilience, capacity and scale

Design: Capacity model; Design review (benefit payments and a Temporal outage); Platform foundations.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| RES-001 | US-ST16-2 | all | Phase A exit: 100,000 policies, simulated year of billing, earning, receipts, lapses, reinstatements and anniversaries | Every reconciliation clean every simulated day. Recorded log replays to an identical state hash | S | A | 1 |
| RES-002 | US-E02-1 | E2 | Price a 50,000-member scheme in one command | Fails the suite. Same work split per billing group or member range passes with a final totalling command | L | A | 1 |
| RES-003 | US-ST16-2 | all | End-of-day earning for 1,000,000 policies | Completes within the budget at the sized rate per partition leader | L | A | 2 |
| RES-004 | US-ST16-2 | all | Lose one whole region during a billing run | No data lost. Leadership recovers. Idempotent rerun produces no duplicate receivables | F | B | 1 |
| RES-005 | US-P43-2 | P43, P11 | S28: workflow service unavailable on a benefit date for six hours | Catch-up order: benefit and claim payments, then billing, then unit orders, then the rest. Each benefit paid once, keyed by claim and benefit date | F | C | 1 |
| RES-006 | US-ST16-2 | all | End-of-day phases rerun after a crash mid-phase | Idempotent by policy and date. No double accrual or double billing | F | A | 1 |
| RES-007 | US-ST16-2 | all | Closed days older than the set age sealed to the cold tier | Hash chain and Merkle roots still verify. A sealed segment is fetched for replay | J | C | 2 |
| RES-008 | US-D08-1 | D8 | FAP asks for every policy under all its advisers across partitions | Served from a read model keyed by adviser and payee. No fan-out to the group-sharded views | L | C | 3 |
| RES-009 | US-ST16-2 | all | Configuration state per partition after 20 simulated years of versions | Measured and reported. Scoped tables present only on their scheme's partition | S | C | 3 |
| RES-010 | US-ST16-2 | all | A failing simulation seed | Seed is captured as a permanent regression test | O | A | 2 |
| RES-011 | US-P17-1 | P17 | Scripted client sends 1,000 mixed commands with timeouts and blind retries | No duplicate effects. Every async write ends in exactly one outcome event | F | A | 1 |
