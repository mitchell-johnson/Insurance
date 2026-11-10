# Test matrix, part 3: servicing and operations

Policy servicing, the anniversary window, annuities, vulnerable customers and representatives, complaints, staff authorisation, payment runs, finance close and regulatory reporting, configuration and the actuary's interface, incident runbooks, documents, and migration. Column meanings are in part 1.

## SVC Policy servicing

Design: Policy servicing.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| SVC-001 | US-P29-1 | P29 | Quote for an increase, then apply it | Quote is a dry run. Result equals the quote. New layer with own start date, decision, commission terms and cession. Existing layers untouched | P | C | 1 |
| SVC-002 | US-P24-1 | P24 | Decrease of $100,000 on a cover with three layers | Layers reduced newest first by default. Unearned premium refunded by value date. Commission clawed back in proportion on the layers reduced. Cession recalculated | J | C | 1 |
| SVC-003 | US-P24-1 | P24 | Cancellation request in the app | Shows what is being given up and the alternatives the product allows before confirmation | J | C | 1 |
| SVC-004 | US-P53-1 | P53 | Future-dated change of waiting period on income protection | Stored as pending state. Applied by a logged command on its date. Old layer ends and a new one starts | J | C | 2 |
| SVC-005 | US-P10-2 | P10 | Schedule after each of three changes | New schedule rendered from policy state each time, hashed and stored. Earlier versions kept | J | C | 2 |
| SVC-006 | US-P03-3 | P3 | Enhancement pass-back, then a claim that only the new wording would pay | Cover bound to both versions. Claim assessed against both and paid under the better | J | C | 1 |
| SVC-007 | US-P10-3 | P10 | Pass-back applied to a migrated cover on legacy wording | Same mechanism. Original wording never taken away | J | D | 2 |
| SVC-008 | US-P50-1 | P50 | Full surrender of UL1 | Sell order for every fund at the next price less any surrender charge. Value raised in benefit payable and paid after payee verification and screening | J | C | 1 |
| SVC-009 | US-P38-1 | P38 | Surrender value on WOL0 from the factor table | Product function of policy facts and versioned surrender factors. Signed by every owner. Lender's consent required | J | C | 1 |
| SVC-010 | US-P06-1 | P6 | END0 matures on 1 Sep 2027 | Workflow contacts him ahead with options. Maturity value raised in benefit payable on the date by a logged time trigger | J | C | 1 |
| SVC-011 | US-P06-1 | P6 | Maturity proceeds unclaimed | Stay in benefit payable. Chased with proof of delivery. Eventually follow the unclaimed money process | J | C | 3 |
| SVC-012 | US-P38-1 | P38 | Policy loan drawn up to the product's share of surrender value | Loan account in the same group. Interest accrues daily and capitalises on the cycle | J | C | 2 |
| SVC-013 | US-P38-1 | P38 | Loan grows to exceed surrender value | Policy ends after notice by the product's rule | J | C | 2 |
| SVC-014 | US-P13-3 | P13 | Paid-up conversion requested | Reduced sum insured from paid-up factors. No further premiums billed | J | C | 2 |
| SVC-015 | US-P36-2 | P36 | Income protection reaches expiry age 65 | Cover event with advance notice and any conversion option. Billing stops | J | C | 2 |
| SVC-016 | US-P02-3 | P2 | Date of birth found to be a year out | Policy re-rated from the start by value date. Difference posted today. Remedy for misstatement applied by the workflow under the legal rule | J | C | 2 |
| SVC-017 | US-P49-1 | P49 | Legal name and title changed | One party change updates every relationship. New schedule issued. Earlier documents kept and marked superseded | J | C | 2 |
| SVC-018 | US-P06-2 | P6, ST5 | Staff enter a change for a customer who rang | Carries staff identity, reason and case ID. Customer approves by the assisted-channel prompt. Second approver above thresholds and for anything backdated | J | C | 2 |
| SVC-019 | US-P07-2 | P7 | Assisted change at a counter | Same quote, same typed command and same refusal rules as the app | J | C | 2 |
| SVC-020 | US-B13-1 | B13, P30 | Key person policy cancelled on sale of the business | Unearned premium refunded. Lender's interest released by discharge first | J | C | 3 |
| SVC-021 | US-P06-3 | P6, P50 | Payout account changed to a new account | Signed step, payee name check and cooling-off before the first payment | J | C | 1 |
| SVC-022 | US-P52-1 | P52 | Customer asks for a self-set hold on refunds with a cooling-off on removal | **Design gap.** Held open until the design says whether this exists | N | C | 3 |
| SVC-023 | US-P02-2 | P2 | Conversion to another product | New policy linked to the old. Terms, waiting periods served, commission and reinsurance carry by their rules | J | C | 3 |
| SVC-024 | US-ST15-1 | ST15, ST12 | Rate table change for in-force business | Applies from each policy's next anniversary with the product's notice period | J | C | 2 |
| SVC-025 | US-P34-1 | P34 | Cancels and takes a new policy elsewhere, then returns | Replacement monitoring flags a new policy soon after a cancellation on the same life | J | C | 3 |
| SVC-026 | US-P24-2 | P24 | Exercises the future insurability option after a birth, then tries again when the product's count of options is used up | First adds a layer without underwriting after the birth evidence is checked. Second refused with the count shown | J | C | 1 |

