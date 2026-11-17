# Operator op-a: SCN-01, SCN-02, SCN-04, SCN-03

Scripts and screenshots were kept outside the repository.

## SHARED STATE WARNING (read first)
**Clearwater (`fap-d9`) is still SUSPENDED and I could not put it back.** I recorded the suspension on screen as `st13`, effective 2027-11-15, source `SCN03-FMA-REG`. Restoring it on screen fails both ways:
- effective 2027-11-15 is refused with "fap-d9 already has a licence status effective 2027-11-15; record the change on another date". This happens even with the same source reference.
- effective 2027-11-16 is refused with "a licence status is recorded once it is in effect".

So nobody can restore it until a business day is closed, and I am not allowed to close one. `adv-d9a` stays read-only for anyone who runs after me. Whoever closes the day (SCN-14) or the stack owner should record `active` the next day. See F-SCN03-1.

My records, all prefixed:
- SCN01 customer `pty_14fe11e17e834641badbd9d0dad834c5` (Ckmdlng Kaocagbejdb, born 1986-01-18), policy SCN01-POL-MWK6X6LA-2.
- A second SCN01 customer `pty_4a3fda2147d44ec49a2daf9d337a7324`, policy SCN01-POL-MWK6WXCK-2. My script accidentally ran twice. That customer declined the stray cancel request.
- SCN02 customer (no phone), policy SCN02-POL-MWK71QJS-2.
- SCN03 customer `pty_7dfa4ba715794252bccca3a493bc7ab2` (Cmchbio Kickfaadhdp, born 1991-06-12), policy SCN03-POL-MWK73ZPL-2. It is now appointed to `adv-d1`.

Set-up that went through the API, as the e2e lib does: customer-master records, subject binding and phone enrolment (`lib/customer.js`), `POST /v1/policies`, and the phone's signature approving a browser. Every decision went through the screens or `/mcp`.

---

## SCN-01 Adviser prepares the wrong change, takes it back, client approves the right one: PARTLY (the approved change never takes effect)

**What I did:**
1. The customer appointed `adv-d1` / `eng-d1` from /customer.
2. Tessa (`sub-d1`) opened the policy in her book and sent "Cancel policy".
3. She found no way to withdraw it on screen, so I withdrew it with MCP `withdraw_request` as `sub-d1`.
4. She sent "Change payment frequency" set to Fortnightly on screen.
5. The customer, on a fresh browser, pressed Approve and was refused. She set up signing (Signing tab, phone approval) and double-pressed Approve.
6. On the second customer, the client declined a cancel request through the page's own dialog.

**Outcome:**
- Approval works, and the double press caused no error and only one approval.
- **The policy never changes.** The request sits at `pending` with `route_to_person: true` and nobody can carry it out.
- The policy screens show no payment frequency anywhere, so neither person can check it.
- The expected fortnightly amount from `oracle.premium` is NZ$53.94 (1/26 of the loaded annual, fee NZ$2.77). The current monthly bill is NZ$116.87, which matches the oracle.

### F-SCN01-1 (blocker) An adviser's change approved by the customer is never carried out
- **Who it hurts:** every customer and adviser. This is the whole purpose of adviser-prepared changes.
- **Steps:**
  1. The customer appoints `adv-d1` / `eng-d1`.
  2. `/adviser` as `sub-d1`: open the policy, Change = Change payment frequency, Fortnightly, "Send to customer".
  3. `/customer`: set up signing, then Approve on the To approve tab.
  4. `GET /app/v1/requests` as `sub-d1`.
- **What happened:** the request is `"status":"pending","signed":true,"approved_by":<customer>,"route_to_person":true`. Minutes later it is still pending, and the policy is unchanged (still billed NZ$116.87 monthly). The same happened to `adv-d9a`'s decline-indexation request in SCN-03.
- **Should:** the request is carried to the core when the customer approves it, and the request shows "approved / done".
- **Cause:**
  - `submit_request` asks the customer master for the routing of the person who submits (`http.rs:1049-1053`). For an adviser that is `adv-d1`, which is not in the master: `GET /v1/parties/adv-d1/routing` gives 404.
  - `customers::route_to_person` (`customers.rs:319-324`) turns any non-200 answer into `true`.
  - `ApproveRequest` ORs the flag (`state.rs:1174`), so it never clears.
  - `servicing::carried_by_sweep` (`servicing.rs:120-123`) then never carries the request.
  - The only remaining route is `/staff/v1/servicing/{id}/carry-out`, which needs `servicing.carry_out`. No role in the cast or seed holds that action (it appears only at `servicing.rs:60`), so the request is stuck for good.
  - E2E-331 only checks that nothing is left waiting, so it misses this.
