# fix-portal

Merged to main on 17 Nov 2026.

Commits:
- f0212b0e phone in local form
- cad3024b staff find
- d4872815 WAF membership limit
- 93199b35 claims console money and finder
- 9339b4f0 portal: the main change

Tests named are in `crates/channels/tests/http.rs` unless another file is given.

## op-a.md

- F-SCN01-1 not mine: fixed earlier by another session (not redone).
- F-SCN01-2 fixed (9339b4f0): the adviser's Requests table has a Withdraw button with a confirm step on awaiting rows. Tests: `a_change_is_shown_in_words_to_whoever_approves_it_and_stays_in_view` (adviser withdraw is not a decline) and the adviser page in the browser check.
- F-SCN01-3 fixed, partly (9339b4f0): both lists say the change in words (`portal.change`), with its choices (`payload`), who prepared it (`prepared_by`) and the adviser's note. Test: `a_change_is_shown_in_words_...`. Left: the new premium for a new frequency, which needs a pricing quote route that is not in the portal.
- F-SCN01-4 fixed (9339b4f0):
  - the customer's own requests keep what she approved or declined (`approved_by`/`declined_by`);
  - the adviser sees "Declined by the customer" or "Carried out";
  - the policy row shows the new frequency and the day it starts, from `BillingChanged`.
  - Tests: `a_change_is_shown_in_words_...` and journeys.rs `a_change_of_frequency_shows_on_the_policy`.
- F-SCN01-5 fixed (9339b4f0): a request declined by someone other than the requester records `declined_by`, and `portal.requestState` says declined or withdrawn. Test: `a_change_is_shown_in_words_...`.
- F-SCN01-6 fixed, partly (9339b4f0):
  - product codes are shown in words on the customer page;
  - the life is shown as "You" or "Life N";
  - request rows show the policy number and not the UUID.
  - Left: names of advisers and providers. Channels holds no names (they are in the customer master or distribution), so this needs a names source.
  - Left: typing the adviser code to appoint one, which needs an adviser directory search.
- F-SCN01-7 fixed (9339b4f0): the Payments stat reads "Due" when anything is outstanding, and a "Pays" stat gives the frequency (customer.html). Covered by the browser check ("$61.84 is due").
- F-SCN01-8 fixed (9339b4f0): pages get the books' business date in a meta tag, `portal.today()` uses it, and `portal.when` and `portal.day` show dates in NZ form. Test: `pages_are_told_the_books_business_date`.
- F-SCN02-1 not mine: fixed earlier by another session.
- F-SCN02-2 fixed (9339b4f0): the tool list is filtered by reach (staff template actions and grants; adviser, scheme admin or customer relationships). Tests: `each_caller_is_offered_the_tools_of_their_sign_in` and `grants_widen_the_tools_offered`.
- F-SCN02-3 fixed (9339b4f0):
  - an integer path value is accepted;
  - "abc" is refused with "a statement number is a whole number";
  - a missing statement gives `no_such_statement` "you have no statement number N...".
  - Test: the commission statement test (statements/2 and /x, plus MCP with `number: 1`).
- F-SCN02-4 left: refusing a key reused for a different request needs a digest of the client's request kept with each remembered key. Commands carry server times and nonces, so a digest of the command would refuse honest retries. This is a change to the channel log's state and to every write route's key, too large to do safely in this pass. Nothing is created on reuse; only the "accepted" wording misleads.
- F-SCN02-5 fixed (9339b4f0): a call to a tool not open to the caller, with arguments that would not do, is told "this tool is not open to you" before anything about its arguments. Test: `an_assistant_is_told_where_its_arguments_go`.
- F-SCN04-1 left: a cast gap (e2e/lib/cast.js, owned by the lead). No role holds `new_business.quote`. There is also no adviser proposal route, which is a product gap for new business.
- F-SCN04-2 left: underwriting.html belongs to the claims/payments/underwriting fixer.
- F-SCN04-3 fixed (9339b4f0):
  - the unengaged-adviser refusal says to ask their advice provider or our distribution team;
  - app.js gives "ask a staff administrator" only on staff pages, and "contact us" elsewhere.
  - Covered by the existing adviser-refusal test text, through `contains`.
- F-SCN04-4 fixed (9339b4f0): `case` given at the top level is told `case goes in the query: {"query": {"case": "..."}}`. Test: `an_assistant_is_told_where_its_arguments_go`. "Ask your manager for the role" when no role carries the action is the cast gap in F-SCN04-1.
- F-SCN03-1 not mine: fixed earlier by another session.
- F-SCN03-2 not mine: fixed earlier by another session.
- F-SCN03-3 left: commission.html belongs to the finance/commission fixer. Searching the module's parties by name needs a names source.
- F-SCN03-4 fixed (9339b4f0): the read-only banner names the licence, its status and since when, and the page says why the adviser cannot send (adviser.html). Covered by the browser check.
- F-SCN03-5 left: commission.html and the commission action list belong to the commission fixer.
- F-SCN03-6 left: the commission and licence history screens belong to the commission fixer.
- F-SCN03-7 fixed (9339b4f0): an unavailable adviser is explained, with "appoint another here or contact us" (customer.html).
- F-SCN03-8 fixed (9339b4f0): request rows carry the policy `number` from the server, so it no longer depends on the current book. Test: `a_change_is_shown_in_words_...` (the number assertion). The "pending" state was the F-SCN03-2 stall, fixed by another session.

## op-c.md

