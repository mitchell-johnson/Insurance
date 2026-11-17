# fix-group: op-b findings

Merged to main on 17 Nov 2026.
Commits: `53a5cc38` (crates/group), `a1d86cb5` (crates/channels, UI, docs).

- F-SCN05-1: fixed (a1d86cb5; `an_administrator_changes_a_member_by_hand`, journeys `scheme_members_and_the_employer`). The employer page lists the administrators and shows "Your reference". A colleague can be named only once they have signed in; otherwise the refusal is 422 `unknown_person`, with words saying what to do. Decided in docs/decisions.md, "Employer changes to members, pools and second administrators".
- F-SCN05-2: fixed (53a5cc38 + a1d86cb5; intake `changes_an_administrator_enters_are_checked_and_approved_like_a_file`). The file answer now lists each change row by row, and employer.html and group.html show it as a "What it changes" table.
- F-SCN05-3: fixed (53a5cc38; intake `a_look_alike_joiner_is_put_to_group_administration`). New `joiner_held_for_review` exception for staff, also listed in the group console.
- F-SCN05-4: fixed (a1d86cb5; http `the_group_console`). Possible-match candidates are read from the customer master by name and birth date under FIND_CUSTOMER and the case, and logged. A candidate closed to the reader shows `closed`. The console sets the case automatically.
- F-SCN05-5: fixed (53a5cc38 + a1d86cb5; intake `changes_an_administrator_enters_are_checked_and_approved_like_a_file`, http `an_administrator_changes_a_member_by_hand`, group `a_hand_change_is_one_row_of_a_change_file`, http `the_employer_portal_serves_administrators`):
  - The new route POST .../group/changes sends `enter_changes`. A wrong employee ID is refused, naming the ID.
  - The second-administrator rule is enforced on hand changes and on files (409 `second_administrator`).
  - group-admin requests are now refused with 400.
  - Decided in docs/decisions.md.
- F-SCN05-6: left to fix-portal (general error-text passthrough in app.js/mcp.rs). The group-specific wordings touched here (frozen, quarantine, unknown pool, entered-change refusals) now say what to do.
- F-SCN05-7: fixed (a1d86cb5, UI). Re-uploading the same file now says "already received ... nothing new was sent". UI only, no Rust test; e2e not edited.
- F-SCN05-8: fixed (a1d86cb5, UI). employer.html explains that a full file must list every member, and offers "Change a member" for one-off changes.
- F-SCN05-9: fixed (a1d86cb5, UI). The preview shows quarantine and scanning messages, not empty counts.
- F-SCN05-10: fixed (a1d86cb5, UI). "Rows not used" shows the spreadsheet line number and explains each refusal code in words.
- F-SCN05-11: fixed (a1d86cb5, UI). Product and status codes appear in words, and the "Held at" column and content hash are removed.
- F-SCN05-12: partly fixed (a1d86cb5, UI):
  - Done: salary as money, hours a week, and cover in force as money.
  - Left: the "outstanding minor" label and numeric money format in #sc, because e2e/tests/11-group.js asserts them (I may not edit e2e). The lead can change both together.