- **Fix direction:** route on the policyholder's master record (the approver's), not on the adviser's.

### F-SCN01-2 (major) The adviser cannot take back a request on screen
- **Steps:** `/adviser` as `sub-d1`, send Cancel policy, then look at "Requests you have made".
- **What happened:** the table has Request (a UUID), Policy, Change and State, with no button. The route exists (`DELETE /app/v1/requests/{id}`): MCP `withdraw_request` as `sub-d1` returned `accepted` and the state became `cancelled`.
- **Should:** a Withdraw button on rows that are still awaiting approval, with a confirm step.
- **Where:** `crates/channels/ui/adviser.html:77`.

### F-SCN01-3 (major) Neither list says what the change is, or who asked for it
- **What happened:**
  - Adviser: "change frequency | awaiting approval". It does not say "to fortnightly".
  - Customer, To approve: Policy, Change "change frequency", Asked. It does not say the new frequency, the new amount, or that Tessa Maddox prepared it.
  - The adviser's "Details" note is never shown to the customer.
- **Should:** "Pay fortnightly instead of monthly: NZ$53.94 a fortnight from …, prepared by your adviser Tessa Maddox".
- **Where:** `customer.html:179`, `adviser.html:77`.

### F-SCN01-4 (major) After approval, nobody can see what happened
- **What happened:**
  - The adviser sees "change frequency | pending". She cannot tell whether it is approved, carried or failed.
  - The customer's "Your requests" says "Nothing to show." both before and after approving, because adviser-prepared requests are never listed there. There is no history of what she approved.
  - No screen shows the policy's payment frequency, so the result cannot be checked.
- **Should:**
  - The adviser sees "approved by the customer on …, done".
  - The customer has a history entry.
  - The policy shows "Pays: fortnightly, NZ$x".

### F-SCN01-5 (minor) A withdrawn or declined request reads as "cancelled"
- **What happened:**
  - Withdrawn by the adviser and declined by the customer both show as "cancelled". The decline dialog says "Whoever prepared it is told", but Tessa only sees "cancel policy | cancelled", which reads as if the policy was cancelled.
  - The decline dialog for a "cancel policy" request has the buttons "Cancel" and "Decline", which is confusing.
- **Should:** use "withdrawn by you" and "declined by the customer".
- **Where:** `customer.html:256-260`, `adviser.html:77`.

### F-SCN01-6 (minor) Codes and references shown instead of words
- The customer must type the adviser's party code and engagement reference (`#ap-adviser` "Adviser reference", `#ap-engagement`). The adviser is then listed as "adv-d1", not "Tessa Maddox".
- Product shows as "term-life", the life as "life-1", the provider as "fap-d6".
- The adviser's policy detail is a raw "What / Details" dump: "cover / Layer 1", "billing / Receivable 1", "policy / Indexation declined –".
- Request references are full UUIDs.
- **Where:** `adviser.html` `open()` (the `others` table) and `customer.html`.

### F-SCN01-7 (minor) "PAYMENTS paid" shown next to "NZ$116.87 is due, the oldest from 2027-11-01"
- **Where:** customer and adviser policy views. The policy row has `standing: "paid"` while `outstanding_minor` is 11687.
- **Should:** say "due", or "within grace until …".

### F-SCN01-8 (minor) Dates use the machine clock in US format
- **What happened:** To approve shows "Asked 11/17/2026" and the adviser detail shows "Submitted 11/17/2026, 4:26:40 AM".
- **Should:** the business date (2027-11-15) in the NZ format.
- **Where:** `customer.html:179` `toLocaleDateString()` on `submitted` (wall clock).

**Good:**
- The unsigned approval says exactly what to do: "Approving needs your signature. Set up this browser under Signing, or approve it on your phone."
- The double press gave no error and one approval.
- Tessa saw no health details. Claims show by status only.

---

## SCN-02 Adviser's AI assistant does the Monday book review: COMPLETED

**What I did:** called `initialize` and `tools/list` as `sub-d1`. The list is saved in `scn02-tools-d1.json`.

