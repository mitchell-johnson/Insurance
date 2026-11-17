# fix-back-office

Merged to main on 17 Nov 2026.
Commits: e180a072 (complaints), 074fe954 (finance), 00f7a41b (pricing), 5b0417b6 (distribution), 476ed6ec (portal and consoles).

## Operations (SCN-23)
- F-23-1: product part fixed (476ed6ec, `customer_master_refusals_say_whose_fault_and_what_is_missing`). A 401 or 403 from the customer master now says the portal's own identity lacks the role, naming it, and that the person's sign-in is fine. Left: the stack's service identity roles belong to the lead.
- F-23-2: fixed (476ed6ec, `customer_master_refusals_say_whose_fault_and_what_is_missing`). Rewrap needs `current_key_id`; reshred needs `since` as a date; a 422 says a field is missing.
- F-23-3: fixed (476ed6ec, `operations_tools_say_what_they_do_and_answer_in_readable_terms`). verify_customer is read-only; reshred is destructive.
- F-23-4: fixed (476ed6ec, `operations_tools_say_what_they_do_and_answer_in_readable_terms`). /ops shows last night's close and the search index, with Check again.
- F-23-5: fixed (476ed6ec, same test). `heard_at` is given in RFC 3339.
- F-23-6: partly fixed (476ed6ec, same test). A bad protocol version lists the versions supported. Left: `resource_metadata` and the public URL are stack configuration.

## Complaints (SCN-21)
- F-21-1: fixed (476ed6ec, browser path; decided in docs/decisions.md). Intake refuses a complainant nobody picked.
- F-21-2: left. Storing the complainant's own words needs the documents service's inbound vault storage.
- F-21-3: fixed (476ed6ec). "Put it right on the spot" offers outcome, reasons, cause and goodwill, with the limit from settings (`a_bundle_is_reviewed_by_a_colleague_and_conduct_counts_stay_with_conduct` covers the settings question).
- F-21-4: fixed (e180a072, `a_vulnerability_marker_takes_the_complaint_from_an_untrained_handler`).
- F-21-5: left. The hardship specialist's template (cast) belongs to the lead, and there is no staff vulnerability feed.
- F-21-6: fixed (e180a072, `a_running_period_counts_nothing_late_before_it_is_due`).
- F-21-7: fixed (476ed6ec, `a_bundle_is_reviewed_by_a_colleague_and_conduct_counts_stay_with_conduct`; decided in docs/decisions.md).
- F-21-8: decided (docs/decisions.md, "A complaint is deadlocked by hand only after a decision or after its time"). Fixed in e180a072 (timers.rs deadlock test). E2E-049 and E2E-050 need changing; see the report.
- F-21-9: fixed (476ed6ec). Colleague pickers by action, cause selects, referral actions filtered by the file's status, authority shown as "level N".
- F-21-10: partly fixed (476ed6ec). Claims are linked at intake and listed in the file. Left: opening the claim from the complaint.
- F-21-11: fixed (e180a072, `a_goodwill_payment_over_the_limit_says_the_limit_and_who_may_authorise_it`).
- F-21-12: fixed (476ed6ec, `a_bundle_is_reviewed_by_a_colleague_and_conduct_counts_stay_with_conduct`).
- F-21-13: not a defect. Complaint timers run on calendar days from the clock by design; the gap from the business date is a stack artefact. Staff pages now take their default dates from the books.

## Held commission and distribution (SCN-20)
- F-20-1: not a defect (product). The task text "no book open to new business (no_book)" is accurate. Left: the stack has no books or schedules (the lead).
- F-20-2: fixed (476ed6ec, 5b0417b6 `compliance_holds_payments_and_refusals_say_who_may`). Parties are picked from the structure's new `parties` question. A new engagement and a relation each have a form, and the engagement's reference is generated.
- F-20-3: fixed (5b0417b6, `compliance_holds_payments_and_refusals_say_who_may`; decided in docs/decisions.md). Compliance may use `payee.hold`. A non-payee refusal says who the party is paid through, or to register it. Refusals say in words who may.
- F-20-4: left. The adviser portal is not fed a payee's payment hold. With F-20-3, compliance no longer has to suspend a licence to hold payments, so the misleading read-only banner no longer arises.
- F-20-5: fixed (476ed6ec, `a_refused_amount_says_what_is_held_and_committed`). The daily control says "Nothing is held"; documents on file are offered. Reason codes stay as text, since no controlled list exists.
- F-20-6: fixed (476ed6ec, `staff_pages_take_their_dates_from_the_books`). The console draws its fields after sign-in, from the business date.

