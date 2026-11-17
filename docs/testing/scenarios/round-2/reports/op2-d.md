# Round 2 operator report: op2-d (SCN2-10, SCN2-11, SCN2-12, SCN2-13)

Operator: op2-d.

## Shared state: what I changed and what I put back

- **Templates (documents module)**
  - I submitted `scn210-lapse-notice` v1 to v5 and activated v4 and v5. Then I disabled both: v4 as part of the scenario, and v5 at the end.
  - I submitted and activated `scn211-policy-schedule` v1 and `scn211-policy-wording` v1, bound to `aia-cancer-care` v1. I disabled both at the end.
  - The only document they issued was the schedule for my own policy SCN211-1.
  - No real template id (`lapse-notice`, `policy-schedule`, ...) was touched.
- **Notice defect `DEF-20271115-DJNDH` (case SCN211-CASE-1) is still open and empty.** I could not close it: its raiser cannot close it, and nobody else holds `documents.remediation` (see F2-11-6).
- **Legal hold.** I placed a hold on policy scn211-1 and released it.
- **Data I made.** All of it is my own and prefixed SCN21x. Nothing seeded was changed. No business day was closed and the date was not moved.

### Customers in the customer master

| Ref | Name | Born | Party | Policy |
| --- | --- | --- | --- | --- |
| SCN211-cust | Tamati Scn-Tuhiwai | 1971-03-09 | pty_a3d8cc11… | SCN211-1, aia_cancer_care |
| SCN212-cust | Rawiri Scn-Paewai | recorded 1979-06-21, corrected to 1978-06-21 | pty_a0dcad5a… | SCN212-1, term_life |
| SCN213-A | Hana Scn-Mason | 1988-02-14, mobile +64215550213 | pty_d8604dfd… | SCN213-A1, term_life |
| SCN213-B | Hana Scn-TeRangi (created by `nb` on /records) | 1988-02-14 | pty_cf315dcc… | SCN213-B1, income_protection |
| SCN213-C | Mere Scn-Kingi | 1990-07-07 | pty_c34db8d8… | SCN213-C1 |
| SCN213-D | Wiremu Scn-Kingi | 1990-07-07 | pty_4d22b2c0… | SCN213-D1 |

- **Set-up done off-screen**, the way `customer.js` and `play.js` do it:
  - the customer master record, as new business
  - identity `bind_subject` and `enrol_device`
  - each policy, through `POST /v1/policies` as new-business
- **Merges left in place.** B is merged into A. C and D were merged and then unmerged.

### Parts I played

- **Azure Communication Services.** I played one Event Grid email delivery report: a bounce (`Bounced`, "550 5.1.1 mailbox does not exist") for the SCN211-1 schedule. It was posted to the documents service's `POST /v1/delivery-reports` with the stack's webhook key.
- **This is not on the brief's list of parts an operator may play.** I used it because the stand-in never reports a delivery. Please confirm it is acceptable.

### Corrections to the scenario set-up

- **No templates are loaded on the stack.** The scenario assumes the shipped placeholder wordings are listed on the Templates view, but the view reads "No template is loaded."
  - Nothing in the seed submits them.
  - So no document of any kind has ever been issued on the stack. The documents service log reports 85+ required documents waiting, each "has no active … template".
- **A product's documents need two templates.** To get any document for a policy I had to author, test, approve and activate a policy schedule template **and** a policy wording template (`required_documents.rs:240-246`). Both had to be bound to one product. I used aia-cancer-care so that other operators' policies were not affected.
- **The customer master field is `mobile_phone`, not `mobile`.**

---

## SCN2-10: The lapse notice wording is wrong. Outcome: **partly**

**What I did**

1. st15 opened `/documents`: all six views loaded, with no console errors. There was no lapse notice in force (no templates at all).
2. st15 submitted versions on Act → "Submit a template", as raw JSON:
   - v1 with `{{ policy.numbr }}`: **accepted**.
   - v2 with one 90-word sentence: **accepted**.
   - v3 with two long sentences: refused, "the wording reads at grade 13.8, above the limit of 12.0". Then v3 went in properly, still carrying the placeholder line.
