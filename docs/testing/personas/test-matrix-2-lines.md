# Test matrix, part 2: the lines of business

Underwriting, claims, reinsurance, unit pricing, group schemes, joint lives and ownership, advisers and commission, composable health, and discounts. Column meanings are in part 1.

## UW Underwriting, underwriting operations and delegated authority

Design: Underwriting; Underwriting operations; Delegated underwriting; Design review (adviser consent, capacity).

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| UW-001 | US-P03-2 | P3 | Clean disclosure on LIFE1 | Rules engine accepts at standard rates. Policy issues with no human step. Decision records rule, question set and rate table versions | J | B | 1 |
| UW-002 | US-P57-4 | P57, D1, ST2 | Tom Clarke's application is referred and accepted with a 50% loading | Offer made in plain words. He accepts with a signed step. Core holds the loading percentage and decision reference only. Reason stays in the clinical store | J | B | 1 |
| UW-003 | US-P22-3 | P22 | One decision on HLTH1: exclusion on one component, loading on another, standard on a third | Different terms written to different cover layers. Exclusion stored as an opaque code | J | B | 1 |
| UW-004 | US-ST01-1 | ST1 | Case for $800,000 with a proposed loading | Not offered to ST1. Decision action absent for that profile. Routed to someone who can decide | X | B | 1 |
| UW-005 | US-ST02-1 | ST1, ST2 | Decline of P58's income protection | Two signatures, each with own reason codes. Disclosable reasons separated from non-disclosable under the stricter class | J | B | 1 |
| UW-006 | US-P58-1 | P58 | Receives the decline | Letter states the reasons he may be told and how to ask for review. Life cover on the same application still issues | J | B | 1 |
| UW-007 | US-P19-2 | P19, B20 | New proposal on a life with cover under two policyholders on different partitions | Capacity step reads the life index accumulation across partitions and reserves capacity for the proposal's life | J | B | 1 |
| UW-008 | US-P34-1 | P34, R2 | $5 million sum insured exceeds the treaty's automatic limit | Referred for facultative terms before any offer. Evidence released from the vault under a logged purpose, never copied to the reinsurance module | J | C | 1 |
| UW-009 | US-P19-2 | P19 | Two proposals on the same life in progress at once, arriving in either order | Accumulation applied in fixed order by commencement date and tie-break. Result identical for both arrival orders | P | B | 1 |
| UW-010 | US-P19-1 | P19, ST2 | Case untouched past the first-touch timer | Escalates to the queue lead, then the manager. Timers reported | O | B | 2 |
| UW-011 | US-D01-2 | D1, P57 | Tom consents to adviser access for this application. Vanessa, on the same application, does not | D1 sees Tom's answers and evidence status. D1 sees only progress and outcome for Vanessa. Neither life sees the other's answers | X | B | 1 |
| UW-012 | US-D01-2 | D1 | Decision is made. D1 opens the application again, and again at claim time | Disclosure access ended at decision and does not return at claim | X | B | 1 |
| UW-013 | US-D01-2 | D1, P29 | Adviser enters answers for the client | Each answer marked as entered by the adviser. The life insured reviews and signs the record herself with her device key | J | B | 2 |
| UW-014 | US-N03-1 | N3, P31 | Two lives on one application, one overseas | One child workflow per life. Issue only when both have a decision and any non-standard offer is accepted | J | B | 1 |
| UW-015 | US-P31-1 | P31, N3 | Overseas doctor does not respond to an evidence request | Timer and automatic chasers. Routes to an underwriter with reason codes. Never defaults to standard or decline | J | B | 2 |
| UW-016 | US-P12-3 | P12 | Applicant has no local medical records before 2016 | Rules request alternative evidence or refer. Outcome reproducible from versions | J | B | 2 |
| UW-017 | US-P60-2 | P60 | Previous insurer overseas asked for claims history | Request through the previous-insurer adapter with status and deadline. Decision records evidence kinds relied on | J | C | 3 |
| UW-018 | US-P03-2 | P3 | Interim accidental death cover while the proposal is open | Issued to the core at once, replaced on the final decision, ended if the proposal lapses | J | C | 2 |
| UW-019 | US-P34-1 | P34, ST2 | Tele-underwriting call answers recorded by the underwriter | Not disclosure until he confirms them in the app with a signed step. Adviser not on the call without consent | J | C | 2 |
| UW-020 | US-P02-2 | P2 | Change of occupation after qualifying | Goes through underwriting. Effective from the decision date. Old layer ends and a new one starts | J | C | 2 |
| UW-021 | US-P02-3 | P2 | Claim reveals smoker status was misstated | Non-disclosure review is its own case worked by someone else with its own approvals. Any avoidance or variation is a command, never an edit | J | C | 1 |
| UW-022 | US-ST02-1 | ST2 | S23: contract entered 10 Nov 2027 and another entered 20 Nov 2027 | Each uses the question set, rules and remedies effective for its contract date. Both regimes run side by side | J | C | 1 |
| UW-023 | US-ST15-1 | ST15, ST2 | Changed underwriting rule set before activation | Backtest over anonymised historical proposals shows the shift in clean, referred and declined. Activation needs dual approval | O | B | 2 |
| UW-024 | US-ST02-1 | ST2 | Peer review sample for a period | Weighted to non-standard terms and declines. Reviewer did not touch the case. Findings recorded against the decision | O | C | 3 |
| UW-025 | US-B17-1 | B17, TPA1 | Delegate sends a typed decision within the per-life ceiling and aggregate | Written as a decision with source delegated and as layer state. Capacity step still runs on the insurer's side | J | C | 1 |
| UW-026 | US-TPA1-1 | TPA1 | Delegate sends a decision above the ceiling, one with an unknown exclusion code, and a duplicate | Each rejected with its reason. Application routes to the insurer's queue. Nothing above authority is bound | N | C | 1 |
| UW-027 | US-B17-1 | TPA1, ST2 | Quality sample of delegated decisions | File obtained through audit access, logged, stored under the life's key on arrival | O | C | 2 |
| UW-028 | US-P58-1 | P58 | Counter-offer after a loading is offered | Case reopens at assessment. New terms are a new decision. Earlier decision stays on file | J | C | 3 |
| UW-029 | US-ST16-2 | all | Inspect workflow inputs, signals and history for an underwriting run | IDs and references only. No clinical content held by the workflow service | X | B | 1 |
| UW-030 | US-P29-1 | P29, P24 | Future insurability option exercised on marriage and on a birth | Workflow checks event evidence and options already used in cover state. No underwriting. New layer | J | C | 1 |