## Commission console (SCN-03)
- F-SCN03-3: fixed (5b0417b6 and 476ed6ec, `compliance_holds_payments_and_refusals_say_who_may`). The picker lists every party in the structure, with its kind and licence. Left: searching by name, because the module holds no names.
- F-SCN03-4: fixed (476ed6ec). The adviser banner names the licence, its state and since when. A disabled Send says why.
- F-SCN03-5: fixed (5b0417b6, `compliance_holds_payments_and_refusals_say_who_may`; decided in docs/decisions.md). Actions the person cannot do are listed apart, naming who does them. The refusal says "that is done by compliance or the distribution administrator".
- F-SCN03-6: partly fixed (476ed6ec). "Done" now repeats what was recorded. Left: who recorded each licence state is in the module's decision log, not in the licence record. The empty POLICIES column comes from F-20-1 (no servicing attribution in the stack).

## Finance (SCN-17)
- F-SCN17-1: fixed (476ed6ec). Journal rows are checked before sending: at most 3 parts, the fund not numeric, debits equal credits, the value date inside the period.
- F-SCN17-2: fixed (074fe954, `an_unbalanced_journal_is_refused_in_dollars`).
- F-SCN17-3: fixed (476ed6ec, `staff_pages_take_their_dates_from_the_books`).
- F-SCN17-4: partly fixed (074fe954, `aged_suspense_says_how_far_the_books_are_fed`). Aged suspense says how far the books are fed. Left: the receipt appears once the close feeds the day, which does not run in the stack.
- F-SCN17-5: fixed (476ed6ec). A refused sign-off or close lists the blocking checks and the sign-offs still needed.
- F-SCN17-6: fixed (476ed6ec). Sign-off buttons are enabled only when the shown period needs them. Role buttons now start disabled until the roles are known (coordinator follow-up for E2E-149 and E2E-162); the pricing and payments consoles got the same treatment.
- F-SCN17-7: left. It may be the feed or the stack: conduct figures come from fed days.
- F-SCN17-8: fixed (074fe954, mart test text: "policy store 0 has not closed that day").

## Unit pricing (SCN-18)
- F-SCN18-1: fixed (476ed6ec). pickMany has `add`.
- F-SCN18-2: fixed (476ed6ec). A blocked or held price gets a message for its status.
- F-SCN18-3: fixed (00f7a41b, `check_figures_are_in_units_percent_and_dollars`).
- F-SCN18-4: fixed (476ed6ec). The customer page has "Your investments" (holdings and orders waiting for a price).
- F-SCN18-5: fixed (00f7a41b, tests/errors.rs message; 476ed6ec banner). Approve is withheld while an error holds the fund.
- F-SCN18-6: fixed (00f7a41b, tests/host.rs `next`; 476ed6ec). The pricing date is the fund's next date, funds show their names, and an empty overdue list says so.
- F-SCN18-7: fixed (476ed6ec). The wrong-price picker loads every fund's prices from the last year.

## Reinsurance (SCN-22)
- F-SCN22-1: fixed (476ed6ec, `reinsurance_reads_a_lifes_accumulation`). The question `retained` and the Lives view. Left: reinsurance operators need `customers.find` in their template to pick the person (the lead).
- F-SCN22-2: partly fixed (476ed6ec, `staff_pages_take_their_dates_from_the_books`). The month-end comes from the business date. Left: the close workflow does not run in the stack.
- F-SCN22-3: fixed (476ed6ec, tests/countersign.rs duplicate assert). A second identical prepare is refused, naming the preparer.
- F-SCN22-4: fixed (476ed6ec). Withdraw says what happened, and a refused signature refreshes the list.
- F-SCN22-5: partly fixed (476ed6ec, `reinsurance_reads_a_lifes_accumulation` for the MCP fields). claim_fact_not_taken rows open the claim-fact tab, and the MCP question lists each question's fields. Left: internal words in exception details.

## Persona suite (asked by the coordinator)
- E2E-034, E2E-039, E2E-044: product defect, fixed (e180a072, `a_handlers_own_complaints_never_fall_off_the_queue`). The queue was cut at 200, and a decided complaint with nothing due fell off the end. The portal now sends the handler, and their own complaints are always listed.
- E2E-056: product defect in complaints.html, fixed (476ed6ec). The register's default dates were set after sign-in loaded, which overwrote what the test had just typed. Defaults now fill only empty fields.
