# Round 2 operator report: op2-e (SCN2-18, SCN2-20, SCN2-22)

## Shared state: what I changed and what I could not put back (read first)

- **eng-d2 is ended for good (SCN2-20).** The distribution administrator ended `eng-d2` (adv-d2 at Harbourline) on 2027-11-15, as the scenario asks. The module cannot reopen an engagement, and a duplicate id is refused (`crates/distribution/src/structure.rs:230`), so `eng-d2` cannot be put back. Viliami's engagements now stand like this:
  - `ENG-20271115-B5WMM`: Kōwhai, term-life, from 2027-11-15, ends 2027-11-16.
  - `ENG-20271115-2Q7T8`: Kōwhai, accreditation "life", from 2027-11-15, ends 2027-11-16 (made by mistake as the scenario asked).
  - `ENG-20271115-F85KD`: Clearwater (fap-d9), from 2027-11-16, ends 2027-11-17 (made by mistake).
  - `ENG-20271115-VLXH9`: Harbourline, every retail product, from 2027-11-16, open. This is the restore. It has **no commission book**, because `book-d2` belongs to `eng-d2`, so d2's new business earns nothing until a change set gives it a book.
- **Impact on SCN2-05 (op2-b).** op2-b's SCN2-05 customer appointed `adv-d2`/`eng-d2` at 09:37, before I ended it. That policy now shows "No adviser on this policy", and it has left d2's book. I could not reach op2-b (SendMessage: "not reachable"). Today (2027-11-15), d2 can only be appointed with `ENG-20271115-B5WMM`.
- **Kōwhai's payee account.** Kōwhai (`fap-kowhai`) now has a bank account, `SCN220-acct-kowhai`. The seed had left it without one for "account missing" cases, but no command removes a payee account, so it stays. I put the agreement reference back to `seed-agreement-d7`. I added payment instruction `SCN220-pi1`, then removed it.
- **Kōwhai's licence.** It is recorded "suspended" and then "active", both effective 2027-11-15. The net effect is active.
- **Statement for Harbourline.** comops cut statement no. 1 for `fap-d6` on 2027-11-15. It holds SCN2-21's commission lines (scn221-tl3, scn221-tl4), so **op2-b's SCN2-21 will find that statement already cut.**
- **Auditor's grant request.** The auditor's own grant request `SCN218-g1` was withdrawn by her. It shows as "declined".
- **What I created:**
  - Customers `pty_39f43b9af6244e8b98b091f6a2b0f193` (Cimhibl Kemacfbhaje, 1971-03-14) and `pty_a17a68ac40cb4a1785738766898082d7` (Cfhpffe Kmikfpmpgeg, 1994-07-17).
  - Policies SCN220-1 and SCN220-2 (term_life), issued with `play.js issue` with adv-d2 and fap-d6.
  - Scheme `scn222` (WPL-SCN222) for hr-e1, with `renewal_date` 2027-12-15 so that a renewal could open without moving the date. It has five members from file 1.
  - File 2 (SCN222-file2.csv) gives E005 a salary of $240k.
- **No date moved, no day closed.**


---

## SCN2-18 The internal auditor's assistant: quarter-end walk-through. Outcome: completed, with gaps

**What I did.** As `sub-auditor`:
- `tools/list`, saved to work/op2-e/auditor-tools.json.
- Every finance, payments, reinsurance and commission read named in the scenario, plus the writes, the tools that are not hers, the protocol mistakes and a burst of 50 parallel calls.
- Then the four consoles in Playwright, tab by tab, and a journal proposal through the Finance screen.

**Stack limit.** The finance ledger is empty: `status` gives entries 0 and `last_date` null, because no business day has been closed. So the trial balance, movements and extract cannot show figures, and the extract is refused with "the books are not yet fed to 2027-11-30: policy store 0 has not closed that day". That message is good. Payment runs existed only because other operators had made them.

**What held:**
- Every unlisted tool (find, staff_policy, claims_question, customer_audit, complaints_question, documents_question) answers "this tool is not open to you…" before anything about its arguments. **F-SCN02-5 fix holds for unlisted tools.**
- A query field given at the top level gets "as_at goes in the query: {"query": {"as_at": "..."}}". **F-SCN04-4 fix holds.**
- 50 parallel reads all came back 200 in 1 s.
- Finance, payments and reinsurance writes are refused with 403.
- The self-grant (`one_off` finance.operate) is recorded as requested, with `approver_action: staff.approve_grant`, and gives nothing meanwhile: `finance_command` is still 403.
- `payee` and `find_payees` refuse a reader. `payer_cases` is refused as compliance's. `notices` for a role she does not hold is refused.
- The reinsurance month-end check returns green. Commission `held_control` agrees with the ledger.

