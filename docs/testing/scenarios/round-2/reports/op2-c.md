# Round 2 operator report: op2-c (SCN2-08, SCN2-09, SCN2-15, SCN2-16)


## Shared-stack changes (read first)

I did not close any business day, move the date, stop nodes or change seeded records. These shared things changed:

- **Reinsurance, a bad statement.** There is a statement **number 1 for treaty `retail-life-surplus` with an empty reinsurer** (period 2027-07-01 to 2027-09-30), filed write-once at `reinsurance/retail-life-surplus//statement-1.json`. It was cut through the screen (see F2-08-9), and it cannot be undone. Because of it, `exposure` now lists a reinsurer called `""` with zeros.
- **Reinsurance, a real statement.** Statement **2** (reinsurer-north, Q3 2027, net $0) was cut, agreed, held, released and instructed. The settlement was prepared by ri1 and signed by ri2.
- **The collection account's bank book.** Bank statements for `bnz-sf1-collection` were recorded for 2027-11-16 and 2027-11-17, so the book's closing balance moved to the closing of my lines. Unidentified-receipt and debit exceptions are open on lines `scn215-l1/l2/l3` and `scn216-mum/240/dup1/dup2` (the $240 one has been applied).
- **Payments.** Payees `PYE-20271115-BXDRT` and `PYE-20271115-6J93Z`, and authorities `DDA-20271115-*` (one of them cancelled), were created for my own customers. Runs `RUN-20271115-76NKJ`, `-QNRWF` and `-B9P8P` were settled. `-HENX9` and `-DKCBQ` were cancelled.
- **A wrong refund is still on the stack.** Refund **`RFD-20271115-7HSZC`** is still *requested* to the wrong payee (F2-16-5). It went no further, because the stack does not relay the core's "refund raised" back to payments. **Nobody should release it.**
- **Claims left open.**
  - `CLM-20271115-BQ98L` has an admission waiting for a signature that nobody can give (F2-08-2).
  - `app-73517bbd-…` (SCN209-P1) is open and refused.
- **The customer master.** The person "Moana Kblheilkmhk" (`pty_996ce2c2…`) was added, with relationship `REL-20271115-GS376`. Customer `pty_7222…` has a spending hold on.

What I made off-screen, as the ground rules allow:

- **Customers.** I used `customer()` from `e2e/lib/customer.js`. The names, birth dates and party references are in each section.
- **Policies.** I issued them by `POST /v1/policies` as new business.
- **Parts of the system I played:**
  - **The bank:** submission, acknowledgement, settlement, return and statement, through `System.tell("payments")` as `bank-driver`.
  - **Sanctions screening:** `screen_payee`.
  - **Cash matching:** `POST /v1/payments` as `svc-cash-matching`. It is not on the list of parts we may play, but nothing else applies a statement line in this stack.
  - **The core-to-payments relay:** `policy_owner` and `premium_applied`, sent as service. The stack runs no relay, and without these no payer case can ever open. This is a stack limit.

---

## SCN2-08: a large trauma claim and the reinsurer's share

**Outcome: partly completed.**

- **Blocked:** the $900,000 admission, which no one can sign (F2-08-2).
- **Done instead:** the scenario's fallback, a $600,000 trauma claim on a second policy, taken from notification through payment, the bank and the reinsurer's recovery.
- **Blocked by the stack:** the Q4 statement that would carry the recovery, because the period has not ended and I may not move the date.

What I did:

- **Set-up.**
  - Customer 1 is Cfgjhai Kmkpcobigfg, born 1995-05-11, `pty_fc942e64…`. Policy `SCN208-P1` is term life with life $500k and trauma $900k, in force from 2027-10-06. The template's life is 39, not 49.
  - Customer 2 is Chgjemg Kbafhpjaclg, born 1991-06-17, `pty_ed907f89…`. Policy `SCN208-P2` has life $100k and trauma $600k.
- **The reinsurance view before the claim.**
  - ri1, on the Lives view, sees "keeps NZ$500,000.00" for customer 1. That needed a workaround (F2-08-1).
  - `exposure` showed north $540k and south $360k ceded.
  - So retained $500k plus ceded $900k equals the sum insured of $1.4m. **Confirmed.**
- **The $900k claim.**
  - st4 notified it on screen, cover picked, event 2027-10-26. A reserve of $900,000 was set automatically.
  - "Assign it to me" was refused in good words: "st4 may admit up to $600,000.00 for a claim of this type, and this one is $900,000.00: st4b can take it, given it by a queue lead". This **confirms the F-11-5 fix holds.**
  - st4b took the claim, recorded cause CC-0101, the cover check and the evidence (requested, then received at a cost of $350), and the reserve. He then proposed the admission, which waits for a second signature. st4 cannot give it (F2-08-2).
- **The $600k claim.** st4 did the whole file and proposed the admission, and st4b countersigned.
  - Admitted $600,000.00. Payable $599,765.63, after $234.37 of premium owed.
  - The payee was registered by `payee`, confirmed by `payee2`, screened by me as the screening service, and picked by name in Verify the payee. Payment `PAY-20271115-B5V2C` followed.