## ANV The anniversary window

Design: The anniversary window.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| ANV-001 | US-P05-1 | P5 | Window opens 60 days before the anniversary | One workflow per policy. Offer is a dry run bound to product, rate table and indexation rate versions. Notice with proof of delivery | J | B | 1 |
| ANV-002 | US-P05-1 | P5 | Customer does nothing | On the anniversary the core applies indexation, re-rating and billing on the new premium in one logged step. Indexation adds a layer | J | B | 1 |
| ANV-003 | US-P20-1 | P20 | Declines indexation for the third consecutive year | Recorded. Third notice said it was the last. Right ends by default | J | C | 2 |
| ANV-004 | US-P24-1 | P24 | Reduces a cover in the window, choosing immediate effect | Mid-term alteration with a pro-rata refund. Anniversary offer recomputed and re-notified | J | C | 2 |
| ANV-005 | US-P29-1 | P29 | Asks for an increase in the window. Decision arrives after the anniversary | Renewal of existing cover never waits. New layer starts on the decision date | J | C | 2 |
| ANV-006 | US-ST15-1 | ST15 | Rate table changes after the offer was sent | Offer honoured. Next anniversary picks up the new table | J | C | 1 |
| ANV-007 | US-P20-1 | P20 | Late decline of indexation 10 days after the anniversary | Indexation layer reversed with the anniversary as value date. Difference refunded. New schedule issued | J | C | 2 |
| ANV-008 | US-P20-1 | P20 | Same request 40 days after the anniversary | Treated as an ordinary mid-term alteration | N | C | 3 |
| ANV-009 | US-P07-1 | P7, P8 | Policy with a lender's interest | Lender receives the same renewal notice. A reduction needs the lender's consent before the anniversary or is an exception | J | C | 2 |
| ANV-010 | US-P17-3 | P17 | Policy lapses during the window and is reinstated inside the grace period | Workflow ended at lapse. Reinstatement reopens the anniversary | J | C | 3 |
| ANV-011 | US-P57-1 | P57 | Death notified during the window | Anniversary workflow for that policy ends and claims takes over. Covers on other lives renew as the product says | J | C | 2 |
| ANV-012 | US-P11-2 | P11 | Anniversary while a claim is in payment | Benefit indexation follows the product's rule. Sum insured on the cover under claim not indexed unless the product says so. Pending increases held | J | C | 2 |
| ANV-013 | US-P18-2 | P18 | Ownership transfer mid-window | Policy moves with its pending changes. Workflow re-registered on the target partition | J | C | 2 |
| ANV-014 | US-P16-1 | P16 | Renewal notice cannot be delivered | Exception item raised | N | B | 1 |
| ANV-015 | US-P05-1 | P5, P4 | Two policies in one group share an anniversary | One notice and one screen. Each keeps its own workflow and pending changes | J | C | 3 |
| ANV-016 | US-ST16-2 | all | Simulated year at scale | Every policy with an anniversary on a business date has an applied renewal after end-of-day. Late run applies with the anniversary as value date | S | A | 1 |
| ANV-017 | US-P05-1 | P5 | Renewal notice content | Shows indexation effect, rate table change and age re-rating separately, choices, window and grace dates, right to cancel, how to complain, wording version | J | B | 2 |
| ANV-018 | US-ST15-1 | ST15 | Product configured with a 20-day window | Refused. Enforced minimum is 30 days | N | B | 2 |
| ANV-019 | US-P53-1 | P53 | Revises choices three times in the window | Last confirmed set wins. Each revision is an event. Every confirmation is a signed step | J | C | 3 |

## ANN Annuities