### Findings

- **F2-18-1 (major): a reader gets full bank account numbers from `payments_question run`.**
  - Who it hurts: the insurer (privacy and payment-fraud exposure) and the payees.
  - Steps: MCP as `sub-auditor`, `payments_question {"question":"run","query":{"run":"run-hq8qn-2177-6"}}`.
  - What happened: each item carries `"account":{"name":"Account Holder","number":"01-0001-8016911-39"}`. The same person asking `payee` is told "payee shows bank details, which a reader does not see". The page's own route `/staff/v1/payments/run?run=…` sends the same JSON to her browser; the screen just does not draw it.
  - What should happen: a reader's `run` answer leaves out or masks `account`.
  - Cause: `crates/channels/src/payments.rs:148`. `DETAILED` lists authority, payee, refund and find_payees, but not `run`.
- **F2-18-2 (major): write tools are listed to a read-only person (advisor note 10 confirmed).**
  - The auditor is listed `finance_command`, `payments_command`, `reinsurance_command`, `reinsurance_decide`, `commission_command` and `rate_table_dry_run`. She holds only `*.view`.
  - Every call is refused. But some listed write tools check arguments before permission:
    - `commission_command` without `purpose` answers "say why: the purpose is recorded with the decision".
    - `reinsurance_decide {}` answers "verb is required".
    - `reinsurance_decide sign SCN218-x` answers "SCN218-x is not waiting" (404), which tells her whether an abandonment exists.
  - Only once the arguments are right does she get the role refusal. This is the F-SCN02-5 problem again, for tools that are listed but not usable.
  - Cause: `reach_of`, which works by module prefix (advisor note 10).
- **F2-18-3 (major): the Finance console offers working action forms to a "View only" person.**
  - Steps: `/finance` as `sub-auditor`. The badge says "View only."
  - Still shown and enabled:
    - Propose a journal (with Propose)
    - Close a financial year
    - File and Restate a return
    - Classify policies
    - Price the book
    - Record what is being done about an aged item
    - Approve a proposal by reference
    - Retry the feed
    - Record a migration cut-over total
  - I filled the whole journal form (SCN218-j1) before learning "you may read the finance console but hold no role to act in it".
  - Payments' Reconciliation tab likewise offers "Propose" for a manual match. Commission correctly sets other roles' actions apart; Payments' "Act" tab is correctly empty.
  - Cause: `crates/channels/ui/finance.html`, where the forms are not gated on `/me` roles.
- **F2-18-4 (major): the auditor cannot answer "payees changed in the last week" or "runs prepared and approved by the same person", for runs not still open.**
  - There is no payee-change history or payee list without account details for a reader: `find_payees` and `payee` are refused outright, with no masked variant.
  - Runs: only `open_runs` lists runs. A closed run can be read only by typing its id.
  - The payments screen's runs table shows "PREPARED BY" but not the approver. The approver appears only after opening a run, in the sentence "Prepared by st10; approved by st17".
  - Journals likewise can only be opened by typing a journal reference ("there is no journal x"). No question lists them.
- **F2-18-5 (minor): question fields the descriptions do not name.**
  - Commission `held_control`, `month_end_blocks` and `clawback_exposure` need `date`, but the tool description lists only `party, as_at, payee, number, period`.
  - Finance `dashboard` needs `today`. A caller should not have to supply "today": it should default to the business date.
  - Finance `period` format (YYYY-MM) is not in the description. "2027-Q4" is first refused for the missing `today`, and the period format is explained only on the second try.
  - Commission `aged_minor` is an unlabelled four-element array: buckets with no names.
  - `staff_me` gives `recertification_due` and grant `requested_at` as Unix seconds.
- **F2-18-6 (minor): `reinsurance_question balance` answers 0 for an unknown reinsurer or treaty** (`{"reinsurer":"no-such","treaty":"no-such-treaty"}` gives `0`). It should say there is no such treaty. Also, `exposure` names reinsurers but no treaties, so the treaty id that `balance` needs is not discoverable.
- **F2-18-7 (minor): a number in `query` is answered "not a question of this module: input contains invalid characters"** (`exposure` with `on` sent as the number 20271115). It should say "`on` is a date written as text, YYYY-MM-DD".
- **F2-18-8 (minor): grant JSON with no example.**
  - `staff_access request_grant` takes `kind` internally tagged (`{"kind":{"kind":"one_off",…}}`) and `from`/`to` as Unix seconds. The description gives no example: it took me three tries, with "missing field `id`" and then "missing field `kind`".
  - A withdrawn request is recorded as `declined` with the requester as `approver` (`crates/channels/src/staff.rs:1292-1297`). The audit trail cannot tell a withdrawal from an approver's refusal.