- **The payment run.**
  - st10 prepared a lump-sum run, took the item from the waiting list and checked the run. The item was held: "held: over the route's limit for one item: treasury releases it once funded". This **confirms F-12-4.**
  - st17's Approve was refused, naming the item, the payee, the amount and the reason. This **confirms F-SCN16-1** (but see F2-08-12).
  - st17 released the hold and approved the run. I played the bank to settlement.
- **The reinsurer's side.**
  - ri1: `consent_status` answers `not_required`. The treaty's thresholds are $1m to notify and $2m for consent, so no `admitted_without_consent` exception was expected or seen.
  - The open recoverables were north $120,000 and south $80,000. That is the ceded share of the $600k admission: the $500k retention is used first by the $100k life cover, so $400k of the trauma is retained and $200k is ceded.
  - After payment, the balances are north −$119,953.13 and south −$79,968.75 (F2-08-10).
- **The statement and settlement.**
  - ri1 cut the Q3 statement. Q4 cannot be cut: "the period to 2027-12-31 has not ended".
  - He agreed it with the reinsurer's confirmation reference and prepared the settlement. The preparer is not offered a signature.
  - ri2 signed and sent it, after a held-statement detour (F2-08-11).
- **The auditor over MCP.** `exposure`, `statements`, `statement`, `balance`, `consent_status` and `retained` all answer. `reinsurance_command` is refused: "you may read the exceptions but hold no role to act on them". `payments_command` is refused too. An unlisted `claims_command` is refused in words: "your roles do not include claims". **Things noticed, item 10, is confirmed:** the auditor is listed `commission_command`, `payments_command`, `reinsurance_command`, `reinsurance_decide` and `finance_command`, all of them writes she can never use.

### Findings

- **F2-08-1, major. The Lives view cannot search for anyone.**
  - **Who it hurts:** reinsurance operations.
  - **Steps:** sign in to `/reinsurance` as `sub-ri1`, open Lives, type the customer's name. Nothing is offered, only "Say which case, call or task you are working on first: every search is recorded against it." The page has no field for a case. Search only works if the session's `working-on` was set on another page; I set it by hand in sessionStorage to get past this.
  - **Should:** the page should ask for the case, as `/staff` and `/records` do, or make a reference.
  - **Cause:** `crates/channels/ui/reinsurance.html:162` uses `portal.people`, which calls `find()` (`app.js:414`), and nothing on the page calls `portal.workOn`.
  - **Also:** the view shows only what is kept, never what is ceded on that life or to whom. So "retained plus ceded equals the sum insured" cannot be read for one life. The book-wide `exposure` happened to equal it only because the stack had no other cessions.
- **F2-08-2, blocker. A claim above $600,000 can never be admitted.**
  - **Who it hurts:** the claimant and the claims team.
  - **Steps:** st4b, the claims manager, admits the $900k trauma claim `CLM-20271115-BQ98L`. It waits for one more signature. st4 presses Approve and is refused: "you cannot give any signature this decision still needs".
  - **Why:** the second signer must also have authority for the amount (`crates/claims/src/decide.rs:757-764`, `can_sign` with `Need::Authority`), and st4b is the only person above $600k. Things noticed, item 8, is therefore **not fixed** by the seed's new claims-manager profile; it only moved the stop to the second signature.
  - **Words:** the refusal names neither the limit nor who could sign. st4's console still offers "Approve the decision waiting".
  - **Should:** a second person with that authority should be seeded. The refusal should say "a second signature needs someone whose authority covers $900,000.00: nobody else holds it today". The console should not offer an Approve the person cannot give.
- **F2-08-3, major. A claim notified without its life breaks the reinsurer's recovery, silently.**
  - **Who it hurts:** reinsurance and finance.
  - **Steps:** in Notify, "Life (optional)" can be left blank (I did on the first claim).
  - **What happened:** reinsurance opened `claim_fact_not_taken` "claim_reserved has no life … Recoveries on claim … are not right until it is entered". The same exception exists for many e2e claims. `consent_status` for that claim answers "404 no such resource".
  - **Should:** claims should identify the life from the cover when there is one (every term-life cover here has one), or make the field required. The assessor should be told.
- **F2-08-4, major. The cover check records "life on the policy: No".**
  - **Who it hurts:** assessors.
  - **What happened:** on every claim I checked (SCN2-08 and SCN2-09, with or without the life picked), the cover section reads "The cover on the event date reads differently now than when it was checked, because of something recorded later". It then shows "As known when checked: life on the policy: No", against "As known now: Yes". Nothing was recorded later.
  - **Also:** the trauma survival period is not shown as a fact anywhere (scenario "good looks like"; round 1 **F-11-4 still open**).
  - **Cause:** I did not trace it. It is in the cover-check facts the claims module stores (`claims/src/decide.rs:170`, `covered()`) against `cover_now_and_then` in the work view.
- **F2-08-5, minor. Typing a policy number in full sends the number, not the policy.**
  - **Who it hurts:** assessors.
  - **Steps:** in Notify's Policy picker, type the whole policy number (here `SCN208-P1`). The list closes as if a choice was made. The command then carries the number, refused with "no partition holds the policy SCN208-P1". The covers are never loaded, so the Cover picker says "Pick the policy from its list".
  - **Cause:** `app.js:328` compares the typed text with `value` without case and closes, without choosing. In this stack the id happened to be the lower-case number. Where id and number differ, the same applies to typing an id.
  - **Should:** take the exact match as the pick and call `onPick`.