## CLM Claims and claims operations

Design: Claims; Claims operations.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CLM-001 | US-P57-1 | P57 | S1: Vanessa notifies Tom's death in the app. A funeral director's letter arrives two days later | One claim, not two. Life marked deceased in the customer master. His other policies restricted. Acknowledgement with proof of delivery names the owner | J | B | 1 |
| CLM-002 | US-P57-2 | P57 | S1: joint first-event claim of $500,000 with a lender's interest of $380,000 | Admitted against the cover, which ends for both lives in the same entry. $380,000 to the lender, $120,000 to Vanessa | J | C | 1 |
| CLM-003 | US-P57-4 | P57, D1, R1, R2 | S1 and retail life exit: Tom's sole LIFE1 of $600,000 paid to the estate | Executor in Perth verified remotely. Payment waits on proof of authority. Reserve, admission, payment and reinsurance recovery post. No clawback on D1 | J | B | 1 |
| CLM-004 | US-P57-1 | P57, ST4 | S1: funeral advance requested in the first week | Considered early as an urgent payment by API with payee verification. Counted against the final amount | J | C | 2 |
| CLM-005 | US-P23-2 | P23 | Lost at sea, no death certificate | Claim registered and reserved. Coroner evidence request with a timer. Refusing to register is never a behaviour | J | C | 1 |
| CLM-006 | US-P01-1 | P1 | Event occurs on day 12 of grace | Payable with outstanding premium deducted as a posting from benefit payable to premium receivable. Claimant sees the line | J | B | 1 |
| CLM-007 | US-P04-2 | P4 | Event occurs while suspended | Cover check returns not in force for the event date. Assessor decides. Decline needs two signatures and states reasons | J | B | 1 |
| CLM-008 | US-ST03-1 | ST3, P26 | Cover check on a member whose leave date was backdated after the event | Facts returned from the two-timeline record: what was true on the event date as known now, and what was known then | J | B | 1 |
| CLM-009 | US-P30-1 | P30 | Partial trauma payment of 25% on a $200,000 cover linked to life cover | $50,000 paid. Trauma reduces by product rule. Linked life cover reduces in the same entry. New schedule issued | J | B | 1 |
| CLM-010 | US-P08-1 | P8, P9 | Member receipt for a dental visit inside limits | Straight through: priced and admitted by rule, excess and co-payment shown, paid in the next run. Rule version and reason codes recorded | J | B | 1 |
| CLM-011 | US-P42-2 | P42, PR1 | Pre-approval for a $24,000 procedure, final invoice $22,600 | Hold placed against limits with an expiry, as replicated state and not a posting. Invoice releases the hold and posts $22,600 less excess | J | B | 1 |
| CLM-012 | US-P42-1 | P42 | Health slice exit: a year of family claims reaches a yearly limit | Counter stops further payment in that category. Resets at the anniversary. Lifetime counter does not | J | B | 1 |
| CLM-013 | US-P42-2 | P42, PR1 | Pre-approval expires unused | Hold released by a logged expiry. Limits restored | J | B | 2 |
| CLM-014 | US-PR1-1 | PR1 | Provider run on its weekly cycle | One payment per provider with a remittance advice. Recoveries offset first | J | B | 2 |
| CLM-015 | US-PR2-1 | PR2, ST13 | Same service billed twice across two members' claims | Fraud rule raises a restricted case. Front-line staff and claimant see nothing. Recovery offsets the next provider run | X | C | 1 |
| CLM-016 | US-P11-2 | P11 | Income protection claim after a 13-week wait, then partial return to work | First benefit on the first benefit date after the wait. Each period computed fresh with that period's income | J | C | 1 |
| CLM-017 | US-P43-2 | P43 | Benefit with accident compensation as other income | Offset shown as its own figure on each benefit line | J | C | 1 |
| CLM-018 | US-P11-2 | P11, ST4 | Income declaration is late for one period | Benefit paid on last confirmed inputs and adjusted afterwards. A task is raised. Payment is not stopped | J | C | 1 |
| CLM-019 | US-ST04-1 | ST4 | Review plan generated at admission | Medical reviews, declarations and benefit end date become workflow timers creating tasks on their dates | O | C | 2 |
| CLM-020 | US-P11-2 | P11 | Same condition recurs inside the product's window after recovery | Same claim reopens with no new waiting period | J | C | 2 |
| CLM-021 | US-P43-2 | P43 | Backdated return to work found two months later | Overpayment calculated by replay of the affected periods. Receivable on the claim. Recovered by agreed offset against future benefits | J | C | 1 |
| CLM-022 | US-P26-2 | P26 | S14: a premium refund is paid to Nikau's account in error and half is spent | Recall requested through the bank. Amount receivable from the payee. Repayment arrangement as a case. Never netted silently | J | C | 2 |
| CLM-023 | US-N01-1 | N1, E3 | Unnamed member claims | Identification step creates a party, links the member record, carries history. Member record never deleted | J | C | 1 |
| CLM-024 | US-B16-1 | B16 | Claim on pool cover | Employer supplies evidence of eligible membership on the event date. Stored with the claim. Party created for the first time | J | C | 2 |
| CLM-025 | US-B24-2 | B24 | Claim on a pilot whose cover exceeds the automatic limit | Amount above the limit paid only where an underwritten layer exists | J | B | 1 |
| CLM-026 | US-N09-1 | N9, B25 | S17: claim on a continuation policy | Assessed on the individual policy with scheme history linked for claims history and reinsurance | J | C | 1 |
| CLM-027 | US-B07-1 | B7, P27 | Claim on company-owned income protection | Benefit paid to the policyholder company. Claim detail visible to the life insured only | X | C | 1 |
| CLM-028 | US-P31-2 | P31 | Beneficiary in Lagos | Identity and account ownership checked, payee screened, paid over the cross-border rail with its own idempotency key | J | C | 1 |
| CLM-029 | US-P15-2 | P15 | Nomination changed from ex-partner to children three months before death | Nomination in force on the date of death is read from the two-timeline record | J | C | 1 |
| CLM-030 | US-P45-1 | P45 | Death on overseas deployment with an exclusion code on the cover | Cover check returns the code as a fact. The assessor decides whether it bites. Decline or admission recorded with reason codes | J | C | 1 |
| CLM-031 | US-P58-1 | P58, ST14 | Declined claim goes to internal review and is overturned | New case worked by someone who did not decide. Overturn reopens the same claim. No second claim created | J | C | 1 |
| CLM-032 | US-ST04-1 | ST3, ST4 | Assessor tries to admit above their limit and to set a reserve above their limit | Both route to a senior. Profile version recorded on the decision | N | C | 1 |
| CLM-033 | US-N07-1 | N7, P60 | Marcus's claim enters the queue where his cousin works | Never offered to N7. Declared relationship conflict enforced in routing | X | C | 1 |
| CLM-034 | US-ST13-1 | ST13, P52 | Claim three weeks after inception with altered documents | Fraud rule fires. Investigation is a restricted case | X | C | 2 |
| CLM-035 | US-P57-2 | P57 | Claim payment returned by the bank | Goes back to benefit payable with a task. Never to suspense | N | B | 1 |
| CLM-036 | US-P58-2 | P58, ST4 | Ex gratia payment approved after a complaint | Separate posting type with its own authority limit. Recoverable from the reinsurer only if the treaty says so | J | C | 2 |
| CLM-037 | US-P12-2 | P12 | Claim paid after the period that attracts late-payment interest | Interest is its own posting type by value date. **Design gap** on whether the payee may decline it | J | C | 3 |
| CLM-038 | US-ST10-1 | all | Daily control | Open reserves plus benefit payable in the core equal the claims module's open claims | R | B | 1 |
| CLM-039 | US-P21-2 | P21 | Death claim with a policy loan and two interests | Loan repaid from benefit payable as a posting line, interests paid in order of notice date, balance to the owner | J | C | 1 |
| CLM-040 | US-N06-1 | N6 | Income protection claimant is imprisoned mid-claim | **Design gap.** Test holds the expected behaviour open until the product rule exists | N | C | 3 |
| CLM-041 | US-P11-3 | P11 | Trauma event while travelling overseas | Registered at once. Routed to the complex queue with a case conference step. Evidence through adapters | J | C | 2 |
| CLM-042 | US-P28-2 | P28, ST4 | Claim on a customer with a communication marker | One owner shown by name. Handover recorded and the claimant told. Customer-side deadlines extended within limits | J | C | 2 |