**(a) Names listed (46):**
- Account and profile: status, my_account, my_profile, change_my_profile, export_my_data, my_spending_hold, my_document.
- Applications: disclosure_to_sign, submit_disclosure, consent_to_sign, give_consent, my_proposals, offer_to_sign, answer_offer, requote, accept_requote.
- Membership and continuations: claim_membership, my_continuations, take_up_continuation, replacement_statement.
- Policies, advisers and representation: my_policy, represented_person, represented_policy, appoint_adviser, remove_adviser, family_consent, set_delegate.
- Requests and devices: my_requests, make_request, withdraw_request, approve_request, my_devices, revoke_device.
- Adviser: policy_claims, adviser_book, adviser_held_balance, adviser_practice, adviser_statement, adviser_replacements, dispute_replacement.
- Employer: employer_schemes, set_scheme_admin, set_second_admin, scheme_command, upload_membership_file, scheme_question.

**(b)** No staff or operations tool is listed. Staff sign-ins (`sub-st5b`) are listed 116 tools: the 70 staff tools plus all 46 customer-realm tools.

**(c)** `remove_adviser`, `withdraw_request` and `revoke_device` are marked `destructiveHint: true`.

**Results:**
- `my_account`, `adviser_book`, `adviser_held_balance`, `adviser_replacements` and `policy_claims` all work.
- `make_request` (body form) is accepted, and the retry with the same key gives `duplicate: true`, done once. The request appears on /adviser and on the customer's policy as "awaiting approval", exactly like one made on screen.
- `approve_request` by the adviser is refused ("no such request", or "step_up_failed: that is not yours to do" with a forged signature).
- `appoint_adviser` and `remove_adviser` on another customer's policy, and on a policy that does not exist, both give "only the policyholder may do this". The answer is the same either way, so nobody can probe whether a policy exists.
- `my_policy` and `policy_claims` on another customer's policy and on a non-existent one both give 404 "no such resource".
- `scheme_question` and `scheme_command` on `pol-kaifresh` give 404, the same as a missing scheme.
- `find` and `staff_policy` (not listed) give "this tool needs a sign-in you do not hold".
- An unknown tool gives JSON-RPC -32602. There were no 500s.
- `adviser_book` as `sub-d2` shows none of Tessa's policies.
- I withdrew all the SCN02 requests afterwards.

**Set-up correction:** the scenario's argument shapes are wrong. Use:
- `make_request {body:{id,policy,kind,module,payload}}`
- `appoint_adviser {policy, body:{adviser,engagement}}`
- `approve_request {id, body:{signed}}`
- `scheme_question {policy, question: scheme|invoices|file|member}`
- `adviser_statement {number: "<string>"}`

### F-SCN02-1 (major) `make_request` accepts any kind and sends it to the customer to approve
- **Steps:** `tools/call make_request` as `sub-d1` with `{body:{id:"scn02-req-…-c", policy:<book policy>, kind:"change_sum_insured", module:"policy-servicing", payload:{…}}}`.
- **What happened:** `accepted: true`. The customer's policy then shows "change sum insured, awaiting approval", a change that can never be carried (`servicing.rs` KINDS has three entries, and anything else gives `not_carried`). The tool's own description suggests `change_sum_insured` as its example (`mcp.rs:285`).
- **Should:** refuse unknown kinds at submission and name the allowed ones (decline_indexation, change_frequency with the frequency values, cancel_policy). Make `kind` and `payload` an enum or schema in the tool.

### F-SCN02-2 (major; finding only, per the scenario) The tool list is by sign-in, not by role
- An adviser is offered the six employer tools, the application tools and set_delegate.
- Every staff member is offered every staff tool plus all customer tools (116).
- Calls are checked, so no listed tool worked outside the adviser's job. This confirms "Things noticed" item 3.

### F-SCN02-3 (minor) `adviser_statement` errors are not in plain words
- An integer number gives "number is required" (it was given, just as a number; the screen field is numeric).
- "abc" gives "Refused (400): Invalid URL: Cannot parse `abc` to a `u32`".
- A statement that does not exist gives "Refused (404 not_found): no such resource". It should say "You have no statement number 7; your statements are 1 to n".

### F-SCN02-4 (minor) Reusing an idempotency key for a different request is reported as accepted
- **What happened:** the same key with a different request id gave `{accepted:true, duplicate:true}`. Nothing was created, but the assistant is told it worked.
- **Should:** refuse with "this key was used for a different request".