Design: Annuities.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| ANN-001 | US-P36-1 | P36 | Immediate lifetime annuity bought with $400,000, ten-year guarantee, 2% escalation | Quote is a dry run of the rating function. Issued income equals the quote. Consideration lands in the annuity consideration account and is applied by command. No receivable and no unearned premium | J | C | 1 |
| ANN-002 | US-P36-1 | P36 | Monthly payments for a year | Each posts from policy liability to benefit payable and out through the benefit run. Liability accrues interest daily at the basis rate | J | C | 1 |
| ANN-003 | US-P36-1 | P36 | Escalation at anniversary | New layer. Window is notice only | J | C | 2 |
| ANN-004 | US-P16-2 | P16 | Proof of life due. Signed declaration returned by post | Check satisfied. Payments continue | J | C | 1 |
| ANN-005 | US-P16-2 | P16 | No confirmation within the grace period | Payments held in benefit payable, not cancelled. Task raised. Contact escalates. Released in full on confirmation | J | C | 1 |
| ANN-006 | US-P59-1 | P59 | S16: death notified four months late, inside the guarantee period | Payments end from the date of death. Guarantee balance paid to the estate. Payments after death recovered from the estate under the overpayment design | J | C | 1 |
| ANN-007 | US-P59-1 | P59 | Niece in Australia acts under a power of attorney | Verified remotely. Authority document in the vault with scope and dates. Letters as the channel | J | C | 2 |
| ANN-008 | US-N10-1 | N10, P10 | Representative satisfies proof of life for a person in care | Accepted within the authority's scope. Recorded with the representative as actor | J | C | 2 |
| ANN-009 | US-P06-1 | P6 | Annuity purchased with maturity proceeds | Proceeds move from benefit payable to consideration by command. No money leaves and returns | J | C | 2 |
| ANN-010 | US-P46-2 | P46 | Payment details change request on an annuity | Payee verification with cooling-off before the first payment to the new account | J | C | 1 |
| ANN-011 | US-P38-3 | P38 | Joint-life annuity, first death | Last-survivor payment rules apply. Reversionary percentage starts if declared | J | C | 3 |
| ANN-012 | US-P36-1 | P3 | Deferred annuity vests | Conversion command applies the unit balance as consideration at the rate table in force or the guaranteed rate on the layer | J | C | 3 |
| ANN-013 | US-ST12-2 | ST12 | Annuity tax attribute | Payment carries a tax attribute per component and the finance ledger reports them separately. Blocked from launch until tax review is recorded | N | C | 2 |

## VUL Vulnerable customers, support people and representatives

Design: Vulnerable customers as a process; Policy servicing (conduct at the point of change).

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| VUL-001 | US-P22-2 | P22 | Tells the insurer what helps and asks for a marker | Recorded with kind, start, review date and adjustments. Visible to her in the app. No diagnosis recorded | J | C | 1 |
| VUL-002 | US-ST06-1 | ST6, ST5, P22 | Routed action reaches trained staff. An untrained officer opens the same customer | Trained staff see adjustments in effect and the recommended approach. The kind is hidden unless the role needs it. No label on any list or header | X | C | 1 |
| VUL-003 | US-P38-1 | P38 | S7: large surrender requested | Paused for a conversation with a trained person. Their decision stands afterwards. The pause never becomes a refusal | J | C | 1 |
| VUL-004 | US-P38-1 | P38 | S7: two children ask the insurer to limit their parents' withdrawals | Refused. No authority. Nothing disclosed. A prompt is raised for a person to consider a marker | N | C | 1 |
| VUL-005 | US-P50-1 | P50, ST6 | S2: third surrender, this time in full, to a newly added overseas account | Conversation, payee cooling-off and specialist review. Earlier payments considered for recall. His decision stands after the conversation | J | C | 1 |
| VUL-006 | US-P50-2 | P50 | S2: daughter calls, worried | Recorded as a third-party concern. Nothing about the policy confirmed. Prompt to consider a marker | N | C | 1 |
| VUL-007 | US-P46-3 | P46, ST6 | S6: beneficiary changed to a tradesperson | Matches a declared abuse indicator. Not acted on until the specialist has reviewed | N | C | 1 |
| VUL-008 | US-P46-2 | P46 | S6: enduring power of attorney activated with a medical certificate | Representative relationship active from the date with document, scope and dates in the vault. Beverley keeps her own sign-in to view | J | C | 1 |
| VUL-009 | US-P46-2 | P46 | Fiona tries to act the day before activation | Refused | N | C | 1 |
| VUL-010 | US-P10-1 | P10, N10 | Attorney changes the payment method and then tries to change the beneficiary to herself | First allowed within scope. Second refused or routed by the abuse indicator. Both record the actor | N | C | 1 |
| VUL-011 | US-P33-1 | P33, N11 | Welfare guardian acts under a court order and asks for a record of her actions | Actions limited to the order's scope. Report of every action with dates is produced from the access and command log | J | C | 2 |
| VUL-012 | US-N11-1 | N11, P33 | Michael's policy and money | Policy sits in his own group, separate from his guardian's. Benefit paid to his account | J | C | 2 |
| VUL-013 | US-P23-1 | P23 | Partner holds a written authority limited to paying premiums | Payment allowed. Change of cover, beneficiary or bank details refused | N | C | 1 |
| VUL-014 | US-P45-1 | P45 | Mother holds a limited authority during deployment | Works within scope for the dated period and ends on its end date | J | C | 2 |
| VUL-015 | US-P57-1 | P57 | Death claim on a joint policy | Creates a prompt for a person to consider a bereavement marker. Never an automatic marker | J | C | 2 |
| VUL-016 | US-P22-2 | P22 | Evidence window and offer life for a marked customer | Customer-side deadlines extended within configured limits. Insurer-side timers unchanged | J | C | 2 |
| VUL-017 | US-ST06-1 | ST6 | Marker reaches its review date | Review task for a trained person. Outcome renew, change or remove. Removal is an event with history kept under the restricted class | O | C | 3 |
| VUL-018 | US-P46-2 | P46 | Capacity is in question before any authority is activated | Process pauses for the authority question with legal involved. Cover and claims continue meanwhile | J | C | 2 |
| VUL-019 | US-N06-1 | N6 | Post-only customer with a representative | Notices by post with proof of delivery. Representative acts within scope | J | C | 3 |
| VUL-020 | US-P52-2 | P52, ST13 | S8: overpay then refund request three times in two months | AML rule raises a restricted case. Front-line staff see a support prompt only | X | C | 2 |
| VUL-021 | US-P21-3 | P21 | Lin prefers Mandarin, Kevin English | Presentation preference is per party. Both documents render from the same data snapshot | J | C | 3 |

