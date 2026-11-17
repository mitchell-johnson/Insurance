# Operator op2-f: SCN2-24, SCN2-23 and SCN2-14

Stack: http://127.0.0.1:17090. Wall clock: 2026-11-17. Books' business date: 2027-11-15. I did not move the date, stop any node or run any e2e test. `mcp.js` sends one JSON-RPC call, `shot.js`, `rec2.js`, `staff2.js` and `cpl.js` drive the screens with Playwright, and the `s-*.js` files are the steps.

## State I changed that others might notice (read first)

- **st10.** As admin I assigned st10 the `payments-approver` template to prove F2-24-2, then removed it within a minute. Removing it revoked st10's sessions once (`sessions_revoked`). Anyone signed in as st10 at that moment has to sign in again. st10's templates are back to `held-balance`, `ledger-operator` and `payments-maker`.
- **n7's connection to cust-p60.** It was removed by admin (step 5) and then declared again by n7 on MCP, as the seed does. Restored.
- **admin is now "connected" to cust-p60. NOT RESTORED, and it cannot be.** I sent `declare_connection` as admin, thinking it would put n7's connection back. It declared one for admin herself. Only `staff.manage` may remove a connection, only admin holds it, and nobody may remove their own. The connection is stuck, which is finding F2-24-9. The effect: admin cannot read cust-p60's record. She holds no customer actions anyway.
- **Staff records.**
  - `newstarter` was employed (team claims, manager st4, template `claims`), then ended.
  - `nobody-scn224` was employed by mistake-proof test (it does not exist anywhere), then ended.
  - The staff profile `scn224-junior` v1 is active and assigned to nobody (assisted_change 25000 cents, complaints_authority 1). I used a new profile so that the seeded `junior` profile was not touched.
- **Grants.** `SCN224-g2` (cover, st5b for st5) was approved, then revoked. `SCN224-self` (admin, one-off `customers.view` on cust-p60) was approved by admin2, then revoked. `SCN224-finance.approve` was declined.
- **Customers and policies I made.** All of them were made through `customer()` in e2e/lib/customer.js, with a phone key.

  | Who | Party | Name | Born |
  |---|---|---|---|
  | SCN223 M, the mother | `pty_92629e1b717341f7a16ed204acc485bc` | Cphikdk Kgdffjcdmke | 13/09/1971 |
  | SCN223 D, the daughter | `pty_f08ed079974a4e1b85e90f3e89a40842` | Conlmhn Kfanilhdoad | 11/08/1990 |
  | SCN214 R, the customer | `pty_ee51bc6ed5ff4954a3a69178da10028a` | Cpldcbo Kekokancmkm | 12/02/1983 |
  | SCN214 P, the prospect | `pty_7cb9095c36894d13b8175ce5bbc363c9` | Clbpmhl Keomkoihoaj | 17/03/1972 |

  - Policies, issued with `play.js issue`:
    - `scn223-m1`, term_life, for M
    - `scn214-r1`, term_life, for R
    - `scn214-r2`, aia_cancer_care, for R
  - Claims, notified by st4 through `/staff/v1/claims/commands` as set-up:
    - `SCN223-CLM1` on `scn223-m1`
    - `SCN214-CLM1` on `scn214-r1`, assigned and withdrawn
  - Customer master set-up, sent as privacy through the route the page uses, because the screen cannot do it (F2-23-1, F2-23-2):
    - M's two postal addresses
    - attorney relationship `SCN223-POA1` on M's record, which turned out to point the wrong way and was ended
    - attorney relationship `SCN223-POA2` on D's record, pointing to M: basis enduring_power_of_attorney, scope view and complain
- **Left in place:**
  - M's capacity is still **in question**. Nobody can settle it (F2-23-6), so D's view of M's policy stays refused.
  - Complaint `SCN223-C1` is decided, with an ex gratia of $1,500 recorded. It carries one bogus representative, which is the test for F2-23-8.
  - R and P are `exited` in the customer master.
  - The legal hold on R was placed and then released.

---

## SCN2-24: the staff administrator's assistant sets up a starter and tidies access