### F-SCN02-5 (minor) Validation runs before the permission check
- `set_scheme_admin` on `pol-kaifresh` by an adviser answers "party is required" before saying whether she may act at all.

---

## SCN-04 New application: adviser submits, underwriter decides, customer accepts (probe): BLOCKED at step 3 (as expected)

**What I did:**
- `/adviser` as `sub-d4`.
- MCP as `sub-d4`.
- MCP `quote` as `st1`, `st2` and `st5b`, for the `term_life` template and for `aia-essentials-life`.
- `/underwriting` as `st1`: every tab, plus a mistyped proposal.

### F-SCN04-1 (blocker; confirms "Things noticed" items 1 and 4) Nobody can quote or start an application
- `d4` has no quote or application tool. Calling `quote` gives "this tool needs a sign-in you do not hold". `my_proposals` gives `[]`.
- `quote` as `st1`, `st2` or `st5b` gives "Refused (403 not_authorised): None of your roles allows this action. Ask your manager for the role or a one-off grant." This is true for the term_life template and for aia-essentials-life. No role holds `new_business.quote` (`staff.rs:45`).
- Operations (`st16`) is not listed the tool.
- There is no proposal route, so steps 4 and 5 (evidence, loading, routing to st2, offer, accept) cannot be done.
- **Test-stack limit:** the underwriting intake is not running, so every queue shows 0 ("Things noticed" item 2 confirmed).
- The oracle gives NZ$69.43 a month for a $400,000 life-only term_life for a 41-year-old female non-smoker. Nothing to compare it with.

### F-SCN04-2 (major) The underwriter must type every reference and code
- **Where:** `/underwriting` as `st1`.
- **What happened:**
  - Cases: "Case" is a typed field even though the page lists "Cases you hold". "Exclusion code" and "Reason codes" are free text.
  - Proposals: "Proposal", "Life" and "Purpose" are typed.
  - Conflicts: "Person you are connected to" is typed.
  - The page declares `reasons` and `exclusions` arrays but never fills or uses them, so the reason and exclusion libraries are not offered.
- **Where:** `underwriting.html:90` and `underwriting.html:124`.
- **Should:** pick the case from the queue and the codes from the libraries (staff-ui-review).

### F-SCN04-3 (minor) An adviser who is not engaged gets advice meant for staff
- **What happened:** `/adviser` as `sub-d4` shows "The adviser portal is for advisers and advice providers, and you are not engaged as either … ask a staff administrator for the role."
- **Should:** an external adviser cannot ask a staff administrator. Tell her to contact the insurer's distribution team or her provider to set up her engagement.
- Confirms "Things noticed" item 5: `d4` is not in the distribution structure.

### F-SCN04-4 (minor) The `quote` tool's input does not match its description
- The description puts `case` in the query. Passing `case` at the top level gives "case is not an argument of quote", and it must go under `query`.
- The refusal "Ask your manager for the role" is not actionable, because no role carries the action.

**Good:**
- A mistyped proposal gives "Nothing was found with that reference. Check it for a slip and try again; if it is right, it may not be open to you."
- Empty queues say so in words.
- "Things noticed" item 7 is partly confirmed: `products/aia_essentials_life.star` exists but there is no template for it and no quote route to try it.

---

## SCN-03 An advice provider loses its licence mid-week: PARTLY (licence not restored, see the warning above)

**What I did:**
1. The customer appointed `adv-d9a` / `eng-d9a`. `d9a` prepared "decline indexation" on screen and left it waiting.
2. `comops` tried to record a licence and was refused. `st13` recorded the suspension on /commission ("Done.").
3. `d9a` refreshed and got a read-only banner. Send is disabled. MCP `make_request` gives "adviser_read_only".
4. The customer approved the in-flight change (accepted), appointed `adv-d1` / `eng-d1` and removed `adv-d9a`.
5. `d6` sees SCN03-POL under adv-d1 and nothing of Clearwater's.
6. Position for `fap-d9` shows the licence history and the conduct case `conduct-fap-d9-2027-11-15`.
7. The restore was refused twice.