## CMP Complaints and disputes

Design: Complaints and disputes.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CMP-001 | US-P58-2 | P58, ST1 | Says in passing to an underwriter that the questions were insulting | Recorded in one step from inside the case. Linked to that context. Acknowledged within the timer with the external scheme's details | J | C | 1 |
| CMP-002 | US-P58-2 | P58, ST14 | Complaint about the underwriter's decision | Handled by someone who did not make the decision, of equal or greater authority. Reviewer sees the file under a logged purpose | X | C | 1 |
| CMP-003 | US-ST14-1 | ST14 | Complaint unresolved at the deadlock period | Deadlock notice sent automatically with the right to refer to the scheme | J | C | 1 |
| CMP-004 | US-ST14-1 | ST14, P58 | Complaint upheld | Remediation through ordinary commands: claim or proposal reopened, refund through payments, goodwill as ex gratia. Nothing adjusted outside the system | J | C | 1 |
| CMP-005 | US-ST14-1 | ST14, ST16 | Reviewer finds the cause affects others | Remediation case opened for the population. Conduct officer told | J | C | 2 |
| CMP-006 | US-ST14-1 | ST14 | Referral to the external scheme | Case state with the scheme's reference and deadlines. Document bundle assembled from the vault, reviewed and its release logged | J | C | 2 |
| CMP-007 | US-P58-2 | D5, P34 | Complaint about an adviser | Adviser sees that a complaint exists and its outcome, not its contents beyond what they must answer. Pattern feeds oversight | X | C | 2 |
| CMP-008 | US-P28-2 | P28 | Complaint resolved on the spot by front-line staff | Still recorded with category and outcome | J | C | 3 |
| CMP-009 | US-P16-3 | P16 | Peter, who is not a customer, complains that he was refused information | Complaint accepted from a non-customer. Party created. Handled without disclosing her affairs | J | C | 3 |

## STF Staff authorisation and permissions

Design: Staff authorisation and permissions.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| STF-001 | US-P60-1 | P60, ST5 | Staff member with no case, queue or scheme path searches for Marcus | No path, no access, whatever the role. Attempt logged and flagged as outside normal pattern | X | B | 1 |
| STF-002 | US-N07-1 | N7 | Staff member looks up her own policy and her cousin's | Own access view only as a customer. Staff path to both refused and flagged | X | C | 1 |
| STF-003 | US-ST05-1 | ST5 | Collections officer requests a response containing a loading | Field masked or omitted and the response says so. Unmasking is a separate action with its own purpose | X | B | 1 |
| STF-004 | US-ST01-1 | ST1 | Call made with no declared purpose | Refused | N | B | 1 |
| STF-005 | US-ST07-1 | ST7 | Cover grant for a colleague on leave | Inherits the colleague's queues and schemes. Does not inherit their authority profile. Expires on its end date. Decision records the grant | J | C | 2 |
| STF-006 | US-ST01-1 | ST1, ST2 | Profile uplift requested for a short-staffed week | Needs the function head. Short maximum life. Cannot exceed the template ceiling or widen data class | N | C | 2 |
| STF-007 | US-ST17-1 | ST10, ST17 | Preparer tries to approve own payment run. Approver tries to edit a payee | Both refused by policy | N | C | 1 |
| STF-008 | US-ST02-1 | ST2, ST4 | One person is offered underwriting and a claim on the same life | Declared conflict. Refused | N | C | 2 |
| STF-009 | US-ST16-1 | ST16 | Break-glass access used | Wider role for a short time. Alert raised immediately. Reviewed within the set period | O | C | 2 |
| STF-010 | US-B25-2 | ST7 | Staff member leaves the insurer | Every relationship ends on the leaving date. Sessions closed through priority revocation | X | B | 1 |
| STF-011 | US-ST15-1 | ST15 | Cedar policy change that widens access | Policy test suite of allowed and denied cases runs in CI. Change needs a security approver | O | B | 2 |
| STF-012 | US-ST15-1 | ST15 | A new response field is added with no data class | Compile fails | N | A | 1 |
| STF-013 | US-ST13-1 | ST13, ST3 | Front-line assessor opens a claim under investigation | Sees the claim. Sees no sign that an investigation exists | X | C | 1 |
| STF-014 | US-ST01-1 | ST1 | Looks at own access | Sees own roles, profile, recent access log and the reason for any denial | J | C | 3 |
| STF-015 | US-ST16-1 | ST16 | External entitlement source unavailable | Existing grants run to their end times. No new grants. Elevation fails closed. Alert raised. Templates keep working | F | C | 2 |
| STF-016 | US-ST16-1 | ST16 | External entitlement with no mapping | Confers nothing. Listed in the alignment report as an exception with an owner | N | C | 3 |
| STF-017 | US-ST15-1 | ST15, ST16 | Product owner and platform engineer open customer data | Neither role holds a personal data class. Refused | X | B | 1 |
| STF-018 | US-ST02-1 | ST2 | Clinical record reached by search, in a bulk export, or through a grant | All refused. Clinical access only through a held or review task, each view logged | X | B | 1 |

