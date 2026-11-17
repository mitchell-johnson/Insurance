# Operator op-f: SCN-20, SCN-21, SCN-23

Scripts and screenshots were kept outside the repository.

Records I made (all prefixed where the system let me choose):
- Customers in the customer master (made as new business, sign-in bound, no phone):
  - `pty_fc62667707214cb5b04838de20245e38` Scnhdnce Kscn21bkmfkjk, born 1966-04-16. Not usable: the family name has digits, so the find box cannot read it as a name (a test-data mistake on my part).
  - `pty_2150bd49d59c4ac796e17ca19b65311d` **Scnbdjkj Kscnuakdllhbj, born 1992-04-10** (SCN-21).
  - `pty_02d52bd88ad1449e9389b00c2e0c8b57` **Scnbiaok Kscnubamnmcjc, born 1980-07-16** (SCN-20).
- Policies: `scn21-pol-1`, `scn21-pol-2`, `scn20-pol-old` (cancelled), `scn20-pol-new` (appointed to adv-d4), `scn20-pol-old2`.
- Claims: `SCN21-CLM-1`, `SCN21-CLM-2` (notified and assigned to st4, left open).
- Complaints: `CMP-20261117-FSE6M` is the main one. It is referred to the scheme, with a $2,000 ex gratia recorded, a bundle `SCN21-bundle-1`, a stray bundle `SCN21-bundle-2`, and conduct case `SCN21-CC-1`. The screen made the CMP references; I could not add a prefix. `CMP-20261117-T63SE` and `CMP-20261117-WHDGD` were made while probing. WHDGD has the free text "Scnhdnce Kscn21bkmfkjk" as its complainant (see F-21-1).
- Distribution: `adv-d4` and `adv-d5` added as advisers. Engagements `scn20-eng-d4` and `scn20-eng-d5` with fap-d6, from 2027-11-01. adv-d5's licence is recorded as suspended (source `SCN20-INV-1`), which opened commission conduct case `conduct-adv-d5-2026-11-17`. adv-d5 was not in the seed. I left it suspended because that is the scenario's state.
- A commission task `task-569-1` (new_business_unattributed) was raised when I played new business for `scn20-pol-old2`.

Parts I played myself: new business issuing the policies (`POST /v1/policies`); `cancel_policy` as policy-servicing on the API; `advise_new_business` to the distribution module as new-business (System.tell); and the customer's adviser appointment through `POST /app/v1/policies/{id}/advisers` with the customer's dev headers (no screen; it was set-up).

---

## SCN-23 Operations' morning health check — outcome: partly (customer-master checks blocked)

**What I did.** I opened `/status` with no sign-in, then `/ops` as `sub-st16` / `operations`, and watched it for 32 s: it asked for the regions 3 times and "Last checked" moved on. I opened `/claims`, `/finance` and `/staff` as st16. Over MCP as st16 I called `tools/list`, `status`, `regions`, `reconciliation`, `customer_index`, `verify_customer`, `rewrap_customers` (no body, then `{}`), `reshred_customers {}`, and the overreach tools `find`, `staff_policy` and `claims_question`. I called `tools/list` as `sub-nobody` and with no headers. I sent a wrong `mcp-protocol-version`, a JSON-RPC batch and `GET /mcp`.

**What worked as "good looks like".**
- `/status` says "All services are working normally" in plain words. It agrees with the `status` tool and with `/ops` "What customers are told".
- `/ops` shows a leader per region, "4 of 4" partitions with a leader, quorum, and the same business date (2027-11-15) as `reconciliation`. That shows clean, with no breaks and 0 unfinished. It refreshes every 10 s.
- `tools/list` for st16 gives exactly 7 tools: `status` plus 6 operations tools. No customer-realm or staff tools are listed (this agrees with design note 3 for this sign-in). Overreach calls are refused with "this tool needs a sign-in you do not hold".
- The consoles refuse st16 with a page: "This page is not open to you / This sign-in is not registered with the insurer / ask a staff administrator for the role".
- `sub-nobody` and no headers: HTTP 401 `{"detail":"sign in as a customer, adviser, employer or member of staff of this insurer"}`.
- Wrong version: 400 "this MCP protocol version is not supported". Batch: -32600 "send one message at a time". GET: 405 with `allow: POST`.
- `rewrap_customers` with no body: "body is required", refused before any call.
- No customer personal data appeared in any operations answer.

**Findings**