## RI Reinsurance

Design: Reinsurance; Design review (capacity, sweep).

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| RI-001 | US-R01-1 | P57, R1, R2 | Surplus treaty with retention $250,000. Tom's $600,000 layer issues | $350,000 ceded, 60% to R1 and 40% to R2. Stored as layer state with the treaty version | J | B | 1 |
| RI-002 | US-P19-2 | P19, B20 | Second policy on the same life under a different policyholder | Retention already used by the first. Whole second layer ceded. Allocation in order of commencement | J | B | 1 |
| RI-003 | US-R01-1 | P57, R1, R2 | Ceded premium each billing period | Debit reinsurance premium expense, credit each reinsurer's payable account, as lines in the billing entry | J | B | 1 |
| RI-004 | US-R01-1 | P57, R1, R2 | S1: recovery on Tom's claim | Recoverable posted with the reserve, trued up on admission, due once paid. Uses the cession in force on the event date | J | B | 1 |
| RI-005 | US-R01-1 | P17 | Policy lapses and is reinstated | Cession recalculated on each event. Ceded premium reversed for the lapsed period by value date | J | B | 2 |
| RI-006 | US-R01-1 | P29 | Indexation at anniversary and a backdated decrease | Cession recalculated. One adjustment posted today by value date | J | C | 2 |
| RI-007 | US-R02-1 | P34, R2 | Facultative terms accepted | Stored as a facultative cession on the cover with the reinsurer's reference and premium basis | J | C | 1 |
| RI-008 | US-P38-3 | P38 | Joint cover on two lives | Counts in full against each life's retention by default. A treaty declaring otherwise changes the result | J | C | 2 |
| RI-009 | US-E03-1 | E3, R1 | Scheme on unnamed member records | Ceded on a scheme basis. Named, underwritten members accumulate per life | J | C | 2 |
| RI-010 | US-R03-1 | B24, R3 | S9 and aircraft accident: several members of one scheme die in one event | Concentration by site reported. Catastrophe treaty evaluated from events. Recovery posted to internal accounts by command | J | C | 1 |
| RI-011 | US-R01-1 | R1 | Quarterly statement and listing | Deterministic rendering. Immutable once issued. Listing holds policy reference, age, sex, smoker status, sum at risk and dates. No names or contact details | X | C | 1 |
| RI-012 | US-R02-2 | R2, BK1 | Net settlement to an overseas reinsurer | Sweep by sequence number on each partition to clearing accounts. One net payment in the treaty currency. Clearing returns to zero | J | C | 1 |
| RI-013 | US-R01-1 | all | Scheduled rebuild of the per-life accumulation from events | Equals cessions stored in the core. A difference raises an exception item | R | B | 1 |
| RI-014 | US-R01-1 | P57 | Large claim above the treaty's notification threshold | Reinsurer notification workflow starts. Where the treaty gives the reinsurer a say, the claims workflow waits with a timer | J | C | 2 |
| RI-015 | US-R01-1 | P11 | Income protection claim in payment | Recovery posted period by period as each benefit is paid | J | C | 2 |
| RI-016 | US-R01-1 | P27 | Replacement of cover on the same life | Cession on the replaced amount continues on the existing treaty basis and inception date. Accumulation unchanged | J | C | 2 |
| RI-017 | US-R01-1 | ST15 | New treaty version activated for new business | In-force covers stay on their original treaty version until a recapture command | J | C | 2 |
| RI-018 | US-R03-1 | P36, R3 | Longevity quota share on annuities | Recoveries posted per payment period, not per claim | J | C | 3 |

## INV Unit pricing and the investment ledger