3. She ran rendering tests (cases as JSON with a SHA-256 I computed myself) and tried to approve her own version.
4. Approvals for v3:
   - legal approved for legal.
   - Legal could not give compliance or product: the screen offers only "legal", and the route refuses both.
   - st13 approved for compliance and st15b for product.
5. st15 tried to activate v3 and was refused: "the wording is a placeholder for legal review; rewrite it and submit a new version".
6. To act out "stop the old one", I made v4 (old number 0800 111 222, no marker) and activated it from 2027-11-01. I then made v5 (0800 500 123) and activated it from 2027-11-15. Each version needed its three approvals again. st15 then disabled v4 with a reason and a case.
7. Checks by the other roles:
   - records looked for "notices by template": not offered on the screen.
   - records: "Submit a template" is not offered, and the route refuses with "your roles do not let you submit template".
   - legal: "Disable" is not offered, and the route refuses with "your roles do not let you disable template".

**Confirmations**

- Round 2 "Things noticed" item 3 (the author is the only product approver) is **resolved by the stack**: st15b approves.
- Maker-checker holds:
  - "an author does not approve their own template".
  - One person cannot give two approvals, because the channel refuses a role the person does not hold.
- A disabled version reads "disabled" with its own in-force date. Versions are never edited: a resubmit with other content is refused with "template … version 1 exists with other content".

**Findings**

- **F2-10-1, major.** Hurts the product owner and every customer.
  - No template is loaded on the stack. The service ships placeholder wordings (`crates/documents/src/placeholders.rs`, `templates/*.txt`), but no person or seed step submits them. The Templates view reads "No template is loaded", and nothing is ever issued: schedules, lapse notices, letters.
  - There is no "start from the shipped placeholder" action, so the product owner must retype a whole letter as JSON.
  - Good looks like: placeholders loaded as drafts, and a "New version from this one" button that copies the wording into an editor.
  - Partly a stack and seed gap, partly a product gap.
- **F2-10-2, major.** Hurts the product owner and the compliance approver.
  - "Submit a template" is a JSON textarea (`documents.html:64`). Kind codes (`lapse_notice`), `alternate_formats`, `primary_language`, `bodies`, `products [{code, version}]` and `review_by` must all be typed from knowledge of the Rust struct.
  - Repro: st15, /documents → Act → Submit a template.
  - Good looks like: a form with kind and product pickers, a wording editor, and a preview.
- **F2-10-3, major.** Hurts the product owner.
  - A rendering test needs JSON cases that include `expected_sha256` of the rendered text and the product's `product_fields` list (`documents.html:62`, `command.rs:71`). Nobody can know the hash without rendering the letter elsewhere, and there is no preview.
  - I had to write my own renderer to pass the test.
  - Good looks like: "Render a sample" shows the letter for a synthetic persona. The person approves what they see, and the hash is taken from that.
- **F2-10-4, major.** Hurts the product owner and customers.
  - An unknown field is not caught at submission. v1 with `{{ policy.numbr }}` was accepted as a draft with required fields "name, policy.numbr" (`template.rs` `validate`).
  - It is caught only at the rendering test ("the product does not supply `policy.numbr`"), and only if the tester's own `product_fields` list is right. `docs/documents.md` says validation checks "every field it uses is known".
- **F2-10-5, minor.** Hurts customers.
  - The reading grade is averaged over the whole letter (`render.rs:440`). One sentence of 90 words passed (estimated grade 10.2 against the 12.0 limit). Two such sentences were needed to trip it.
  - The grade found is not shown on an accepted template.
- **F2-10-6, major.** Hurts the product owner and customers.
  - A lapse notice template with no product binding validates, passes, is approved and activates. It then serves no product: `template_for_kind` picks by kind **and** product binding (`state.rs:849-859`).
  - My `scn210-lapse-notice` v5 was "active" and would never be used. The screen never says "this version serves no product".
- **F2-10-7, minor.** Hurts the product owner.
  - "Run a template's rendering tests" answers "Done: Run a template's rendering tests." even when every case failed. The failures appear only under Templates → Show.
- **F2-10-8, minor.** Hurts the product owner.
  - The self-approval refusal "an author does not approve their own template" does not say who can approve (st15b holds product approval).
- **F2-10-9, minor.** Hurts the product owner, legal and compliance.
  - Drafts can never be withdrawn. v1 to v3 stay "draft" with Approve and Activate buttons forever. Disable refuses with "a draft is not in use, so there is nothing to disable".