**What I did:** Everything went through MCP as admin, admin2, st5b, st10, st15 and n7, then I compared with `/staff` as admin and st5b. Before starting I read the `StaffCommand` enum (crates/channels/src/staff.rs:437), because the tool's description gives no command shapes.

**Outcome:** partly completed. Steps 1 to 6 and 9 ran. Step 7 (break-glass) can only be refused, as the scenario expected. Step 8 recertification is impossible (F2-24-6), and "suspension" is only possible as ending the employment.

**Tool lists:**
- admin: `status`, `staff_me`, `staff_admin`, `staff_access`, `second_approval`, `changes_for_second_approval`, `colleagues`. This is exactly what the scenario expected, with no claims, customers or payments tools.
- admin2: `status`, `staff_me`, `staff_admin`, `staff_access`, `colleagues`.
- st5b: the same list before the cover grant, while it was active, and after it was revoked. That is right for a cover grant, which passes paths and not authority (see F2-24-3).
- The new starter, once employed: the claims tools were added. After `end_employment`, the leaver was offered 35 customer-app tools (F2-24-10).

**What worked:**
- **Employing.** `employ` is one call. A repeat with a fresh key is refused with "422 already_staff: that person is already a member of staff", and an unknown template with "422 unknown_template: there is no template claim-assessor".
- **Grants.** A requester approving their own grant is refused with "maker_checker: a grant is approved by someone other than its holder and requester". admin's own grant was approved by admin2, so item 13 of "Things noticed" is now **refuted** by the stack addition. Revoking works.
- **Separation of duties at request.** `payments.approver` for st10 is refused when asked, with both duties named: "st10 holds payments.maker, which is kept apart from payments.approver: someone who does not hold payments.maker takes this on". **The F-SCN16-7 fix holds at request time**, but see F2-24-2.
- **Configuration maker-checker on the proposer's side.** st15 naming herself as approver is refused with "maker_checker: configuration needs a second person: the proposer cannot approve". admin, who holds no `staff.configure`, is refused with "403 the proposer may not change staff configuration".
- **Break-glass.** It is refused with "422 not_break_glass: that template cannot be taken under break-glass".

### F2-24-1: configuration "approval" is the proposer's word: one person activates a profile (or a template, conflict, settings or mapping)
- **Severity:** blocker
- **Who it hurts:** the insurer's access control, auditors and the regulator. Four-eyes on staff configuration does not exist.
- **Steps:**
  1. Over MCP as `sub-st15`, call `tools/call staff_access` with `{"body":{"activate_profile":{"profile":{"id":"scn224-junior","version":1,"limits":{"complaints_authority":1,"assisted_change":25000}},"approved_by":"admin2"}}}`.
- **What happened:** `accepted: true`. The profile is active. admin2 never saw anything and never approved anything. admin2's tools list no pending configuration, and her `staff_admin` shows none.
- **What should happen:**
  - The proposal waits as "waiting for admin2 (or anyone holding staff.approve_configuration)".
  - It takes effect only when the approver acts, as the channels countersign module does for reinsurance and underwriting.
  - It then shows both names and the old and new limit in dollars.
- **Cause:** `require_config_approval` (crates/channels/src/staff.rs:895) only checks that the named `approved_by` holds the action. The same pattern applies to `DefineTemplate`, `DeclareConflict`, `ChangeSettings` and `ActivateMapping` (lines 1043 to 1084 and 1446). `SetProfile` (line 1170) also takes `approved_by: admin2` on admin's word. That makes `staff.approve_profile_uplift` a name to quote, not an approval to give. `countersign::RULES` (crates/channels/src/countersign.rs:30) has no entry for the staff module.

### F2-24-2: the administrator joins maker and approver by assigning a template, which the grant check is meant to prevent
- **Severity:** major
- **Who it hurts:** payments control. The F-SCN16-7 fix is bypassed "by another route".
- **Steps:**
  1. As `sub-admin`, send `staff_access` `{"assign_template":{"party":"st10","template":"payments-approver"}}`.
- **What happened:** It was accepted. `staff_me` for st10 then listed both `payments.maker` and `payments.approver`. I removed the template again straight away.
- **What should happen:** The same `separation_of_duties` refusal that `request_grant` gives, naming the two duties.
- **Cause:**
  - `AssignTemplate` (staff.rs:1136) never consults `SEPARATE_ACTIONS` (staff.rs:30). Only the `OneOff` grant kind does (staff.rs:957).
  - `Employ` does not check them either.
  - The `held-balance` template itself carries both `held_balance.prepare` and `held_balance.approve`. This may be intended per item, but is worth a look.

