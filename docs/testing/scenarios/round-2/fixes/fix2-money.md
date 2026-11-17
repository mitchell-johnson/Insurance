# fix2-money: round 2 fixes for op2-c (SCN2-08, SCN2-09, SCN2-15, SCN2-16)

Merged to main on 17 Nov 2026.

Commits:
- `faf8959c` Claims: a large admission's second signature is a peer's check; run payment references fit the bank's end-to-end id
- `d863c2d9` Claims: refusals that say the limit and who can act; a blank life takes the cover's; review dates in words
- `c695ecde` Payments: authorities checked by their check digits and confirmed by a second person; hardship stops collection; refunds only to the owner's own account
- `20de6ed4` Reinsurance: no statement for no reinsurer; a held settlement is not prepared; the console shows holds and asks for the case

## Blockers

- **F2-08-2** fixed (`faf8959c`, `a_large_admission_inside_the_proposers_authority_needs_a_peer_not_a_second_limit`, `a_signature_nobody_else_can_give_is_said_in_words`). Decided in docs/decisions.md "A large claim's second signature": when the proposer's own authority covers it, the second signature is a peer's (accredited for the line, own limit at the two-signature level), so st4 can countersign st4b's $900k. The work view names who can sign (`waiting.signers`); the console offers Approve only to them; the refusal names the amount and who could sign, or that nobody can.
- **F2-15-1** fixed (`c695ecde`, `an_authority_needs_a_valid_account_and_a_second_person`). NZ check digits on authority accounts; second payee maintainer confirms (`confirm_authority`) before anything is collected. Decided in docs/decisions.md "Direct debit authorities, hardship and refunds to a payee".
- **F2-16-5** fixed (`c695ecde`, `a_refund_goes_only_to_the_owners_own_account`). A refund to a payee only goes to the policy owner's own payee; refused while the owner is unknown. Same decisions entry. A direct credit's refund goes to the owner (no rail back to a third-party account).
- **F2-X-1** (lead's extra, ISO 20022 id length) fixed (`faf8959c`, `a_run_payment_reference_fits_the_bank_end_to_end_id`): a run payment reference over 35 characters becomes `YYYYMMDD-<26 hex digest>`; the readable form is kept when it fits.

## Majors

- **F2-08-1** partly fixed (`20de6ed4`, UI only): the Lives view has its own "case, call or task" field and records searches against it. Left: showing what is ceded on one life and to whom needs a new reinsurance question (per-life cessions); not built.
- **F2-08-3** fixed (`d863c2d9`, `a_claim_notified_without_its_life_takes_the_covers`): the core's cover facts carry the life they are for; a blank life takes it.
- **F2-08-4** left: not reproduced or traced in the time; the known-then cover facts are recorded at the cover check (`covered()`), and "life on the policy: No" then would mean the core answered without the life at that time, which F2-08-3 now fills. Needs a re-test; the survival period as a shown fact (F-11-4) is still open.
- **F2-08-9** fixed (`20de6ed4`, `a_statement_for_no_reinsurer_is_refused`): the module refuses a reinsurer not on the panel (and with no money held); the Cut form has its own reinsurer picker. The treaty is still typed (left, minor).
- **F2-08-11** fixed (`20de6ed4`, `the_reinsurer_accounts_are_run_from_the_console`): a held settlement is refused before it is prepared (409 `settlement_held`); the statements table reads each statement's hold so Release is offered and Prepare is not; Net in NZ$. Left: the preparer still does not see their own prepared item (countersign list is for signers).
- **F2-09-1** fixed (`d863c2d9`, UI only): the claim lookup has the case field beside it.
- **F2-09-2** fixed (`d863c2d9`, UI only): review dates in words. Left: review tasks under Tasks (they are dues on the schedule, not tasks).
- **F2-09-4** left: paying a month ahead and the payments list/remittance contradiction need a change to how benefit runs pick periods (paid in arrears to the business date) and to the work view's payment rows; too large for this pass. Needs a claims decision on arrears versus advance.
- **F2-09-5** left: partial-benefit display and the end-of-benefit overpayment on the partial rate need the schedule's income offsets in the overpayment arithmetic (`overpay.rs`); not done.
- **F2-09-6** fixed (`d863c2d9`, `recovering_the_insurers_own_error_needs_conduct_and_legal`): the refusal names 1% to 25% of each payment. Left: the overpayment picker and "offset with nothing left to offset" prompt.
- **F2-09-7** left: a referral for investigation needs a new command, role and route (claims, channels, MCP); product decision on who refers. Not built.
- **F2-15-2** left: payments pickers for policy, party and authority need new find questions; not built.
- **F2-15-3** fixed (`c695ecde`, `every_returned_collection_is_told_to_collections` and the dishonour test): every returned collection raises a `collection_dishonoured` notice for collections with why and what next. Left: `/collections` arrears on the wall clock (stack date, Things noticed 12) and the customer page.
- **F2-15-5** fixed (`c695ecde`, `a_payer_in_hardship_is_not_collected_from`): hardship (with optional last day) stops new collections and holds one in a run. Decided (same entry).
- **F2-15-6** fixed (`c695ecde`, `a_suspended_or_changed_authority_waits_for_a_second_person`): refused at add with the reason and who clears it; reinstatement needs a second person; `change_authority_account` with confirmation and cooling-off.
- **F2-15-7** not a defect in the matching, stack limit in the dates: the module expects a settled collection file as one bank line quoting the submission ID (as a real bank's batch credit shows it; documented in docs/payments.md "Runs"), and the operator's statement quoted run/item ids. The −363 days is the business date (2027) against the wall clock (2026) on this stack.
- **F2-16-2** left: matching the payer account to the customer master's own bank details needs a master question; not built.
- **F2-16-3** fixed (UI, `c695ecde`): the page says the relationship was sent to the customer master and the case stays open. The made-up reference is already refused when the master answers.
- **F2-16-4** left: needs the receipt-match threshold shown on the page and a second reconciler in the cast (stack).
- **F2-16-6** stack limit for the relay (no `record_customer_hold` relay on the stack); "Ask to lift it" is the customers fixer's area. Legal override in payments: not built (left).

## Minors

- **F2-08-5** fixed (`d863c2d9`, app.js): an exact value or label typed is chosen and `onPick` runs.
- **F2-08-6** left (cause code picker, names for payee/policy on the claim summary).
- **F2-08-7** left (sharp reserve move prompt).
- **F2-08-8** left: likely product configuration (`ri-life-base@1` has no trauma rate); needs the reinsurance config owner.
- **F2-08-10** left: whether the reinsurer shares premium set off against a claim is a treaty question; not changed.
- **F2-08-12** left (payee names on runs).
- **F2-08-13** left (claim closes on release; round 1 F-11-6 wording kept).
- **MCP notes (SCN2-08)** fixed (`20de6ed4`): `reinsurance_command` lists its commands; `reinsurance_decide` says settlements as well as abandonments.
- **F2-09-3** fixed (`d863c2d9`, `an_income_benefit_stops_at_the_options_period_and_age`).
- **F2-09-8** left (customer's end date and claim reference).
- **F2-09-9** fixed (`d863c2d9`, `a_claim_is_offered_only_to_someone_who_may_work_it`). Left: whether a health assessor may read other lines' work views (claims lead).
- **F2-09-10** left (partition and run typed in the claims console).
- **F2-15-4** stack limit (no payments-to-core relay), as round 1 F-15-3.
- **F2-15-8** left: the module knows roles, not who holds them; the portal would need the directory.
- **F2-16-1** stack limit (relays).
- **F2-16-7** fixed (UI, `c695ecde`): the refund action reads "Cancel it: give the money back to suspense" and is offered only while owed.
- **F2-16-8** left (`/records` dates).

## Stack changes these fixes need (lead)

- `e2e/tests/06-payments.js:533` creates an authority with `01-0001-0000005-00`, which fails the check digits now. Use a valid number, e.g. `01-0001-1234579-00` or `12-3140-0010001-00`, and confirm it with a second payee maintainer (`confirm_authority`) before collecting. Any seeded authority must be confirmed the same way.
- Scenario data typing NZ accounts for authorities must use valid numbers (Payments NZ check digits).
- A refund to a payee needs the core-to-payments `policy_owner` relay (or the stack playing it); without it refunds to a payee are refused (`owner_unknown`).
- With the peer rule, no second claims manager is needed for SCN2-08; adding one would still be realistic.