- **F2-18-9 (minor): refusals do not say who can act.** Payments, finance and reinsurance refusals say "you may read … but hold no role to act". Unlike commission's refusal ("that is done by the distribution administrator"), they do not name the role that may.
- **F2-18-10 (minor): money shown in three forms.** Screens show money as "NZ$…", while MCP gives cents. The payments description explains cents well; the reinsurance, commission and finance descriptions do not mention units.

**Things noticed:** item 10 is confirmed (F2-18-2). Item 16, F-SCN02-4, was not touched.

---

## SCN2-20 An adviser changes advice provider. Outcome: completed, with major defects

**What I did:**
- Made two customers with policies issued with adv-d2/fap-d6. Each appointed `adv-d2`/`eng-d2` on `/customer` by typing both references.
- d10 and d6 on `/adviser`.
- dist on `/commission` "Change something":
  - ended eng-d2 on 2024-12-01 (refused), then on 2027-11-15;
  - d2 on `/adviser` and both clients on `/customer`;
  - started a Kōwhai engagement with accreditation "life", then overlapping ones;
  - client 2 appointed adv-d4/eng-d4, and client 1 re-appointed adv-d2 under the new engagement;
  - d6, d10 and d7 views;
  - the payee, account and instruction steps;
  - comops cut a statement;
  - the licence was suspended in error and corrected;
  - then the restore.

**Correction to the scenario's set-up.** Kōwhai now has a sign-in (`d7`, `fap-kowhai`, added in round 2), so nothing is stopped by its absence. d7 sees adv-d2 and adv-d4 under it.

**What held:**
- d10 sees Harbourline's advisers and nothing of Kōwhai or Clearwater; d6 sees d1 and d2.
- An end date before the start is refused.
- The engagement reference is generated (ENG-20271115-…) (**F-20-2 partly holds:** generated, but see F2-20-5).
- The licence correction holds (**round 1 fix holds**). After "active" on the same day, d2's read-only banner and the customer's "Cannot act for you" are both gone.
- A payee instruction is refused to comops, with words naming the distribution administrator.
- The statement's closing total (223751 cents) equals the sum of its 8 lines.

### Findings

- **F2-20-1 (major): when an engagement ends, the customer's adviser silently disappears.**
  - Who it hurts: the customer.
  - Steps: `/commission` as `sub-dist`, Change something, End an engagement `eng-d2` on 2027-11-15. Then `/customer` as `sub-pty_39f4…`.
  - What happened: "No adviser on this policy." Nothing says that Viliami can no longer act or why, and nothing says how to appoint another.
  - What should happen: the same treatment as a lost licence (F-SCN03-7): "Your adviser can no longer act for you from …; appoint another below, or contact us."
  - Cause: `crates/channels/src/state.rs:568`, `AdviserStanding::Ended => continue`.
- **F2-20-2 (major): appointing an adviser under an ended engagement reports success and does nothing.**
  - Steps: as the client, `/customer`, adviser `adv-d2`, engagement `eng-d2` (ended), Appoint.
  - What happened: "Your adviser was appointed." The table directly below says "No adviser on this policy."
  - What should happen: a refusal: "Viliami's engagement with Harbourline ended on 15/11/2027; ask him for his current reference."
  - Cause: the appoint route does not check the engagement's standing; display filters Ended (see F2-20-1).
- **F2-20-3 (major): overlapping engagements are accepted without a word.**
  - The same adviser got two Kōwhai engagements from the same day (accreditations "life" and "term-life"), plus a Clearwater engagement from the next day. Each was "Done: …".
  - d7's page now lists adv-d2 twice as Active.
  - Cause: `crates/distribution/src/structure.rs:230-251`, where `start_engagement` has no overlap check.