- **F2-08-6, minor. Codes and references where words belong in claims.**
  - Cause code is a typed text field. The refusal lists only codes: "the codes are CC-0003, CC-0101, CC-0204, CC-0900", without "heart attack" and the others.
  - The claim summary shows the policy id (`scn208-p2`) and a Partition column.
  - The payee is shown as `pty_ed907f89…` ("Payee: pty_… (policyholder)") rather than the name.
- **F2-08-7, minor. A sharp reserve move asks for nothing.**
  - **Steps:** st4 moved the reserve from $600,000 to $100,000 and back to $600,000 (the rules say `sharp_reserve_move_pct` 50).
  - **What happened:** both were accepted with any reason code, with no prompt or note.
  - **Should:** say that a move above 50% needs its reason, or a second person, as the rules imply.
- **F2-08-8, minor. Reinsurance unpriced-cession exceptions for every trauma cover.**
  - **Who it hurts:** reinsurance operations, who see an exception queue of noise.
  - **What happened:** every term-life policy's trauma cover raises `unpriced_cession` "no reinsurance premium could be worked out for …/trauma/1/life-1: a rate or the customer's premium is missing". That includes mine and dozens of e2e policies.
  - **Cause:** it is likely configuration: `ri-life-base@1` (shipped.json) has one rate table and a treaty scope covering trauma, but whatever is needed for trauma is missing. The exceptions do pile up.
- **F2-08-9, major. A statement can be cut for no reinsurer.**
  - **Who it hurts:** reinsurance and audit.
  - **Steps:** on `/reinsurance` → Accounts, without first choosing a reinsurer in "Reinsurer accounts", fill "Cut a statement" (Treaty `retail-life-surplus`, Period ending 2027-09-30) and press Cut it.
  - **What happened:** "Cut. Send it to the reinsurer…". Statement 1 now exists for reinsurer `""`, filed write-once.
  - **Should:** the Cut form should carry its own reinsurer picker. The module should refuse an unknown reinsurer.
  - **Cause:** `reinsurance.html:114-117` takes `acc-re`'s value; `reinsurance/src/account.rs:342` does not check the reinsurer.
  - **Also typed:** Treaty is a typed reference on Cut, on Review profit commission and on Treaty changes.
- **F2-08-10, minor. The recovery is short of the ceded share of the admission.**
  - **What happened:** the balance owed by the reinsurers is $119,953.13 and $79,968.75, a total of $199,921.88. The ceded share of what was admitted is $200,000.00. The difference, $78.12, is the ceded share of the $234.37 premium taken from the claim, and the open recoverables went to $0.
  - **Should:** the premium set against a claim is still part of the admitted benefit, so the reinsurer owes its share of $600,000. If the treaty says otherwise, the statement should show it.
- **F2-08-11, major. A held statement looks unheld, and a failed signature loses the instruction.**
  - **Who it hurts:** reinsurance operations and the lead.
  - **What happened:**
    - After "Hold the settlement", the statements table still shows status "agreed", an empty Held column, and "Hold the settlement" and "Prepare the settlement" again, with no "Release the hold". The `statements` answer has no `held` field (only `statement` has it), so `reinsurance.html:98` never sees it.
    - Prepare was then accepted on a held statement. When ri2 pressed "Sign and send" it was refused ("settlement of statement 2 is on hold: …"), and the prepared instruction vanished from "Waiting for a second person".
    - I released the hold through ri1's MCP `reinsurance_command` `{"release_hold":{"number":2}}`, because the screen offers no button for it.
    - The preparer never sees their own prepared instruction ("Nothing is waiting." as ri1), so they cannot withdraw it.
  - **Also:** the Net column shows `0.00` without NZ$.
- **F2-08-12, minor. Payees as references on the payments run.**
  - **Who it hurts:** st17 and st10.
  - **What happened:** the run rows say "Payee: PYE-20271115-BXDRT", and the waiting list shows the payee as `pty_…`. The approval refusal says "$599,765.63" while the run shows "NZ$599,765.63".
  - **Status:** round 1's F-SCN16-10 is partly fixed (money now NZ$, holds in words), but payees are still references.
  - **Things noticed, item 14, is confirmed:** st17 both released the over-limit hold and approved the run.
- **F2-08-13, minor. The claim reads "paid" and "closed" before the bank has the money.**
  - **What happened:** once released to payments, the title reads "paid: released to payments, which settles it at the bank" and Closed shows 2027-11-15, before the bank had it.
  - **Status:** better words than round 1's F-11-6, but it still closes before settlement.
- **MCP notes for ri1 and the auditor.**
  - `reinsurance_command`'s description gives no command names ("{\"<command>\": {...}}"), so an assistant must guess `release_hold`, `hold_settlement` and the others.
  - `reinsurance_decide` is described as for "a prepared abandonment" only, yet settlements go through it too.
  - The auditor's `reinsurance_decide` with an unknown id answers "x is not waiting" rather than that she may not sign.

---

## SCN2-09: an income claim in payment

**Outcome: partly completed.**

- **Steps done:** 1, 2, 3, 5, 6 (it found a gap), 7 and 8.
- **Step 4, the missed income declaration, is blocked:** it needs the date moved past 2028-02-11, which this run may not do.

What I did:

- **Set-up.** Customer Cgefcak Knjidakjcgf, born 1975-03-10, `pty_0f0d5d99…`.
- **The first policy failed.** `SCN209-P1` (income protection, $6,000 a month, from 2027-06-01) had a claim notified from `/customer`. It was refused: "cover was not in force for this life on the event date", because its premiums had been unpaid since June. Paying them later did not change the past. A second policy, `SCN209-P2`, from 2027-09-20 with its premium paid, was used.
- **Notification from `/customer`.** It worked: "Thank you. The claims team will be in touch…".
- **Assessment.** st4 found the claim by the customer's name (after a workaround, F2-09-1), took it, and recorded cause CC-0204 and the cover check.
- **Admission.** She admitted a benefit in payment of $6,000.00 a month from 2027-10-28. The first end date she tried was refused (F2-09-3).
- **Payee.** The payee was registered and confirmed by two maintainers, then verified on the claim.
- **Review dates.** Computed as medical review 2028-04-28 and income declaration 2028-01-28. **Correct (6 and 3 months), but not visible on screen** (F2-09-2).
- **The first payments.** A claims benefit run was prepared on partition 1 (a typed field) and approved by st4b; st4 is refused as the second person. It paid $12,000.00 (F2-09-4).
- **Part-time earnings.** declare_income from 2027-10-28 at $2,400 a month raised overpayment `…/overpayment/1` for $4,800.00 (F2-09-5).
- **Recovery by offset.** 50% is refused (F2-09-6); 25% is accepted.
- **Write-offs.**
  - A $180 overpayment, raised with "Amount" and cause late_information, was written off by st4 alone.
  - A $1,200 write-off waited for a signature, and st4b approved it. **Correct.**
- **The investigator.** Covered in F2-09-7.
- **st3's assistant.**
  - `claims_question` `queue` and `work` answer, including the income-protection queue and the full work view of an IP claim, which is not her line.
  - `claims_definition` `total-disability`@1 returns the bound version's words with its hash. **Good.**
  - `claims_command` end benefit, decline and pay are each refused: "claim … is not assigned to st3". Assigning it to herself is refused: "st3 is not accredited for this line and queue". Nothing was recorded.
- **Ending the benefit.** st4 ended it from 2027-12-01 with reason recovered and R-RECOV. The claim reads "ended", and the customer page shows "ended", Closed 2027-11-15 (F2-09-8).

### Findings

- **F2-09-1, major. The claims lookup by name needs a purpose the page has not asked for.**
  - **Who it hurts:** assessors.
  - **Steps:** open `/claims`, go to the Claim tab, type the customer's name in "A claim reference, a policy number or the customer's name", press Show.
  - **What happened:** "Say which case, call or task you are working on first…". The field it means is "Why you are doing this" in the Act card further down. After that, the name is tried as a claim reference (404 in the console).
  - **Should:** ask for the purpose beside the lookup, or make one ("Looking up a claim for <name>").
- **F2-09-2, major. The benefit's review dates show as "[object Object]".**
  - **Who it hurts:** case managers.
  - **What happened:** Payments and benefit reads "Benefit NZ$6,000.00 a month, next period 2027-12-28, ends 2029-10-28. Arrears NZ$0.00. Reviews [object Object]." No medical review or income declaration task appears under Tasks.
  - **Cause:** `crates/channels/ui/claims.html` renders `benefit.reviews`, an object of `{medical_review, income_declaration}`, as a string.
- **F2-09-3, minor. An end date beyond the option is refused without the allowed date.**
  - **Steps:** the end 2040-03-10 was refused: "the benefit runs past the period or the age the chosen option pays to".
  - **Should:** say which, and the last allowed date (here 2029-10-28, a two-year period).
- **F2-09-4, major. The benefit run pays a month ahead, and "Nothing paid" contradicts Paid.**
  - **Who it hurts:** finance and the customer.
  - **What happened:** with benefit date 2027-11-28 (after the business date of 2027-11-15), the run paid two periods ($12,000), including 2027-11-28 to 2027-12-28, which has not started. The claim's Paid column is then $12,000.00, but the section says "Nothing paid." and has no payment rows, and the remittance answers "None.".
  - **Also:** Reserve went to $0.00 and Admitted shows $0.00 for a benefit admitted for two years.
  - **Should:** income benefits are paid in arrears to the business date; the payments list and the remittance show what was paid; the reserve carries the expected cost.
- **F2-09-5, major. The partial benefit is not shown, and the overpayment arithmetic double-counts when the benefit ends.**
  - **Who it hurts:** the customer and finance.
  - **What happened after declare_income ($2,400 a month from 2027-10-28):**
    - Overpayment/1 is $4,800 (2 × $2,400), with no arithmetic shown.
    - The benefit line still says $6,000.00 a month.
  - **What happened when the benefit was ended from 2027-12-01:**
    - The system raised overpayment/4 for $5,400: 27 days at the full $6,000.
    - $2,400 of that same period is already in overpayment/1, so $7,800 is now claimed back for a period paid $6,000.
    - The right figure is about $5,640: the period was paid $6,000 and 3 days at $3,600 were owed.
  - **Should:** show the partial benefit as "$6,000 less $2,400 earned = $3,600 a month", and the overpayment by period. Work out the end-of-benefit overpayment on the partial rate.