### F2-24-3: "cover collections for a fortnight" cannot be expressed: no grant gives a role for a period
- **Severity:** major
- **Who it hurts:** the staff administrator, and the service officer who covers.
- **Steps:**
  1. As `sub-st5b`, request a `one_off` of `payments.collections` for 14 days. It is refused with "422 too_long: a one_off grant may last at most 86400 seconds". The one-off must also name one record.
  2. A `cover` grant of st5 for 14 days is accepted and approved. st5b's tool list and actions do not change, because cover passes paths and not authority.
- **What should happen:** A time-boxed role grant, meaning a template for a period, that expires on its own. The only route today is `assign_template`, which never expires and needs someone to remember to remove it.
- **Cause:** `GrantKind` (staff.rs:135) has no such kind.

### F2-24-4: `staff_access` leaves the assistant guessing every field
- **Severity:** major
- **Who it hurts:** anyone using the assistant, which is part (d) of the MCP checks.
- **What happened:**
  - The tool's description is `{"<command>": {...}} as the staff module takes it`, with no command names and no shapes. I had to read the source.
  - The grant kind is internally tagged under a field also called `kind`: `"kind":{"kind":"cover","covered":"st5"}`. My first attempt, `{"cover":{...}}`, was answered "missing field `kind`".
  - `from` and `to` are Unix epoch **seconds** on the **wall clock** (2026), while the books are in 2027. Nothing says so.
  - `staff_admin` answers raw JSON with epoch seconds for every date.
  - Limits are in cents with no unit: "assisted_change of 25000 is above the ceiling".
  - A misspelt command is answered with a serde list that includes `bootstrap`, `record`, `external_sync` and `external_elevation`. Those are internal commands nobody types.
- **What should happen:**
  - The description lists the commands a person can send, with an example of each.
  - Dates are NZ dates, money is in dollars, and kinds are flat.

### F2-24-5: `employ` takes any string as a person and any string as their manager
- **Severity:** major
- **Who it hurts:** the integrity of the staff directory.
- **Steps:**
  1. As `sub-admin`, send `{"employ":{"party":"nobody-scn224","team":"claims","manager":"st4","templates":["claims"]}}`. It was accepted. (I ended that employment afterwards.)
- **What should happen:** Refuse a party that is not bound to a sign-in or not in the workforce directory, and a manager who is not current staff. Employing with `administrator` or `access-approver` should also need a second person.
- **Cause:** `StaffCommand::Employ` (staff.rs:1085).

### F2-24-6: nobody can recertify seeded staff, and a manager cannot be set after employment
- **Severity:** major
- **Who it hurts:** the administrator and auditors. "My access" shows "recertification due 16 May 2027" for everyone.
- **Steps:**
  1. As `sub-admin`, send `{"recertify":{"party":"st5b","confirmed":["service"]}}`. It is refused with "403 only the person's manager recertifies their access".
- **What happened:** Every seeded member has `manager: null` (see `staff_admin`), and no command sets a manager after `employ`. There is also no recertification of a *grant*, which step 8 of the scenario asks for.
- **What should happen:**
  - A `set_manager` command for the administrator.
  - A fallback recertifier (the administrator) when there is no manager.
  - A refusal that says what to do next.

### F2-24-7: removing a declared connection asks no reason and leaves nothing visible
- **Severity:** minor
- **Steps:**
  1. Send `{"end_connection":{"staff":"n7","party":"cust-p60"}}`.
- **What happened:** It was accepted with `effects: []`. `EndConnection` has no reason field (staff.rs:463). `staff_admin` and `/staff` "Access administration" do not show connections at all, so the administrator cannot see what she removed.
- **What should happen:** A reason is required, the removal is logged with who did it, and the screen shows it.

### F2-24-8: the "Access administration" screen is thin compared with the tools, and an empty tab is shown to people without access
- **Severity:** minor
- **Steps:**
  1. On `/staff`, sign in as `sub-admin` and open the "Access administration" tab.
  2. Sign in as `sub-st5b` and open the same tab.
