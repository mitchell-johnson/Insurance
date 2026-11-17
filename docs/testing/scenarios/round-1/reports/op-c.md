# Operator op-c: SCN-08, SCN-10, SCN-13, SCN-06

Scripts and screenshots were kept outside the repository.

Set-up the scenarios got wrong, and what I did instead:
- SCN-08: you cannot make "a deliberately wrong email (`.con`)" with `customer()`, because it hard-codes `@example.nz`. I copied it into `mkcust.js` with an email option.
- SCN-13: `Journey.policy` sets every life's party to the policyholder and keeps the template's birth date (`1988-06-02`). To make a policy owned by one person with another person insured, and with the right birth date, I passed `lives` through `over`. Separately, the staff screen has **no "reduce sum insured" change** (F-13-1), so step 4 could not be done as written. I used a change of frequency with an amount above the limit to exercise the second-approval path instead.
- SCN-06: the records officer cannot open `/records` or `/documents` (F-06-1). No one in the cast can open `/documents`.

---

## SCN-08: a customer sets up signing, fixes a mistyped email and tells us about a claim

**Outcome: partly completed.**

**What I did:** I made a customer with the email `scn08.hurtback@example.con`, issued an `income_protection` policy and signed in on `/customer`. I read the policy and tried to save a new email before signing was set up. I then set up the browser (the phone approves it through the route, which is test-stack set-up) and saved bad and good emails and a local-format mobile number. I told them about a claim with a future date, then with last Tuesday (2027-11-09). I put a hold on, asked to lift it and downloaded my record. I signed out and back in, then checked the claims console as `st3`.

**What worked:**
- The premium shown, NZ$194.61, equals `oracle.premium` (19461 cents).
- Before signing is set up, saving says "Changing your details needs your signature. Set up this browser under Signing first."
- Setting up the browser works, and both devices are listed.
- After a correct save, the new email is held.
- The claim form asks nothing about health. The claim shows at once under Claims as "open", told 2027-11-15.
- The claim is in the `income protection new` queue in the claims console.
- The hold goes on.
- The export downloads only once the browser can sign, and holds the corrected email, the history of the change and the hold.
- After signing out and back in, everything is as I left it and the browser still signs.

### F-08-1 (major): every customer-master refusal on the customer page reads "The request failed (422)."
- **Who it hurts:** every customer changing their details or their hold.
- **Steps:** sign in on `/customer`, set up signing, go to Your details. Type the email `scn08 hurtback` (or any bad value the browser does not catch), or the mobile `021 555 0123`, and press Save. Or press "Ask to lift it" on a hold.
- **Happened:** the message under the form is "The request failed (422)." The real reason ("email must be an email address", "mobile_phone must be a phone number in international form…", "removing the hold needs a passed identity check…") is in the response and never shown. Nothing is shown beside the field.
- **Should:** say the reason in plain words beside the field.
- **Cause:** `crates/channels/src/http.rs:474` (`mastered`) passes the master's `{"error":{"code","message"}}` body through unchanged. `crates/channels/ui/app.js:226-227` reads only `data.detail` (problem+json), so it falls back to the generic text. `intake_answer` (`http.rs:488`) has the same shape.

### F-08-2 (major): a NZ mobile in local form is refused, and the email typed with it is lost
- **Who it hurts:** customers; nearly every NZ customer types `021 …`.
- **Steps:** on Your details, type a correct email and `021 555 0123`, then press Save.
- **Happened:** the save is refused with a 422 (shown as F-08-1), and the email change in the same save is not stored. Only `+64 21 555 0123` is accepted.
- **Should:** turn local NZ form into `+64…`, as the group file reader already does, or say the format beside the field.
- **Cause:** `crates/customers/src/validate.rs:79-84` (`phone` accepts only `+` form). The group file reader normalises with `international` (`crates/group/src/mapping.rs:344`), and the customer route does not.

### F-08-3 (blocker for this step): a customer cannot lift their own hold, because the code it asks for is never sent
- **Who it hurts:** customers who put a hold on.
- **Steps:** on Your details, press "Put a hold on", then "Ask to lift it".
- **Happened:** a dialog says "Enter the code we sent to your phone". No code is sent, and nothing anywhere issues one. Any value is refused as "The request failed (422)". The underlying error is `verification_needed`: "removing the hold needs a passed identity check from the last 24 hours". So "Keep the hold" and the "lifts on <date>" message could not be reached.
- **Should:** either send and check a code, or say "call us: we check it is you, then the hold lifts in a week".
- **Cause:** `crates/channels/ui/customer.html:295-299` asks for a code. `crates/customers/src/party.rs:836-854` wants the `evidence_ref` of a passed identity verification, which only staff record. It is unclear whether a missing driver should send this code; no stand-in exists for it.