## PAY Payment runs and bank connectivity

Design: Money in and money out.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PAY-001 | US-ST10-1 | ST10, ST17, P11, P43 | Benefit run for a benefit date | Prepared with count and control total, checked, approved by a second person, submitted, acknowledged, settled, closed. Totals reconcile to the bank statement | J | C | 1 |
| PAY-002 | US-BK1-1 | BK1, ST10 | Same payment file resubmitted | Rejected by content hash. Repeated API call returns the original result | N | C | 1 |
| PAY-003 | US-ST10-1 | BK1 | Bank accepts 97 of 100 items | Accepted items proceed. Three return to their payable account with tasks | J | C | 1 |
| PAY-004 | US-BK2-1 | BK1, BK2, P57 | Primary bank channel down with an urgent claim due | Runs queue. Urgent payment switches to the second bank by configuration. Customer sees the expected date move | F | C | 1 |
| PAY-005 | US-P37-1 | P37 | S10: refund sent to a bank account that has since closed | Return reverses to the payable account and raises a task. Never lands in suspense. Paid to a verified overseas account instead | J | C | 1 |
| PAY-006 | US-ST17-1 | ST17 | Same payee and amount appear in two runs within the window | Duplicate check raises a task before payment | N | C | 2 |
| PAY-007 | US-ST10-1 | ST10 | Run total exceeds the per-run limit, or disbursement account is not funded | Run blocked | N | C | 2 |
| PAY-008 | US-ST17-1 | ST17 | Urgent payment at 2am | By API with two approvers from the on-call list. Reported next morning | O | C | 3 |
| PAY-009 | US-ST10-1 | all | Every bank account, every day | Statement balance equals the mirrored settlement account. Every line matched or raised as an exception with an owner | R | B | 1 |
| PAY-010 | US-ST11-1 | P57, P3 | One customer payment covers a life policy and an investment policy in different statutory funds | Lands in a collection account. Allocation assigns each dollar to its fund. Daily settlement moves cash so each fund's bank accounts equal its ledger | J | C | 1 |
| PAY-011 | US-P01-1 | P1, BK1 | Direct debit collection file for a billing day | Settlement file allocates. Later dishonour file reverses | J | B | 1 |
| PAY-012 | US-P37-1 | P37 | Refund routing | Goes to the account the premium came from where possible | J | B | 2 |
| PAY-013 | US-ST10-1 | ST10, ST17 | Funding forecast with a $600,000 claim admitted | Flagged as soon as admitted. Large single payment pre-advised to the bank | O | C | 3 |
| PAY-014 | US-P57-2 | P57, ST10 | Run prepared after the channel cutoff | Submits in the next window. Customer sees the expected date | J | C | 3 |

## FIN Finance close, statutory funds and regulatory reporting