- **What happened:**
  - For admin, the grants table shows only GRANT, HOLDER and STATE. It has no kind, reason, dates, requester or approver.
  - It has no staff list, no employ form, no templates, no connections and no break-glass review list. "Owner" is shown as "–".
  - For st5b, the tab is offered. Behind it is a 403 on `GET /staff/v1/admin`, a console error, and two bare headings with no words.
  - "My access" for st5b shows "Grants: Nothing to show", although SCN224-g2 had been approved and revoked.
  - Nothing on screen lets a person ask for a grant.
- **What should happen:** The screen matches the tools, and the tab is hidden from anyone who holds no administration action.

### F2-24-9: a declared connection can never be removed from the only administrator
- **Severity:** minor
- **Steps:**
  1. As `sub-admin`, send `{"declare_connection":{"party":"cust-p60"}}`. It is accepted.
- **What happened:** Only `staff.manage` may end a connection, never for oneself, and only admin holds it. (This is left on the stack.)
- **Related:** the refusal of break-glass does not say who could enable it. The scenario wanted the review list to read "empty in words", but there is no list of break-glass uses at all, either in `staff_admin` or on screen.

### F2-24-10: smaller items
- **Severity:** minor
- **Items:**
  - A reused idempotency key with a different body is answered as a duplicate success. `employ` with key `SCN224-employ-1` and a different template and manager gave `duplicate: true`. **F-SCN02-4 is reproduced** (item 16).
  - A grant request's answer names the action needed (`approver_action: staff.approve_grant`) but not who holds it.
  - `colleagues` lists only references (`st4`) with no names (item 16).
  - After `end_employment`, the leaver's sign-in is offered 35 customer tools (`my_account`, `make_request`, and so on) although they were never a customer.
  - Before being employed, `staff_me` from `sub-newstarter` over MCP answers "this tool needs a sign-in you do not hold" rather than `{"staff":false}`.
- **Where `staff.approve_profile_uplift` is needed:** `set_profile` requires the named `approved_by` to hold it. admin2 holds it, but see F2-24-1: it is only quoted, never exercised.

---

## SCN2-23: a daughter with a power of attorney complains about a privacy breach

**What I did:**
- **Set-up:** made M and D, M's policy and claim, and M's postal history with an old flat from 2020 and Hamilton from 2026-10-17.
- **Attorney:** recorded D as attorney for M. I tried on `/records` as privacy, which failed (F2-23-2). Then I used the same route with scope and basis.
- **Calls and complaint:** front searched on `/staff`. st5b checked the caller on screen. front recorded the complaint on `/complaints`.
- **Concern and marker:** privacy recorded the concern on `/records`. st6 set the marker on `/staff` "Markers to review".
- **Handling:** st14j tried to take the complaint and st14 took it. I followed the claim link. st14 added the representative, decided "upheld" and recorded the $1,500 ex gratia.
- **Capacity:** privacy questioned M's capacity, and D's MCP `represented_policy` was then refused. Settling capacity was not possible.

**Outcome:** partly completed. Blocked at:
- step 6 (nobody can settle capacity)
- step 5 (no letter is ever issued on the stack, so there is no delivery record to read)
- step 8 (the customer has no complaints view)

**What worked:**
- **Caller check on the staff console.** The caller is picked by name and birth date ("Conlmhn Kfanilhdoad, born 11 Aug 1990 · 0 policies"). Once the authority was recorded the right way round, it answered "You may discuss this policy with the caller. This caller holds an authority to be told about this policy…".
- **Caller check while capacity is in question.** It answered `refer` with words naming legal. D's `represented_policy` answered "403 authority_under_review: your authority to act is being reviewed; we will be in touch". That is words, not a bare 403, **as hoped**.
- **The marker takes the file from the junior.** It is refused with "a complaint with a vulnerability marker goes to trained staff". This partly meets F-21-4: it says *what* kind of handler, not *who*.
- **The claim link from the complaint.** It opens `/claims#?claim=SCN223-CLM1`, and for st14, who has no claims role, the page says "This page is not open to you. Your roles do not include claims. If you need it for your work, ask a staff administrator for the role." **F-21-10's fix holds** for the refusal case.
- **Ex gratia.** It is shown as "NZ$1,500.00" when entered as 1500 in "Amount".