- **F2-10-10, minor.**
  - Activation accepted a backdated in-force date (2027-11-01 on business date 2027-11-15) with no warning.
- **F2-10-11, major.** Hurts the records officer.
  - The page cannot answer "notices by template".
  - The channel has the question `notices_by_template` (`correspondence.rs` QUESTIONS), and the Act note on "Add notices to a defect" says notices "are listed under Look something up". But the Look something up select (`documents.html:14`) has no such option.
  - Only MCP `documents_question` or the raw route reach it. The route answered `[]` for v4.
- **F2-10-12, minor.** Hurts everyone.
  - People appear as keys: "Approved by legal: legal, product: st15b, compliance: st13", "Author st15".
  - Dates are ISO and UTC: "Last rendering test At 2027-11-14T12:00:00Z", "Review by 2028-11-15", "In force from 2027-11-15".
  - Settings show "Version 0" and "Max reading grade x10 120".

---

## SCN2-11: Notices that went out wrong and a letter that came back. Outcome: **partly**

There was no notice with a notice period without a date move. I worked the defect on the policy schedule, which is the only document I could get issued (see the set-up notes).

**What I did**

1. Set-up:
   - I activated the SCN211 schedule and wording templates.
   - I issued SCN211-1 (AIA Cancer Care) to Tamati, whose email is undeliverable.
   - The service issued `required:schedule:scn211-1:2027-11-15:PolicySchedule` by email.
   - I played the bounce, and the delivery became "undeliverable".
2. records, on /documents → Documents:
   - The policy **number** "SCN211-1" gave "No document for this policy".
   - The internal id "scn211-1" listed the schedule as undeliverable, with "Deliver again".
3. records raised a defect, case SCN211-CASE-1, "Lapse warnings gave the old contact centre number". The form has no template-version field.
   - "Add notices" with the schedule was refused: "… is not a document that had to be proven".
   - Closing was refused: "whoever raised a defect does not close it".
   - st5 has no documents actions.
4. Tamati changed his email himself on /customer (signed). records pressed "Deliver again", which was prefilled with the party id and "email, post".
   - The delivery shows a second attempt to `t***@example.nz` on the **same** document. Status "in progress": the stand-in never confirms.
   - The customer sees one document, "Policy schedule (SCN211 test)", under the policy.
5. Letters requested: none exists for this policy, and no screen lets anyone ask for one. I did not cancel other operators' letters.
6. Legal hold:
   - legal placed a hold on policy scn211-1, case SCN211-LEGAL-1.
   - records had no Release button, and the route refused with "your roles do not let you release legal hold".
   - legal released it.
7. Downloads through `GET /staff/v1/documents/<id>/pdf`:
   - st5b got 403 "your roles do not include downloading documents".
   - records with no purpose got 400 "say why you need it: the purpose is logged with the download".
   - records with a purpose got 200 and a PDF.

**Findings**

- **F2-11-1, major.** Hurts the records officer.
  - A policy's documents are found only by the internal policy id ("scn211-1"). The number a person knows, "SCN211-1", answers "No document for this policy" (`documents.html:100`, `showPolicy` passes the text through).
  - The policy picker offers only policies with a waiting trigger or letter.
  - Good looks like: the number is resolved, or a picker finds any policy.
- **F2-11-2, major.** Hurts records and customers.
  - Nothing can issue a corrected notice. `NoticeSpec.corrects` is only ever `None` (`required_documents.rs:309`), and no staff action or driver path issues a correction.
  - So a defect's notices can only be excluded, never corrected. "Treat the bad ones as not given, send good ones" cannot be done.
- **F2-11-3, major.** Hurts records.
  - A notice defect is not tied to a template version. The raise form takes reference, case, description, cause and dates only (`documents.html:50`, `state.rs` `raise_defect`).
  - So "raise a defect against the template version from SCN2-10 (picked from the list)" is impossible, and the population must be pasted in as document ids.
  - The "Remediation case" is free text with no list.
- **F2-11-4, major.** Hurts records.
  - There is no way to record a returned letter. `AttemptOutcome::Returned` exists in the model, but no command, staff action or route records it.
  - "A letter that came back" can only be played as an email bounce.