Design: Unit pricing; Ledger mapping (investment ledger).

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| INV-001 | US-P03-1 | P3 | Buys NZD 1,000 of F-BAL at a price of 2.0000 | Debit pending cash $1,000, credit fund cash position $1,000, debit fund unit position 500 units, credit her unit account 500 units. Balances in dollars and in units separately | J | B | 1 |
| INV-002 | US-P03-1 | P3 | Order logged after the fund's cut-off, and one on a non-pricing day | Each takes the next pricing date. Pricing date assigned at logging and carried in the command | J | B | 1 |
| INV-003 | US-P03-1 | P3 | Cancels an order before the price is activated, then tries after | First succeeds. Second refused | N | B | 2 |
| INV-004 | US-P03-1 | P3 | Switch from F-CASH to F-GRO where both price that day | Sell and buy execute on the same pricing date | J | B | 1 |
| INV-005 | US-P34-2 | P34 | Switch into F-USEQ on a day New Zealand prices and the US market is closed | Proceeds wait in pending cash for the second fund's price from its own calendar | J | C | 2 |
| INV-006 | US-FA1-1 | FA1, ST8 | Units on issue in the core differ from the administrator's by 0.000001 units | Price is blocked | N | B | 1 |
| INV-007 | US-ST08-1 | ST8 | Price moves 9% against a benchmark move of 1% | Check fails. Price held for review | N | B | 1 |
| INV-008 | US-ST09-1 | ST8, ST9 | Maker tries to approve own price | Refused by policy. Only an approved price activates | N | B | 1 |
| INV-009 | US-ST16-2 | all | Execution workflow reruns for a pricing date | Keyed by order ID and price ID. No order executes twice | F | B | 1 |
| INV-010 | US-P21-1 | P21, P3, FA1 | S19 and investment slice exit: F-BAL struck wrong for one date, found after P21 withdrew in full | Corrected price is a new version for the same date. Every order replayed. P3 adjusted in units today with the original pricing date as value date. P21 owed cash as a case. Fund made whole through the error account | J | B | 1 |
| INV-011 | US-P21-1 | P21 | Error below the materiality threshold | Not compensated. Threshold and decision recorded with the error | J | C | 3 |
| INV-012 | US-P03-1 | P3 | Monthly policy fee on a unit-linked policy | Sell order raised by the fee feature on the cycle date. Fund management fee never appears as a policy transaction | J | B | 2 |
| INV-013 | US-P36-1 | P36 | Regular monthly withdrawal in drawdown | Sell order each month, proceeds through pending cash to benefit payable and out | J | C | 2 |
| INV-014 | US-FA1-1 | FA1 | Fund suspended | No price activated. Orders stay pending with their pricing dates. Suspension event drives notifications | J | C | 2 |
| INV-015 | US-ST10-1 | FA1 | Daily net flow per fund | One instruction to the administrator. Fund clearing account zero after settlement | R | B | 1 |
| INV-016 | US-ST10-1 | all | Reconciliation on every pricing date | Policy units equal the fund unit position on every partition. Units on issue equal the administrator. No order older than the pricing lag without a suspension | R | B | 1 |
| INV-017 | US-P03-1 | P3 | Statement and app view | Statement shows opening units, transactions with units and price, closing units, price and value. Pending orders appear in the app only. Value always labelled with the price date | J | B | 2 |
| INV-018 | US-P29-2 | P29 | Joint owners, both to sign. One requests a withdrawal | Refused until the second signs | N | C | 1 |
| INV-019 | US-P41-1 | P41, ST13 | Three single premiums of $150,000 in six months | Each raises one source-of-funds case linked to the earlier ones | J | C | 1 |
| INV-020 | US-P34-3 | P34, ST13 | Single premium top-up at bonus time | One case per event with prior answers visible to the investigator | J | C | 2 |
| INV-021 | US-P50-1 | P50, ST13 | Early full surrender soon after a large contribution | AML rule for early surrender fires alongside the vulnerability route | J | C | 2 |
| INV-022 | US-B23-1 | B23, P51 | Trust-owned policy contributes monthly, a parent alone asks to withdraw | Contributions buy units. Withdrawal refused without the trust's signing rule satisfied | J | C | 2 |
| INV-023 | US-P41-2 | P41, P21 | Annual international reporting file | Built by joining tax residency in the customer master with policy values. Dual residency reported for P41 | J | C | 2 |
| INV-024 | US-P03-1 | P3 | Rounding on unit calculation | Units computed in D256 and rounded to micro-units by the fund's rule. Residual sits in the fund's rounding account and is reported | P | B | 2 |
| INV-025 | US-P36-1 | P36 | CGA1 account with a declared crediting rate | Uses the daily accrual engine. No units. Rate activated through the log | J | C | 3 |

## GRP Group scheme administration