- **F-23-1 (major, test stack; possibly a deployment trap). `customer_index` and `verify_customer` never work: the operations engineer cannot check search lag or a record's chain.** Hurts: platform engineer.
  - Repro: MCP as `x-dev-subject: sub-st16`, `x-dev-roles: operations`, `tools/call {"name":"customer_index","arguments":{}}`. The same happens with `verify_customer {"party":"cust-nonexistent-x"}`.
  - Got: `Refused (401 unauthorised): invalid token: the token does not carry the role operations`.
  - Cause: the portal calls the customer master as itself with role `operations` (`crates/channels/src/customers.rs:172`, `desk()` at :518). The e2e stack's app registration gives `ic-portal` only `["portal","staff"]` on the customers audience (`e2e/stack/stack.js:43`). `docs/operations.md:62` says the portal also needs `privacy` and `operations` there. So the privacy-officer desk (audit) would fail the same way.
  - Product part: the 401 text blames *his* token ("the token does not carry the role operations") when it is the portal's service identity that lacks the grant. That sends the engineer after the wrong problem. `desked()` in `crates/channels/src/http.rs:3777` passes the master's 401 through unchanged. It should be a 502/503 that says "the portal's identity is not granted operations on the customer master".
  - Because of this, scenario step 4 (verify a customer from another scenario, and a party that does not exist) could not be judged. Even when it works, he can only verify a party whose `pty_…` reference someone hands him: he has no way to find it.
