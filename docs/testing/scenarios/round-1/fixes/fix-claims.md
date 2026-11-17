# fix-claims

Merged to main on 17 Nov 2026.

## op-d.md: SCN-11 (health claim)
- F-11-1: fixed (A; channels tests/http.rs `a_claims_payee_comes_from_the_payments_register`, `the_claims_console`; payments tests/intake.rs `a_claim_payment_goes_to_the_payee_claims_picked_from_the_register`). verify_payee now names a payee from the payments register (picker), and the portal refuses one that is closed, unconfirmed, unscreened, or of the wrong kind.
- F-11-2: fixed (B; claims suite `intake::a_health_invoice_is_recorded_on_the_claim`). New `record_invoice` command on the claims console. The cover picker sets the claim type (A).
- F-11-3: fixed (B; same test). A wrong invoice is put right with `record_invoice` before any decision. The category must be one the rules or product pay, and the refusal lists them. Notify itself still takes any category (changing that would touch the sim/finance crates' fixtures).
- F-11-4: left. The cover check needs the core's CoverFacts (in force, paid to, definitions, exclusions, limits) shown on the work view and ticked. Needs a claims WorkView field fed from the core's cover facts, plus a checklist in claims.html.
- F-11-5: fixed (B; claims suite `intake::a_claim_is_offered_only_to_someone_who_may_work_it`). The authority refusal names the person's limit, the claim's amount and who can take it. Still open: a non-lead cannot hand a claim on. That is by design: a queue lead or claims manager assigns.
- F-11-6: fixed (B; browser check.js, claims.html). "paid" now reads "paid: released to payments, which settles it at the bank". The customer page shows words (see F-24-5).
- F-11-7: fixed (B; claims.html). The money section explains a payable below the admitted amount (premium owed or offset).
- F-11-8: fixed (B; channels tests/http.rs `staff_find_customers_and_policies_from_what_they_know`). A search on one name says "add the given name or the date of birth".
- F-11-9: partly fixed. Amounts read in dollars (A). The cause-code refusal lists the codes (B; claims suite `intake::the_owner_records_cause_and_cover_check`). Reason-code refusals list the fitting codes (A; `decisions::a_wrong_reason_code_is_answered_with_the_codes_that_fit`). Left: life names in the Life picker, policy number in place of the reference, evidence kind/from lists, and an optional cost. These are claims.html work that needs the core's party names on the work view.
- F-11-10: fixed (A; claims.html opens the new claim after any notify).

## op-d.md: SCN-12 (death claim)
- F-12-1: left for the lead (configuration). No claims rules ship in the repo; only e2e/stack/seed.js seeds them. The seed needs a reason code allowed for `funeral_advance` (e.g. `"R-FUNERAL": ["funeral_advance"]`) and `non_disclosure_reasons: ["R-NDISC"]`. The refusal now says the claims rules owner adds one (A; claims suite `decisions::a_wrong_reason_code_is_answered_with_the_codes_that_fit`).
- F-12-2: fixed (B; claims suite `views::the_policy_status_shows_an_adviser_each_claim_without_its_cause`). A policy's claims asked by staff leave out claims they are related to. Left for the lead: the staff policy view part needs a personal connection for n7 to cust-p60 in the e2e entitlements seed.
- F-12-3: fixed (A; payments tests/controls.rs `an_estate_account_waits_on_legals_worklist`). Legal gets a notice for each payee awaiting its approval. The payment is held until then (it already was).
- F-12-4: decided (docs/decisions.md "A claim payment above the item limit (17 Nov 2026)"). Treasury releases an over-item-limit hold; no split, no higher limit. Implemented (A; payments tests/runs.rs `every_hold_is_named_and_treasury_lets_a_large_claim_through`).
- F-12-5: partly fixed (B; claims suite `intake::triage_routes_by_skill_and_ranks_by_urgency`). Added `family_member` notifier. Left: the caller's name. That needs a Notification field, which every crate's fixtures construct (finance, sim, reinsurance, payments-service), so it is for a change that can build those crates.
- F-12-6: fixed (A; claims.html signature banner names who signed, the proposer included).

## op-d.md: SCN-24 (non-disclosure)
- F-24-1: fixed (B; channels tests/http.rs `a_claim_is_referred_to_underwriting_and_hears_the_outcome`, `a_review_outcome_is_told_to_the_claim_it_was_opened_for`). Underwriting's review is opened by the claims referral. The outcome, once a second senior signs, is told to the claim. The remedy can vary with an exclusion from the review's start (underwriting.html).
- F-24-2: fixed (B; same tests). The claims console "Refer to underwriting: possible non-disclosure" opens the review and the claim's task.
- F-24-3: fixed (A; claims suite `decisions::a_non_disclosure_decline_stands_on_underwritings_remedy`).
- F-24-4: fixed (B; channels tests/http.rs 503 test asserts the words). "Not connected" says the platform team connects it. That it is not connected in the stack is a test-stack limit.
- F-24-5: fixed (B; customer.html status words; browser check.js).

## op-d.md: SCN-15 (payments)
- F-15-1: fixed (A; payments tests/runs.rs `every_hold_is_named_and_treasury_lets_a_large_claim_through`). Approvers release duplicates and treasury releases over-limit holds, from a Release button per held item. Not a defect: twin items are each decided on their own, by design.
- F-15-2: fixed in part (P; payments tests/runs.rs `sending_again_says_why_nothing_goes`). The refusal names the rejected items and how they are paid again. Not a defect: a rejected item's reference is spent and claims is told (`PaymentReturned`), by design. The reconciliation exceptions list is for statement lines, not rejections.
- F-15-3: not a defect (test-stack limit). The core's bank_returned already tells claims PaymentReturned on a settled return. The stack runs no payments outbox relay.
- F-15-4: fixed in part. Held items are named with payee, dollars, hold and who clears it (A). The approved run no longer offers Approve, and a repeat says "already approved" (A). Left: matching run type to payment kind (provider vs lump sum). That needs a decision on whether a claims provider payment may go in a lump-sum claims run: claims releases its own runs, and the accounting by run type is shared with claims.
- F-15-5: left to the app.js business-date fix (F-SCN17-3, another fixer). portal.today uses the browser clock.
- F-15-6: fixed (A; payments.html run total, acknowledged column, open-runs label).
- F-15-7: fixed (B; channels tests/http.rs `a_payments_question_that_finds_nothing_says_what`). Payee pickers (find_payees) on "A payee" and on add item/refund "Payee or authority". Left: the register-payee party is still typed. A hospital is not in the customer master.
- F-15-8: fixed (A; payments.html offers resend only on an approved run with pending items).

## op-a.md
- F-SCN04-2: not reproducible on this base. underwriting.html already wires pickers for case, proposal, life, person, reason and exclusion libraries. B adds the exclusion picker on the remedy.

## op-e.md: SCN-16 (payments)
- F-SCN16-1: fixed (A; payments tests/runs.rs `every_hold_is_named_and_treasury_lets_a_large_claim_through`).
- F-SCN16-2: fixed (A; payments tests/runs.rs `an_approval_refusal_says_where_the_run_stands`).
- F-SCN16-5: fixed (P; payments tests/intake.rs `a_large_admitted_claim_is_flagged_and_forecast`). The forecast lists each paying account with what it will hold, what the runs need and the shortfall. Readiness drills: test-stack limit.
- F-SCN16-6: fixed (B; channels tests/http.rs `the_staff_console_asks_the_four_questions`). A "what" refusal names the action needed.
- F-SCN16-7: fixed (B; channels tests/staff.rs `a_grant_never_joins_duties_kept_apart`). A one-off grant that would join maker/approver or approver/payee maintainer is refused when asked. Revoking a request says to withdraw it with decline_grant, which the requester may already do.
- F-SCN16-8: fixed (B; channels tests/http.rs `a_payments_question_that_finds_nothing_says_what`, `the_payments_console`).
- F-SCN16-10: fixed (A; payments.html money in NZ$, holds in words, B: payee pickers).

## Coordinator item: nine claims journeys failing on main (E2E-071, 078, 079, 080, 081, 082, 094, 107, 108)
- Not a product defect: the tests must follow a deliberate change. 878e9751 made claims enforce the trauma cover's 14-day survival period. To serve it, that commit moved the e2e claim event from opening-3 to opening-14. t.policy incepts every policy at opening-14, so every claim's event now falls on the day its cover began. That has three effects:
  (a) The early-claim rule (seed `early_claim_days: 7`; event <= cover start + 7) opens a restricted investigation on every claim, so every payment is `payment_held`: 079, 080, 081, 082, 107, 108.
  (b) On its own due date the first premium is not yet overdue. Standing on the event date is "paid", not "in_grace", so nothing is deducted and payable is the full 100000.00: 078.
  (c) `early` (opening-12) is two days after the start but only 12 days before opening, inside the 14-day survival period: `survival_period` on 094.
  071 most likely fails for the same reason (its claim opens under an early-claim case). Confirm with ONLY=E2E-071 after the change.
- E2E change for the lead, in e2e/tests/03-claims.js. Start these policies 25 days before opening and keep the event eleven days after the cover began, as it was before 878e9751:
  - Replace lines 10-13 with:
    // Policies claimed on began 25 days before opening. A claim's event eleven days after the cover began is past the
    // 7-day early-claim window, has served a trauma cover's 14-day survival period by the opening day, and falls while
    // the first premium is in grace, so that premium is taken from what is payable.
    const began = (t) => Journey.before(t.stack.opening, 25);
    const event = (t) => Journey.before(t.stack.opening, 14);  // eleven days after the cover began
    const early = (t) => Journey.before(t.stack.opening, 23);  // two days after the cover began
  - In notified() (line 20): `const policy = await t.policy(kind, party, { inception: began(t) });`
  - In E2E-071 (line 74) and at line 497: `const policy = await t.policy("term_life", "cust-p18", { inception: began(t) });`
  Both term_life and health_plan have 30 grace days, so the first premium is in grace on the event date (11 days) and on the opening day (25 days). Neither lapses before the run's closes.