Design: Finance month-end and quarter-end; Regulatory and solvency reporting.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| FIN-001 | US-ST11-1 | ST10, ST11, ST12 | S27: quarter-end close on the synthetic insurer | One workflow with named steps. Each step an event. Dashboard shows owner and blockers | J | C | 1 |
| FIN-002 | US-ST11-1 | ST11 | Sign-off attempted with a suspense item over the threshold and no documented action | Refused | N | C | 1 |
| FIN-003 | US-ST11-1 | ST11 | Finance lead waives a blocking check | Waiver recorded with reason, remediation owner and date. Reported. The item reopens next period. The waiver's author cannot own the remediation | J | C | 2 |
| FIN-004 | US-ST12-2 | ST12 | Movement extract for the quarter | Opening in-force plus movements equals closing in-force. A difference is a defect | R | C | 1 |
| FIN-005 | US-ST12-2 | ST12 | Extract cut, then a leaver backdated into the extracted period | Extract unchanged and its hash still ties to the Merkle root. The change appears as a movement in the next extract tagged with its value date | J | C | 1 |
| FIN-006 | US-ST12-2 | ST12, ST10 | Actuarial journals returned | Posted to the finance ledger with the extract ID, maker and checker, and the actuary's sign-off recorded | J | C | 1 |
| FIN-007 | US-ST11-1 | ST10 | A correction is needed to a closed period | Period never reopens. Correction posts in the current period with the original value date as a prior-period item | N | C | 1 |
| FIN-008 | US-ST11-1 | ST11 | Rerun the close from the same extracts and versions | Same numbers | P | C | 1 |
| FIN-009 | US-ST12-2 | ST12 | Regulatory data mart contents | IDs and rating facts only. No names, contact details or medical evidence | X | C | 1 |
| FIN-010 | US-ST11-1 | ST15 | Regulator changes a return line's definition | New mapping version with an effective date and tests. Filed return immutable. Restatement is a new version with a reason | J | C | 2 |
| FIN-011 | US-ST11-1 | ST10 | Any figure on a prudential return | Opens to the query, extract and definition version behind it | J | C | 2 |
| FIN-012 | US-ST11-1 | ST10 | Accounting group and cohort assignment | Set by command at issue from a classification rule. Never changes afterwards | P | C | 2 |
| FIN-013 | US-ST11-1 | ST10 | Transfer between statutory funds | Approved journal first with evidence, then the cash movement. Never the other way round | N | C | 2 |
| FIN-014 | US-ST10-1 | all | Tax reconciliation at close | GST on health premiums and fees, levies and withholding reconcile to posting types | R | C | 2 |
| FIN-015 | US-P56-1 | P56, B8, P55, ST13 | AML rule for a payer who is not the owner | Fires once per unexplained payer. Cleared cases with a recorded relationship do not re-raise each month | J | C | 2 |
| FIN-016 | US-B08-1 | B8, P43 | Business account pays the owner's personal premium | Payer relationship recorded once. Rule stops firing | J | C | 3 |
| FIN-017 | US-ST12-2 | ST12 | Experience data request including cause of claim | Supplied as categories from inside the claims module, de-identified with small-cell suppression | X | C | 2 |
| FIN-018 | US-ST11-1 | ST11, ST12 | Conduct measures | Claims turnaround, decline and overturn rates, complaints, lapse and replacement patterns are read models built from events | O | C | 3 |

## CFG Configuration operations and the actuary's interface

Design: Configuration operations; The appointed actuary's working interface; Design review (activation scope).

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CFG-001 | US-ST15-1 | ST15 | Activation attempted with no change record or with a missing approver | Refused by Cedar on the activation command | N | B | 1 |
| CFG-002 | US-ST15-1 | ST15 | Author approves own change. Approver activates alone | Both refused | N | B | 1 |
| CFG-003 | US-ST12-1 | ST12 | Mortality table with a missing age, a value out of range, and rates falling with age | Each validation fails and blocks activation with findings returned | N | B | 1 |
| CFG-004 | US-ST12-1 | ST12 | New table moves one cell by a factor of ten | Listed by the comparison with the previous version as beyond tolerance | N | B | 1 |
| CFG-005 | US-ST12-1 | ST12, ST15 | Actuary signs the dry-run report | Activation command carries the table hash, manifest, report ID and approvers. Every posting under the version traces to the change record | J | B | 1 |
| CFG-006 | US-E01-2 | E1 | Scheme rate table activated with scope of one scheme | Present only on that scheme's partition. Follows the group if it migrates | J | B | 2 |
| CFG-007 | US-ST15-1 | ST15 | Global change staged through canary partitions | Staff and synthetic policies first, then a share, then all, each with a watch period | O | C | 2 |
| CFG-008 | US-ST15-1 | ST15 | Activation attempted on a billing peak day or during close | Needs a second approver from operations and a reason | N | C | 2 |
| CFG-009 | US-ST16-1 | ST16, ST15 | Kill switch after a bad activation | Previous version activated by a prepared command with one approver. Postings already made are remediated by replay, not reversed by the rollback | J | C | 1 |
| CFG-010 | US-ST15-1 | ST15 | Register query | Shows the version active on each scope with effective date, change record and approvers. Generated from the log. Drift report lists scopes on different versions | R | C | 2 |
| CFG-011 | US-ST15-1 | ST15 | Retire a product version still referenced by an in-force policy | Refused | N | C | 2 |
| CFG-012 | US-ST15-1 | ST15 | Change skips the synthetic insurer run | Refused. Rate and factor changes need a simulated billing cycle, an anniversary and a claim first | N | B | 2 |
| CFG-013 | US-ST12-1 | ST12 | Reserve for a claim in payment above the assessor's authority | Actuarial team reviews and approves the figure as a task on the claim | J | C | 2 |
| CFG-014 | US-P06-1 | P6, ST12 | Bonus declaration on the participating fund | Arrives as a logged command naming rate and fund. Feature applies it to each policy's sum insured as a posting | J | D | 3 |