- **F2-11-5, minor.** Hurts records.
  - The Work view does not show undeliverable documents. It has three cards: triggers, letters and sends in progress.
  - "Documents the core's events require" does not say **why** each waits. The service log knows ("product term-life has no active PolicySchedule template; its documents are waiting").
- **F2-11-6, major (cast gap plus product rule).** Hurts records.
  - Only `records` holds `documents.remediation`, and the module refuses a defect's raiser closing it. So a defect raised by records can never be closed.
  - Mine, DEF-20271115-DJNDH, stays open.
  - The refusal does not say who can close it.
- **F2-11-7, minor.** Hurts records.
  - Document rows show the recipient as a party id (`pty_a3d8…`). There is no name and no channel, sent or delivered date on the list.
  - Lookup → document shows `wrapped_key`, `blob` path, content and data hashes, and ISO UTC times ("Issued at 2027-11-14T12:00:00Z"). Key material should not be shown to staff.
- **F2-11-8, minor.** Hurts records.
  - The refusal text rewrote the document id ("required:schedule:scn211-1:2027-11-15:policy schedule is not a document…"). A person copying it would get the wrong reference.
  - The refusal does not explain that only notices and proven letters can be added.
- **F2-11-9, minor.** Hurts records.
  - "Raise a notice defect" generates a fresh DEF reference each time the action is drawn. The reference shown before sending is not the one recorded if the form redraws. My script's first attach went to a non-existent DEF.
  - The raised defect's reference is not shown in the "Done" message.
- **F2-11-10, major.** Hurts records and service staff.
  - Neither /documents nor /staff offers a download, although records holds `documents.download`. Only the raw route and MCP `staff_document` serve it.
  - The download did ask for a purpose. However, st5b's refusal does not say who can download (records).
- **F2-11-11, minor.** Hurts legal and records.
  - A legal hold shows only scope and case ("policy scn211-1 | SCN211-LEGAL-1"). There is no who placed it, when or why.
  - Neither placing nor releasing asks for a reason.
- **F2-11-12, minor.** Hurts customers.
  - On /customer the product shows as "Aia cancer care" (a code in words, not "AIA Cancer Care").
  - Cover is "held at level 0".
  - Dates are ISO ("the oldest from 2027-11-15").

---

## SCN2-12: Access request and a disputed correction. Outcome: **completed, with findings**

Round 1 finding F-08-2 (a local-format mobile stored with +64) **holds**: "021 555 0123" is stored as `+64215550123`.

**What I did**

1. Rawiri's own copy:
   - On /customer (browser enrolled from his phone) → Download my record, which gave `my-record.json`.
   - By MCP `export_my_data` without a device it was refused: "Refused (401 device_required): … set up this browser under Signing first". Good.
2. st5b read SCN212-1 on a call (`/staff/v1/policies/scn212-1?case=SCN212-CALL-1`).
   - By MCP, `customer_export` and `customer_audit` were refused: "this tool is not open to you …".
   - nb got "… The action you need is customers.audit" and "… customers.export". Good.
3. privacy, on /records:
   - "A customer is on the phone" made the case CALL-20271115-CUS8R.
   - "Rawiri Scn-Paewai 21/6/1979" opened the record.
   - History, Relationships, Who read or changed it and Save everything held all work, with no console errors.
4. Birth date correction, on Change the record:
   - privacy declined the correction to date of birth with his statement.
   - She then recorded date_of_birth 1978-06-21 with the reason "Passport sighted".
5. He changed his mobile himself on /customer. His own copy shows the declined statement.
6. MCP as privacy:
   - Listed: `customer`, `customer_history`, `customer_export`, `customer_audit`, `actions_by`, plus `documents_command`, `merge_customers` and others.
   - `staff_policy` is not listed, and a call answered "… The action you need is policy.view". `claims_question` and `payments_question` were refused.
   - The first calls put `case` at top level and were refused with "case goes in the query: {"query": {"case": "..."}}". That is clear, but the schema does not mark it required (MCP check d).

**Findings**

