# fix2-people: round 2 fixes for op2-f (SCN2-14, SCN2-23, SCN2-24) and op2-d (SCN2-12, SCN2-13)

Merged to main on 17 Nov 2026.

Commits:
- 51241658 Customer master: a merge never shows one person another's record; merges need a reason and a second look after a dismissal; facts dated by the books
- 43636e3c Documents: a shred under a legal hold is refused as the hold, and an unknown person is told there is nothing to shred
- 47543bfc Staff access: configuration and profile changes wait for a real second person; templates never join duties kept apart; roles for a period; managers named; customer master work from the console

Tests run: ic-customers (all), ic-channels `staff`, `customers` and `http` (91 passed, 1 ignored as before), ic-documents `lifecycle`. fmt and clippy (`-D warnings`, all targets) are clean for ic-channels, ic-documents and ic-customers. `tools/check_test_docs.py`: 2108 tests, 0 without a documented purpose. I did not measure coverage, because the disk is shared and nearly full. Every changed branch has a test, except the UI (see the note at the end).

## SCN2-24 staff access administration (op2-f)

- F2-24-1 (blocker): fixed (47543bfc). Configuration commands and `set_profile` are now proposed (`ChangeProposed`, `chg-N`). They are applied only when a different person who holds the approving action approves them. Neither the proposer nor the subject can approve. An `approver` the proposer names restricts who may approve; it is not an approval. Tests: `staff::configuration_waits_for_its_second_person`, `http::the_staff_console_asks_the_four_questions`.
- F2-24-2: fixed (47543bfc). `assign_template`, `employ` and a role grant refuse (`separation_of_duties`) to join a SEPARATE_ACTIONS pair across templates. One template that carries both was itself approved by two people. Test: `staff::templates_never_join_duties_kept_apart`.
- F2-24-3: fixed (47543bfc). New grant kind `role { template }`, at most 31 days by default (settable). It needs an approver and must not join duties. Test: `staff::a_role_is_granted_for_a_period`.
- F2-24-4: partly fixed (47543bfc). The `staff_access` tool describes every command and its fields (`PEOPLE_COMMANDS`). A command the console does not understand gets a 400 that lists the commands. Left: times are still epoch seconds and limits are still cents in the command; converting them needs a decision on the console's input units. Test: `http::the_staff_console_asks_the_four_questions`.
- F2-24-5: partly fixed (47543bfc). `employ` refuses a manager who is not current staff (`bad_manager`). Left: binding the party to a known sign-in and putting employ itself behind a second person. Both need the identity mapping flow and a seeding decision (the first administrator bootstraps alone). Test: `staff::managers_are_named_and_someone_always_recertifies`.
- F2-24-6: fixed (47543bfc). A new `set_manager` command, and an administrator recertifies anyone with no manager. The refusal names the manager or points to `set_manager`. Test: `staff::managers_are_named_and_someone_always_recertifies`. Decided: docs/decisions.md "Staff access and the customer master".
- F2-24-7: fixed (47543bfc). `end_connection` needs a reason (`no_reason`). Removals are logged in `connections_ended` and shown on the admin view. Test: `staff::personal_connections_and_flagged_attempts`.
- F2-24-8: left. A fuller "Access administration" screen (pending changes, connections, roles) and hiding the tab from people without access are UI work. The data is now in `/staff/v1/admin` (`pending_changes`, `changes_i_may_approve`, `decided_changes`, `connections`, `connections_ended`).
- F2-24-9: fixed (47543bfc). An access approver (`staff.approve_grant`) may also remove a connection, never one of their own. Test: `staff::personal_connections_and_flagged_attempts`. Decided: docs/decisions.md.
- F2-24-10: left. These are smaller wording and screen items. They need UI work beyond this round.

## SCN2-23 attorneys, capacity, addresses (op2-f)