## INC Insurance incident runbooks

Design: Insurance-specific incident runbooks.

Each runbook is rehearsed on the synthetic insurer with its detection measure deliberately tripped.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| INC-001 | US-ST16-1 | ST16 | Runbook 1: anniversary run misses one partition | Detected by the invariant after end-of-day. Run applied with the anniversary as value date. Next billing picks up the difference | O | C | 1 |
| INC-002 | US-ST16-1 | ST16, ST12, P20 | Runbook 2 and S25: wrong indexation rate applied to a cohort | Previous rate activated. Affected anniversaries replayed. Layers resized, premiums, commission and cessions adjusted. Corrected notices sent. Those who declined on the wrong figure asked again | O | C | 1 |
| INC-003 | US-B24-1 | B24, ST7 | Runbook 3 and S24: truncated file confirmed in error | Correcting file reinstates members with original join dates and no gap. Invoice adjustment nets to zero. Claim in the gap assessed on corrected membership | O | C | 1 |
| INC-004 | US-P21-1 | P21, ST8, ST9 | Runbook 4: price error after withdrawals | Fund's price activation suspended until the corrected price is approved, then as INV-010 | O | C | 1 |
| INC-005 | US-R01-1 | R1, ST10 | Runbook 5: reinsurer listing does not reconcile | Settlement held for the period. Accumulation rebuild finds the mis-cession. Supplementary listing sent before paying | O | C | 2 |
| INC-006 | US-D01-1 | D1, D6, D7 | Runbook 6: commission cycle paid the wrong parties after a rule was activated with the wrong dates | Next run held for affected books. Events replayed. Differences post between payees. Statements reissued as new versions | O | C | 1 |
| INC-007 | US-ST10-1 | ST10, BK1, P11 | Runbook 7: benefit run fails part way | Same file not resubmitted. Bank confirms accepted items. Rest go in a new run. Nobody paid twice even if the bank's confirmation is late | F | C | 1 |
| INC-008 | US-ST16-1 | ST16, P5 | Runbook 8: renewal notice sent with wrong content | Template disabled. Recipients found from notification events. Correction with proof of delivery. Mandatory notice treated as not given and its clock restarted | O | C | 1 |
| INC-009 | US-P16-1 | P16, D1 | Runbook 9: a lapse ran without a confirmed notice | Reinstated by command with no gap and no conditions. Write-offs and clawbacks reversed. Proper notice sent. Sequence restarts from the valid step | O | C | 1 |
| INC-010 | US-ST16-1 | ST16, ST3 | Runbook 10: definitions version activated in error | Previous version activated. Covers rebound. Claims assessed in the window reassessed | O | C | 1 |
| INC-011 | US-ST12-1 | ST12, P38 | Runbook 11: wrong surrender factor table | Previous version activated. Billing, surrenders and maturities replayed. Overcharges refunded with interest where required. Undercharges absorbed by default | O | C | 1 |
| INC-012 | US-ST16-1 | ST16 | Runbook 12: entitlement mapping granted too much | Previous mapping activated. Grants revoked through priority channel. Every action under the wrong entitlement reviewed. Privacy assesses notifiable breach | O | C | 1 |
| INC-013 | US-ST16-1 | all | Any remediation case | Closes only when every affected policy is adjusted or excluded with a reason. Adjustments idempotent per policy and case | P | C | 1 |
| INC-014 | US-B11-1 | B11, P44 | S5: the merchant's own scheme premium fails because its card was among those compromised | Failure tied to the incident. Scheme standing and fees remediated. Merchant told it is treated as a victim | O | C | 2 |
| INC-015 | US-P30-2 | P30, P28, P40, ST6 | S9: cyclone. Regional premium relief | Override applied in bulk by a population query on region, each policy evented and customer told. Undeliverable notices pause lapses in the region | O | C | 1 |
| INC-016 | US-P30-2 | ST13 | S9: claims from fake relief agencies and a surge of real ones | Fraud rules run on the surge. Real claims keep their service timers. Bulk remittances with poor references go through matching and suspense with owners | O | C | 2 |

## DOC Policy documents and correspondence