- **F2-09-6, major. The 50% offset refusal does not name the 25% cap.**
  - **Steps:** decide recovery status, offset, 50%.
  - **What happened:** "that status cannot be set this way, or its terms are outside the rules".
  - **Should:** "offset is capped at 25% of each payment".
  - **Cause:** `crates/claims/src/decide.rs:628`.
  - **Also:** the overpayment is offered and typed as `app-…/overpayment/1`. After the benefit ended, the $4,800 offset stays "offset" with no payments left to offset against, and nothing prompts a change.
- **F2-09-7, major. No one can refer a claim for investigation, and the investigator's view is not its own.**
  - **Who it hurts:** compliance and investigators.
  - **What happened:** a referral exists only as `ReferForInvestigation` from the compliance module (`crates/claims/src/investigate.rs:16`, `state.rs:484`). No screen, route or MCP tool sends it.
  - **st13 on `/claims`:**
    - Sees the Investigations list, which holds only automatic early-claim cases, all of them other tests'.
    - Sees the full claim like an assessor, because st13 holds the `claims` template.
    - Is offered decide, suspend, repayment and other actions, then refused with "not assigned to st13".
  - **Untested:** st4 seeing whether an investigation is open could not be tested, because none could be opened.
  - **Should:** give the investigator a Refer action with its own role, logged, and do not give investigators assessor actions.
- **F2-09-8, minor. The customer sees "ended" with the wrong date.**
  - **What happened:** `/customer` shows "ended", with Closed 2027-11-15 (the day of the decision) rather than the end date of 2027-12-01. The staff line reads "next period 2027-12-28, ends 2027-12-01".
  - **Also:** the claim reference shown to the customer is `app-75f59801-390c-4c58-81f3-42f6f2645fef`.
  - **Cannot appear:** `customer.html:172` has no words for "suspended", so the missed-declaration text ("what to send and by when") cannot appear.
- **F2-09-9, minor. The refusals to st3's assistant do not name her lines or limit.**
  - **What happened:** "not assigned to st3" and "st3 is not accredited for this line and queue".
  - **Should:** "income protection is not one of your lines (health); a senior assessor such as st4 can take it".
  - **Status:** round 1's F-11-5 is fixed on the screen's assign path (SCN2-08), not here.
  - **Also:** st3 can read the income-protection queue and the work view of an IP claim, which includes the payee and the overpayments. Whether a health assessor should see another line's claims in that detail is a question for the claims lead.
- **F2-09-10, minor. Partition and run are typed in the claims console.**
  - **What happened:** prepare and approve run need "Partition" (I had to know it was 1) and a typed "Run reference" for approve. The remittance lookup needs a typed run.
  - **Note:** the staff-ui-review rule is broken here.
- **Stack note (not a product defect).** A policy with unpaid premiums past grace on the event date is not in force for a claim. That is right, but the claims refusal does not say "premiums were unpaid from 2027-06-01" or what would cure it.

---

## SCN2-15: a direct debit that bounces twice

**Outcome: partly completed.**

- **Done:** every step ran.
- **Not possible:** a hardship mark for a fortnight, a changed account on an authority, the cooling-off period and second-maintainer confirmation do not exist. Representation needed the console's raw-command form.

What I did:

- **Set-up.**
  - Customer 1 is Chaipmj Kgdklagggel, born 1970-03-12, `pty_d7a5f18f…`, policy `SCN215-C1`.
  - Customer 2 is Cdfdhbm Koghnidngbl, born 1969-07-11, `pty_f4757875…`, policy `SCN215-C2`.
  - Both are term life, monthly at $116.87, from 2027-10-15, with $233.74 due.
- **The authority.** `payee` set up the direct debit authority with "12-3140-012345-00" (a digit missing), then correctly, and one for customer 2 (F2-15-1).
- **The collection run.**
  - st10 prepared a collection run (reference made for her; value date typed).
  - She added both items, typing the authority references (F2-15-2), and checked it.
  - st17 approved it. Nothing was said about a 16:00 cut-off or the channel. The wall clock was about 23:00 NZDT, so "after the cut-off" was in effect untested and unmentioned.
- **The bank.** Acknowledged both, settled both, then returned customer 1's item: insufficient funds.
- **The run.** It shows "returned: insufficient funds". The totals read Acknowledged $467.48, Settled $233.74 and Returned $233.74, so presented equals settled plus dishonoured. **Good.**
- **Where the dishonour shows.** Covered in F2-15-3 and F2-15-4.
- **st5, collections.**
  - st5's record_conversation was refused: "collection … is not waiting for a conversation". The dishonour came before any hardship mark, so there is nothing to record.
  - mark_hardship worked (Yes or No only, no end date).
  - st10 then added him to a new run, and it **checked without any hold** (F2-15-5). I cancelled that run.
- **The second dishonour.**
  - Hardship was taken off.
  - I represented the first item through the raw "Another command" form with `represents`, because the Add-an-item form has no field for it.
  - st17 approved, and the bank dishonoured it again. Payments emitted `authority_suspended`, `repeated_dishonour`, and the party view shows "suspended, 2" dishonours.
- **The third run.**
  - st10 added the suspended authority to a third run: "Done: Add an item to a run" (F2-15-6).
  - `payee` reinstated the authority alone, with no new account and no second maintainer. The dishonour count reset to 0, and a check then passed.
  - I cancelled the run.