Design: Group scheme administration; Design review (excess loadings billing).

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| GRP-001 | US-E01-1 | E1, D12 | S30 and group slice exit: twelve monthly files on a 5,000-member named scheme | Each file quarantined, parsed, validated, compared, previewed, approved and applied in chunks. Every reconciliation clean | J | B | 1 |
| GRP-002 | US-B25-1 | B25 | S12: 400-row file, three rows fail validation | Rejected rows listed with reasons in the preview. Remaining 397 apply after approval | J | B | 1 |
| GRP-003 | US-BK1-1 | B25 | Same file uploaded twice | Second upload changes nothing. Commands keyed by file and row | N | B | 1 |
| GRP-004 | US-B24-1 | B24 | S24: file would remove 34 of 40 members | Held for explicit confirmation by the safety check | N | B | 1 |
| GRP-005 | US-B15-2 | B15 | Leaver effective 12 March notified in the May file | Adjustment line on the next invoice calculated by value date. Issued invoices unchanged. Earned premium adjusted by one posting today | J | B | 1 |
| GRP-006 | US-ST07-1 | B15, ST7 | Backdated change beyond the scheme's backdating limit | Needs the approval tier. Refused without it | N | B | 2 |
| GRP-007 | US-P01-2 | P1, E1 | Row for a member below minimum hours | Rejected with reason and shown in the preview. Later hours change joins her from its effective date after the waiting period | J | B | 2 |
| GRP-008 | US-P26-1 | P26, E3 | Hours cross the threshold in alternate weeks | Eligibility evaluated at each effective date. Cover follows | J | C | 2 |
| GRP-009 | US-B24-2 | B24, ST2 | Pilot's formula cover exceeds the automatic acceptance limit | Covered to the limit at once. Excess starts underwriting and becomes a second layer when accepted. Employer sees whether it is in place, never why | J | B | 1 |
| GRP-010 | US-E01-1 | E1 | Underwritten excess with a loading, scheme setting is employer pays with one total | Invoice shows excess cover as one scheme-level total, no member breakdown. Invoice records the setting version | J | B | 1 |
| GRP-011 | US-B24-2 | B24 | Fewer members hold underwritten excess than the minimum-count rule | Loading portion billed to the member as voluntary cover. Standard-rate portion stays in the employer's total | X | B | 1 |
| GRP-012 | US-B15-1 | B15 | Invoice with three billing groups | Schedule per billing group. Invoice equals the sum of member premiums | R | B | 1 |
| GRP-013 | US-B06-1 | B6 | Employer remits 80% of the invoice | Allocated in the order the scheme's terms say. Shortfall becomes an adjustment on the next invoice. No member individually in arrears | J | B | 1 |
| GRP-014 | US-B06-1 | B6, ST7 | Third consecutive short payment | Escalates to group administration. Scheme standing may suspend new joiners before touching existing members | J | C | 2 |
| GRP-015 | US-B25-3 | B25 | S12: one remittance covers two invoices and 120 members, then is recalled as a duplicate | Remittance advice parsed and matched. Recall reverses the allocation by new entries. Standing recalculated from original due dates | J | C | 2 |
| GRP-016 | US-P14-2 | P14, E2 | Voluntary top-up cover unpaid by the member | Its own receivable and standing. Employer's standing untouched | J | C | 1 |
| GRP-017 | US-P20-2 | P20 | Dependants added as voluntary cover on a group health scheme | Billed to the member. Employer sees tier, not who the dependants are | X | C | 1 |
| GRP-018 | US-B03-2 | B3 | Employer-paid core with member-paid options | Cores priced from the scheme rate table and billed to the scheme. Options priced by retail functions and billed to members | J | C | 2 |
| GRP-019 | US-B03-1 | B3 | Payroll API sends a change | Same preview and approval steps as a file. Same commands | J | C | 2 |
| GRP-020 | US-E04-1 | E4 | Payroll export changes column order and date format | New mapping version must pass tests built from sample files. Old mapping rejects the new file. A successful preview confirms the mapping | J | C | 2 |
| GRP-021 | US-E01-2 | E1, ST12 | Renewal workflow | Starts the set time before renewal with a membership snapshot and de-identified experience. Rate table returned, accepted and activated with the renewal date, scoped to the scheme | J | B | 1 |
| GRP-022 | US-E01-2 | E1 | No agreement by the renewal date | Scheme's terms decide: old rates continue for the set period, or the scheme ends | J | C | 2 |
| GRP-023 | US-N09-1 | N9, B25 | Member leaves. Continuation taken up on day 20 of the window, and attempted by another member on the day after expiry | First issues an individual policy without underwriting on her own group, linked to the scheme cover. Second refused | J | B | 1 |
| GRP-024 | US-P33-2 | P33, E4 | Seasonal leave in June and rejoin in February | Leave and join effective-dated. Continuation window opens on leaving. Waiting period rule applied on rejoin | J | C | 2 |
| GRP-025 | US-B16-1 | B16 | Deposit premium on a pool, year-end declaration higher than the estimate | Deposit earned daily. Adjustment premium posted and recognised in the current period | J | C | 2 |
| GRP-026 | US-B16-1 | B16 | Declaration late | Task raised by the timer. Renewal can be held | N | C | 3 |
| GRP-027 | US-B02-1 | B2 | One director submits a change above the scheme's second-administrator setting | Waits for the second administrator. Both identities recorded | N | C | 2 |
| GRP-028 | US-B25-2 | B25 | S12: administrator leaves at noon and a new one starts | Leaver's sessions closed through the priority revocation channel. Joiner active from the relationship date | X | C | 1 |
| GRP-029 | US-B16-2 | B16 | Treasurer handover at the annual meeting | Relationship ends and starts on the date. Former treasurer's command refused afterwards | X | C | 2 |
| GRP-030 | US-P54-2 | P54, B15 | Third site opens: 14 staff added as a bulk join with their own billing group | Billing group created. Members joined from the opening date. Next invoice schedule shows three groups. Site headcount recorded for concentration | J | C | 2 |
| GRP-031 | US-B10-1 | B10, B11, P44 | One person administers two schemes | Two policies on two groups. One sign-in with both relationships. Nothing shared between the schemes | X | C | 2 |
| GRP-032 | US-B02-2 | B2 | Employer opens a member's record | Sees tier and premium. No health, claim, underwriting reason, loading or dependant identity | X | B | 1 |
| GRP-033 | US-B04-1 | B4, P40 | Named directors with different occupation classes | Each member's category and class drive the invoice line | J | B | 2 |
| GRP-034 | US-E01-1 | E1 | Scheme terminated | All covers end on the date. Every eligible member receives a continuation offer. Claims with earlier event dates remain payable. Final adjustment invoice settles | J | C | 2 |
| GRP-035 | US-E01-1 | E1 | Takeover from a previous insurer with continuity terms | Members accepted on the terms recorded with a takeover reason code. People already on claim stay with the previous insurer. Actively-at-work rule applied | J | C | 2 |
| GRP-036 | US-E02-1 | E2 | Scheme split across several billing-group policies | Read model presents one scheme. Membership count and sum insured in the core equal the group module's read model | R | C | 2 |
| GRP-037 | US-P37-1 | P37, B24 | S10: member on a working holiday visa leaves the employer and the country | Leaver applied. Continuation offered. Non-resident status recorded | J | C | 3 |
| GRP-038 | US-P45-1 | P45 | Overseas posting on a scheme whose rule continues cover for twelve months | Cover continues to the rule's limit and the member is told the end date | J | C | 2 |
| GRP-039 | US-P14-1 | P14, E2 | Member registers | Same identity verification as any customer. Sees own cover, can take up voluntary cover | J | B | 2 |
| GRP-040 | US-B05-1 | B5 | Shareholding moves to 60% for the buyer. Administrators change | Dated relationship changes. Former administrator's actions refused from the date | J | C | 3 |

## JNT Joint lives, ownership, assignments and dependants