- **F2-12-1, major.** Hurts the customer and the privacy officer.
  - The staff export for a Privacy Act request (`customer_export`, "Save everything held") is only the master's raw event stream: `{party, streams:{…:[{seq, caller, actor:"ic-e2e-new-business", body:{type:"created"…}}]}}`.
  - It has no policies, documents, requests or devices, no "as at" and no headings in words.
  - The customer's own copy has `as_at`, `your_details`, `policies`, `requests` and `devices`. The two do not hold the same facts.
  - Neither copy is a file a person could send:
    - money is in cents (`outstanding_minor: 23575`)
    - enrolment is in Unix seconds
    - `valid_from: 0001-01-01`
    - service ids appear
    - the documents held are not listed
- **F2-12-2, major.** Hurts the customer, who asked "who looked at my file".
  - The audit lists only master record reads. st5b's read of the policy on a call does not appear.
  - Audit lines carry no case: `purpose: servicing` for every privacy read, although the page says "Every look-up and change is recorded against the case".
  - The actor is a key ("privacy") or a party id, the times are ISO UTC, and one open of the record logged 7 reads in 2 seconds.
  - No refusal appears. The only refusals in the run were MCP tool-level refusals, which are not tied to a party.
- **F2-12-3, major.** Hurts the customer and pricing.
  - The corrected birth date was recorded "true from 2026-11-17", which is the wall-clock today, not the business date and not birth.
  - The form says "True from (blank for today)", which invites this mistake for a correction. As-of reads before that date still say 1979.
  - Nothing reaches the policy: no re-rating, no task, nobody told. No subscriber in `crates` reacts to a master date-of-birth change. SCN212-1 stays rated at age 48 instead of 49.
  - The privacy officer cannot even see the policy to raise it (round 2 "Things noticed" item 15 **confirmed**).
- **F2-12-4, minor.**
  - The declined correction is not shown in History on /records (only statements are shown).
  - It stays "declined" in his own copy after the correction was accepted, with no link between the two.
- **F2-12-5, minor.**
  - The record shows "Lifecycle prospect" for a man with an in-force policy, in both the staff view and his copy.
  - "TRUE FROM 0001-01-01" appears for facts with no date.
  - "RECORDED 11/17/2026, 9:40:18 AM" is a US-format wall-clock date.
  - History "Source: staff" does not say which person.
- **F2-12-6, minor.**
  - st5b's refusal for `customer_export` and `customer_audit` does not name the permission ("this tool is not open to you … ask whoever looks after your access"). nb's names it.
- **F2-12-7, minor (test-stack limit or route).**
  - st5l's `GET /staff/v1/core/policies/scn212-1?case=…` answered 404, so I could not see whether the policy's own factors changed. It needs checking.

---

## SCN2-13: The same woman twice: merge, regret, unmerge. Outcome: **partly** (the scheme match step was not done; see the end)

**What I did**

1. nb on /records → Add a person: Hana Scn-TeRangi, 14/2/1988, with A's mobile.
   - First try: "This person may already be on file. Search for them under Record; …".
   - With "Add even if …", the record was created (pty_cf315dcc…), **but the page then showed "None of your roles allows this action. … customers.view"**.
2. privacy → Duplicates listed two reviews, A/B and C/D. C/D was raised when D was created with `allow_duplicate`.
   - She pressed Merge on A/B. The form came prefilled with **survivor = B** (the new record) and absorbed = A.
   - I corrected it to survivor A and absorbed B, with the review id. Merge done.
3. Searching "Hana Scn-TeRangi 14/2/1988" opens A, showing Scn-Mason. The absorbed id answers for the survivor (`asked_for` B gives A).
4. A signed in on /customer: only SCN213-A1 shows. **B's income-protection policy SCN213-B1 does not.**
5. privacy pressed "Not the same person" on C/D, then merged C into D from the form.
   - **C (Mere) signed in and saw D's details: "Given name Wiremu", wiremu.kingi email.** D saw only his own policy.
   - Undo a merge worked. Both see their own details again, and each stream carries merged_into/absorbed then unmerged/released.
   - A second undo was refused: "… is not merged into …".
6. Merging D into D was refused: "a party cannot be merged into itself". Good.

**Findings**