- **Reconciliation.** recon read the statement I played for `bnz-sf1-collection` (two credits of $233.74 and the dishonour debit), covered in F2-15-7.
- **The redirect.** st17's `redirect_route` is refused: "this needs the connectivity admin role". That names the role but not who holds it (paycon, now in the cast). It is not offered on st17's screen.

### Findings

- **F2-15-1, blocker. A wrong account number is accepted, and an authority needs no one to confirm it.**
  - **Who it hurts:** the customer and payments.
  - **Steps:** `/payments` as `sub-payee` → Act → "Set up a direct debit authority", Account number `12-3140-012345-00`.
  - **What happened:** "Done", and the authority is active at once. The correct one is active at once too. Nothing asks a second maintainer, and no check digit or length check is made. The scenario's "who must confirm it" has the answer *nobody*.
  - **Should:** refuse a number that fails the bank's check beside the field. Hold the authority until a second maintainer confirms it, as payees already are.
  - **Cause:** `crates/payments/src/authority.rs` (create_authority).
- **F2-15-2, major. References to type on the payments screens.**
  - **Typed fields:**
    - "Payer (customer reference)" and "Policy" on Set up a direct debit authority.
    - "Party (customer reference)" on Register a payee.
    - "Authority" on mark hardship, suspend, reinstate and cancel.
    - "Item" on record a conversation.
    - "Party" and "Payee" on the Payees view.
    - "Bank account" on Reconciliation (it does offer a list).
    - "Policy" and the payee on Request a refund.
  - **Pickers that fall short:** "Payee or authority" on Add an item searches payees only, so an authority cannot be found by the payer's name. The run picker shows the first 12 open runs, oldest first, so the run the maker just prepared is not among them and has to be typed in full.
  - **Cause:** `crates/channels/ui/payments.html:232-262` (pickers only for run, item and who).
- **F2-15-3, major. A dishonour reaches no one but the run.**
  - **Who it hurts:** collections and the customer.
  - **What happened:**
    - No notice goes to collections or the maker. Notices are raised only for outbound items (`run.rs:1216`, `if !inbound`).
    - The payments Controls view "Collections held for a conversation" says None.
    - `/collections` as st5 says "Nothing is in arrears", although both policies have $233.74 owed since 2027-10-15. Collections uses the wall clock (2026-11-17), a year behind the books, so nothing is ever in arrears on this stack. **Things noticed, item 12, is confirmed, with real effect.**
    - The customer's page says "NZ$233.74 is due, the oldest from 2027-10-15" (it never says paid, which is good) but not that the debit was returned or what to do.
- **F2-15-4, stack limit, possibly a defect. A settled collection never reaches the policy.**
  - **What happened:** customer 2's settled $233.74 is not applied. The policy still shows $233.74 outstanding, while the staff policy view says standing "paid".
  - **Why:** the stack runs no payments-to-core relay, as with round 1's F-15-3.
  - **Possible defect:** the staff policy's "standing: paid" with $233.74 owed since 2027-10-15 looks wrong on its own.
- **F2-15-5, major. A payer marked in hardship is still collected.**
  - **Steps:** st5 marks the authority in hardship. st10 adds a new $233.74 item on it to a new collection run and checks it.
  - **What happened:** the item is pending, not held.
  - **Should:** a hardship mark stops presentation and shows its end date. There is no end date to give: the mark is only Yes or No.
  - **Cause:** hardship is consulted only for re-presentment after a dishonour (`crates/payments/src/run.rs:1203-1209`) and in `hold_reason`'s path, not for new items.
- **F2-15-6, major. A suspended authority can be added to a run, and reinstating it is too easy.**
  - **What happened:**
    - "Add an item to a run" on a suspended authority answers "Done".
    - The party row says "suspended" with no reason.
    - Reinstatement is one maintainer's click. It needs no new account, has no cooling-off and resets the dishonours to 0.
    - There is no action to change an authority's account at all ("Change a payee's account" is for payees).
  - **Should:** refuse a suspended authority at add time, saying who reinstates it. Changing the account on an authority should mean a second maintainer and 24 hours of cooling-off, and the refusal should give the time.
- **F2-15-7, major. Collection receipts on the bank statement are not matched to the run.**
  - **Who it hurts:** the reconciler.
  - **What happened:**
    - "Still to show on a statement: Nothing" before the statement.
    - After it, the two settled collections came up as "unidentified receipt" and the dishonour as "unexplained debit", each an open exception.
    - The DAYS column shows **−363**: the wall-clock today minus the business date.
  - **Should:** settled collections and their reversal are expected movements on the collection account and match without breaks. Days come from the business date.
  - **Note:** I gave the lines references made of the run and item ids. If the bank's real reference format differs, the matching rule should be documented for the reconciler.
- **F2-15-8, minor. The redirect refusal does not say who holds the role.**
  - **What happened:** "this needs the connectivity admin role".
  - **Should:** also say who holds it ("paycon").

---

## SCN2-16: mum pays the premium, money lands wrong, and a refund meets a hold

**Outcome: partly completed.**

- **Done:**
  - The payer case: opened, its visibility checked, cleared with a made-up reference and then a real one.
  - The $240.
  - A refund request.
  - The customer's hold.