### F2-23-1: a postal address can never be recorded from Customer records
- **Severity:** major
- **Who it hurts:** the privacy officer, and every customer whose address changes.
- **Steps:**
  1. On `/records`, sign in as `sub-privacy`, open Cphikdk Kgdffjcdmke and go to "Change the record".
  2. Pick "Record or correct a fact", Fact "postal address", type an address and Send.
- **What happened:** "postal_address must be a list of address lines". The form has a single text box, and the page sends the text as a string. Residential address is the same.
- **What should happen:** Address-line inputs, or the page splits the lines itself.
- **Cause:** crates/channels/ui/records.html:103-104 sends `v.value` as a string. crates/customers/src/validate.rs:125 wants an array.

### F2-23-2: an attorney cannot be recorded on screen; the direction is backwards from what the page says; the attorney and the policy are typed as references
- **Severity:** blocker
- **Who it hurts:** the privacy officer, the attorney, and the vulnerable customer.
- **Steps:**
  1. On `/records`, open M, choose "Change the record" and then "Add a relationship".
  2. Type a "Relationship reference", pick Relationship = attorney and To a = party, and in "Policy, scheme or person" type D's party id. Typing a name gives only M herself as a suggestion.
  3. Fill From and Send.
- **What happened:** "a representative needs scope from view, pay_premiums, …, all". The form has no scope, basis or authority-document fields, so an attorney can never be added from the screen.
- **Direction:** The page's note ("To a policy, a scheme, or another person (an attorney, a guardian…)") leads one to add it on the *donor's* record. But `authority` (crates/customers/src/service.rs:579-586) reads the relationship from the *attorney's* record pointing to the donor. My first relationship, added on M's record, made **M** the attorney of D, and the caller check refused D.
- **What should happen:**
  - On the donor's record, "Add someone who acts for them": pick the person by name, pick a basis and scopes from lists, and attach the document.
  - The two directions are stated in words.
- **Cause:** records.html:106-108 (the fields and the `build` of `add_relationship`), and crates/customers/src/party.rs:596-618.

### F2-23-3: frontline cannot check a caller or open the policy, and cannot record the concern the caller raises
- **Severity:** major
- **Who it hurts:** frontline staff and the caller.
- **Steps:**
  1. On `/staff`, sign in as `sub-front` and search "Cphikdk Kgdffjcdmke 13/09/1971". The search finds M and the policy.
  2. Press Open. It is refused with "None of your roles allows this action… The action you need is policy.view."
- **What happened:**
  - "Check the caller" lives on the policy page, so frontline cannot check a caller at all.
  - Recording a concern ("Record a concern someone raised") is only on `/records` under `customers.change`. Neither front nor st5b holds that, so the step "front (or st5b) records a concern" is impossible for both. I used privacy.
  - The search result also said "Not a strong match: check the name and birth date with the caller", although the name and birth date were exact.
- **What should happen:**
  - "Check a caller" and "record a concern" are available wherever a call is taken: on the person found, without opening the policy.
  - The frontline template gets what it needs for those two acts.

### F2-23-4: the complaint cannot say "D for M"; its representative is typed and never checked
- **Severity:** major
- **Who it hurts:** the complainant, the reviewer and the regulator report.
- **Steps:**
  1. On `/complaints`, sign in as `sub-front`.
  2. "Complainant is the" offers policyholder, life insured, claimant, beneficiary, scheme member, employer, adviser or third party. It has no attorney or representative, and no "on behalf of". I recorded D as third party.
  3. "Policies it is about" suggests only the complainant (D), who has 0 policies. M's policy had to be typed.
  4. "Claims it is about (claim numbers, comma separated)" is typed.
  5. "Summary document" is required (a browser bubble "Please fill out this field"). It must be a document-vault reference that frontline has no way to create. I typed `SCN223-DOC1` and it was accepted unchecked.
  6. Later, st14 used the action "add representative". Every field is typed: "Representative (customer reference)", "Relationship" (free text) and "Authority document".
     - The SCN214 prospect with relationship "bestie" was accepted with "Done: add representative".