### F-08-4 (minor): a future claim date is refused after the claim is sent, the wrong next step is given, and the rejected claim stays in the list
- **Who it hurts:** customers.
- **Steps:** on Your policies, under Tell us about a claim, choose "Unable to work", enter 2027-12-01 and press Tell us.
- **Happened:**
  - The date field has no `max`, so the date is sent.
  - The message is "Claims could not take this: the event date is after today. Please call us." The next step should be "choose a date on or before today", not a call.
  - The form is then reset, so the browser now says "Please fill out this field".
  - A "notify claim — rejected" row stays in Your requests.
- **Should:** refuse beside the date before sending, say how to fix it, and leave no rejected request in the list.
- **Cause:** `customer.html:20-21` (no `max`) and `:171-181`.

### F-08-5 (major): the claims queue and claim view do not show who the customer is, or their phone
- **Who it hurts:** claims assessors (`st3`), and customers who wait for a call.
- **Steps:** sign in on `/claims` as `sub-st3`, choose the queue "income protection new", then open the claim.
- **Happened:**
  - The queue row is `app-33f5f638-…  6  0  unassigned`: no name, no policy number, no cover.
  - The claim view shows the policy *id* in lower case, `life-1`, and no name.
  - The customer's new phone `+64215550123` appears nowhere, so "the claims team will call" has no number to call.
- **Should:** show the customer's name, the policy number and a contact number (allowed for the claims template, which holds `personal`).
- **Cause:** `crates/channels/ui/claims.html:283` (queue columns) and `:197-201` (summary).

### F-08-6 (minor): claims console amounts have no dollar sign
- **Who it hurts:** claims assessors.
- **Steps:** on `/claims` as `sub-st3`, open the claim.
- **Happened:** the reserve reads `6000.00`, and admitted, payable and paid read `0.00`.
- **Should:** read NZ$6,000.00.
- **Cause:** `claims.html:27` defines a local `money` without the currency; `portal.money` is the shared one.

### F-08-7 (minor): the claim finder hides why it found nothing
- **Who it hurts:** claims assessors.
- **Steps:** on `/claims` → Claim, with no purpose filled in, type the customer's name or the policy number.
- **Happened:** "No claims found for that." The real reason, "Say which case, call or task you are working on first", is swallowed. With a purpose filled in, the same text finds the claim at once. The purpose field sits far below, in the Act card.
- **Should:** say "Say which case you are working on first", and put the purpose field above the finder.
- **Cause:** `claims.html:265`, `portal.policies(text).catch(() => [])`.

### F-08-8 (minor): codes instead of words on the customer's own policy
- **Who it hurts:** customers.
- **Steps:** sign in on `/customer` and open the policy.
- **Happened:**
  - The product reads `income-protection` and the cover `income`.
  - "Life insured" reads `life-1`, not the person's name.
  - The income benefit is "NZ$6,000.00" with no "a month".
  - "Payments: paid" sits beside "NZ$194.61 is due".
  - Device "Set up" dates are in US format from the real clock (11/17/2026), not the business date.
  - The claim reference is a raw `app-<uuid>`.
- **Should:** show product and cover names in words, the life's name, "NZ$6,000.00 a month", a payment standing that agrees with the amount due, and NZ-format dates.

### F-08-9 (minor): the Privacy Act copy is a raw event dump of the customer-master record only
- **Who it hurts:** customers.
- **Steps:** on Your details, once signing is set up, press "Download my record".
- **Happened:** `my-record.json` is the internal event stream: `seq`, `caller: new-business`, `actor: ic-e2e-new-business`, `valid_from: 0001-01-01`. It has no policies, claims, requests or devices.
- **Should:** be a readable copy of everything held, as the page promises ("everything we hold about you").
- **Cause:** `crates/channels/src/http.rs:887-893` returns the master's export as it is.

### F-08-10 (minor): a stale message stays under the email field
- **Who it hurts:** customers.
- **Steps:** before signing is set up, save a new email. Then set up signing, come back and type an email without `@`.
- **Happened:** the browser's own tooltip blocks the save, but the old "needs your signature" message is still under the form, which now reads as the reason.
- **Should:** clear the old message when the person acts again.