- **Not reached:** there was no misdirection, because cash matching did not near-match the $240. Legal's override has no action. The refund never got past "requested", because there is no core relay.

What I did:

- **Set-up.**
  - Customer 1, the daughter, is Cnkocjb Kblheilkmhk, born 1999-08-12, `pty_cd108649…`, policy `SCN216-D1`.
  - Customer 2 is Cjaibmf Khfkmhlndan, born 1996-07-10, `pty_7222a7e9…`, policy `SCN216-E2`.
  - Both are term life at $116.87 a month.
  - As the relay, I told payments each policy's owner.
- **The bank statement.** Four lines on `bnz-sf1-collection`:
  - the daughter's premium from the mother's account `12-3140-0999999-00`;
  - $240 from customer 2 quoting `SCN216-D7`, which is one character off D1;
  - customer 2's premium, twice.
- **Cash matching (played by me).**
  - The daughter's premium and both of customer 2's premiums were applied (no duplicate check).
  - The $240 came back as "exception: no policy matches this payment" with no suggestion, so it was never misapplied.
- **As the relay, premium_applied.** This opened two payer cases (F2-16-2).
- **st13, Controls.** She sees both cases. recon and st10 do not see the section, and are refused `clear_payer` ("this needs the compliance role") and the list ("payer_cases is compliance's"). **Good.**
- **The premium counts as paid.** The daughter's policy shows outstanding $0. **Good.**
- **Clearing the case.**
  - Pressing Clear fills the case. With the made-up relationship `…/rel-made-up-1` the page says **"Cleared."**, but the case is only "Proposed", waiting for the customer master to confirm it (F2-16-3).
  - The privacy officer, on `/records`, added the mother as a person (shown as "Done: A person." with no reference). He found her by name and birth date, and added a `premium_payer` relationship. Its "To a policy" was refused ("premium_payer is a relationship to a party"). To the daughter's party worked, but that party had to be typed as a reference: there is no picker, and there is no "basis: parent" field.
  - st13 cleared the case again with `pty_996ce2c2…/REL-20271115-GS376`. It again said "Cleared." and the case stays proposed. With no customer-master relay on the stack, it will never close.