- **F2-13-1, blocker.** Hurts customers: a privacy breach.
  - After a wrong merge, the **absorbed** person signs in and sees the survivor's personal details: name, email and birth date. The customer page reads the master by the session's party, and the master answers with the survivor's record.
  - Repro:
    1. privacy, /records → Duplicates → Merge two records, survivor D (pty_4d22b2c0…), absorbed C (pty_c34db8d8…).
    2. Sign in as `sub-pty_c34db8d8…` on /customer → Your details shows Wiremu.
  - Good looks like:
    - a sign-in bound to an absorbed record is suspended or re-confirmed after a merge
    - a merge is refused or needs a second person when a review was just dismissed as not the same person
  - Cause, as I believe it: the master's absorbed-to-survivor redirect (`crates/customers/src/store.rs:1014`, merge), used by `/app/v1/profile`.
- **F2-13-2, major.** Hurts the customer.
  - A merge does not move policies. After B was merged into A, Hana's sign-in still shows only SCN213-A1, so she loses sight of her income protection.
  - Nothing in the merge result or on the screen says what moved (policies, relationships, consents, sign-in).
- **F2-13-3, major.** Hurts the customer.
  - The survivor keeps only its own facts. Hana's married name Scn-TeRangi is not on the merged record, and history shows only A's statements. "Her name as she last gave it" fails.
- **F2-13-4, major.** Hurts the privacy officer.
  - The Duplicates list shows only `rev_…` and `pty_…` ids, with "Raised 11/17/2026". There are no names, birth dates or match strength, and no link to open either record.
  - "Open both records side by side" cannot be done.
  - The new business duplicate refusal names no candidate (`records.html:156`).
- **F2-13-5, major.** Hurts the privacy officer.
  - The Merge button pre-fills the **newer** record as the one that stays (`records.html:136`: survivor = `x.party`). If accepted, the customer's original record would be absorbed with her sign-in.
  - The merge has no confirmation, no reason field and no "hard to undo" warning.
- **F2-13-6, major.** Hurts privacy and customers.
  - "Not the same person" asks no reason (it posts `{}`, `records.html:137`).
  - A later merge of the same pair goes through with no warning that it was dismissed.
- **F2-13-7, major.** Hurts the new business officer.
  - After "Add even if they may already be known", the page tries to open the new record. nb lacks `customers.view`, so the page reports "None of your roles allows this action … customers.view" as if the add had failed (`records.html:176`). She is likely to add the person a third time.
- **F2-13-8, minor.**
  - The record pickers call `/staff/v1/find?q=` with an empty query, which answers 400 and logs console errors on every Duplicates visit.

**Not done:** step 4 (st7 deciding a scheme's possible match) and step 7 (merging a held record). I ran out of the call budget after the C/D breach. Self-merge was checked.

---

## Round 2 "Things noticed while designing": confirmed or refuted

- **Item 3, the author is the only product approver.** Refuted for this stack: st15b approves.
- **Item 15, privacy has no `policy.view` and records has no `customers.find`.** Confirmed.
  - privacy is refused `staff_policy` and cannot see SCN212-1 to have it re-rated.
  - records could not find Tamati's policy by number, and needed the internal id.
- **Item 16, names shown as references.** Confirmed throughout /documents and /records: approvers, author, recipients, the audit actor and duplicates.
- **Item 12, two "todays".** Confirmed in the customer master:
  - recorded and valid-from dates are the wall clock (2026-11-17), while the books say 2027-11-15
  - "Issued at 2027-11-14T12:00:00Z" on documents is the business date in UTC.

## Findings by severity

- **Blocker:** F2-13-1.
- **Major:**
  - SCN2-10: F2-10-1, F2-10-2, F2-10-3, F2-10-4, F2-10-6, F2-10-11
  - SCN2-11: F2-11-1, F2-11-2, F2-11-3, F2-11-4, F2-11-6, F2-11-10
  - SCN2-12: F2-12-1, F2-12-2, F2-12-3
  - SCN2-13: F2-13-2, F2-13-3, F2-13-4, F2-13-5, F2-13-6, F2-13-7
- **Minor:**
  - SCN2-10: F2-10-5, F2-10-7, F2-10-8, F2-10-9, F2-10-10, F2-10-12
  - SCN2-11: F2-11-5, F2-11-7, F2-11-8, F2-11-9, F2-11-11, F2-11-12
  - SCN2-12: F2-12-4, F2-12-5, F2-12-6, F2-12-7
  - SCN2-13: F2-13-8
