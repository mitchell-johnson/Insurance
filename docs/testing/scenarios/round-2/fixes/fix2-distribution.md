# fix2-distribution: round 2 fixes

Merged to main on 17 Nov 2026.

## op2-e: SCN2-18 (auditor)
- F2-18-1: fixed (0d534bde; `http.rs::the_payments_console`, `payments::tests::a_reader_sees_accounts_masked`). Readers get account numbers masked to the last four digits on every payments answer, and overseas addresses withheld.
- F2-18-2: fixed (0d534bde; `http.rs::each_caller_is_offered_the_tools_of_their_sign_in`, `http.rs::the_commission_console`). A new `Reach::Acting`: the module write tools, the reinsurance sign/withdraw, the group admin acts and the finance dry run are no longer listed to readers. The commission command and reinsurance decide routes refuse a reader first.
- F2-18-3: fixed (0d534bde; browser `check.js` visit `/finance sub-aud`, which was not run here because Playwright is not installed). On the finance page, a reader no longer sees the forms that act. On payments, the manual-match form is hidden from anyone who is not a reconciler.
- F2-18-4: left. Needs new read models: payee change history without account details, lists of closed runs with the approver, and a journal list. Each is a new question in payments or finance.
- F2-18-5: fixed in part (0d534bde; `http.rs::the_finance_console`). The dashboard defaults to the books' date. The commission and finance MCP descriptions now list each question's fields and the period format. Left: the unlabelled `aged_minor` buckets and the Unix-seconds fields in `staff_me`.
- F2-18-6: fixed (0d534bde; `reinsurance tests/driver.rs::the_hosted_module_answers_its_questions`, `http.rs::reinsurance_reads_a_lifes_accumulation`). An unknown treaty, or a reinsurer not on its panel, now gives "not found". The exposure answer lists each reinsurer's treaties.
- F2-18-7: fixed (0d534bde; `http.rs::reinsurance_reads_a_lifes_accumulation`).
- F2-18-8: left. Needs a grant JSON example in the `staff_access` description and a `withdrawn` grant status distinct from declined (staff.rs). Not done.
- F2-18-9: fixed for commission and reinsurance: their refusals now name who acts (0d534bde). Left for payments and finance.
- F2-18-10: fixed in part (0d534bde). The commission and reinsurance descriptions now state cents. The finance ledger counts in millionths of a cent, so I did not add a units sentence there without checking each field.

## op2-e: SCN2-20 (adviser changes provider)
- F2-20-1: fixed (94a2fd3f; `journeys.rs::access_ends_with_the_engagement`).
- F2-20-2: fixed (94a2fd3f; same test). An ended engagement is refused with `engagement_ended`. One that has not begun yet is still accepted (a broker takeover is appointed ahead; `an_incoming_broker_sees_the_scheme_from_the_appointment_day`).
- F2-20-3: fixed (7566f0f1; `units.rs::engagements_do_not_overlap_and_one_entered_in_error_is_withdrawn`). Same adviser and same provider may not overlap. Different providers may (decided, docs/decisions.md).
- F2-20-4: fixed (7566f0f1; same test). Ending an engagement on its start day withdraws it.
- F2-20-5: left. Staff UI review work: an engagements question with a picker on `/commission`, accreditations picked from product codes, and an adviser finder on `/customer`.
- F2-20-6: fixed (94a2fd3f; `queries.rs::a_provider_and_its_group_see_the_book_under_them`). `former` advisers now appear on the practice page.
- F2-20-7: decided (docs/decisions.md, "Readers, advisers changing provider and scheme renewals"). Bank details are controlled in payments. Still open: checking the name-check and step-up references against payments, and a command to remove an account.
- F2-20-8: fixed (7566f0f1; `config.rs::an_engagement_without_a_book_is_a_task_until_it_has_one`).
- F2-20-9: left. Needs an "Add a payment instruction" form on `/commission`, and a warning when re-registering a payee would replace its agreement reference.
- F2-20-10: fixed in part (068674bd). The confirmation now names the engagement. This is a UI-only change, not covered by a test that ran. Left: the statement number in the confirmation, and the period in the statement header.
- F2-20-11: fixed in part (94a2fd3f; `journeys.rs::a_lost_licence_makes_the_adviser_read_only`). The licence wording now says whether it is the adviser's licence or the provider's. Left: names instead of references, which needs names from the customer master.
- F2-20-12: left. The adviser's own page should explain an empty book between engagements.

## op2-e: SCN2-22 (scheme renewal)
- F2-22-1: decided as left (docs/decisions.md). Per-scheme rates and salary multiples cut across group terms, the core premium and cover, and the product engine. This needs a design pass.
- F2-22-2: fixed (716c096a; `year.rs::terms_agreed_inside_the_window_open_the_renewal`, `year.rs::agreed_terms_take_effect_on_the_renewal_date`). Left: showing the renewal date and terms on `/employer` and `/group`.
- F2-22-3: fixed (716c096a; `driver.rs::a_refused_delegated_authority_is_raised_until_the_request_is_taken`). Refusals and unlinked members are now raised as `underwriting_not_reached`. Stack: the likely root cause is that no group underwriting rule set is seeded (only `individual@1`). The lead should seed one for group-life.
- F2-22-4: left. The screens need to label which limit applies: the $500k cap from the retention/`RequireUnderwrittenExcess` versus the product's $750k. The e2e oracle should then use the same cap.
- F2-22-5: left. Employer invoices should show what the outstanding amount covers.
- F2-22-6: left. The file preview needs old and new salary, the above-automatic flag, and a file picker.
- F2-22-7: not a defect. The employer sees only that cover is pending underwriting, never health details or loadings. The finding itself accepts this as intended.
- F2-22-8: left. Staff UI review on `/group`: a scheme list, a terms view, and money shown in dollars.

## op2-b: SCN2-21 (commission)
- F2-21-1: left. Needs a cost preview before a change, the adviser's statement list, and statement lines in words.
- F2-21-2: left. Clawback exposure per payee in dollars, attribution by cause, and pickers.
- F2-21-3: left. Needs a clawback-dispute path and a compliance report by adviser and cause.
- F2-21-4: left. Accrual and payment holds should show to staff and the adviser in words, with a form for the accrual hold.
- F2-21-5: left. Adviser portal and MCP wording: product names, the business date, and which tool wants the id and which the number.
- F2-21-6: left. The statement says payment is held even though the account is only within its cooling-off, and the adviser cannot see the reason.

## Stack changes these fixes need
- Seed a group underwriting rule set for `group-life` (F2-22-3), otherwise the new exception fires for every executive.
- A seed or journey that appoints an adviser under an engagement that has already ended is now refused (`engagement_ended`).
- A seed that starts an engagement with no book now leaves an open `needs_commission_book` task.
- A seed that starts a second overlapping engagement with the same provider is now refused.