- **What should happen:**
  - The complainant and the person complained for are both picked.
  - Their authority comes from the customer master, with its kind and scope.
  - Policies and claims are picked from the customer's list.
  - The complainant's words can be attached here.
- **Cause:** crates/channels/ui/complaints.html (the intake form and the `add_representative` fields). The complaints module does not check against the customer master.

### F2-23-5: free-text adjustments are split at commas, and timers ignore both the adjustments and the marker
- **Severity:** minor
- **Steps:**
  1. Record a complaint with "Vulnerable? Yes" and Adjustments "Speak to the daughter, who holds an enduring power of attorney; mother has dementia".
- **What happened:**
  - The queue's API holds two adjustments: `"Speak to the daughter"` and `"who holds an enduring power of attorney; mother has dementia"`.
  - The file's timers are the standard ones (acknowledge 2026-11-22, update 2026-11-27, decide 2026-12-07, deadlock 2026-12-27), although st6 set a "more time" marker.
  - The dates are ISO, not NZ form, and on the **wall clock**, a year before the books' 2027-11-15 (item 12 confirmed).
  - The complainant is shown as `pty_f08ed…` rather than a name. "Cause" stayed "–" after the decision, because the cause select is only on intake under "Put it right on the spot".
- **What should happen:**
  - Adjustments are picked from the same list as markers.
  - The timers show the extra days.
  - Dates use NZ form and the business date.

### F2-23-6: capacity questioned can never be settled: legal has no way to do it
- **Severity:** blocker
- **Who it hurts:** the attorney, who is locked out indefinitely, and legal.
- **Steps:**
  1. Privacy questions capacity on M. The answer is `capacity_questioned`. st14 cannot, and is refused with "action you need is customers.change".
  2. Legal looks for a way to settle it. `/records` has no "settle capacity". `POST /staff/v1/customers/<M>/commands {"command":"settle_capacity",...}`:
     - as legal: "403 … The action you need is customers.change"
     - as privacy: "502 customer_master_grant: the customer master refused the portal's own sign-in in the role staff … ask the platform team to grant it. Nothing is wrong with your own sign-in." This is misleading: the policy is that only the *legal* service settles capacity.
- **What should happen:** Legal settles capacity from a screen, picking the outcome ("lacks capacity", "has capacity"). The attorney's view then returns.
- **Cause:** config/cedar/customers.cedar:225-226 permits `settle_capacity` only for `Service::"legal"`, and no channel route calls the master as `legal`. records.html has no such action.

### F2-23-7: the attorney cannot see her own complaint's outcome, and has no list of the people she acts for
- **Severity:** major
- **Steps:**
  1. Over MCP as D, `tools/list` shows no complaint tool.
  2. `customer.html` has no complaints view. Its only mention is in the claim words.
  3. `represented_person` requires `party`, the donor's internal reference, which D cannot know.
- **What happened:**
  - D cannot read the outcome, as the representative or otherwise.
  - Neither the customer page nor MCP offers a change of bank details at all. That stops the act, but without the words "staff will check her authority".
- **What should happen:** A "people I act for" list and a "my complaints" view, with the outcome letter addressed to D as representative.