- **F2-20-4 (major): an engagement made in error cannot be withdrawn the same day, and the refusal is wrong.** Ending `ENG-20271115-2Q7T8` on its start date 2027-11-15 is refused with "an engagement cannot end before it began" (`structure.rs:256`, `on <= e.from`). It did not end before it began. It has to run at least one day, and with an accreditation entered in error it stays in force meanwhile. There is no "withdraw / entered in error" command.
- **F2-20-5 (major, staff UI review): references must be typed.**
  - On `/commission`, End an engagement asks for "Engagement" as free text. No question lists engagements (`relations` holds only structure links; `parties` has no engagements), so dist cannot find eng-d2 or its start date on screen.
  - Adviser, Provider and Accreditations are free text, with no datalist offered (checked: no `input[list]`).
  - "life" is accepted as an accreditation although it matches no product (stack-additions item 1 confirmed: the console does not say that accreditations are product codes, and does not validate them).
  - The customer must type "Adviser reference" and "Their engagement reference" on `/customer` (`crates/channels/ui/customer.html:14`).
- **F2-20-6 (major): a provider loses all history of a departed adviser.**
  - After the end date, d6 and d10 no longer see adv-d2 or the policies he held for them up to that day. The scenario expects history kept up to the end date.
  - Cause: `crates/channels/src/directory.rs:376-399`. `practice()` keeps only engagements in force on the day.
- **F2-20-7 (major): a payee bank account change waits for no second person.**
  - dist added Kōwhai's account (`SCN220-acct-kowhai`) and got "Done" at once. The only safeguard is the cooling-off period.
  - The name-check and step-up "references" are free text that the administrator types and nobody verifies.
  - There is no command to remove an account added in error.
  - Cause: `crates/distribution/src/state.rs:1070-1106`.
- **F2-20-8 (major): a new engagement has no commission book, and nothing tells the administrator.** Books are per engagement (`book-d2` on `eng-d2`), and a book is made only by a commission change set. So after the move, nothing Viliami writes or services earns commission with Kōwhai (or with Harbourline again after the restore). The goal "make sure he's paid by the right provider" cannot be met from the console. No open task or warning appeared (`open_tasks` = []).
- **F2-20-9 (minor): a payment instruction has no form.** Adding a payment instruction is not in the "Change something" list; only "Remove" is. dist had to use "Another command" with raw JSON (`add_payment_instruction`). Re-registering an existing payee silently replaces its agreement reference.
- **F2-20-10 (minor): unhelpful confirmations and statement header.**
  - "Done: end an engagement (on 2027-11-15)." does not name the engagement.
  - "Done: cut a payee's statement now for fap-d6." does not give the statement number.
  - The statement header has a date but no period (from–to). It says `payment_held: no verified account past its cooling-off` for Harbourline's seeded account.
- **F2-20-11 (minor): references where names belong (round 1 leftover, item 16).**
  - The customer sees "adv-d2", not Viliami Fifita.
  - Provider pages show adv-d1, fap-d6 and "term-life".
  - The customer's licence wording says "their adviser licence is not in force" when it was the provider's licence that was suspended.
- **F2-20-12 (minor): d2 between engagements is told nothing.** His page shows "Your book: Nothing to show". The Engagements tab says "Ended", but the book does not say why it is empty or what to do. A request he prepared for a former client is answered 404 "no such policy". Round 1's F-SCN03-7 expected "sees why in words".

**Things noticed:** item 9 not touched. Stack-additions item 1 confirmed (F2-20-5).

---

## SCN2-22 A scheme renewal with a rate rise and an executive above the automatic limit. Outcome: blocked at steps 2 and 5, partly done otherwise

**What I did:**
- Registered scheme `scn222` for hr-e1, the way `register()` does, with `renewal_date` 2027-12-15.
- Five members by file, uploaded and approved with the e2e lib: three staff, executives at $150k and $200k.
- st7 opened the scheme and members on `/group`.
- Sent `agree_renewal` from the console's "Another command".
- e1 read `/employer`.
- st2 looked in the `/underwriting` queues.
- e1 uploaded file 2 (director at $240k), previewed it and approved it on the page.

### Findings

- **F2-22-1 (blocker): a renewal can change neither rates nor salary multiples.**
  - `agree_renewal` takes `terms` (`SchemeTerms`) and `rate_table`, which is a free-text label kept "for the record" (`crates/group/src/command.rs:308-314`). The salary multiples live in the product's categories (`products/group_life.star`: staff 2×, executive 4×, not the 5× the scenario assumed). The rates live in the rate table `group-life-standard`.
  - So "rates up 8%, executive 5→6×" cannot be agreed, and a mistyped multiple of 60 cannot even be expressed. I tried `executive_multiple: 60`; the answer is the serde error "unknown field `executive_multiple`, expected one of `basis`, …".
  - st7 must write the whole terms JSON by hand (about 25 fields) to agree a renewal.