- F2-23-1: fixed (47543bfc). Customer records now splits an address typed with commas or new lines into its lines. This is a UI-only change, and the UI has no JS test harness.
- F2-23-2 (blocker): fixed (47543bfc). Customer records has an "Add a representative" action. The attorney is picked from people, not typed. The basis (enduring power of attorney, court order, written authority) and the scope are lists. The relationship is recorded on the attorney's record, pointing at the open person, which is the direction the master expects. UI-only, so no JS test.
- F2-23-3: left. Frontline checking a caller and opening the policy needs a frontline template and route decision, which is outside this area's code.
- F2-23-4: left. "D for M" on a complaint and checking its representative belong to the complaints module (another fixer's area).
- F2-23-5: left. Adjustments split at commas and timers that ignore the marker belong to complaints and markers.
- F2-23-6 (blocker): fixed (47543bfc). A new action `customers.settle_capacity`. The desk command goes to the master as role `legal`, purpose `capacity`, and Customer records shows "Settle capacity" to holders. Tests: `customers::the_master_is_asked_for_staff_work_in_the_role_it_needs`, `http::staff_work_in_the_customer_master`. Needs a stack change (below).
- F2-23-7: left. The attorney's own view of the people she acts for and their complaint outcomes needs a portal route and a relationship-scoped read. That is a product decision on what an attorney may see.
- F2-23-8: stack limit. No letter is issued on the stack, so there is no delivery record to read. The records display of dates and times was tidied (47543bfc).
- F2-23-9: fixed (51241658, 47543bfc). The master dates facts by the books' date (`x-ic-business-date`, or the later of the books and the calendar), and the console passes the books' date. Tests: `api::facts_are_dated_by_the_books_where_the_caller_says`, `customers::the_master_is_asked_for_staff_work_in_the_role_it_needs`.
- F2-23-10: left. Showing names and wording for marker references and codes, and adding "speak to the attorney", is markers and complaints UI work.

## SCN2-14 retention and erasure (op2-f)

- F2-14-1: stack limit. No document is issued for a stack customer. The refusal is fixed so a shred under a hold is refused as the hold, and an unknown person is told there is nothing to shred (43636e3c). Test: `lifecycle` hold-then-shred.
- F2-14-2: fixed (47543bfc). The desk refuses `exit` (409 `cover_in_force`) while the person has a policy that has not ended, lists the policies, and gives the seven-year rule. Test: `http::staff_work_in_the_customer_master`. Left: issued policies still leave no relationship in the master. That is policy-admin to master integration, for another area.
- F2-14-3: left. A "what we keep, until when, why" view and erasure on request need a retention schedule across modules. This is a product and design item.
- F2-14-4: left. Recording who placed a legal hold and when is a documents state change, and it overlaps fix2-documents. Not done here, to avoid conflicting edits.
- F2-14-5: fixed (47543bfc). A misspelt documents command is answered with the commands this person may send. Test: `http::the_documents_console`.

## SCN2-12 privacy and access (op2-d)

- F2-12-1: left. Needs a privacy-officer report across modules.
- F2-12-2: left. "Who looked at my file" for the customer needs the access log exposed per subject on the portal. That is a product decision on what is shown.
- F2-12-3: partly fixed (51241658). The master's dates come from the books. Left: re-rating on a corrected fact belongs to pricing.
- F2-12-4: left. A minor screen item outside the master's code.
- F2-12-5: partly fixed (47543bfc). Records shows "always" for open-ended facts, NZ-local recorded times and clearer "true from" wording. UI only.
- F2-12-6: fixed (47543bfc). A staff tool that is not open to the caller names the actions that open it. Test: `http::an_assistant_is_told_where_its_arguments_go`.
- F2-12-7: stack limit (as reported).

## SCN2-13 merge and unmerge (op2-d)

- F2-13-1 (blocker, privacy): fixed (51241658). A sign-in whose party was merged gets 409 `record_combined` on any self-service read of itself or the survivor. A self-access export returns only the person's own stream. Test: `api::a_merge_never_shows_one_sign_in_another_persons_record`. Decided: docs/decisions.md (self-service never follows a merge).
- F2-13-2: not a defect, by design. The sign-in is not moved to the survivor until the person re-verifies, because moving it automatically is the breach in F2-13-1. Recorded in docs/decisions.md.
- F2-13-3: left. A customer-facing unmerge request needs a service flow.
- F2-13-4: fixed (47543bfc). The duplicates list shows names, birth dates and contacts, with buttons to open each record. UI only.
- F2-13-5: fixed (51241658, 47543bfc). A dismissal of an open duplicate review needs a reason, and the reason is stored. The same person cannot merge a pair they judged apart (`judged_apart`, which names the date and the reason). Merges need a reason. Test: `api::a_pair_judged_apart_is_not_merged_by_whoever_judged_it`.
- F2-13-6: fixed (47543bfc). The merge form defaults the survivor to the older record, asks for a reason, and confirms. UI only.
- F2-13-7: fixed (47543bfc). An add by someone without view is acknowledged ("Added ...") rather than shown as a failure. UI only.
- F2-13-8: fixed (47543bfc). A blank search finds nobody instead of everybody. UI only.

## Stack changes needed (e2e/ not edited)

- ic-portal's identity on AUD.customers needs the `legal` role (e2e/stack/stack.js around line 46) so it can settle capacity.
- The seeded legal staff template needs `customers.settle_capacity` (and `customers.find` and `customers.view` to reach the record).
- A second seeded administrator (such as admin2) needs `staff.approve_configuration`, and `staff.approve_profile_uplift` for profile changes. Otherwise seeded configuration changes stay pending.
- Seeded merges must send `reason`, and dismissals of open duplicate reviews must send `{ "reason": ... }`.
- Seeded configuration commands now return `ChangeProposed` and need an `approve_change` by the second person.

## Note

The UI changes (records.html, app.js) have no automated tests, because the repo has no JS test harness. They were checked by reading only, not in a browser.

Counts: 40 findings. 19 fixed, 4 partly fixed, 3 stack limit, 1 not a defect, 13 left.