---

## SCN-10: a customer's assistant tries to do everything by MCP

**Outcome: completed.** It found one serious defect.

**What I did:** I made a customer with a phone, a `term_life` policy, adviser `d1` appointed and a `decline_indexation` request prepared by `d1`, plus a second customer with their own policy. Over `/mcp` as the customer I called:
- `initialize` and `tools/list`;
- the reads;
- `make_request` without `signed`, then `approve_request` with no signature and then with a forged one;
- `change_my_profile` and `export_my_data`;
- `my_document`;
- `revoke_device` for the phone, then an approval signed with a challenge the phone obtained before it was revoked;
- `represented_person`, `my_policy` on the other customer's policy and on a made-up ID;
- `claim_membership` eleven times.

I then signed in as `sub-p18`.

**Tools:** the customer is listed 46 tools: `status`, the `my_*` and request tools, and also the 7 adviser tools and 6 employer tools. A staff tool (`find`) is not listed, and calling it answers "this tool needs a sign-in you do not hold". `adviser_book` is refused in words ("the adviser portal is for advisers…"). `employer_schemes` *works* and returns `[]`, which is harmless.

**What worked:**
- All reads work.
- Approving without a signature is refused: "approving this needs your device's signature".
- A forged signature is refused.
- `change_my_profile` and `export_my_data` are refused as "needs proof of an enrolled device's key".
- After `revoke_device`, the phone is listed with `revoked` set, and an approval signed by it is refused. Directly, the refusal is `device_revoked`: "this device is no longer enrolled".
- Another person's policy and a made-up one answer the same 404, and so do a stranger and a made-up `represented_person`.
- `my_document` answers 503 `documents_unavailable` for every ID, so they are indistinguishable. The documents service is not in the stack.

### F-10-1 (blocker, security): a customer's session with no device can cancel their policy, unsigned and with no confirmation
- **Who it hurts:** customers, and the insurer (cover lost, possible refund and commission clawback).
- **Steps:** POST `/mcp` with `x-dev-subject: sub-<customer>`, calling `tools/call` `make_request` with `{"body":{"id":"…","policy":"<own policy>","kind":"cancel_policy","module":"policy-servicing","payload":{}}}`. The same works on `POST /app/v1/requests` with any customer session.
- **Happened:** `accepted`, status `applied`. `my_account` then shows the policy `cancelled`, `ended: true`. It was never signed, and there was no confirmation step. Changing an email, by contrast, needs the device.
- **Should:** cancelling a policy (and any change that gives up cover) needs the enrolled device's signature, like `surrender`, which *is* refused unsigned.
- **Cause:** `crates/channels/src/state.rs:89`, where `MONEY_KINDS` lacks `cancel_policy` (and `decline_indexation`), and the check at `state.rs:914`. `change_frequency` also applied unsigned.