Design: Policy documents and correspondence lifecycle; Worked example: one person, several hats.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| DOC-001 | US-P10-2 | P10 | Render the same statement twice from the same template version and data | Byte-identical documents with the same hash. Totals equal the ledger | P | B | 1 |
| DOC-002 | US-P12-1 | P12 | Schedule and renewal notice in Arabic | Rendered from the same data snapshot as English. Document says the primary version controls | J | C | 2 |
| DOC-003 | US-P37-2 | P37 | Template review | Plain-language and accessibility requirements checked. Translation versioned together with the primary | O | C | 3 |
| DOC-004 | US-ST15-1 | ST15 | Template references a field the product does not supply | Rendering test fails before activation | N | B | 2 |
| DOC-005 | US-P16-1 | P16 | Mandatory notice by post | Proof of delivery recorded. Anything depending on the notice waits for it | J | B | 1 |
| DOC-006 | US-P40-1 | P40, B4 | Scheme invoice, family renewal notice and a claim letter for the same person | Invoice to the work address, renewal home, claim letter to personal contact points by default | J | C | 2 |
| DOC-007 | US-P48-1 | P48, P25 | Correspondence on her own policies and on the partnership's | Addressed to her by her relationship's contact points. No template assumes a primary owner | J | C | 2 |
| DOC-008 | US-P10-2 | P10 | Error found in an issued document | Never edited. New version with a reason. Both visible to the customer | N | C | 2 |
| DOC-009 | US-P49-1 | P49 | Contact point changed on the party | Every relationship using it updates. Choosing a different one for a relationship changes only that relationship | J | C | 3 |
| DOC-010 | US-ST15-1 | ST15 | A clause used by ten products is changed | One change record. Register shows ten products affected | O | C | 3 |

## MIG Migration

Design: Migration plan; migration notes in each section.

These run on S26, the synthetic legacy book built from P6, P10, P13, P16, P28, P38, P46, N10 and B14.

| ID | Story | Personas | Scenario | Expected | Type | Ph | Pri |
| --- | --- | --- | --- | --- | --- | --- | --- |
| MIG-001 | US-P10-3 | P10, N10 | 1974 policy with paper underwriting file | Loaded through ordinary commands with a migrated flag and original dates. Bound to legacy wording by hash. File stored as received | J | D | 1 |
| MIG-002 | US-P10-3 | N10 | Legacy system has no evidence of an underwriting decision | Unknown-terms marker and a case. Never defaulted to standard rates | N | D | 1 |
| MIG-003 | US-P10-3 | P13 | Legacy exclusion clause with no clean code | Carried as a verbatim-clause code pointing at the original wording. Nothing widened or narrowed | J | D | 1 |
| MIG-004 | US-P13-1 | P13 | Policy mid-grace at cutover | Keeps standing and dates. Arrives at the dunning step its age implies. No notice sent twice | J | D | 1 |
| MIG-005 | US-B14-1 | B14, P53 | Premium holiday granted in 2020 | Override loads with its original dates | J | D | 2 |
| MIG-006 | US-P10-3 | P6 | Legacy holds only a current sum insured | Loads as a single layer with the assumption recorded | J | D | 2 |
| MIG-007 | US-P10-3 | P38 | Joint cover held as one record with two names and a paper assignment | Lives separated into parties through matching. Interest loaded with its original notice date. Unclear interest flagged and restricted | J | D | 1 |
| MIG-008 | US-ST10-1 | all | Cohort load | Migration clearing accounts per cohort, currency and unit type return to zero. Reconciled to control totals to the cent and the count | R | D | 1 |
| MIG-009 | US-ST10-1 | all | Parallel run for one billing cycle | Premiums, earning, commission, cessions, invoices and statements compared line by line | R | D | 1 |
| MIG-010 | US-D01-1 | D1, D6 | Legacy single agent code | Fills writing, servicing and entitlement with the assumption recorded. Mapping checked against what each payee was last paid | J | D | 2 |
| MIG-011 | US-R01-1 | R1 | Per-life accumulation after load | Rebuilt from migrated covers and compared with legacy retention records before cutover | R | D | 1 |
| MIG-012 | US-P16-2 | P16, P59 | Annuity in payment | Migrates with layers, liability, payment history, proof-of-life status and next due date. First run reconciles to the legacy run | J | D | 1 |
| MIG-013 | US-P11-2 | P11 | Income protection claim in payment at cutover | Schedule, offsets, indexation history and next review date carried. First payment matches the last from the old system | J | D | 1 |
| MIG-014 | US-FA1-1 | FA1 | Unit-linked cohort | Cutover on a pricing boundary. Units reconciled across legacy, core, administrator and custodian | R | D | 1 |
| MIG-015 | US-ST16-1 | ST16 | Abandon a cohort before the directory flips | Load discarded. Nothing changed for anyone | F | D | 2 |
| MIG-016 | US-ST12-1 | ST12 | Surrender basis held in a workbook | Captured as a table and proven against legacy values across a grid of ages and durations | J | D | 2 |
| MIG-017 | US-ST16-1 | ST16 | S26: trial load of the synthetic legacy book | Exceptions produced equal the exceptions the book was built to produce, by type and count | S | D | 1 |
| MIG-018 | US-ST06-1 | ST6 | Legacy free-text vulnerability notes | Reviewed by a trained person before any becomes a marker | O | D | 3 |