### F2-23-8: no delivery record can be read, because no letter is issued on the stack
- **Severity:** test-stack limit, blocking step 5
- **What happened:**
  - The documents report says `documents_awaiting_trigger: 171`, and only one document was ever issued (by another operator's SCN211 template).
  - `/documents` → Documents → "A policy's documents" for `scn223-m1` says "No document for this policy".
  - The "Policy" picker on that form suggests other policies, not the one typed. Typing "SCN223-M1" or "scn223-m1" makes no difference.
  - So there is no claim letter or delivery record to read.
  - The records officer can open policies but cannot find a customer by name (item 15 confirmed). The address history is readable by privacy on `/records` → History.
  - In that History, "RECORDED AT" is raw `2026-11-17T09:32:40.685984202Z`, and in Facts "RECORDED" is US-style `11/17/2026, 9:32:40 AM`. Dates of birth are shown as `1971-09-13` and "TRUE FROM" as `0001-01-01` (minor).

### F2-23-9: customer-master dates use the wall clock: facts dated in the books' "last month" are refused as future
- **Severity:** minor
- **Steps:**
  1. Record postal address true from 2027-10-15, which is a month before the business date.
- **What happened:** "postal_address cannot be stated for a future date".
- **What should happen:** One "today" across screens. This extends item 12 to the customer master.

### F2-23-10: the markers list shows references and codes; the adjustments lack "speak to the attorney"
- **Severity:** minor
- **What happened:**
  - "Markers to review" lists the customer as `pty_92629e1b717341f7a16ed204acc485bc`, "WHY IT IS HERE" as `capacity`, and "SINCE" as `2026-11-17`.
  - The adjustment list (more time, plain language, large print, speak to trained person, pause before large withdrawal, no phone calls, support person present, written follow up, check understanding) has no "deal through the attorney or representative".
- **Note:** "Recorded." came back for a next review of 2027-05-17, which is six months on the wall clock but six months *before* the books' date.

Not reached: the conduct counts (`conduct`) and the regulator notification. The `refer` action to `privacy_regulator` needs a typed "Forum's reference" and a "Response due". Nothing models "notify the Privacy Commissioner within N days" with an owner.

---

## SCN2-14: "delete everything you hold on me" with a dispute still open

**What I did:**
- **Set-up:** made R with `scn214-r1` (term life) and `scn214-r2` (AIA Cancer Care), and a withdrawn claim `SCN214-CLM1`.
- **Privacy, first look:** asked documents for retention by kind and for the policy's documents.
- **Hold and shred:** legal placed a hold on R, privacy tried to shred, legal released the hold, and privacy shredded again.
- **Closing the records:** privacy closed (exited) R's and the prospect P's master records.
- **Operations (st16):** `verify_customer` for R, for a missing party and for a malformed id, and `rewrap_customers` and `reshred_customers` with empty bodies.
- **Misspelt command:** privacy sent `documents_command` with a misspelt type.

I did not do the customer's own cancellation on `/customer`, because time was spent on the blockers below. The policies are still in force, and that is what makes F2-14-2 sharp.

**Outcome:** partly completed. Steps 2 to 4 are blocked: no document is ever on file for a stack customer.

**What worked:**
- `verify_customer` for R answered `{"links":3,"intact":true}`. For a missing party and for a malformed id it answered "404 not_found: no such party" (**F-23-1 and F-23-2 hold**).
- `rewrap_customers {}` answered "current_key_id is needed: the full key ID of the current key-encryption key version". `reshred_customers {}` answered "since is needed: the day the restored backup was taken, as 2027-11-15".
- `reshred_customers` is annotated `destructiveHint: true`. `rewrap_customers` is a write (`readOnlyHint: false`, `destructiveHint: false`), which is reasonable since it re-encrypts.
- st16's tools carry no personal data. `reshred_customers` called as privacy is refused with "this tool needs a sign-in you do not hold". (The words could say "for operations".)

### F2-14-1: no document is ever issued for a stack customer, so a shred under a hold cannot be tried
- **Severity:** blocker for this scenario (test-stack or seed limit)
- **What happened:**
  - Policy schedules wait in "Documents the core's events require" (171 waiting).
  - The documents report counts `issued_by_kind: {"policy_schedule":1}`.
  - `policy_documents` for `scn214-r1` and `scn214-r2` is `[]`, even 20 seconds after issuing an AIA Cancer Care policy, whose schedule template another operator activated.
  - Shredding R is refused with "409 unknown_party: the party has no key here", both under the hold and after its release.
- **Product side:** that refusal does not mention the hold, and it does not tell the privacy officer "nothing is held for this person". Either an issuer is not running on the stack, or no core trigger is turned into a document without a person. Worth confirming which.

### F2-14-2: the customer master closes, and would erase as a prospect, a person with cover in force; issued policies leave no relationship in the master
- **Severity:** major
- **Who it hurts:** the customer, the insurer's retention duty, and the privacy officer.
- **Steps:**
  1. As `sub-privacy`, MCP `change_customer` `{"party":"pty_ee51…","query":{"case":"SCN214 erasure request"},"body":{"command":"exit"}}`.
- **What happened:**
  - `exited`. `customer_relationships` for R is `{"relationships":[]}` although R owns two in-force policies.
  - The page's own note for "Close the record" says "The person has no cover or money with us".
  - Afterwards R's own `my_profile` shows `"lifecycle":"exited"` while `my_policy scn214-r1` still answers.
  - M likewise showed "Lifecycle: prospect" with "1 policy".
- **Why it matters:** retention.rs keeps a party only while a relationship is in force. With no relationship, R falls under the two-year prospect rule (`PROSPECT_YEARS`) instead of seven years after his last policy ends.
- **The prospect P:** P (no policy) was exited in the same way, with the same answer. **Nothing differs** between a customer with cover and a prospect, which is what step 6 asks about.
- **Cause:** policies issued through `POST /v1/policies` (the new-business route the stack uses) never add `owner` or `life_insured` relationships in the customer master. The `exit` command does not ask the core whether cover is in force.
- **Stack or product:** possibly part stack (no new-business service exists, so `play.js` stands in for it). Either way, `exit` should refuse while the core holds a policy.

### F2-14-3: no screen or answer says what is kept, until when, and why, for a person; erasure "on request" does not exist
- **Severity:** major
- **What happened:**
  - Documents' retention is asked one kind at a time. `retention` without `kind` is refused with "retention needs kind", and nothing lists the kinds. The answer is `{"basis":"policy_end","days":2557}`, in days and not as an until-date.
  - The master's record shows `"legal_hold": false` for R while documents held a legal hold on R. The two holds are separate, and neither screen shows the other.
  - The master has no "erasable from" date and no "erase now" command. Erasure happens only by schedule (crates/customers/src/retention.rs:44, `erasable_from`), so the officer cannot tell the person "your record will be erased on …".
  - The erasure register could not be checked: nothing became erasable.
- **What should happen:** Per person, a list by kind of document and class of fact: kept until a date and why, or erased now.

### F2-14-4: legal holds do not say who placed them or when
- **Severity:** minor
- **What happened:** `documents_question legal_holds` answers `[[{"party":"pty_ee51…"},"SCN214-LEGAL-1 …"]]`, which is the scope and the case only. The `legal_hold_placed` event also carries only `case`.
- **What should happen:** The scenario wants a refusal under a hold to name the hold and who placed it, which these answers cannot support.
- **Also:** On `/documents` the Act forms take "Policy, party or document" and "Party" as typed references (documents.html:46-68).

### F2-14-5: a misspelt documents command does not list the commands there are
- **Severity:** minor
- **Steps:**
  1. As `sub-privacy`, call `documents_command` `{"body":{"type":"shred_parti","party":"x"}}`.
- **What happened:** "400 shred_parti is not a command people send to documents".
- **What should happen:** It should add "… try one of: shred_party, place_legal_hold, …", filtered to the caller's roles.
- **Related:** The customer's `my_document` needs a document id that the customer has no list to find. Any unknown id is "404 not_found: no such resource", which is the right answer for "not found", but the customer can never find his own documents.

---

## "Things noticed while designing": items I touched

| # | Verdict |
|---|---|
| 4 | `staff.approve_profile_uplift`: now held by admin2. It is needed by `set_profile` only, and only quoted (F2-24-1). |
| 12 | **Confirmed and extended.** Complaint timers and staff recertification dates are on the wall clock (2026). The customer master refuses facts dated after the wall-clock day (F2-23-9). Grant `from` and `to` are wall-clock epoch seconds. |
| 13 | **Refuted on this stack.** admin2 approved admin's own grant. |
| 15 | **Confirmed.** The records officer cannot find a customer by name, and privacy has no `policy.view`. |
| 16 | **F-SCN02-4 reproduced** (F2-24-10). Staff and advisers are shown only by reference (`colleagues`, markers, complaint file). |

## Round 1 fixes re-tested

- **F-SCN16-7:** holds at request time, and is bypassed by `assign_template` (F2-24-2).
- **F-21-4:** partly. The refusal says a trained handler is needed, but not who.
- **F-21-10:** the claim link refusal is in words. Holds.
- **F-23-1, F-23-2:** hold.
- **F-SCN02-2:** the tool list did not widen for a cover grant, which is correct. No grant kind exists that would widen it (F2-24-3).