Design: Policy model: joint lives, ownership and dependants.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| JNT-001 | US-P07-1 | P7, P8 | Joint first-event cover is underwritten | One decision per life. Loadings and exclusions stored per life inside the cover. Issued only when both have decisions | J | C | 1 |
| JNT-002 | US-P38-3 | P38 | First death on a joint last-survivor cover | Recorded as an event that changes cover state. Pays nothing. Second death pays | J | C | 1 |
| JNT-003 | US-P18-1 | P18 | S11: Hemi alone asks to cancel. Kiri asks for a freeze | Cancellation refused without both owners' signatures. No freeze is needed beyond the signing rule. Both contacts recorded | N | C | 1 |
| JNT-004 | US-P18-2 | P18 | S11: joint cover split into two single covers where the product offers it | No underwriting. New covers linked to the old with terms and history. Second policy created on the other person's group | J | C | 1 |
| JNT-005 | US-P18-3 | P18 | S11: family health plan split on separation | Each adult keeps terms and waiting periods served. Children follow the chosen parent. Counters used carry | J | C | 1 |
| JNT-006 | US-P18-1 | P18 | S11: split requested on a policy with a lender's interest | Refused without the lender's recorded consent | N | C | 1 |
| JNT-007 | US-P17-2 | P17, B3 | Assignment notice from the overdraft lender received on 3 May 2027 | Interest recorded with party, notice date, extent and priority on both timelines. Restriction active | J | C | 1 |
| JNT-008 | US-P07-1 | P7, P8 | With an interest standing: surrender, withdrawal, reduction, policy loan and change of owner are each attempted | All refused without the interested party's consent. Increase and payment are allowed | N | C | 1 |
| JNT-009 | US-P21-2 | P21 | Second lender gives notice a month after the first | Ordered by the date notice reached the insurer | J | C | 2 |
| JNT-010 | US-P18-1 | P18 | Kiri's mother gives notice of her informal loan after the bank | Recorded as a later interest if valid. Priority by notice date. Disputed interest flagged and restricted until a case resolves it | J | C | 3 |
| JNT-011 | US-B22-1 | B22 | Lender sends a signed discharge | Release command stored with the discharge in the vault. Restricted commands work again | J | C | 2 |
| JNT-012 | US-P36-3 | P36, B5 | Vendor's interest in a key person policy | Recorded and enforced exactly as a lender's. Released only on his discharge | J | C | 2 |
| JNT-013 | US-B05-1 | B5 | Ownership of the key person policy passes to the buyer | Request signed by every current owner, due diligence on the new owner, life insured's consent where required, relationship change, partition move | J | C | 1 |
| JNT-014 | US-P57-3 | P57 | S1: policyholder of the family health plan dies | Surviving adult becomes policyholder through the ownership workflow. Children's cover continuous. New payment authority requested | J | C | 1 |
| JNT-015 | US-P57-2 | P57 | Joint owner who is not the only life insured dies | Ownership passes to the surviving joint owner under the relationship's survivorship rule | J | C | 2 |
| JNT-016 | US-P05-2 | P5 | Newborn added 20 days after birth, inside the window | No underwriting. Effective from date of birth as a backdated join by value date. Premium adjustment posted | J | B | 1 |
| JNT-017 | US-P51-1 | P51 | Twins added in one request. A third child added after the window closes | Twins as above. Late addition goes through underwriting | J | B | 2 |
| JNT-018 | US-P42-3 | P42 | S21 and health slice exit: adult daughter reaches the age limit | Continuation offer starts before the anniversary. Cover ends at the next anniversary. Terms, waiting periods served and lifetime limits used go with her | J | B | 1 |
| JNT-019 | US-P09-2 | P9, P55 | Continuation taken up without underwriting | New policy on her own group. Lifetime counters carried, not reset | J | C | 1 |
| JNT-020 | US-P56-2 | P56 | Tries to add a cousin and a parent as dependants | Refused with the eligibility reason | N | B | 3 |
| JNT-021 | US-P39-1 | P39 | Either-to-sign joint owners. James alone reduces a cover | Applied on one signature. Schedule and notice sent to both | J | C | 2 |
| JNT-022 | US-B20-1 | B20, P19 | Trust with three trustees, any two to sign | Request with two valid trustee signatures applies. One signature refused | J | C | 1 |
| JNT-023 | US-B21-1 | B21 | S13: one trustee dies, one resigns, two appointed, one in Sydney. A request signed by the resigned trustee arrives the next day | Relationship changes dated on the resolution. Sydney trustee verified remotely. Request refused | J | C | 1 |
| JNT-024 | US-P44-2 | P44, B22 | Accountant trustee must co-sign | Signing rule enforced. Raj and Sunita alone are refused | N | C | 2 |
| JNT-025 | US-P25-2 | P25, B12 | Connor joins the partnership as owner and life | Relationship change with due diligence. His cover is a new layer after underwriting. Lender's consent where the change is restricted | J | C | 2 |
| JNT-026 | US-B12-1 | B12 | Partnership asks to reduce cover while the lender's interest stands | Quote shows the consent requirement. Refused without it | N | C | 2 |
| JNT-027 | US-B19-1 | B19, P54 | Notice of assignment arrives the same day as a second lender's | Each interest carries the time notice was received. Priority follows it | J | C | 3 |
| JNT-028 | US-P23-1 | P23 | Blended children on one plan: partner's two and his son | Each a party related to the policyholder with own life record and terms | J | B | 3 |
| JNT-029 | US-N05-1 | N5 | Official in a bankruptcy claims the surrender value | **Design gap.** Held open until the design says whether this is an interest, an ownership change or a restriction | N | C | 3 |
| JNT-030 | US-P50-1 | P50 | S2: attempted transfer of ownership to an unrelated person overseas | Due diligence and screening on the new owner. Abuse indicator routes to a specialist. Not acted on until reviewed | N | C | 1 |
| JNT-031 | US-P51-3 | P51, B23 | Newly formed trust applies as policyholder with three trustees | Due diligence and screening on the trust and each trustee complete before issue. Signing rule recorded on the ownership relationship. Issue refused while any check is open | J | C | 2 |

## ADV Advisers and commission