### F-SCN03-1 (blocker for compliance; shared-stack impact) A licence cannot be restored on the day it was suspended, nor ahead of time
- **Steps:** `/commission` as `sub-st13`, Change something, Record a licence state, Party `fap-d9`, Licence is `active`, Effective 2027-11-15 (or 2027-11-16).
- **What happened:**
  - Effective 2027-11-15 gives 409 "fap-d9 already has a licence status effective 2027-11-15; record the change on another date". This happens even with the original source reference.
  - Effective 2027-11-16 gives 409 "a licence status is recorded once it is in effect".
  - So a mistaken or same-day-reversed suspension cannot be corrected. The adviser stays read-only at least until the next close, with no override and no correction path.
- **Cause:** `crates/distribution/src/structure.rs:266-284`. The doc comment says "a record for a date already on file replaces it only if the source is the same notice", but the code accepts only an identical record. There is also a separate in-effect check for future dates.
- **Should:** allow a correcting record for the same date from the same notice, or a "correct a mistaken entry" action with a second person.

### F-SCN03-2 (major) The customer approves a change from a suspended adviser without warning, and it stalls
- **What happened:** To approve shows "decline indexation, Approve / Decline" with nothing marking it. Approve says "Approved: decline indexation." The request then sits `pending` for good (the same cause as F-SCN01-1).
- **Should:** either refuse with "Your adviser's licence is suspended; this change can't go ahead. Appoint another adviser or ask us", or mark the change as from a suspended adviser before the customer approves.

### F-SCN03-3 (major) The provider picker is empty: compliance must type `fap-d9`
- **What happened:** typing "Clear", "fap" or "d9" in the Party combobox gives "Nothing matches that."
- **Cause:** the picker lists only parties "the page has seen in the module's answers" (`commission.html:51-61`). On a fresh page it has seen none.
- **Should:** search the module's parties by name ("Clearwater Advice Ltd").

### F-SCN03-4 (minor) The read-only banner says neither which licence nor since when
- **What happened:** the banner says only "Your access is read-only…". The reason and date appear only on the Engagements tab, as "licence of fap-d9 is suspended", a code rather than "Clearwater Advice Ltd".
- The disabled "Send to customer" button has no reason next to it.
- **Where:** `adviser.html` `load()` and `standing()`.

### F-SCN03-5 (minor) Commission ops is offered an action it cannot do, and the refusal is a code
- **What happened:** "Record a licence state" is in `comops`'s Action list. Sending it gives "none of your roles allows licence.record", with no next step.
- **Should:** hide the action, or say "Licences are recorded by compliance."

### F-SCN03-6 (minor) The record of what was done is thin
- "Done." after recording the licence does not repeat what was recorded.
- The licence history (position) has State, Effective and Source ref, but not who recorded it.
- The conduct case's POLICIES column is empty, although `adv-d9a` had SCN03-POL at the time.
- "Every step in the history with who did it" is not met on these screens.

### F-SCN03-7 (minor) The customer is told little about the suspended adviser
- **What happened:** the adviser is shown as "adv-d9a, Not available: contact us". Nothing says why, or that they can appoint another adviser here.
- Appointing another adviser and removing the old one worked at once, so the customer is not stranded (good).

### F-SCN03-8 (minor) A policy reference shows instead of its number
- **What happened:** after the policy left `d9a`'s book, her "Requests you have made" shows the raw `scn03-pol-mwk73zpl-2` instead of the number, and the state is still "pending".
- **Where:** `adviser.html:76-77` builds its number lookup from the current book only.

**Not checked:** held lines under "Held from you". The new policy earned no commission, so nothing was held ("Nothing is being held from you."). I could not test "why it is held".

---

## "Things noticed while designing": status of the items I touched
1. **Confirmed.** No path anyone can use: quote is refused for st1, st2 and st5b, and no adviser screen or tool exists.
2. **Confirmed for underwriting.** All queues show 0.
3. **Confirmed.** The adviser is listed 46 customer-realm tools including employer tools. Staff are listed 116 (70 staff + 46 customer). Calls are checked by route.
4. **Confirmed for `new_business.quote`.** Also new: no role holds `servicing.carry_out`, which is fatal together with F-SCN01-1.
5. **Confirmed for d4.**
7. **Partly confirmed.**

## Test-stack limits (not product defects)
- The underwriting intake is absent, so queues are empty.
- The phone app is played by Node crypto.
- `oracle.js:109` has no quarterly or half-yearly periods, although the adviser screen offers them.