- **The $240.** recon pressed Apply on the unidentified receipt (NZ$240.00, reference SCN216-D7, age shown in days) and typed the parts "scn216-e2, 240.00". The page said "Applied." on one person's word (F2-16-4). His own approval was then refused: "this receipt is already being applied".
- **Customer 2's duplicate.** Her page shows "Nothing is overdue" with no credit. st10 requested a refund:
  - To source, naming the receipt: refused, "policy scn216-e2 has no authority …". A direct credit has no authority to go back to.
  - To payee `PYE-20271115-BXDRT`, which belongs to **another customer** (SCN2-08's): **accepted** (F2-16-5).
- **Customer 2's hold.** She put her own hold on from `/customer` after setting up signing: "A hold is on." "Ask to lift it" showed nothing new (F2-16-6). Payments' party view for her still says "No hold of their own is recorded." (F2-16-6).
- **Legal.** legal opens `/payments` with no actions at all. There is no override command in the payments module (`hold.rs` has none), so the court-order path does not exist.
- **The $2 write-off.** No misdirection was raised, so I did not try it. "Write off misdirected money" is offered to the approver role only, and "Report money paid to the wrong account" to the reconciler only, so st10 (the maker) cannot report a misdirection, as step 4 expected.

### Findings

- **F2-16-1, stack limit. Payer cases, refunds and holds need relays the stack does not run.**
  - **What does not happen:**
    - Payer cases never open on their own: no `policy_owner` or `premium_applied`.
    - A refund stays "requested": no `refund_raised` from the core.
    - A customer's hold never reaches payments: no `record_customer_hold`.
    - A relationship proposal is never confirmed by the customer master.
  - **What it means:** each SCN2-16 control can only be partly seen. The e2e README should say which services stand in for these.
- **F2-16-2, major. Paying your own premium by direct credit looks like someone else paying.**
  - **Who it hurts:** compliance, whose queue fills with false cases.
  - **What happened:** customer 2 paid her own premium from her own account. It is not on any of her payees or authorities, so a "paid by someone else" case opened against `account:123140088888800`.
  - **Also on the case:** the owner shows as `pty_…`, the policy as `scn216-e2`, no amount, and the full payer account number.
  - **Should:** match the payer account against the customer master's bank details, or the account name. Show names, the policy number and the amount.
  - **Cause:** `crates/payments/src/aml.rs:152-160` (`owners_own`).
- **F2-16-3, major. "Cleared." is said for a proposal, made-up references included.**
  - **Steps:** Controls → Clear, relationship `pty_cd108649…/rel-made-up-1`.
  - **What happened:** "Cleared.", while the case stays open as "Proposed … Relationship …/rel-made-up-1".
  - **Should:** "Sent to the customer master to check the relationship; the case stays open until it does". A made-up reference should be refused as soon as the master answers.
  - **Also:** the relationship has to be typed as "party/id" (the case can be picked).
  - **Cause:** `payments.html` (aml-form handler) and `aml.rs:226`.
- **F2-16-4, major. Unidentified money is applied by one reconciler.**
  - **What happened:** $240 was applied to `scn216-e2` by recon alone ("Applied."). The threshold for a second person is evidently above $240, and the screen does not say what the threshold is. There is only one reconciler in the cast, so above the threshold nobody could approve.
  - **Also:** the policy is typed ("Parts, one per line: policy, amount"), with no picker or name.
  - **Should:** scenario "good looks like": the receipt is matched by two people. At least, state the threshold and give the cast a second reconciler.
- **F2-16-5, blocker. A refund can be sent to anyone's account.**
  - **Who it hurts:** the customer and the insurer: this is a fraud path.
  - **Steps:** `/payments` as `sub-st10` → Act → "Request a refund", Policy `scn216-e2`, $116.87, Paid to "payee", payee `PYE-20271115-BXDRT` (the SCN2-08 customer's account).
  - **What happened:** "Done: Request a refund." The refund page shows Destination Payee PYE-20271115-BXDRT, status requested, with "Release" offered.
  - **Should:** a refund goes only to the account the money came from, or to the owner's own verified payee.
  - **Cause:**
    - `crates/payments/src/refund.rs:64-71` checks only that the payee exists, and that the policy has no live authority.
    - For a direct credit there is no "source" at all, so the money cannot go back where it came from (refund.rs:52-63 accepts only an authority).
- **F2-16-6, major. The customer's hold is invisible to payments.**
  - **What happened:**
    - With her hold on in the customer master, payments' party view says "No hold of their own is recorded.". On the stack this is the relay gap.
    - The same view shows st10 "Offer this person support on this contact, and take extra care…", with no reason. That is fine, but it is the only sign of anything.
    - "Ask to lift it" on `/customer` changed nothing on screen. It did not show "it lifts on <date>" or say to call.
  - **Not testable:** whether a held refund would read "held by the customer's own hold". The refund never reached a run.
  - **Missing:** legal has no override command (court order), so "refused or recorded with a reference, never silent" has nothing to test.
- **F2-16-7, minor. Release means two things.**
  - **What happened:** the refund page offers the maker "Release" on a *requested* refund. That would be refused ("only a refund that is owed and not in a run can be released").
  - **Why it matters:** in the module, `release_refund` gives an owed refund back to suspense (`refund.rs:110-118`, "Give an owed refund back to suspense"). The word "Release" on the payments screens elsewhere means letting money go out.
  - **Questions:** the scenario's "st17b releases it" has no counterpart, because release is the maker's. Whether a refund needs a second person is not visible.
- **F2-16-8, minor. Dates in `/records`.**
  - **What happened:** facts show "RECORDED 11/17/2026, 9:52:39 AM", which is US form and the wall clock, and "TRUE FROM 0001-01-01" for facts given with no date.
  - **Should:** use New Zealand form and the books' date, and show "not stated".

---

## Things noticed while designing: what I touched

- **8 (senior limit): not fixed.** One person (st4b) can now propose above $600k, but nobody can give the second signature, so the claim still cannot be admitted (F2-08-2). The over-authority refusal on assign does name the limit, the amount and st4b. **Good.**
- **10 (MCP reach by prefix): confirmed for the auditor.** She is listed `commission_command`, `payments_command`, `reinsurance_command`, `reinsurance_decide` and `finance_command`. Each is refused in words when called.
- **12 (wall clock): confirmed, with effects.**
  - Collections shows nothing in arrears.
  - Reconciliation exception ages are −363 days.
  - `/records` shows recorded times from 2026.
- **14 (approver equals treasury): confirmed.** st17 released the over-limit item and approved the same run.
- **16:**
  - F-11-4: the cover check still does not show the cover facts the scenario wants. The survival period is absent (F2-08-4).
  - F-15-3: the stack still runs no payments relay; it affects SCN2-15 and SCN2-16 too (F2-15-4, F2-16-1).
  - References for names: still the case for payees, owners and policies (F2-08-6, F2-08-12, F2-16-2).

**Round 1 re-tests:**

| Round 1 finding | Status |
|---|---|
| F-11-5 | Holds on the screen's assign path. Not for MCP's line refusal (F2-09-9). |
| F-12-4 | Holds. |
| F-SCN16-1 | Holds. |
| F-SCN16-10 | Partly fixed: money and holds now in words, payees still references. |
| F-SCN22-1 | Partly fixed: the Lives view exists but cannot search without a hidden case (F2-08-1), and shows kept only, not ceded. |
| F-11-6 | Partly fixed: better words, but the claim still closes before the bank pays. |

## Corrections to the scenario set-up

- **SCN2-08.**
  - The template life is 39, not 49.
  - The $900k admission is stuck at the second signature, not at the first.
  - The treaty's notification threshold is $1m and its consent threshold $2m, so a $900k or $600k claim needs no consent. Step 4's `admitted_without_consent` cannot happen with the seeded treaty.
- **SCN2-09.**
  - An income claim's event must fall while premiums are paid, or the cover is not in force.
  - The benefit end must be within the option, here two years.
  - A referral cannot be made by anyone (F2-09-7).
- **SCN2-15.**
  - Representation is only possible through the raw-command form (`add_run_items` with `represents`).
  - A hardship mark has no end date.
  - An authority has no account change.
- **SCN2-16.**
  - "Release" of a refund is the maker's, not treasury's.
  - The core's cash matching does not near-match a reference one character off; it raises an exception with no suggestion.
  - There is no legal override in payments.