Design: Advisers and commission; Advisers and commission: scenarios tested; Worked example: replacement under a new adviser with a top-up.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| ADV-001 | US-D01-1 | D1, D6, P17 | 1 Mar 2027: D1 at D6 writes P17's policy. Renewal commission of $100 is earned on a premium | Layer joins book "D1 at D6". $75 to Maddox Advice Ltd, $25 to D6. Posted in the same entry as the receipt, one posting per payee | J | B | 1 |
| ADV-002 | US-D01-1 | D1, D6, D7 | S18: from 1 Nov 2028 a new rule version with a 25% tail to D6 ending 31 Oct 2030 | On $100: $25.00 to D6, $56.25 to Maddox Advice Ltd, $18.75 to D7 | J | C | 1 |
| ADV-003 | US-D01-1 | D1, D6, D7 | Premium received on 1 Nov 2030 | $75 and $25 with no new rule activated. The tail line ended itself | J | C | 1 |
| ADV-004 | US-D06-1 | D6, P17 | P17 lapses in January 2029, inside the clawback period | Exposure item names D6 as liable. Clawback debited to D6 though it now receives a quarter of the trail | J | C | 1 |
| ADV-005 | US-P29-1 | P29, D1, D7 | Increase in 2029 after the move | New layer in book "D1 at D7". Its initial, renewal and clawback belong to the new arrangement. D6's tail applies only to the original layer | J | C | 1 |
| ADV-006 | US-D10-1 | D10, D7 | Group override as a separate component from a shared fragment | Paid on top of base, not carved out of it. Fully allocated | J | C | 1 |
| ADV-007 | US-D07-1 | D7, D10, D11 | D7 changes aggregator | One fragment activation. D10 keeps override on books existing at the change. New books reference D11's fragment | J | C | 1 |
| ADV-008 | US-D01-1 | D1, D6, D7, D8 | Second move with overlapping tails | Both tails are lines on the whole with own end dates. D1 and D8 share the remainder. Each tail drops out on its date | J | C | 2 |
| ADV-009 | US-D02-1 | D1, D2 | Jointly written policy at 60 and 40. D1 moves | Only D1's book changes rule. D2's book untouched | J | C | 2 |
| ADV-010 | US-D02-1 | D2, P17 | D2 adds a cover to a policy D1 wrote | Added cover is a layer with its own writing engagement and book. Each premium split across layers | J | C | 2 |
| ADV-011 | US-D02-1 | D2 | Adviser holds two engagements at once | Each proposal lands in the book of the engagement it was written under. Nothing shared | J | C | 3 |
| ADV-012 | US-D03-1 | D3, D1, D2, D4 | Retiring adviser's book split among three successors with a tail | Layers moved by file into three new books each carrying the tail. Successors' existing books pay no tail | J | C | 2 |
| ADV-013 | US-P27-1 | P27, D3, D4 | Customer appoints D4. Setting is a six-month stand-down | Servicing and access change at once. Renewal commission moves by a future-dated book assignment. D3's open exposure items stay D3's | J | C | 1 |
| ADV-014 | US-D06-1 | D1, D6, P1 | Premium dishonoured after the rule changed | Reversal follows the original entry's lines. The parties who were paid are debited | J | C | 1 |
| ADV-015 | US-D06-1 | D1, D6, D7 | Backdated cancellation spanning the move | Each refunded period reverses the lines of the entry that recorded it | J | C | 2 |
| ADV-016 | US-D06-1 | D6, P17 | Lapse then reinstatement after the move | Reinstatement reverses the clawback entry and restores the same party. Exposure items resume | J | C | 2 |
| ADV-017 | US-D06-1 | D4, D3, P27 | New adviser's alteration triggers clawback. Default setting, then clawback by cause switched on | Default falls on the original liable party. With the switch it falls on the engagement that submitted the alteration | J | C | 2 |
| ADV-018 | US-D01-1 | D1, ST15 | Wrong commission option keyed at issue and found a year later | Layer terms corrected with a past effective date. Events replayed. Commission expense changes and differences post today | J | C | 2 |
| ADV-019 | US-D09-1 | D9 | S22: FAP loses its licence | New business refused by Cedar. Lines directed to held accounts. Servicing reassigned and customers told | J | C | 1 |
| ADV-020 | US-D09-1 | D9, ST10 | Wound-up FAP owes clawback with no guarantor | Debt written off to commission bad debt by an approved command. Amounts owed to it held until a liquidator instructs | J | C | 2 |
| ADV-021 | US-D04-2 | D4, D15 | Commission assigned to a financier | Payment instruction pays the lender first from each statement. Entitlement and statement unchanged | J | C | 2 |
| ADV-022 | US-D15-1 | D4, D15 | D4 tries to remove the instruction | Refused without the lender's recorded consent | N | C | 2 |
| ADV-023 | US-D01-3 | D1, D6 | S20: disputed share of a book | Disputed share goes to a held account. Commission expense still recognised. Others paid. Signed agreement releases it in the agreed shares | J | C | 1 |
| ADV-024 | US-D05-1 | D5, ST13 | Payment hold during an investigation | Accrual continues. Statements issue marked held. Nothing recalculated when the hold lifts | J | C | 2 |
| ADV-025 | US-D14-1 | D14, P18 | Referral fee on issue | Fixed fee component with own line and clawback terms. Partner has no engagement and no customer access | J | C | 3 |
| ADV-026 | US-D04-1 | D4, D3, P27 | S29: replacement of $500,000 with a top-up to $800,000 | Replacement layers carry the original book, zero initial, trail on original lines, original exposure clocks. Top-up of $300,000 is a new layer in D4's book with full initial and its own exposure item | J | C | 1 |
| ADV-027 | US-P27-2 | P27 | Same replacement from the customer's side | No gap and no new waiting period on the replaced amount. Only the increase is underwritten. One cover, one sum insured, one premium | J | C | 1 |
| ADV-028 | US-D04-1 | D4, P27 | Replacement confirmation attempted before the replacement disclosure is confirmed | Refused. Disclosure states the adviser earns nothing on the replaced amount | N | C | 1 |
| ADV-029 | US-D05-1 | D5, P34 | Disguised replacement: cancellation then a new policy on the same life nine months later through a related adviser | Detection rule matches. Matched amount treated as a replacement layer. Adviser told at submission and may dispute, sending matched commission to the held account | J | C | 1 |
| ADV-030 | US-D12-1 | D12, D13, E1 | Group scheme changes broker mid-year | Scheme's book changes from the appointment date with any split as dated lines | J | C | 2 |
| ADV-031 | US-D13-1 | D13, E1 | Incoming broker opens the portal the day before appointment and the day of | Refused, then scoped to the scheme | X | C | 2 |
| ADV-032 | US-D01-1 | all | Every commission event in simulation | Allocated lines sum to gross commission to the cent. Designated line absorbs rounding | P | B | 1 |
| ADV-033 | US-ST15-1 | ST15 | Activate a distribution rule that leaves 5% of a component unallocated on one date | Refused at validation | N | B | 1 |
| ADV-034 | US-ST10-1 | all | Statement cut and sweep | Payable accounts across partitions equal the distribution read model and the statement. Held accounts equal open disputes and frozen books | R | C | 1 |
| ADV-035 | US-D08-1 | D8, D1, D10 | Portal scope | Adviser sees own lines. FAP sees everything under it. Group sees its overrides and totals. None sees clinical data or claim cause | X | C | 1 |
| ADV-036 | US-P27-1 | P27, D3 | Customer ends an adviser's authority in the app | Access ends at once regardless of who is paid commission | X | B | 1 |
| ADV-037 | US-D01-1 | D1, ST17 | Payee bank account changed | Signed step, payee name check and cooling-off before the first payment to the new account | J | C | 1 |
| ADV-038 | US-D01-1 | D1, P57 | Commission disclosure at point of sale | Dry run of the same commission function. Equals what is later paid | P | B | 2 |
| ADV-039 | US-D06-1 | D1, P43 | Premium waived on claim, and premium paused in hardship | Waived premium is posted as received and earns unless the schedule says otherwise. Paused premium earns nothing | J | C | 2 |
| ADV-040 | US-D03-1 | D3 | Adviser dies after retiring | Engagement ended. Agreement decides whether entitlement passes to the company or estate, to the FAP, or ends | J | C | 3 |
| ADV-041 | US-D08-1 | D8 | Authorised body under D8 gains its own licence | Engagements and place in the hierarchy change from a date. Books and payee unchanged. Fragments above replaced once | J | C | 3 |
| ADV-042 | US-D01-1 | D1, ST15 | Move agreed late and activated with a past effective date | Events since replayed under the corrected rule. Differences post today as transfers between payees. Commission expense unchanged | J | C | 2 |
| ADV-043 | US-D09-1 | P17 | Direct business and orphaned business | Direct policy sits in no book and earns nothing. Orphan sits in the house book | J | B | 3 |
| ADV-044 | US-D06-1 | D6, P57 | S1: Tom's cover ends because a claim was paid inside the clawback period | Exposure items close with nothing clawed back | J | B | 1 |
| ADV-045 | US-D11-1 | D11, D7, D4 | Book created after D7 joined D11 | References D11's override fragment. D10 receives nothing on it | J | C | 2 |