- F-08-1 fixed (9339b4f0):
  - the master's, intake's and group's refusals gain a `title` and `detail` (`passed` in http.rs);
  - app.js reads `error.message`;
  - the MCP result reads every shape.
  - Test: `a_refusal_from_another_service_is_said_in_its_own_words`.
- F-08-2 fixed (f0212b0e): a leading 0 is read as +64, and spaces, dashes, brackets and dots are dropped. Test: crates/customers/src/validate.rs, the phone test. Decided in docs/decisions.md, "What the portal offers and asks of people".
- F-08-3 decided (9339b4f0): lifting a hold needs a person's identity check, so the page says to call and no longer asks for a code. Recorded in docs/decisions.md, same entry.
- F-08-4 fixed (9339b4f0): the claim date field has `max` set to the business date, and a rejected date says to choose a day on or before today (customer.html).
- F-08-5 left: the claims queue and claim view would need the claims module's answer to carry the customer's name and contact. This belongs to the claims fixer.
- F-08-6 fixed (93199b35): the claims console shows amounts with `portal.money`.
- F-08-7 fixed (93199b35): the claim finder shows why the policy search was refused.
- F-08-8 fixed, partly (9339b4f0): the product and cover are in words, and the life reads "You". The names of other lives need a names source.
- F-08-9 fixed (9339b4f0): the access copy now holds `your_details`, `history_of_your_details`, `policies`, `requests`, `devices` and `as_at`. Test: the profile test (the export assertions).
- F-08-10 fixed (9339b4f0): the contact message is cleared on input (customer.html).
- F-10-1 not mine: fixed earlier by another session.
- F-10-2 not mine: fixed earlier by another session.
- F-10-3 fixed (9339b4f0): `device_required` says to do it on the phone or set up this browser under Signing, and a forged signature says to sign again on the phone. Test: `a_device_refusal_says_what_to_do`.
- F-10-4 fixed (9339b4f0): `no_customer_record` "...call us". Test: `a_refusal_from_another_service_is_said_in_its_own_words`.
- F-10-5 fixed (d4872815): a Front Door WAF rate limit on POST /app/v1/memberships, 10 per 5 minutes per client. It is in infra/edge.bicep and the runbook. There is no local test for Front Door rules.
- F-10-6 fixed, partly (9339b4f0): `make_request` lists the kinds carried, and a missing `body` quotes what it should hold. Test: `an_assistant_is_told_the_fields_the_commands_and_the_units`. Left: `lifecycle: "prospect"`. The customer master activates a record when a policy relationship is recorded there (party.rs), and the stack's seeded policy recorded none. That is the cast and the new-business step, not the portal.
- F-13-1 fixed (9339b4f0): staff can reduce cover. `reduce_cover` is a servicing kind carried to the core's ReduceCover, with a staff form. Test: the servicing command test.
- F-13-2 fixed (cad3024b): two-digit years and `-` or `.` separators are read. Test: find.rs unit tests.
- F-13-3 fixed (cad3024b, 9339b4f0): a family name alone gives a hint, and a policy is found by the end of its number. Tests: find.rs unit tests and `staff_reach_a_policy_by_the_number_the_caller_reads_out`.
- F-13-4 fixed (9339b4f0): staff results say "Not the name you typed" and "Not a strong match" (staff.html).
- F-13-5 fixed (9339b4f0): the previous policy is cleared before a lookup, and a number opens in any case. Test: `staff_reach_a_policy_by_the_number_the_caller_reads_out`.
- F-13-6 fixed (9339b4f0): the change is said in words with its choices and who prepared it, and stays in view afterwards. Test: `a_change_is_shown_in_words_...`. The new amount is left, as in F-SCN01-3.
- F-13-7 fixed, partly (9339b4f0): staff queue rows show policy numbers, changes are in words, and dates are in NZ form. Left: the names of staff and parties, which need a names source.
- F-13-8 fixed (9339b4f0): a number search lists a policy closed to the searcher only as `{closed: true}`. Test: `staff_reach_a_policy_by_the_number_the_caller_reads_out`.
- F-13-9 fixed (9339b4f0): staff get no customer-realm tools. Test: `each_caller_is_offered_the_tools_of_their_sign_in`.
- F-06-1 left: a cast gap. No template in e2e/lib/cast.js holds documents.*, and the lead owns the cast.
- F-06-2 left: a cast gap. The records officer holds no customers.find or records action (cast).
- F-06-3 not a defect: this is a test-stack limit (no documents service), and the refusal is already clean.

## Cross-cutting items from other reports

- F-SCN16-9 fixed (9339b4f0): by reach, as in F-SCN02-2.
- F-SCN07-1 fixed (9339b4f0): the fields come from each module's list. Test: `an_assistant_is_told_the_fields_...`.
- F-SCN16-4 fixed (9339b4f0): same change and test.
- F-SCN22-5 fixed (9339b4f0): same change and test.
- F-SCN16-3 fixed (9339b4f0): cents are given in the descriptions and the initialize instructions. Test: `an_assistant_is_told_the_fields_...`.
- F-23-3 fixed (9339b4f0): readOnly, destructive and idempotent hints. Test: `each_caller_is_offered_...` (annotations).
- F-SCN05-6 fixed (9339b4f0): as F-08-1.
- F-SCN07-2 fixed (9339b4f0): as F-08-1.
- F-SCN17-3 fixed (9339b4f0): as F-SCN01-8.
- F-15-5 fixed (9339b4f0): as F-SCN01-8.
- F-20-6 fixed (9339b4f0): as F-SCN01-8.
- The F-13 date findings fixed (9339b4f0): as F-SCN01-8.