- **F-23-2 (minor). `reshred_customers` and `rewrap_customers` with `{}` give an unhelpful refusal.** Got: "Refused (422 customer_master): the customer master did not take it". It does not name the missing `since` / `current_key_id`. It was refused, so it is safe. Cause: `desked()` `crates/channels/src/http.rs:3790` falls back to a generic text when the master's body is not `{error:{message}}`.
- **F-23-3 (minor). Tool annotations.** `reshred_customers` ("Erase again after a restore") is `destructiveHint: false`. An erasure is destructive, and an assistant relies on this hint to ask before calling. `verify_customer`, a read-only check, is marked as a write (`readOnlyHint: false`, takes an `idempotency_key`). Cause: `Tool::listed` / `writes()` `crates/channels/src/mcp.rs:85-150` derive the hints from the HTTP method only.
- **F-23-4 (minor). `/ops` does not answer two of his four questions.** It has no reconciliation panel (did last night's close reconcile, the checks per partition) and no customer-search lag. Only MCP has them. `crates/channels/ui/ops.html:16` reads only `/ops/v1/regions`. The `reconciliation` answer itself is just `{business_date, clean, breaks, unfinished}`, with no per-partition checks.
- **F-23-5 (minor). The `regions` MCP answer gives `"heard": 1794889540`**, a raw epoch. `/ops` shows the same value as a date. The `/ops` header also labels st16 "Staff" (`crates/channels/ui/app.js:431`), although he is not on the staff directory. The sidebar offers him every staff console, and each one refuses him.
- **F-23-6 (minor, test stack). The 401 has `www-authenticate: Bearer` with no `resource_metadata`**, and `/.well-known/oauth-protected-resource/mcp` is 404, because `public_url` is unset in the stack. So the pointer to how to sign in exists only as prose. In production, `metadata_url` (`mcp.rs:511`) would supply it. The 400 for a bad version does not list the supported versions.
- Scenario detail wrong: there are **two regions** (auckland: 2 nodes, all 4 leaders; sydney: 1 node), not three.

---

## SCN-21 A complaint that crosses four teams — outcome: partly

**What I did (all on `/complaints` screens).**
1. As `front`: found the complainant by name with the picker, which works. Picked claimant / Claims, ticked policy `SCN21-POL-2` from his policies, and tried to save without the summary (the browser blocked it: "Please fill out this field"). Recorded it, with the words typed into the "Summary document" box.
2. Junior `st14j` assigned the complaint to herself, which worked because it was not yet vulnerable. `st6` opened `/complaints` and was refused ("Your roles do not include complaints"). `st14` set the vulnerability marker. The junior then recorded the acknowledgement, which was accepted.
3. `st14` assigned it to herself and opened the file. She tried deadlock, decided without and then with a written outcome, tried $6,000 and then $2,000 ex gratia, then deadlock again, then close.
4. `legal` referred to the scheme and released the bundle with herself as reviewer, then with st14, then with `zz-nobody`.
5. `conduct` opened `SCN21-CC-1` and linked the complaint. `st14` looked at the Conduct cases tab and the file.
6. The register for 2026-11-01..30, as conduct and as st14.

**What worked.**
- The CMP reference is pre-filled and is fresh after each save.
- Finding by name and birth date works (once the name has no digits), and the complainant's policies are offered to tick.
- The summary is required.
- Timers show due dates (update 2026-11-27, decide 2026-12-07, deadlock 2026-12-27).
- An outcome needs a written document ("an outcome is written, with the reasons and the external scheme's details").
- $6,000 is refused and $2,000 is accepted. The amount is typed in dollars.
- The bundle cannot be released with the releaser as reviewer.
- `st14` gets "Conduct cases are open to the conduct function and legal only", and her file shows "Cases None", with no trace of `SCN21-CC-1`.
- The register lists the complaint.

**Findings**

- **F-21-1 (major). Intake records a complaint against whatever text is in the complainant box, even when nobody was picked.**
  - Hurts: complainant, complaints team, regulator reporting.
  - Repro: `/complaints` as `sub-front`. Type a name the search does not find (e.g. "Scnhdnce Kscn21bkmfkjk": the picker says "Nothing matches that."), fill a summary, then Record. Got "Recorded CMP-20261117-WHDGD", with complainant = the literal text. It sits in the queue and the register.
  - Should: refuse until a person is picked from the search, and say "find the complainant and pick them from the list".
  - Cause: `crates/channels/ui/complaints.html:166` (`$("i-who").dataset.party || $("i-who").value.trim()`), and the route/module does not check that the party exists.
- **F-21-2 (major). The complainant's own words cannot be recorded.**
  - The form asks for a "Summary document … The complainant's own words go in the document vault; give the document's reference here". Frontline has no way to put words into the vault (the documents service is not in the stack, and no screen uploads). So they must type a reference they cannot know, which goes against the staff UI rule.
  - I typed the words into that box and they were stored as a "document reference" (`Summary document: SCN21-words-…` shows on the file).
  - Cause: `complaints.html:15` and the `summary_document` field of `record_complaint`.
- **F-21-3 (major). Frontline goodwill on the spot is not on any screen.** Step 2 ($150 refused, $100 allowed) cannot be done. The module supports it only as `resolved_on_the_spot.put_right` at intake: `crates/complaints/src/state.rs:473-498`, limit `front_line_limit_minor` = $100. That also closes the complaint at once, and only for a `service` matter, never for a claims complaint like this one. The intake form (`complaints.html:9-17`) has no resolution or goodwill fields. Frontline's banner also reads "intake. Authority level 3." The standard profile gives frontline level 3, which suggests a higher authority than the $100 frontline limit.
- **F-21-4 (major). Adding a vulnerability marker does not take the file from an untrained handler.**
  - Hurts: a vulnerable complainant (conduct breach).
  - Repro: complaint `CMP-20261117-FSE6M`, not vulnerable. `st14j` assigns it to herself ("Done: assign."). `st14` sends set_vulnerability Yes ("Done"). The queue now shows owner `st14j`, "needs a trained handler: yes". `st14j` then sends record_acknowledgement: "Done: record acknowledgement."
  - Should: on the marker, the file returns to the queue (or the untrained owner is refused) with words saying who can take it.
  - Cause: `crates/complaints/src/state.rs:211-220` (`SetVulnerability` only sets the flags). The training check is only in `assign` (:565).
- **F-21-5 (minor). The hardship specialist (`st6`, "trained") cannot add the marker at all.** `/complaints` refuses her ("Your roles do not include complaints"). The `hardship` template has no complaints action (`e2e/lib/cast.js:17`). The scenario's hand-off from hardship to complaints does not exist. A vulnerability recorded in `/staff` (`service.vulnerability_kind`) does not reach complaints.
- **F-21-6 (major). The register counts complaints as "acknowledged late" before they are due.**
  - Repro: Register and report, 2026-11-01 to 2026-11-30, viewed on 2026-11-17. It shows "Acknowledged late 2" and "Vulnerable late 2" for two complaints received that day, which are due 2026-11-22 and not yet acknowledged.
  - Should: count them as not yet due.
  - Cause: `crates/complaints/src/report.rs:193-196`. `None if to > due => Some(false)` compares against the end of the period, not against today. Any report for the current month overstates breaches, and that is the report that goes to the regulator.
- **F-21-7 (major). The bundle's reviewer is just typed by the releaser; nothing records that the reviewer reviewed.**
  - Repro: as `legal`, release_bundle with "Reviewed by" = `st14`, then = `zz-nobody`. Both give "Done: release bundle." st14 never saw it, and `zz-nobody` is not a person.
  - Should: the reviewer approves in their own session (maker and checker), or at least must be a member of staff who holds the action.
  - Cause: `crates/complaints/src/state.rs:~855-862` checks only `reviewed_by != actor`. The field is free text: `complaints.html` ACTIONS.referral.release_bundle.
- **F-21-8 (major, against the scenario's expectation). Deadlock order.**
  - `declare_deadlock` was accepted on the day of receipt, before any outcome. After the outcome was decided and the complainant rejected it, deadlock was refused ("only a complaint without an outcome can be deadlocked").
  - `decide` was then accepted on a complaint already deadlocked.
  - The scenario expects the opposite: no deadlock before an outcome, and deadlock after a rejected outcome. The "deadlock notice" is only a typed document reference, so nothing shows that it carries the scheme's details.
  - Cause: `crates/complaints/src/state.rs:791-800`. This needs a product decision. As built, a handler can deadlock a complaint on day 0 and push it to the scheme.
- **F-21-9 (major, staff UI review). The Act form is references all the way down.**
  - Every action makes the person type references they cannot know: complaint, "Handler (staff reference)", acknowledgement/update/outcome/deadlock/notice documents, "Reference in the owning module" for remediation, "Owner (staff reference)" and the conduct case ID for conduct, "Reviewed by", and Forum's reference.
  - The close "Cause code" is a free text box, although the module holds a fixed list (`delay`, `wrong_decision`, …).
  - The file shows the complainant as `pty_2150bd…`, not his name, and the policy as `scn21-pol-2`, not `SCN21-POL-2`, with no link.
  - The queue has no complainant name. "Authority needed" shows `0`, and adjustments show as raw codes.
  - The junior and conduct are offered all 25 actions of their seat, including referral-only ones.
  - Cause: `crates/channels/ui/complaints.html:48-90` (ACTIONS spec).
- **F-21-10 (major). The claim cannot be reached from the complaint.** Intake links policies only, not the claim, and the file's "Cases" is empty. The file has no link to the claim, and no way to ask the claims handler for access to the notes (grant_access goes the other way: it lets the complaints owner grant access to the complaint). Step 4 is blocked.
- **F-21-11 (minor). The over-limit refusal does not say the limit or who may authorise.** Got: "the goodwill payment is above the authoriser's limit". Should be something like "$6,000.00 is above your limit of $5,000.00; a level-N handler may authorise it". Cause: `crates/complaints/src/state.rs:769`.
- **F-21-12 (minor). The complaints team sees a trace of conduct work.** The register shown to `st14` includes "Open conduct cases 1". Cause: `crates/complaints/src/report.rs` (period report has no per-seat filter).
- **F-21-13 (minor, observation). Two clocks.** Complaint dates follow the Auckland wall clock (2026-11-17), while the ledger's business date is 2027-11-15. In this stack the complaint timers sit a year before every policy and claim date. This is a stack artefact, but it means complaints are never tested against the business date.
- Not exercised: the vulnerable extra days. The module adds `vulnerable_extra_days` only to the complainant's own reply window (`ask_complainant`, `state.rs:629`), not to the insurer's deadlines, so the file timers do not change. That is reasonable. Also not checked: whether the customer's file in `/staff` links to the complaint. The "A customer's complaints" search on `/complaints` exists.

---

## SCN-20 Held commission — outcome: blocked (after the structure step)

**What I did.**
- `dist` on `/commission`: Change something → add_party `adv-d4` and `adv-d5` as adviser. Then Another command → raw JSON `start_engagement` for each with `fap-d6`.
- Issued `scn20-pol-old` and cancelled it, then issued `scn20-pol-new` on the same life. The customer appointed `adv-d4` (engagement `scn20-eng-d4`), and it appears in d4's book.
- Played new business with `advise_new_business`.
- Looked at `/replacements` as conduct and as comops, and `/adviser` as d4 and d5.
- `st13` and `comops` tried to hold d5's payments. `dist` tried as well. `st13` recorded a suspended licence for d5.
- On `/staff` → Held balances, `st10` tried hold-pending, a $500 release and a $999,999 write-off.
- `comops` read month-end blocks, the aged held report and the held control.

**Findings**

- **F-20-1 (blocker, test stack / missing driver). No commission can be written, so there is never a match or a held balance.**
  - `advise_new_business` for a policy raises task `new_business_unattributed`: "engagement eng-d1 has no book open to new business (no_book)". The seed creates no commission books and no product schedules (`e2e/stack/seed.js:144-159` only adds parties, licences and engagements). Nothing writes layers either: customer appointment (`crates/distribution/src/config.rs:1105`) only moves existing layers.
  - So `/replacements` says "The rule has matched nothing". d4 sees "None of your business has been treated as a replacement", and every party on the workbench holds $0.
  - Steps 1–2 (match, dispute, conduct decision) and steps 4–6 with real money could not be done. Design note 2 is confirmed for commission as well: no new-business attribution exists in the stack.
- **F-20-2 (major, staff UI review). Putting an adviser into the structure means typing internal references and raw JSON.**
  - The party picker says only "Type the party's reference.", so dist had to know `adv-d4` and `adv-d5`.
  - A new engagement has no form. It needs JSON in "Another command": `{"start_engagement":{"id":"scn20-eng-d4","engagement":{"adviser":"adv-d4","with":"fap-d6","from":"2027-11-01","to":null,"accreditations":["life"]}}}`. dist has to invent the engagement ID and know the provider's `fap-d6`.
  - The page itself says "For the few commands with no form here (a new engagement …)".
  - Cause: `crates/channels/ui/commission.html:10-11, 31-44`. Design note 5 (d4 and d5 not in the structure) is confirmed.
- **F-20-3 (major). Compliance cannot put an adviser's payments on hold for an investigation, and the refusal speaks code.**
  - Repro: `/commission` as `sub-st13`, Change something, set_payment_hold `adv-d5`. Got: "none of your roles allows payee.manage". Commission operations got the same.
  - Only `distribution_admin` may (`config/cedar/distribution.cedar:14`), and for an adviser with no registered payee even dist gets "adv-d5 is not a registered payee".
  - The only lever compliance has is to record the licence as *suspended*. That opened a conduct case and put d5's portal into "Your access is read-only … until the licence … is restored". That is wrong for an adviser who is only under investigation, and it misstates his licence.
  - The console also offers st13 all 12 actions, most of which it then refuses.
- **F-20-4 (minor). d5's portal says "Nothing is being held from you."** It also shows the read-only licence banner, with no mention of the investigation. Nothing is held in money because of F-20-1, so whether "held, since when and why" reads well could not be judged.
- **F-20-5 (minor). On the held-balance workbench the refusals do not give the amounts, and the daily control is blank when nothing is held.**
  - Release of $500 and write-off of $999,999 both return "the amount is more than the held balance not already committed to another action". Neither names the held amount. Hold-pending returns "nothing is held for that party in that currency".
  - The Daily control section renders empty instead of "Nothing held, NZ$0.00". comops' held control shows "Held total NZ$0.00 / Agrees with ledger yes", which is good.
  - "Reason code", "Account to pay into" and "Document on file it rests on" are free text boxes.
  - The workbench setting is two approvers above $50,000 (`two_approver_above_minor: 5000000`), not the $100,000 the scenario assumes. The profile's `held_balance` limit is $100,000.
  - Cause: `crates/channels/ui/staff.html:19-30, 214-246`.
- **F-20-6 (minor, stack artefact). Date fields on the commission console default to the wall-clock date (2026-11-17), not the business date (2027-11-15).** As a result, d5's case reads "effective 2026-11-17, opened 2027-11-15". `portal.today()` is used for every date field (`commission.html` draw()).
- `comops` on `/replacements` is refused with a clear page ("Your roles do not include replacement matches"). `conduct` sees "You decide disputed matches as the conduct officer." Month-end blocks: "None."; aged held: NZ$0.00.
- Also noticed, not my scenario: on the workbench `adv-d9a` is "normal" while its provider `fap-d9` is "licence suspended" (from SCN-03).

---

## Design notes touched

- **Note 2 (missing drivers):** confirmed. Commission/new-business attribution is missing too: no books or schedules exist, so no layer, match or held money (F-20-1).
- **Note 3 (MCP lists by sign-in):** for the operations sign-in, the list is exactly status plus 6 operations tools; no customer or staff tools are offered to st16 (confirmed: no leak there).
- **Note 4 (actions nobody holds):** extended. The *portal's own service identity* also lacks the `operations` and `privacy` roles on the customer master in the stack (F-23-1), so the operations customer tools fail even for the right person.
- **Note 5 (cast gaps):** confirmed. d4 and d5 are not in the structure, and adding them needs typed references and raw JSON (F-20-2).