## HLT Composable health product

Design: Composable health product.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| HLT-001 | US-P03-2 | P3 | Tries to select specialist without hospital, and to drop hospital while specialist remains | Both refused by composition rules | N | B | 1 |
| HLT-002 | US-P03-2 | all | Any valid subset of components | Premium equals the sum of cover premiums under the discount chain | P | B | 1 |
| HLT-003 | US-P03-2 | all | Add then remove a component | Policy returns to its prior state by value | P | B | 2 |
| HLT-004 | US-P42-1 | P42 | Surgery with specialist consultation and post-operative physio | Splits into lines by each schedule's rules, each against its own counters and excess | J | B | 1 |
| HLT-005 | US-P08-1 | P8 | Claim line for a component the life does not hold | Declined with a reason naming the component. App offers a quote to add it with waiting periods plain | N | B | 2 |
| HLT-006 | US-P24-1 | P24 | Maternity component added, claim inside its waiting period | Waiting period runs from the component's start date. Claim not payable under it yet | J | B | 2 |
| HLT-007 | US-P22-3 | P22 | Mental health component added later | Rules ask only what that component needs. Prior disclosures are inputs. Nothing asked twice | J | C | 2 |
| HLT-008 | US-P05-2 | P5 | Dental for the children only, mental health for one adult | Per-life cover sets on one policy. Notice shows lines per life | J | B | 2 |
| HLT-009 | US-P42-1 | all | Counters across covers | Never cross covers unless the shell declares a shared counter | P | B | 1 |
| HLT-010 | US-P03-2 | P3 | Changes excess on one cover mid-term | New layer on that cover from the effective date | J | C | 3 |
| HLT-011 | US-P12-1 | P12 | Schedule assembled per component | Shell terms then each cover's section with wording version, waiting periods, limits, excess and terms for that life | J | B | 2 |

## DSC Discounts and pricing components

Design: Discounts and pricing components.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| DSC-001 | US-P05-1 | P5 | Multi-cover and multi-life discounts in a sequential chain | Each applies to the remainder in declared order. Each is its own posting line against a contra-income account | J | B | 1 |
| DSC-002 | US-P05-1 | all | Any chain | Computed in D256, net rounded once. Component lines sum to the net with the designated last line absorbing rounding | P | B | 1 |
| DSC-003 | US-P03-4 | P3 | Wellness module awards tier two for next quarter | One entitlement command with dates. Core holds a tier code only. Discount from the next billing period | J | C | 2 |
| DSC-004 | US-P03-4 | P3 | Tier lost | Takes effect from the next billing period. Past discounts not clawed back. Notice before the first affected billing | J | C | 2 |
| DSC-005 | US-N04-1 | N4 | Cannot take part in a step-count programme | Manual award ends in the same entitlement command. Who holds the discount and why is reportable | J | C | 1 |
| DSC-006 | US-ST05-1 | ST5, ST6 | Retention discount override above the officer's limit | Needs a second approver. Carries reason code and date range. Reported | N | C | 2 |
| DSC-007 | US-P48-1 | P48 | Total cap and floor both bite | Reduction taken from the last discount in the order. Net never below the floor | J | C | 2 |
| DSC-008 | US-P40-1 | P40, B4 | Multi-cover discount across the policyholder's group | Counts personal covers together. Excludes the scheme, which is the company's | J | C | 2 |
| DSC-009 | US-P03-4 | P3 | Entitlement backdated inside a billed period | Adjustment posts by value date | J | C | 3 |
| DSC-010 | US-P17-3 | P17 | Lapse and reinstatement inside the window | Entitlements survive. Overrides survive unless their end date has passed | J | C | 3 |
| DSC-011 | US-ST15-1 | ST15 | A pricing component that would increase the premium is declared as a discount | Refused. Discounts never go negative | N | B | 2 |