- **F2-22-2 (blocker; stack limit and product): the renewal never opens without a day close.**
  - Even with `renewal_date` 30 days ahead (inside the 90 lead days), `agree_renewal` is refused 409 "the scheme has no renewal open", and the scheme shows "renewal open: no". The renewal opens only in `tick_renewal` (`crates/group/src/work.rs:1404-1418`), which runs on the business-day roll.
  - The refusal does not say when it will open: "opens on 16/09/2027 or at the next day close" would be better.
  - The employer page shows no renewal date or terms at all, so step 3 could not be judged beyond that.
- **F2-22-3 (blocker): the excess above the automatic limit never reaches underwriting.**
  - E004 ($100k) and E005 ($50k + $250k, and after file 2 + $160k) show "pending" on `/group` and `/employer`. But st2's queues (Medical 2, Financial 0, Occupational 0, Tele interview 0, Senior review 0, Reinsurer liaison 0) hold only SCN2-03's cases. `play.js proposal grp-uw:scn222:1..3` gives "underwriting holds no proposal".
  - The scheme shows "queued 0, in flight no". Work waiting shows "Nothing is waiting". There is nothing in the group or underwriting logs.
  - The request goes out through `Outbound::Underwriting` (`work.rs:1006-1040`) to `adapters.rs:493`. That path keeps a refused request in the outbox silently, for example when the member is not linked, or "with no rules active for the product". The only rule set seeded is `individual@1`.
  - Steps 5-7 (st2's offer, the loading, the "too few members" exception and `reoffer`) could not be done.
  - Whatever the cause (no group rule set seeded, or an unlinked member), a request stuck like this should raise a group exception.
- **F2-22-4 (major): the automatic limit shown differs from the product's, without explanation.**
  - The product's automatic acceptance limit is $750,000 (`group_life.star:24`, which the e2e oracle uses). Every executive is held at $500,000 "AUTOMATIC".
  - E004 at $600k (under $750k) has $100k "above automatic".
  - E005 has two separate pending rows, $50,000 and $250,000, then a third ($160,000) after the salary rise. Presumably $750k is the product limit and $500k a reinsurance retention cap (`RequireUnderwrittenExcess`), but no screen says which row is which or why.
  - The oracle (`e2e/lib/oracle.js groupMember`) would compute premiums on $750k for executives. The outstanding $154.75 matches $500k caps: 11.00 + 12.83 + 10.08 + 60.42 + 60.42.
- **F2-22-5 (minor): invoice and outstanding disagree on screen.** `/employer` Invoices shows one notice, "0 | 1 member | NZ$11.00", while "Outstanding NZ$154.75" sits above it. The employer cannot see what the other $143.75 is for or which members it covers. Period is shown as "0".
- **F2-22-6 (minor): file preview leaves out what matters.**
  - The file 2 preview says "E005 changes 2027-11-15 salary". It does not give the old and new salary, or that the change puts cover above automatic (the summary had `above_automatic_cover: 1`, which is not shown).
  - The employer must type the file name to preview it.
- **F2-22-7 (minor): the employer sees the director is being underwritten.** `/employer` member record for E005 shows the three "ABOVE AUTOMATIC … pending" rows. That reveals an underwriting case on a named person, not a loading, so it is acceptable if intended. Health and loadings are not shown.
- **F2-22-8 (minor, staff UI review): st7's console needs typed ids and raw numbers.**
  - Scheme, file, member and policy must be typed (`sc-id`, `fi-id`, `mb-emp`); there is no scheme list on `/group`.
  - Scheme view rows: "outstanding minor 154.75" (labelled minor but shown in dollars), "cover in force group-life 1370000.00", and cover tables "600000.00" with no $ or separators.
  - The scheme view's "Terms" heading shows no terms (renewal date, lead days, no-agreement rule), so step 1 cannot be read on screen.

**Stack note.** e1b is not an administrator of a new scheme until e1 names her on `/employer`. I did not do this, for lack of budget.

---

## Summary by severity

- **Blocker:** F2-22-1, F2-22-2, F2-22-3
- **Major:** F2-18-1, F2-18-2, F2-18-3, F2-18-4, F2-20-1, F2-20-2, F2-20-3, F2-20-4, F2-20-5, F2-20-6, F2-20-7, F2-20-8, F2-22-4
- **Minor:** F2-18-5 to F2-18-10, F2-20-9 to F2-20-12, F2-22-5 to F2-22-8