- F-SCN05-13: left. Date fields default to the browser clock through the general app.js `portal.today`, which is shared with other areas. It needs the business date from the portal (fix-portal/app.js).
- F-SCN07-1: fixed (a1d86cb5; http `every_route_is_a_tool_or_left_out_for_a_reason` / MCP tests). The `scheme_command` description now gives the real `{"approve_file":{file}}|{"withdraw_file":...}|{"declare_pool":{...}}` shape. Added the tools `scheme_admins` and `scheme_member_change`.
- F-SCN07-2: general part left to fix-portal. The group refusal texts behind it are improved as in F-SCN05-6.
- F-SCN07-3: fixed (53a5cc38; year `a_pool_scheme_is_run_on_declarations`). `implausible_salary` applies above $1,000,000.00 a head. Decided in docs/decisions.md.
- F-SCN07-4: fixed (53a5cc38; year `a_pool_scheme_is_run_on_declarations`):
  - An employer cannot create a pool (`unknown_pool` names the scheme's pools).
  - A 0/0 declaration ends a pool, but not the last one (`last_pool`).
  - Decided in docs/decisions.md.
- F-SCN07-5: decided (docs/decisions.md, same entry). A scheme is pools or named members, by design; the two need two schemes.
- F-SCN07-6: fixed (53a5cc38 + a1d86cb5; billing test asserting invoice totals). Invoices carry members, total and amount due, and are shown as a dollar table.
- F-SCN07-7: fixed (53a5cc38; intake `a_file_is_quarantined_until_scanned_and_a_bad_one_is_refused`). The quarantined-file refusal now says to wait for the virus check, and other statuses appear in words.
- F-SCN07-8: left. `my_profile` for an employer administrator gives a 404 because the seeded HR people have no customer-master record (a cast gap, as in F-SCN19-5). The general MCP 404 text belongs to fix-portal.
- F-SCN09-1: left to the lead (test-stack role).
- F-SCN09-2: fixed (a1d86cb5; http `a_member_claims_their_membership_and_keeps_cover_on_leaving`). A member claims by the scheme's policy number, and another policy's number is not found. The page says "Scheme number".
- F-SCN09-3: fixed (a1d86cb5, UI). customer.html asks for claims only when the person holds a policyholder, adviser or employer-admin relationship.
- F-SCN09-4: fixed (a1d86cb5; same http test asserts `number`). Each continuation offer carries the scheme's policy number, and the take-up limit is in dollars.
- F-SCN09-5: fixed (53a5cc38; leavers `a_leaver_can_continue_their_cover_as_an_individual_policy`). Staff see a member's continuation offer and its status in words, both in the answer and in group.html.
- F-SCN09-6: not a defect. Signing keys are kept per browser by design.
- F-SCN19-1: left. Registering a scheme from an issued policy needs a portal route that builds `register_scheme` from the policy, the product and a terms form. That is a feature of its own (and the same applies to the split form); not done here.
- F-SCN19-2: fixed (a1d86cb5; http `the_group_console`). `scheme_of` takes the policy number, case-insensitive, through `PolicyNamed`.
- F-SCN19-3: fixed (53a5cc38 + a1d86cb5; driver `the_service_routes_admit_only_what_a_portal_may_do`):
  - The service, and the console's double, seal the mapping on Set.
  - The console strips the hash, gives a starter layout for a new scheme and fills the next version, and explains an empty box.
- F-SCN19-4: fixed (a1d86cb5, UI). The trial and the held file show the per-row "What it changes" table.
- F-SCN19-5: not a defect in the product. The seeded HR person has no customer-master record (cast gap). The typed reference stays accepted for a first administrator who has not signed in yet, as recorded in decisions.md.
- F-SCN19-6: fixed (a1d86cb5, UI). A held file shows "would remove N of M members (P%)" with the plain question, on both the staff and employer pages.
- F-SCN19-7: fixed (53a5cc38 + a1d86cb5). The frozen refusal says that files wait for the hold to be lifted, and the employer's standing shows a hold banner. The general passthrough is fix-portal's.
- F-SCN19-8: fixed (a1d86cb5, UI). Invoices are shown as a table, and the outstanding balance is explained beside them.
- F-SCN19-9: partly fixed (a1d86cb5, UI):
  - Done: hours shown as "hours a week".
  - Not reproduced: the empty #me chip (E2E-301 asserts its text).

Counts: fixed 27 (2 of them partly: F-SCN05-12, F-SCN19-9), decided 1, not a defect 2, left 6 (F-SCN05-6, F-SCN05-13, F-SCN07-2, F-SCN07-8, F-SCN09-1, F-SCN19-1).