### F-10-2 (major): `change_sum_insured` is advertised but silently never carried out
- **Who it hurts:** customers, and assistants acting for them.
- **Steps:** call MCP `make_request` with `kind: "change_sum_insured"` (the tool's own example in its schema).
- **Happened:** accepted with no error. In `my_requests` it stays `pending` for ever, and nothing tells the customer that it will not happen.
- **Should:** refuse it at once with "this change is not available here; call us", or carry it out.
- **Cause:** `crates/channels/src/mcp.rs:286` gives `change_sum_insured` as the example. `crates/channels/src/servicing.rs:27-38` carries only `decline_indexation`, `change_frequency` and `cancel_policy`, and `carried_by_sweep` (`:126-129`) leaves unknown kinds pending, with no refusal back to the customer.

### F-10-3 (minor): device refusals say what is missing but not what to do
- **Who it hurts:** customers' assistants.
- **Steps:** call MCP `change_my_profile` or `export_my_data` without the device, or `approve_request` with a forged signature.
- **Happened:** "Refused (401 device_required): changing your details needs proof of an enrolled device's key" does not say "do this in the app on your phone, or set up a browser under Signing". A forged signature is refused with "that is not yours to do", which reads like a permissions problem rather than "the signature did not check out".

### F-10-4 (minor): `p18`, a sign-in with no customer record, gets an empty refusal
- **Who it hurts:** a customer we have no record for yet.
- **Steps:** POST `/mcp` with `x-dev-subject: sub-p18`, calling `tools/call` `my_profile`.
- **Happened:** "Refused (404 ): " with no code and no words. `my_account` answers `policies: []`, which is fine.
- **Should:** "We have no customer record for you yet…".
- **Cause:** the master's 404 body is passed through `mastered` (`http.rs:474`) and has no `error.code` or `error.message` that the MCP wrapper reads.

### F-10-5 (minor): nothing slows guessing employee IDs
- **Who it hurts:** scheme members whose IDs could be guessed.
- **Steps:** as a customer with the device, POST `/app/v1/memberships` with `kaifresh` and employee IDs `1001` to `1010` (or an unknown scheme). Over MCP every `claim_membership` is refused `device_required`, so an assistant cannot guess at all.
- **Happened:** every attempt gets the same answer, `no_such_member`: "no member of that scheme has that employee ID and your birth date". That reveals nothing about who is employed. But eleven tries in a row are not slowed or stopped.
- **Should:** slow or stop repeated guesses.

### F-10-6 (minor): MCP schemas leave an assistant guessing
- **Who it hurts:** customers' assistants.
- **Happened:**
  - `make_request`'s `payload` is `{...}`, and the tool does not list the kinds that are carried (`servicing::KINDS` holds that text).
  - `approve_request` with no `body` says "body is required", although the approval itself has nothing to fill in except the signature.
  - `my_profile` reports `lifecycle: "prospect"` for a customer with a policy in force.

---

## SCN-13: a phone call with only a name and a birth date

**Outcome: partly completed.** Reducing the sum insured is not possible; I used a change of frequency for the second-approval path.

**What I did:** I made Mereana `Tawhaiscndli` (born 1984-03-07, owner and life insured of `SCN13-POL-MWK75DL8-4`) and Mereanna `Tawhaiscndli` (born 1984-07-03, life insured on `SCN13-POL-MWK75DL8-5`, which is owned by Hemi `Rautaoscn`). As `st5b` on `/staff`:
- I pressed "A customer is on the phone" and searched in several forms;
- I opened the right policy and checked the caller on both policies;
- I entered a change above the limit, tried to agree it myself (by route; the screen gives her no queue), and read My access.

Then `st5l` agreed it in the queue, and the customer approved it signed on `/customer`. I then made MCP calls as `st5b`, and `n7` declared a connection and searched.

**What worked:**
- "A customer is on the phone" makes a case reference, and it fills the purpose everywhere.
- "Mereana T… 7/3/1984" and "1984-03-07" find only her. Search results show name, birth date and that person's policies.
- Check a caller:
  - for the policyholder: "You may discuss… This is the policyholder. Confirm who they are…";
  - for Mereana on Hemi's policy: "Do not discuss… Say: I'm sorry, I can't confirm whether we hold a policy…", with usable words;
  - every check is in the access log.
- The change above the $500 limit went to the second-approval queue. `st5l`'s row shows the amount in dollars and "above their limit", and Agree works.
- The customer saw "1 change is waiting" and approved it with a signature.
- Self-approval by route is refused: "the person who entered a change is not its second approver".
- Over MCP, `find`, `check_caller` and `changes_for_second_approval` work. `claims_question` and `finance_question` are refused "your roles do not include claims/finance". `customer_export`, `merge_customers` and `quote` are refused "None of your roles allows this action. Ask your manager for the role or a one-off grant."
- After `n7` declared the connection, a name search shows "A record closed to you… Pass the call to a colleague", and opening the policy is refused with clear words.

### F-13-1 (blocker for the scenario's goal): staff cannot reduce cover for a customer
- **Who it hurts:** service officers, and customers who want lower cover.
- **Steps:** sign in on `/staff` as `sub-st5b`, open a policy and look at "Make a request for the customer" → Change.
- **Happened:** the only options are Change payment frequency, Decline indexation and Cancel policy. There is no reduction of sum insured, although the core has `Command::ReduceCover` (`crates/core/src/command.rs:846`).
- **Should:** reducing cover is the commonest service change, so it should be offered, with the cover and the new amount in dollars.
- **Cause:** `crates/channels/ui/staff.html:14` and `crates/channels/src/servicing.rs:27-38` (see F-10-2).

### F-13-2 (major): a two-digit birth year finds nobody and says nothing
- **Who it hurts:** service officers, while a caller waits.
- **Steps:** on `/staff`, press "A customer is on the phone", search "Mereana Tawhaiscndli 7/3/84" or "07/03/84".
- **Happened:** "Nobody and no policy matches that."
- **Should:** read 84 as 1984, or say "write the year in full".
- **Cause:** `crates/channels/src/find.rs:45` uses `%d/%m/%Y`, which takes "84" as the year 0084 and searches with that date.

### F-13-3 (major): a family name alone, or the end of a policy number, finds nothing and the message misleads
- **Who it hurts:** service officers.
- **Steps:** on `/staff`, search "Tawhaiscndli" (two people hold that name), then "DL8-4" or "MWK75DL8-4" (the end of the policy number).
- **Happened:** "Nobody and no policy matches that… a policy number can be typed in part." Only the *start* of the number matches.
- **Should:** say "add a given name or birth date to search people". Either match the end of a policy number, or say that only the start works.
- **Cause:** `find.rs:62-108`; prefix matching at `find.rs:124`.

### F-13-4 (minor): a date in US order silently finds the look-alike
- **Who it hurts:** service officers, and both customers.
- **Steps:** on `/staff`, search "Mereana Tawhaiscndli 3/7/1984".
- **Happened:** the only result is **Mereanna**, born 3 Jul 1984, which is the other woman. Nothing flags that the given name differs or that the match is weak, although the API carries `strong: false` and `score: 60`.
- **Should:** mark weak and near-name matches ("not the name you typed"). Showing "Strong match" only when it is strong is not enough of a warning.

### F-13-5 (major): a failed policy lookup leaves the previous policy on screen, under its own title
- **Who it hurts:** service officers, and the customer whose policy is acted on.
- **Steps:** open Mereana's policy from search. Then type Hemi's policy *number* `SCN13-POL-MWK75DL8-5` into "Open a policy you have the number of" and press Open, without picking from the list.
- **Happened:** "Nothing was found with that reference…" (a 404 on `/staff/v1/policies/SCN13-POL-…`, because the route wants the lower-case id). The heading still says "Policy SCN13-POL-MWK75DL8-4" and the caller check and change form still act on that earlier policy. A caller check I then ran was recorded on the wrong policy.
- **Should:** accept the policy number as typed, and clear or disable the old policy view when a lookup fails.
- **Cause:** `staff.html:90-92` (`show(… $("pol").value)` sends the typed number as the id; `pol-view` is not hidden on failure). The MCP `check_caller` also needs the internal id, and the policy number gives a 404.

### F-13-6 (major): the customer approves "change frequency" without being told to what or for how much, and afterwards nothing shows it
- **Who it hurts:** customers.
- **Steps:** after `st5l` agrees the request, sign in on `/customer` as the policyholder → To approve.
- **Happened:** the row reads "change frequency". It does not say "to yearly" or give the new premium (the calculator gives NZ$1,913.22 a year against NZ$165.58 a month). After "Approved: change frequency." the request is `applied`, but the policy view shows no frequency and the bill still reads NZ$165.58 monthly.
- **Should:** show the change in words with the amount before signing, then the new frequency and the next premium.
- **Cause:** `customer.html:224-226` (`waiting` shows only `words(r.kind)`) and `open()` (`:127-151`, no frequency). The staff queue also omits the frequency: `staff.html:161`, where `what()` shows the note and amount but not `payload.frequency`.

### F-13-7 (minor): references instead of words on staff screens
- **Who it hurts:** service officers, team leads, and claims staff with a personal connection.
- **Happened:**
  - The `st5l` queue row shows the policy *id* (`scn13-pol-…`) rather than its number, and "st5b" rather than a name.
  - The policy view shows `pty_…` in "About", `life-1`, "Layer 1", "Receivable 1" and `term-life`.
  - `n7`'s confirmation dialog and the connections list show `pty_648a…` instead of the name.
  - Dates are US-format from the real clock.
  - `st5b`, who may not agree changes, is still offered "Agree one by its request reference".

### F-13-8 (minor): after declaring a connection, a policy-number search still reveals the policy
- **Who it hurts:** the customer `n7` is connected to.
- **Steps:** as `n7`, declare the connection, then search `/staff` for `SCN13-POL-MWK75DL8-4`.
- **Happened:** the result lists the policy number, product and "in force". Opening it is refused (403, with clear words).
- **Should:** show only "closed to you".

### F-13-9 (minor): a staff member is offered all customer-realm tools over MCP
- **Who it hurts:** staff using an assistant, and the insurer.
- **Steps:** POST `/mcp` with `x-dev-subject: sub-st5b`, calling `tools/list`.
- **Happened:** `sub-st5b` is listed 116 tools, which include all 46 customer, adviser and employer tools (`my_account`, `approve_request`, …), as well as every staff tool.
- **Should:** list only the tools the person may use. This confirms the design note "tool lists by sign-in, not role", and goes beyond it: a realm she does not hold is listed too.

---

## SCN-06: records and legal, a legal hold and a notice that went out wrong (probe)

**Outcome: blocked.**

**What I did:** I opened `/records` and `/documents` as `records`, `legal` and `st5b`; as `records` I tried `/staff` to find and open a policy; and I called the document routes directly.

### F-06-1 (blocker): the records officer, legal counsel and service officer cannot open the Documents console
- **Who it hurts:** records officers and legal counsel.
- **Steps:** sign in on `/documents` as `sub-records` (or `sub-legal`, `sub-st5b`).
- **Happened:** "This page is not open to you. Your roles do not include documents" (403 on `/staff/v1/documents/me`). So I could not try legal holds, templates, notice defects, letters requested or pending sends at all.
- **Cause:** the console needs `documents.view` (and `documents.legal`, `documents.remediation`, …), listed at `crates/channels/src/correspondence.rs:31-47` and checked at `http.rs:2652`. No role template in `e2e/lib/cast.js` holds any of them: `records` has only `documents.download` and `policy.view` (`cast.js:47`). This is mainly a cast/seed gap and should be added to design note 4. But a "records" role that cannot open the documents console is also a product question.

### F-06-2 (blocker): the records officer cannot find or open a policy at all
- **Who it hurts:** records officers.
- **Steps:** sign in on `/records` as `sub-records`; then on `/staff`, with a case, search the policy number or type it in "Open a policy".
- **Happened:**
  - `/records` says "You do not work on customer records."
  - On `/staff`, the search answers "None of your roles allows this action" (no `customers.find`).
  - Opening by the internal id answers "Your roles do not include the class of data this action needs", because the template holds only `ordinary` and a policy view needs `personal`.
  - All staff tabs are still shown (Second approvals, Held balances, Access administration).
- **Should:** a records officer can find a policy by number and see its documents list.

### F-06-3 (test-stack limit): downloading a document
- **Steps:** as `sub-records`, GET `/staff/v1/documents/<id>/pdf?purpose=…`.
- **Happened:** 503 `documents_unavailable`, "the documents module is not configured here". The documents service is not in the stack, and the answer is a clean refusal rather than a hang. For a user it should read "Documents can't be fetched just now; try later." There is no banner, because no one reaches the page.

---

## Things noticed while designing: confirmed or refuted

- **#2 (documents service not in the stack): confirmed.** `my_document` and `staff_document` answer 503 `documents_unavailable`, and `/documents` has no one who can open it (F-06-1).
- **#3 (MCP tool lists by sign-in, not by role): confirmed, and worse than stated.** A customer is listed 46 tools including adviser and employer tools. `st5b` is listed 116, including all 46 customer-realm tools (F-13-9). Calls outside the job are refused in words. One listed tool outside the job works but is harmless: `employer_schemes` returns `[]` for a customer.
- **#4 (staff actions nobody holds): confirmed** for `customers.export`, `customers.merge` and `new_business.quote` (refused to `st5b` with "None of your roles allows this action…"). Add every `documents.*` action except `download` (`documents.view`, `.legal`, `.remediation`, `.privacy`, `.triggers`, `.delivery`, `.configuration`, `.approve_*`), so the Documents console has no user (F-06-1).
- **#5 (cast gaps): confirmed for `p18`.** `my_account` answers with no policies; `my_profile` gives an empty 404 (F-10-4). Raw `{...}` MCP bodies confirmed for `make_request` (F-10-6).

## Parts I played (test-stack, not product)
- I opened the customer-master records, bound the sign-ins and enrolled the phones as `e2e/lib/customer.js` does.
- I issued policies with `POST /v1/policies`.
- The phone app's approval of a browser and its device-proofed calls were made with Node's crypto, as the e2e lib does.
- The adviser's prepared request went through `POST /app/v1/requests` as `d1`, as the e2e `prepared()` does.
- Every decision (saving, claiming, holds, approvals, caller checks, entering and agreeing changes, declaring connections) went through the screens or `/mcp`.
- No business day was closed.
- I cancelled one of my own policies (`SCN10-POL-MWK73KVK-3`) to show F-10-1; nothing seeded was touched.
