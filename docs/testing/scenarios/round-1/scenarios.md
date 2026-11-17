# User scenarios for the insurance core (local e2e stack)

Twenty-four scenarios for an operator who acts as the user against the running stack (`node e2e/stack/stack.js`, portal at http://127.0.0.1:17090, business date 2027-11-15). They are written from the user's side: a goal, a starting situation, the steps as the person thinks of them, a realistic complication, and what "good" looks like. They are meant to find what the 328 scripted journeys in `e2e/tests` miss: whole tasks, hand-offs between people, mistakes, and the `/mcp` endpoint (which no scripted test touches).

## Index

| ID | Title | Persona (cast key) | Channel | Flags |
|---|---|---|---|---|
| SCN-01 | Adviser prepares the wrong change, takes it back, client approves the right one | D1 Tessa Maddox (`d1`), customer | screens | hand-off |
| SCN-02 | Adviser's AI assistant does the Monday book review | D1 (`d1`), D2 (`d2`) | mcp | |
| SCN-03 | An advice provider loses its licence mid-week | D9 (`d9`, `d9a`), D6 (`d6`), ST13 (`st13`), Distribution (`dist`) | screens | hand-off, shared-state |
| SCN-04 | New application: adviser submits, underwriter decides, customer accepts | D4 (`d4`), ST1 (`st1`), ST2 (`st2`), customer | both | hand-off, probe |
| SCN-05 | Employer's monthly file with a bad row, a look-alike joiner and a second approver | E1 (`e1`, `e1b`), ST7 (`st7`) | screens | hand-off |
| SCN-06 | Records and legal: a legal hold and a notice that went out wrong | Records (`records`), Legal (`legal`), ST5 (`st5b`) | screens | probe |
| SCN-07 | A labour-hire employer's assistant runs the scheme by MCP | E3 (`e3`) | mcp | |
| SCN-08 | A customer sets up signing, fixes a mistyped email and tells us about a claim | own customer (P-style) | screens | hand-off to SCN-11 |
| SCN-09 | A member who left work links their cover and keeps it | own customer, ST7 (`st7`), E1 (`e1`) | screens | hand-off |
| SCN-10 | A customer's assistant tries to do everything, including what needs the phone | own customer, P18 (`p18`) | mcp | |
| SCN-11 | A health claim with a missing invoice, above the assessor's limit | ST3 (`st3`), ST4 (`st4`), Payments (`payee`, `payee2`) | screens | hand-off |
| SCN-12 | A death claim: two signatures, a funeral advance and an estate account | ST4 (`st4`, `st4b`), Legal (`legal`), N7 (`n7`) | screens | hand-off |
| SCN-13 | A phone call with only a name and a birth date | ST5 (`st5b`, `st5l`), N7 (`n7`) | both | hand-off |
| SCN-14 | A bounced premium, an arrangement and a hardship referral | ST5 (`st5`), ST6 (`st6`) | screens | needs-date-move |
| SCN-15 | A claims payment run where the bank sends one item back | ST10 (`st10`), ST17 (`st17`, `st17b`), Payments (`recon`, `paybad`) | screens | |
| SCN-16 | Treasury approver works the afternoon cut-off through an assistant | ST17 (`st17`) | mcp | |
| SCN-17 | Finance gets the month ready to close | ST10 (`st10`, `st10b`), ST11 (`st11`), ST12 (`st12`), Auditor (`auditor`) | screens | |
| SCN-18 | A unit price that moved 9% on a quiet day | ST8 (`st8`), ST9 (`st9`, `st9l`) | screens | shared-state (one fund) |
| SCN-19 | Group administration takes on a new scheme whose payroll file changed | ST7 (`st7`, `st7r`), E3 (`e3`) | screens | |
| SCN-20 | Held commission: a disputed replacement and an adviser under investigation | D4 (`d4`), D5 (`d5`), Conduct (`conduct`), Commission (`comops`, `comapp`), ST10/ST11 | screens | hand-off |
| SCN-21 | A complaint that crosses four teams | Frontline (`front`), ST14 (`st14j`, `st14`), Conduct (`conduct`), Legal (`legal`), ST6 (`st6`) | screens | hand-off |
| SCN-22 | Reinsurance reviews a large life and the month-end sweep | Reinsurance (`ri1`, `ri2`), Auditor (`auditor`) | both | |
| SCN-23 | Operations' morning health check | ST16 (`st16`), Anyone (`stranger`) | both | |
| SCN-24 | A claim that turns up non-disclosure | ST4 (`st4`), ST2 (`st2`, `st2b`), ST1 (`st1`) | screens | hand-off |

MCP is used in SCN-02, 04, 07, 10, 13, 16, 22 and 23. Only SCN-14 moves the business date.

## Ground rules for the operator

**Signing in.** Every page shows the development sign-in card. Type the subject `sub-<key>` from `e2e/lib/cast.js` (for example `sub-d1`, `sub-st5b`); leave roles empty, except the platform engineer: subject `sub-st16`, roles `operations`. `sub-nobody` is a sign-in the insurer does not know.

**MCP.** `POST http://127.0.0.1:17090/mcp`, one JSON-RPC message per call (`initialize`, `tools/list`, `tools/call` with `{name, arguments}`), with the same development headers a browser session carries: `x-dev-subject: sub-<key>` and, for operations, `x-dev-roles: operations`. Do not send an `Origin` header from another site. The tool list is by sign-in (public 1, customer realm 45, staff 70, operations 6), not by role, so an adviser is offered employer tools and a pricing maker is offered claims tools; each call is then checked by its route. In every MCP scenario record (a) the names listed, (b) that no tool of a realm the person does not hold is listed, and (c) that a listed tool outside the person's job is refused with words that say why. Treat a listed tool that *works* outside the person's job as a serious finding.

**Data you must make yourself.** The seed gives staff, two advice providers (Harbourline `fap-d6` with advisers `adv-d1`, `adv-d2` on engagements `eng-d1`, `eng-d2`; Clearwater `fap-d9` with `adv-d9a`), and one group scheme `kaifresh` (policy `pol-kaifresh`, number `WPL-KAIFRESH`, administrator `e1`). It gives no retail policies, no claims, no complaints and no customer-master records for `p1` or `p18`. The scripted tests only read `kaifresh`: do not change it.

- *A retail policy.* There is no new-business screen. Issue one the way `Journey.policy` in `e2e/lib/suite.js` does: take a template from `e2e/.run/shipped.json` (`applications.term_life` $500,000 life, `income_protection` $6,000 a month, `health_family`, `invest_plan`), give it your own `policy`, `number`, `group`, set `policyholder` and the life's `party`, inception a fortnight before the business date, and `POST /v1/policies` on the API as caller `new-business`. Use a prefix of your own (for example `SCN05-...`) in every reference.
- *A customer who can sign.* Follow `customer()` and `enrolBrowser()` in `e2e/lib/customer.js`: open a record in the customer master as new business (random name and birth date, because the master matches on them), bind the sign-in, enrol a phone key you hold, and when the page asks, sign `enrol\n<device>\n<key>` with that key. Write down the name and birth date: staff scenarios find this person by them.
- *A group scheme of your own.* As `register()` in `e2e/lib/group.js`: issue the scheme policy on the API, then as `st7` register the scheme, its file layout and its first administrator through the group console. Files are CSV with the head `Emp No,First,Last,DOB,Category,Salary,Site`, birth dates as dd/mm/yyyy. After an upload, write the virus-scan result on the file as `scan()` does (Defender is not in the stack).
- *The parts of missing drivers.* The bank pump, sanctions screening, the unit-pricing file reader, the reinsurance driver, the underwriting intake and the documents service are not running. Where a scenario needs what one of them would report (a file settled, an item rejected, a valuation, a payee screened), report it to the module as the tests do (`System.tell` in `e2e/stack/cluster.js`), and note in your result that you played that part. Every decision a person makes goes through a screen or `/mcp`.

**Shared stack.** Other operators use the same stack. Do not close business days (only SCN-14 may, last, and alone). Do not suspend, freeze or end anything seeded without putting it back (SCN-03, SCN-18). Prefix your references.

**What to record everywhere.** Any internal reference a person had to type that the screen could have offered (party, case, payee, partition, cents); any amount shown without dollars or in cents; any code shown instead of words (`benefit_payments`); any refusal that does not say what to do next; any console error or failed request the page did not explain; anything a person could see or do that their role should not allow.

---

## Advisers and advice providers

### SCN-01 Adviser prepares the wrong change, takes it back, client approves the right one
- **Persona / channel:** D1 Tessa Maddox (`d1`), then her client (a customer you create); screens (`/adviser`, `/customer`).
- **Goal (Tessa):** "My client rang: she is paid fortnightly now and wants her premium to match. I'll set it up and she can approve it tonight."
- **Start:** A customer with a phone (see ground rules) holding a `term_life` policy issued monthly. The customer appoints Tessa from `/customer` ("Your adviser": adviser `adv-d1`, engagement `eng-d1`).
- **Steps:**
  1. Tessa opens her book, finds the policy by its number and opens it; checks cover, status, claims (status only) and documents.
  2. In a hurry she picks **Cancel policy** instead of **Change payment frequency** and sends it.
  3. She notices under "Requests you have made" and wants to take it back. (There is a withdraw route; see whether the portal offers it or whether she is stuck.)
  4. She sends the right change: fortnightly.
  5. The client signs in on a browser that has never signed. She sees both (or one) request under "Waiting for you", tries to approve, is told to set up signing, does so, declines the cancellation if it is still there, and approves the frequency change.
  6. Tessa refreshes: the request shows approved; the policy shows fortnightly.
- **Complication:** the wrong request; the unsigned browser; the client double-presses Approve.
- **Good looks like:** the request list shows the policy **number** and the change in words; nothing changes on the policy until the client approves; the cancel request can be withdrawn by Tessa or declined by the client and never takes effect; the new fortnightly amount is shown in dollars and is 1/26 of the loaded annual premium (compare with `e2e/lib/oracle.js`); a second press of Approve changes nothing and does not show an error; the approval refusal before signing tells the client exactly what to do; Tessa never sees health details.
- **Stories:** US-D01-2, US-D01-3, US-P01-3, US-P18-1.

### SCN-02 Adviser's AI assistant does the Monday book review
- **Persona / channel:** D1 (`d1`), with D2 (`d2`) as the contrast; MCP only.
- **Goal:** "Ask my assistant: what is in my book, is anything held from me, were any of my new policies treated as replacements, and line up Mrs X's frequency change for her to approve."
- **Start:** The policy from SCN-01 (or a new one) in Tessa's book; a second policy of another customer that is *not* appointed to her.
- **Steps:**
  1. `initialize`, `tools/list` as `sub-d1`. Save the list.
  2. `my_account`, `adviser_book`, `adviser_held_balance`, `adviser_replacements`, `policy_claims` for a policy in the book.
  3. `adviser_statement` with a number the adviser guesses (she does not know one).
  4. `make_request` to prepare a change of frequency for the client.
  5. The assistant overreaches: `my_policy` on the policy not in her book; `adviser_practice` (she is not a provider); `employer_schemes`; `scheme_question` on `pol-kaifresh`; `approve_request` on her own prepared request; `appoint_adviser` naming herself on the other customer's policy; a staff tool by name (`find`, `staff_policy`) even though it is not listed.
  6. Repeat `adviser_book` as `sub-d2`: Viliami sees none of Tessa's policies.
  7. Retry step 4 with the same `idempotency_key`.
- **Complication:** a guessed statement number; an assistant that tries tools because they are listed.
- **Good looks like:** the list has no staff or operations tool; each employer or other-customer call is refused or answers "not found" identically for a policy that exists and one that does not (no way to probe existence); calling an unlisted tool gives a clear MCP error, not a 500; the prepared request appears on `/adviser` and on the client's `/customer` page exactly as one made on screen; the retried write is done once; `approve_request` by the adviser is refused because the customer's device signs; the missing statement is answered in plain words; tool annotations mark `remove_adviser` and `revoke_device` destructive. Record as a finding (not a failure of this scenario) that the list is not narrowed to what an adviser may do.
- **Stories:** US-D01-2, US-D02-1, US-D08-1.

### SCN-03 An advice provider loses its licence mid-week
- **Persona / channel:** Compliance (`st13`) and Distribution (`dist`) at `/commission`; Clearwater's adviser (`d9a`) and Clearwater (`d9`) at `/adviser`; Harbourline (`d6`); a customer; screens. **Shared-state:** restore Clearwater's licence to active at the end and say so in the result.
- **Goal (compliance):** "The register shows Clearwater's licence suspended from today. Stop new business, hold what is owed, keep their customers serviced." (adviser): "Why can't I do anything, and what happens to my clients?"
- **Start:** A customer with a policy who appoints `adv-d9a` (find the engagement reference from the commission console or `adviser_book`).
- **Steps:**
  1. `d9a` opens the book: active, can prepare a change. Prepares one and leaves it waiting.
  2. Compliance records the licence as suspended from today, picking the provider from a list rather than typing `fap-d9`.
  3. `d9a` refreshes: what does the portal say, and what happened to the waiting request?
  4. The customer opens `/customer`: can they still approve the waiting change? Can they appoint a Harbourline adviser instead (`adv-d1`, `eng-d1`) and remove the old one?
  5. `d6` (Harbourline) checks "Advisers under you": the moved policy shows under Tessa, and nothing of Clearwater's shows.
  6. Compliance restores the licence; `d9a`'s portal comes back.
- **Complication:** the change in flight when the licence is suspended; the customer is not told by anyone and finds out from the page.
- **Good looks like:** the adviser portal is read-only with the reason in words and the date; Send is disabled, not failing after a press; the in-flight request is either refused at approval with a reason the customer can act on, or clearly marked; the customer is never stranded (can appoint another adviser at once); held lines show to `d9a` under "Held from you" with "why it is held"; `comops` cannot record a licence (role); every step is in the commission history with who did it.
- **Stories:** US-D09-1, US-D08-1, US-D06-1.

### SCN-04 New application: adviser submits, underwriter decides, customer accepts (probe)
- **Persona / channel:** D4 Anika Sharma (`d4`), ST1 (`st1`), ST2 (`st2`), a customer with a phone; screens and MCP.
- **Goal (Anika):** "Quote AIA Essentials Life, $400,000, for a 41-year-old non-smoker, submit it, and get her on cover." (underwriter): "Decide what is mine, pass up what is not." (customer): "Read the offer and accept it."
- **Start:** Nothing. This is a probe: the underwriting intake is not in the stack, the adviser portal has no application screen, and `d4` is bound to `adv-d4` but has no engagement in the seed. The point is to record exactly how far a real user gets and what each dead end says.
- **Steps:**
  1. Anika signs in to `/adviser`. What does an adviser with no engagement see? Is there any way to start or resume an application? (Expected: none. Record it.)
  2. Through MCP as `sub-d4`, look for a quoting or application tool. (Only staff have `quote`.)
  3. As `st1` through MCP, call `quote` with an application for `aia-essentials-life`, then for the shipped `term_life` template. Note who, if anyone, may quote: no role template in the cast carries `new_business.quote`.
  4. As `st1` at `/underwriting`: queues with counts, "Cases you hold", the reasons and exclusions libraries. Try to open a proposal that does not exist by a mistyped reference.
  5. If a proposal can be put in by any supported route (ask the stack owner; do not write to stores), carry it on: `st1` requests evidence, tries a loading (not allowed for a junior) and the case routes to `st2`; `st2` offers terms with a loading and an exclusion picked from the library; the customer sees the offer under "Your applications", asks us to look again once, then accepts with a signature; the policy issues and the first bill equals the quote.
- **Complication:** the adviser comes back two days later to a half-finished application and expects it to be where she left it.
- **Good looks like:** each dead end is said in plain words (no blank page, no raw 403 or 404 JSON); a quote, where allowed, is in dollars per period with fee and GST shown and equals the independent calculation; a junior is never *offered* a decision above $500,000 or a loading; the customer's offer answer is refused without the device signature and says so; nobody types a proposal, life or case reference that a queue could have given them.
- **Stories:** US-D04-1, US-ST01-1, US-ST02-1, US-D01-2.

---

## Employers and scheme administrators

### SCN-05 Employer's monthly file with a bad row, a look-alike joiner and a second approver
- **Persona / channel:** E1 Kai Fresh (`e1`, `e1b`) at `/employer`; ST7 (`st7`) at `/group`; screens.
- **Goal (HR):** "Send November's joiners and leavers, fix whatever the insurer rejects, and get the bill right."
- **Start:** A scheme of your own for employer `hr-e1` (not `kaifresh`), with `e1` as administrator, founding member E001 (staff, $60,000, Auckland), and two members already applied by a first file.
- **Steps:**
  1. `e1` names `e1b` as an administrator and ticks "Removing a member" as needing a second administrator.
  2. `e1` uploads `2027-11.csv`: three good joiners, one row with the birth date in US order (13th month), one row with salary `sixty thousand`, one joiner with the same name and birth date as an existing member under a new employee number, one salary change, and E001 twice.
  3. She waits for the virus check, opens the preview and reads who joins, leaves and changes, and which rows were refused and why.
  4. She approves. The look-alike goes to group administration.
  5. `st7` sees the item in the work waiting, presses Decide, is shown the candidates by name and birth date, and decides it is the same person.
  6. `e1` asks to remove a member from the "Ask for a change" form but mistypes the employee ID; then correctly. `e1b` approves from the row under "Your requests". `e1` tries to approve her own.
  7. `e1` uploads the corrected two rows as a second file, then by mistake the first file again.
  8. She checks Members, one member's record and Invoices.
- **Complication:** bad rows; a duplicate; the same file twice; a wrong employee ID.
- **Good looks like:** each refused row is listed with its row number, the employee number and a reason a payroll clerk understands; the good rows go ahead; the preview's counts equal what is applied; a member's cover is the category multiple of salary up to the automatic limit, in dollars; the invoice equals the independent calculation for the members on cover; the repeated file changes nothing and says so; the remove request with a wrong ID is refused naming the ID; `e1` cannot approve her own change; the member record shows category, salary and cover and nothing about health or matching; the employer never sees that a joiner matched an existing customer.
- **Stories:** US-E01-1, US-P01-2, US-ST07-1, US-E04-1.

### SCN-07 A labour-hire employer's assistant runs the scheme by MCP
- **Persona / channel:** E3 Southline Labour Hire (`e3`); MCP only.
- **Goal:** "We do not hand over names. Declare this month's headcount and payroll for the warehouse pool, send the small named file for office staff, and tell me what we owe."
- **Start:** A scheme of your own for employer `hr-e3` with `e3` as first administrator. (If an unnamed pool has to be set on the scheme at registration, register it with one; note what you needed.)
- **Steps:**
  1. `tools/list`; `my_account`; `employer_schemes`.
  2. `scheme_question` `scheme`, then `invoices`.
  3. `scheme_command` `declare_pool` for the warehouse category: headcount 140, total salary $7,840,000 from today. Then again with headcount 0 and with salary typed in cents by mistake.
  4. `upload_membership_file` with a three-row CSV as base64; write the scan result; `scheme_question` `file` until the preview is ready; `scheme_command` `approve_file`.
  5. `scheme_question` `member` for one employee, and for an employee who does not exist.
  6. Overreach: `employer_schemes` shows no other employer; `scheme_question` on `pol-kaifresh`; `set_scheme_admin` removing herself (the only administrator); `policy_claims` on the scheme (status only).
- **Complication:** units (dollars against cents) with no screen to guide; the only administrator removing herself.
- **Good looks like:** the tool descriptions are enough to form each body without reading source (record every place the assistant had to guess a field name: `declare_pool` and `approve_file` bodies are only sketched as `...`); money is unambiguous in every answer; the pool rates and bills on attributes alone and no person record is created; the other employer's scheme is "not found", the same as a scheme that does not exist; removing the last administrator is refused with the reason; a file that is still in quarantine cannot be approved and says to wait for the scan.
- **Stories:** US-E03-1, US-E01-1.

---

## Customers

### SCN-08 A customer sets up signing, fixes a mistyped email and tells us about a claim
- **Persona / channel:** a customer you create (in the manner of P1 or P18), holding an `income_protection` policy; `/customer`; screens.
- **Goal:** "I've hurt my back and can't work. I want to tell them, make sure they have my right email, and stop anyone moving money while I'm laid up."
- **Start:** Customer with phone; policy issued. The customer-master record has a deliberately wrong email (`.con`).
- **Steps:**
  1. Signs in, reads the policy: cover, what they pay and when, in dollars.
  2. Tries to change the email before setting up signing; is told what to do; sets up the browser and approves it "from the phone".
  3. Types the new email with a space and no `@`, then correctly. Changes the mobile in local format (`021 555 0123`).
  4. "Tell us about a claim": picks the cover, "Unable to work", a date in the future by mistake, then last Tuesday.
  5. Puts a hold on money going out; then asks to lift it; then keeps it.
  6. Downloads a copy of everything held.
  7. Signs out and in again: everything is as left.
- **Complication:** signing not set up; bad email; a future date.
- **Good looks like:** what is owed equals the independent calculation; a bad email or future date is refused beside the field in plain words, nothing saved; the claim form takes no health details and says the claims team will call; the claim shows under Claims with a status and date at once, and appears in the claims console queue (SCN-11) with the customer's name; lifting the hold says the date it takes effect (a week); the export downloads only after signing and contains the corrected email, the hold and the history of the change; a claims assessor later sees the *new* phone number.
- **Stories:** US-P01-3, US-P18-1, US-ST03-1.

### SCN-09 A member who left work links their cover and keeps it
- **Persona / channel:** a customer you create who is also a member of your scheme; ST7 (`st7`); E1 (`e1`); `/customer`, `/employer`, `/group`; screens.
- **Goal:** "I've finished at Kai Fresh. HR said I can keep my life cover without health questions. How?"
- **Start:** Your scheme from SCN-05; a member row whose name and birth date equal the customer's master record, applied by file.
- **Steps:**
  1. The customer opens "Cover from your workplace" and links it with the scheme code and employee ID, first with a colleague's employee ID (birth date will not match), then their own.
  2. The employer removes the member effective last Friday (second administrator approves).
  3. The customer returns: the page shows cover that may be continued, the products, the most they may continue and by when.
  4. They ask for more than the offer allows, then the allowed amount, monthly; sign.
  5. They read the new policy and its first premium.
  6. `st7` looks up the ex-member in the group console.
- **Complication:** the wrong employee ID; asking for too much; coming back after the removal is backdated.
- **Good looks like:** the wrong ID is refused without revealing whether that employee exists or whose it is; the offer shows a deadline as a date and the limit in dollars; too much is refused naming the limit; taking it up asks no health questions, issues a policy of the customer's own that appears under Policies with the premium in dollars; the offer cannot be taken twice; the employer's page shows the member gone and nothing about the continuation; group administration sees the continuation offer and that it was taken.
- **Stories:** US-P01-2, US-E01-1, US-ST07-1.

### SCN-10 A customer's assistant tries to do everything, including what needs the phone
- **Persona / channel:** a customer you create (with phone), and `p18` Kiri Parata as the "known sign-in with no record"; MCP only.
- **Goal:** "Check my cover and what I owe, change to yearly payments, send me my documents, and my phone was stolen so kill it."
- **Start:** Customer with phone and a `term_life` policy; an adviser-prepared request waiting (as in SCN-01 step 4).
- **Steps:**
  1. `tools/list`; `my_account`; `my_policy`; `my_profile`; `my_requests`; `my_devices`; `my_spending_hold`.
  2. `make_request` for a change of frequency without `signed`.
  3. `approve_request` on the adviser's request without a signature, then with a made-up signature.
  4. `change_my_profile` to a new email; `export_my_data`.
  5. `my_document` for a document on the policy, and for an ID that is not theirs.
  6. `revoke_device` for the phone. Then `my_devices`, and try `approve_request` properly signed by the revoked key.
  7. Overreach: `represented_person` for a stranger; `my_policy` on another customer's policy; `claim_membership` with a guessed employee ID three times.
  8. As `sub-p18`: `my_account`, `my_profile`.
- **Complication:** every step that needs the phone; the documents service is not running; a sign-in with no customer record.
- **Good looks like:** reads work; each phone-only step is refused with words that say "this needs your enrolled device" and what to do, never a bare 401 or a stack trace; a forged signature is refused; after revoking, the device is listed as revoked and its signature no longer approves; documents answer either with a PDF or a plain "documents are unavailable right now" (not a 500); another person's document and a missing one are indistinguishable; repeated membership guesses do not reveal who is employed (note whether anything slows or stops guessing); `p18` gets a clear "we have no record for you yet" rather than an error.
- **Stories:** US-P18-1, US-P01-3.

---

## Claims

### SCN-11 A health claim with a missing invoice, above the assessor's limit
- **Persona / channel:** ST3 (`st3`), ST4 (`st4`), payee maintainers (`payee`, `payee2`); `/claims`, `/payments`; screens.
- **Goal (assessor):** "Mrs K had knee surgery. Check she is covered, get the surgeon's invoice, decide and pay the hospital."
- **Start:** A customer with a `health_family` policy. The claim is notified by the customer (SCN-08 style) or by the assessor from a call using "Notify a new claim".
- **Steps:**
  1. `st3` arrives: the queue loads itself; the new claim is there with the customer's name and policy number. She takes it.
  2. Cover check: in force, premium paid to, definitions, exclusions, limits for surgery (per event $50,000, excess $500).
  3. She needs the invoice and the surgeon's report: requests both, with a due date.
  4. A week later only the invoice has come: $7,850. She records it and its cost, picks the cause from the list.
  5. She tries to admit: above her $5,000 limit. The claim goes to someone who can; `st4` picks it up from the queue with the file as `st3` left it.
  6. `st4` admits $7,350 (after the $500 excess) and goes to pay the hospital: the payee is not verified. A maintainer registers the hospital's account, a second confirms it (you play the screening result).
  7. `st4` pays; the payment waits for a run in `/payments`.
- **Complication:** missing evidence; over the limit; an unverified payee; `st3` types the invoice as 78.50 first.
- **Good looks like:** nobody types a claim, policy, cover or task reference; the cover check gives facts, not a verdict; outstanding evidence shows as an open task with its date; an over-limit admission is not offered to `st3`, or is refused naming her limit in dollars and who can take it; the excess and the payable amount are worked out and shown ($7,850 less $500); nothing can be paid to an unverified payee and the refusal says who verifies payees; the payment appears under payments "waiting" as a provider payment with the same amount; the customer's `/customer` page shows the claim's status changing but never the cause; the adviser and employer views show status only.
- **Stories:** US-ST03-1, US-PR1-1, US-ST17-1.

### SCN-12 A death claim: two signatures, a funeral advance and an estate account
- **Persona / channel:** ST4 (`st4`), second signature (`st4b`), Legal (`legal`), payee maintainers, N7 Leilani Fonoti (`n7`); `/claims`, `/payments`; screens.
- **Goal:** "The family rang. He died on Saturday. They need money for the funeral now and the rest to the estate."
- **Start:** A customer with a `term_life` policy ($500,000 life), in force more than seven days. The caller is the widow, who is not the policyholder.
- **Steps:**
  1. `st4` finds the policy from the dead man's name and birth date, notifies a death claim, records the caller and her relationship.
  2. She asks for the death certificate; while waiting, pays a funeral advance: first $20,000 (over the $15,000 maximum), then $15,000.
  3. The certificate arrives. She admits $500,000: above $250,000 it needs a second signature. She tries to countersign herself; `st4b` countersigns with the file open.
  4. The estate's account is registered by a maintainer; it is held until `legal` approves it.
  5. She pays the balance ($485,000, plus late interest only if past 30 days).
  6. `n7` (related party `cust-p60`): if you issue a second policy to party `cust-p60` and raise a claim on it, Leilani must have no path to it from the queue, the search or a typed reference.
- **Complication:** the over-limit advance; self-countersign; the estate account; a caller who is not the owner.
- **Good looks like:** the advance is refused above $15,000 naming the limit; the balance is the sum insured less the advance, shown in words and dollars; the admission does not take effect until a different senior signs; the estate payee cannot be paid until legal approves, and the screen says who must act; the payment to the estate exceeds the run item limit ($100,000) seeded for the route, so see what the payments console says and whether there is an honest path (record this as a configuration or product finding either way); the policy ends; no further premium is billed; the reinsurance console (SCN-22) later shows a recovery or notice for the ceded part.
- **Stories:** US-ST04-1, US-N07-1, US-ST17-1, US-R01-1.

### SCN-24 A claim that turns up non-disclosure
- **Persona / channel:** ST4 (`st4`), ST2 (`st2`, `st2b`), ST1 (`st1`); `/claims`, `/underwriting`; screens.
- **Goal (assessor):** "The GP notes show he was treated for chest pain a year before he applied and never said. Underwriting needs to tell me what we would have done." (underwriter): "Prepare the remedy and get it signed."
- **Start:** A customer with an `income_protection` policy and an open claim on it (notified from a call).
- **Steps:**
  1. `st4` records the GP notes as evidence received and wants to refer the claim to underwriting. Is there a button from the claim, or does she have to tell someone outside the system?
  2. `st2` opens the underwriting console, starts a non-disclosure review for the policy (picked, not typed), reads what was disclosed (`read disclosures`, with a purpose), and prepares a remedy: an exclusion from inception, reason picked from the library.
  3. `st2` tries to sign it herself. `st1` (junior) tries. `st2b` opens it, reads it and signs and sends.
  4. Back in claims, `st4` sees the policy's new terms and declines the claim: she must have spoken to the claimant first, and uses a decline reason (`R-NDISC`).
  5. The customer looks at `/customer`.
- **Complication:** the intake's disclosure record may not exist for a policy issued straight to the core: the console must say so plainly.
- **Good looks like:** the remedy waits for a second senior and the preparer can only withdraw it; a junior is not shown the remedy forms; a missing disclosure record is "nothing on file", not an error; the decline is refused until a conversation with the claimant is recorded; the claimant-facing status never shows the cause or the words "non-disclosure" to the adviser or employer; the customer sees a decision and where to complain; every reader of the disclosure is in the access log with a purpose.
- **Stories:** US-ST02-1, US-ST01-1, US-ST03-1.

---

## Service, collections and hardship

### SCN-13 A phone call with only a name and a birth date
- **Persona / channel:** service officer (`st5b`), service lead (`st5l`), N7 (`n7`); `/staff` and MCP.
- **Goal:** "A woman rings: 'It's Mereana, born 7th of March 1984, I've moved and I want to lower my cover.' I need to find her, make sure I may talk to her, and do it."
- **Start:** Two customers you create with the same family name and near-identical given names (one is the caller); the caller holds a `term_life` policy; the other is the life insured on a different policy owned by someone else. Note the names and birth dates.
- **Steps:**
  1. `st5b` presses "New call" and types the name and `7/3/84` in the one box. Then tries `3/7/1984` (US order), the family name alone, and the last five characters of the policy number.
  2. Two people come back. She picks the right one by birth date and opens the policy.
  3. The caller turns out to be the policyholder's ex-partner on one of them: "Check a caller" tells her whether to talk and the words to use.
  4. For the real policyholder she enters a reduction of the sum insured from $500,000 to $300,000, with the reason. It is above her limit: it waits for a second person.
  5. She tries to agree it herself. `st5l` finds it in the second-approval queue and presses Agree.
  6. The customer approves on `/customer` with a signature; the policy shows $300,000 and the next premium falls.
  7. MCP as `sub-st5b`: `find` with the same text, `check_caller`, `changes_for_second_approval`; then tools outside her job: `claims_question`, `finance_question`, `customer_export`, `merge_customers`, `quote`.
  8. `n7` declares a personal connection to the caller (found by name) and then searches for her.
- **Complication:** two look-alike people; date order; the wrong caller; over the limit.
- **Good looks like:** search answers name and birth date only until a record is opened; the case in hand fills every purpose field; she never types a party reference; the caller check is logged and gives usable words; the refusal to self-approve says who can; the queue row shows policy number, change in words and amounts in dollars; after approval the premium equals the independent calculation for $300,000; through MCP the same reads work and each out-of-role tool is refused in words; after declaring the connection `n7` sees "closed to you" and nothing else; every opening is in the access log (`staff_me`).
- **Stories:** US-ST05-1, US-N07-1, US-P18-1.

### SCN-14 A bounced premium, an arrangement and a hardship referral (`needs-date-move`)
- **Persona / channel:** collections officer (`st5`), hardship specialist (`st6`), service lead (`st5l`); `/collections`, `/staff`, `/customer`; screens. Run last and alone; restart the stack afterwards.
- **Goal (collections):** "Work today's queue: who is closest to lapsing, call them, agree something they can keep to." (customer): "My pay was short. Don't cancel me."
- **Start:** Three customers with monthly `term_life`, `income_protection` and `health_family` policies. Before moving the date, `st5` (or whoever may) checks "Products sending nothing" and sets each product's dunning sequence from the shipped best estimate.
- **Steps:**
  1. Close days until the first premiums are overdue and reminders are due (use `ic-e2e-close`, one day at a time, checking each close reconciles). Pay one policy in full, one in part (`POST /v1/payments` as cash matching, quoting the policy number), one not at all.
  2. `st5` opens `/collections`: the queue is ordered by days to lapse, then annual premium.
  3. For the part-payer she proposes three instalments; first with dates in the past, then with one more instalment than an officer may agree (goes to the team lead).
  4. The non-payer says she has lost her job after an illness. `st5` records the outcome "Referred to the hardship team" and applies for a premium holiday, ticking "touches on health".
  5. `st5` tries to read the note afterwards. `st6` decides it; whoever applied cannot decide.
  6. Close further days: the arrangement's instalment falls due and is paid; the health policy passes 60 days and lapses with no suspension first; the customer on holiday is not chased.
  7. The customer checks `/customer` throughout.
- **Complication:** part payment; an arrangement beyond authority; a health-related hardship note.
- **Good looks like:** arrears, instalments and what clears them are in dollars and equal the independent calculation (`oracle.arrears`); the queue order is as the story says; a product with no sequence is listed under "Products sending nothing" before any notice is missed; the health note is unreadable to collections; the maker cannot decide the hardship case; suspension and lapse fall on the product's own days; the customer page says what is owed and by when, and never surprises; notices wait under "Notices waiting for the documents service" (the service is not running) rather than vanishing. Note: no scripted test opens `/collections`, so everything here is first contact.
- **Stories:** US-ST05-1, US-ST06-1, US-P01-1, US-P01-3.

### SCN-06 Records and legal: a legal hold and a notice that went out wrong (probe)
- **Persona / channel:** records officer (`records`), legal counsel (`legal`), service officer (`st5b`); `/records`, `/documents`; screens.
- **Goal (records):** "Legal says the Parata file is going to a dispute. Put a hold on its documents and pull the policy schedule. And the lapse notice template has the wrong phone number: stop it going out."
- **Start:** A customer with a policy. No scripted test opens `/records` or `/documents`, and the documents service is not started in this stack, so this is first contact with both pages.
- **Steps:**
  1. `records` opens `/records` and `/documents`. What loads, what says it is waiting, what says it cannot be reached?
  2. Finds the policy's documents by policy number (role has `policy.view` but not `customers.find`: can a records officer find a policy at all?).
  3. Places a legal hold with a reason; checks it under legal holds; tries to download the schedule with a purpose.
  4. Raises a notice defect on a template (picked from the template list) and disables it; checks "letters requested" and "pending sends".
  5. `legal` and `st5b` open the same pages: who may do which?
  6. Lift the hold at the end.
- **Complication:** a dependent service that is down.
- **Good looks like:** with the documents service absent, each page says so in a banner and keeps working where it can; no spinner forever, no raw error; templates, holds and defects are picked from lists; a hold needs a reason and shows who placed it; a download asks for a purpose and is in the access log; a role that may not act is not offered the button.
- **Stories:** US-ST14-1 (evidence), US-ST13-1; no direct story exists for the records officer (a gap in the story pack).

---

## Payments, finance and unit pricing

### SCN-15 A claims payment run where the bank sends one item back
- **Persona / channel:** maker (`st10`), approvers (`st17`, `st17b`), reconciler (`recon`), the conflicted approver (`paybad`), payee maintainers; `/payments`; screens.
- **Goal (maker):** "Get today's claim payments out before the 4 pm file cut-off." (reconciler): "Find out why $1,200 came back and get it to the right account."
- **Start:** Four payees registered and confirmed by two different maintainers and screened clear (you play screening); a fifth entered but unconfirmed. Payments waiting: ideally the ones from SCN-11 and SCN-12; otherwise items added to a run as the tests do.
- **Steps:**
  1. `st10` arrives: sees what waits, prepares a lump-sum claims run (reference and date made for her), takes the waiting payments, adds one to the unconfirmed payee by mistake, and one twice (same account and amount within three days).
  2. She checks the run. She looks for Approve (not offered).
  3. `st17` opens it: approval is stopped by the unconfirmed item and the possible duplicate. `st10` cannot release the duplicate herself; she removes the unconfirmed item.
  4. `st17` approves. `paybad` (approver who also maintains payees) tries to approve a run containing a payee he confirmed.
  5. You play the bank: acknowledged, three items settled, one rejected (account closed), then the day's statement.
  6. `recon` sees the exception on its row, takes it and resolves it; the rejected payment returns to waiting, the payee's account is corrected by the maintainers, and it goes out in tomorrow's run.
  7. `st10` reads the control totals.
- **Complication:** unconfirmed payee, duplicate, a rejected item, a conflicted approver.
- **Good looks like:** run types read as words; all amounts in dollars; submitted equals acknowledged plus rejected, and settled plus rejected equals the run total, on screen; the rejected item is an exception with the bank's reason, not a silent gap; the claim behind it shows "payment returned" to the assessor; maker cannot approve, approver cannot edit payees, the conflicted approver is refused with the reason; a second release needs a second person; the statement reconciles with no open break; still-typed references (party, payee) are noted against the "Still typed" table in `docs/staff-ui-review.md`.
- **Stories:** US-ST10-1, US-ST17-1, US-BK1-1.

### SCN-16 Treasury approver works the afternoon cut-off through an assistant
- **Persona / channel:** ST17 (`st17`); MCP only (with `st10` preparing a run on screen first).
- **Goal:** "I'm between meetings. Tell me what is waiting for me, whether we have the money, approve the runs that are clean and tell me why the others are not."
- **Start:** Two checked runs from `st10`: one clean, one holding an unconfirmed-payee item.
- **Steps:**
  1. `tools/list`; `staff_me`; `payments_question` `me`, `open_runs`, `readiness`, `funding_forecast`, `control_totals`, `run` for each.
  2. `payments_command` to approve the clean run; then the blocked run.
  3. Approve the clean run again with a new idempotency key.
  4. Overreach: register a payee; confirm a payee; prepare a run; `find` a customer (no `customers.find`); `claims_command` to admit a claim; `finance_command` to post a journal; `staff_access` to grant herself `payments.maker`.
  5. Ask for a run that does not exist; send a command with a misspelt `type`.
- **Complication:** the assistant has only `{"type": "<command>", ...}` to go on.
- **Good looks like:** the approval through MCP is the same record as one on screen (shows on `/payments` under `st17`'s name); the blocked run is refused with each blocking item named; a second approval is a no-op or a clear "already approved", never a double submission; every overreach is refused with the missing permission in words; a misspelt command is answered with the valid command names or a pointer to them (record if the assistant is left guessing: the description gives no list); a self-grant waits for an administrator and is not active.
- **Stories:** US-ST17-1, US-ST10-1.

### SCN-17 Finance gets the month ready to close
- **Persona / channel:** operator (`st10`), approver (`st10b`), lead (`st11`), actuary (`st12`), auditor (`auditor`); `/finance`; screens.
- **Goal (lead):** "It is the 15th. I want to know today what will stop November closing, fix what we can and line up the waivers."
- **Start:** Some business on the books: at least the policies and payments made by other scenarios (new business counts in the conduct figures).
- **Steps:**
  1. `st10` arrives: role, state of the books, work waiting in words; opens November's close: every check with owner, due date and why it is red.
  2. She keys an accrual journal: mistypes an amount (`1,2500.00`), unbalanced by a cent, into an account code she half-remembers; then correctly, with evidence.
  3. She tries to approve it herself. `st10b` approves. Trial balance and movements show it.
  4. A check she believes is wrong: she reports it, saying what is wrong.
  5. `st11` prepares a waiver for a red blocking check: reason, owner (picked from colleagues), remediation date (a fortnight out by default).
  6. `st11` tries to sign off November now: refused (not yet ended, or red checks). `st12` tries the actuary's sign-off.
  7. Aged suspense: one receipt with a mistyped policy number (pay as cash matching with a reference that matches nothing); `st10` documents it.
  8. `auditor` reads everything and can change nothing. `st12` asks for a frozen extract and checks opening plus movements equals closing.
- **Complication:** a mistyped amount and account; a wrong check; sign-off too early.
- **Good looks like:** the period is filled in for her everywhere; the mistyped amount is caught before sending; an unbalanced journal is refused naming the difference in dollars; maker cannot approve; the trial balance balances and totals to the same figure before and after as debits and credits; sign-off refusal lists exactly what stops it and which sign-offs remain; the unmatched receipt is in aged suspense with its amount and age; the auditor is offered no action buttons; the account code is the one place she still types a code (known gap) and the refusal for a wrong code names it.
- **Stories:** US-ST11-1, US-ST10-1, US-ST12-2.

### SCN-18 A unit price that moved 9% on a quiet day
- **Persona / channel:** maker (`st8`), checker (`st9`), lead (`st9l`); `/pricing`; screens. **Shared-state:** work on one fund only, on its next pricing date as the module reports it; tell other operators which fund; resume it if you suspend it.
- **Goal (maker):** "Strike today's price for the growth fund." (checker): "Why has it moved 9% when the benchmark moved 1%?"
- **Start:** One of the three shipped funds. You play the administrator's file: a valuation and units on issue for the next pricing date.
- **Steps:**
  1. `st8` tries to prepare before the valuation arrives.
  2. The valuation arrives with units on issue that differ from the core's: blocked. A corrected file arrives with a 9% move.
  3. `st8` reads the valuation and prepares: held for review. She looks for a way to approve it herself.
  4. `st9` opens it from the work list, reads each failed check, and rejects with a reason ("asset value looks a digit out").
  5. A third valuation with a 0.8% move: `st8` prepares again, `st9` approves; it is in force on every partition.
  6. Two days later the administrator admits day one was wrong after all: `st9l` raises a price error from the price's own row ("It is wrong"), no new date can be approved meanwhile, the correction is made, and she closes the error with a summary.
  7. A customer with an `invest_plan` policy (and their adviser) look at the value of their units.
- **Complication:** units mismatch; a held price; an error found after the fact.
- **Good looks like:** each block says which fact is missing or which check failed, with the numbers; mid, buy and sell equal the independent calculation (`oracle`); maker and checker differ, always; a held price cannot be approved without accepting each failed check with a reason; while an error is open no later date can be approved, and the screen says why; the customer's and adviser's unit value uses the price in force and its date; dates default to today; no price reference is typed.
- **Stories:** US-ST08-1, US-ST09-1, US-FA1-1.

---

## Group administration, commission, conduct and complaints

### SCN-19 Group administration takes on a new scheme whose payroll file changed
- **Persona / channel:** group administrator (`st7`), read-only colleague (`st7r`), the employer (`e3` or `e1`); `/group`, `/employer`; screens.
- **Goal:** "Southline's new scheme starts this month. Get it registered, prove their payroll export reads properly, give their HR person access, and keep an eye on the first file."
- **Start:** A scheme policy issued on the core for the employer (ground rules). The HR person has a customer-master record with a known name and birth date (create one and bind a sign-in, or use the employer cast member and note how they are found).
- **Steps:**
  1. `st7` registers the scheme against its policy, found from the policy number.
  2. The employer's sample export has the columns in a different order and `Date of Birth` instead of `DOB`. She tries the old layout against the sample (refused, naming the missing column), adjusts the mapping, tries again (nothing stored), then sets it.
  3. She names the first administrator, found by name and birth date; then tries to name a second "first" administrator.
  4. The employer uploads the first real file. It would remove most of the founding members (HR exported only one site): it is held. `st7` sees it in work waiting, opens it, rings HR, and does not confirm; HR withdraws it and sends the full file.
  5. `st7` freezes the scheme for an hour while a query is open (reason required): files stop. Unfreezes.
  6. `st7r` opens the same scheme: sees, cannot act. `st7` checks a member's cover and the loadings-in-total view.
- **Complication:** a changed layout; a file that would wipe the scheme.
- **Good looks like:** the trial run shows what each sample row would do and stores nothing; the refusal names the column; the mass removal is held with the count and proportion and a plain question; nothing changes on cover until confirmed; frozen means uploads are refused with the reason shown to the employer; the read-only colleague is offered no changing buttons; the administrator is found without typing a party reference; the first invoice equals the independent calculation.
- **Stories:** US-E04-1, US-ST07-1, US-E01-1, US-E03-1.

### SCN-20 Held commission: a disputed replacement and an adviser under investigation
- **Persona / channel:** D4 Anika Sharma (`d4`) or D1 (`d1`) as the writing adviser, D5 Joe Tane (`d5`), conduct officer (`conduct`), compliance (`st13`), commission operations and approver (`comops`, `comapp`), held balance (`st10`, `st10b`, `st11`), distribution (`dist`); `/adviser`, `/replacements`, `/commission`, `/staff` (held balance workbench); screens.
- **Goal (adviser):** "They've treated my new policy as a replacement and held my commission. It wasn't: the old cover was his employer's." (conduct): "Decide it." (finance): "Release what should be released, with two people."
- **Start:** `d4` and `d5` have no engagement in the seed: `dist` adds each as an adviser and starts an engagement with Harbourline (note what `dist` had to type). A customer gives up cover on one policy (cancel it) and a new policy on the same life is appointed to the adviser soon after. If no match is raised without the commission driver, record that and continue with the held-balance part.
- **Steps:**
  1. The adviser sees the match under "Treated as replacements" with the matched amount and reason, and disputes it with a reason.
  2. `conduct` opens `/replacements`, sees the dispute, decides "not a replacement". `comops` tries to open the page (refused).
  3. For Joe Tane: `st13` opens a conduct case and places payment on hold; `d5`'s portal shows the amount held and why, in words he can read.
  4. On the held balance workbench `st10` prepares a hold pending review, then a release for part; tries to approve it; `st10b` approves; a release above the standard profile's $100,000 goes further up.
  5. `st10` tries a write-off for more than is held.
  6. `comapp` and `comops` check the month-end blocks and the aged held report.
- **Complication:** advisers not yet in the structure; a dispute; maker and checker.
- **Good looks like:** the adviser sees exactly what is held, since when and why, and the rest of his income is unaffected; a disputed match shows "Disputed, waiting for a decision" and then "Decided: new business" with the commission released; commission keeps accruing for `d5` while payment is held; the daily control on the workbench equals the sum of held lines before and after; nobody approves what they prepared; refusals name the limit in dollars.
- **Stories:** US-D04-1, US-D05-1, US-D01-3, US-D06-1.

### SCN-21 A complaint that crosses four teams
- **Persona / channel:** frontline (`front`), junior handler (`st14j`), reviewer (`st14`), conduct officer (`conduct`), legal (`legal`), hardship specialist (`st6`); `/complaints`, `/staff`; screens.
- **Goal (frontline):** "He is furious: his claim was declined and nobody rang him back for three weeks. Record it now, in his words." (reviewer): "Get it acknowledged, decided and, if he is still unhappy, to the dispute scheme on time."
- **Start:** A customer (name and birth date known) with a policy and a declined or slow claim (from SCN-11 or SCN-24, or a claim left open).
- **Steps:**
  1. `front` finds the complainant by name, ticks the policy it is about, tries to save without his words, then records them verbatim. He mentions he has just been widowed and is struggling.
  2. `front` offers $150 goodwill on the spot (frontline limit is $100): refused; offers $100.
  3. The queue: `st14j` tries to take it. A vulnerability marker is added (`st6`, trained): the junior, who is not trained, can no longer hold it; `st14` takes it, with every timer showing (acknowledge 5 days, update every 10, decide 20, plus the extra days for a vulnerable complainant).
  4. `st14` needs the claim file: opens the claim from the complaint by link, not by typing; asks the claims handler for access to notes.
  5. She writes the outcome (must be in writing), offers $2,000 ex gratia (within level 3's $5,000; try $6,000), and closes with a cause code from the list (`delay`).
  6. He rejects it. She declares deadlock and prepares the bundle for the external scheme; `legal` reviews it (not the person who releases it).
  7. `conduct` sees a pattern (third `delay` complaint about the same team), opens a conduct case and links this complaint as evidence. `st14` cannot see the conduct case.
  8. The register for this month shows it, with Open.
- **Complication:** over-limit goodwill, vulnerability changing who may handle, an outcome rejected.
- **Good looks like:** the CMP reference is made for them; the complainant and policies are picked; timers show due dates, not day counts to work out; limits are refused in dollars with who may authorise; an untrained handler cannot be given the file and the screen says who can; deadlock cannot be declared before an outcome exists and its notice carries the scheme's details; the bundle's reviewer differs from its releaser; the complaints team sees no trace of the conduct case; the customer's file in `/staff` links to the complaint.
- **Stories:** US-ST14-1, US-ST06-1, US-ST13-1.

---

## Reinsurance and operations

### SCN-22 Reinsurance reviews a large life and the month-end sweep
- **Persona / channel:** reinsurance operations (`ri1`), lead (`ri2`), auditor (`auditor`); `/reinsurance` and MCP.
- **Goal (operations):** "One man now has three policies with us. Are we over our retention on him, did the cessions go, and is anything stopping month end?"
- **Start:** One customer (same party) as the life on three `term_life` policies issued with sums of $500,000, $900,000 and $1,500,000 (override the template's sum insured), which together should pass the retail surplus treaty's retention. The reinsurance driver is not running: instructions will show as not delivered, and that is the true state.
- **Steps:**
  1. `ri1` arrives: what he may do, the exceptions waiting, each worked from its row.
  2. He looks at the life's accumulation: through the console if it offers it, otherwise MCP `reinsurance_question` `exposure` (find out what it needs: he knows a name and policy numbers, not a party reference). Also `balance`, `statements`, `consent_status`.
  3. An exception or notice for one cession cannot be delivered: he retries, then prepares an abandonment with a reason. He tries to sign it. `ri2` signs; the module still checks it can be done.
  4. He prepares a second abandonment by mistake and withdraws it.
  5. Month-end check (starts at last month's end): what in reinsurance stops the close. `st11` should see the same blocker in the finance close (SCN-17).
  6. `auditor` reads it all on screen and via MCP `reinsurance_question`; `reinsurance_command` and `reinsurance_decide` are refused.
- **Complication:** an accumulation across policies; undelivered instructions.
- **Good looks like:** accumulation is shown per life across policies, with sum insured, retained and ceded in dollars adding up to the total; the over-retention part is ceded or flagged for facultative referral, not silently retained; the statement's totals equal the sum of its cession lines; the preparer can only withdraw; someone without the approval role cannot sign; "not delivered" is said plainly with a count; the auditor is offered nothing that changes anything. Record whether a person can get to a life's exposure without typing an internal reference.
- **Stories:** US-R01-1, US-R02-1, US-R03-1, US-ST11-1.

### SCN-23 Operations' morning health check
- **Persona / channel:** platform engineer (`st16`, roles `operations`), a stranger (`stranger`); `/ops`, `/status` and MCP.
- **Goal:** "Before stand-up: are all three regions up with leaders, did last night's close reconcile, is customer search keeping up, and what are customers being told?"
- **Start:** The running stack with whatever the other scenarios have done.
- **Steps:**
  1. Open `/status` without signing in, then `/ops` as `st16`: regions, nodes, leaders, the channel module, "What customers are told". Leave it open five minutes: it refreshes itself.
  2. MCP as `sub-st16` with roles `operations`: `tools/list` (expect `status` plus the six operations tools; note whether customer-realm tools are also offered to a sign-in that is nobody's customer, and that no staff tool is, because `st16` is not on the staff directory).
  3. `regions`, `reconciliation` (business date, every partition's checks, policies still to run), `customer_index` (how far behind).
  4. `verify_customer` for a customer made in another scenario (he has only a name: can he get the party without `find`?), and for a party that does not exist.
  5. He must not run `rewrap_customers` or `reshred_customers` on a shared stack; instead confirm from the tool list that both are marked as writes and that a call with an empty body is refused before doing anything.
  6. Overreach: `find`, `staff_policy`, `claims_question` by name; open `/claims` and `/finance` in the browser.
  7. As `sub-nobody` and with no headers at all: `tools/list`.
  8. A wrong `mcp-protocol-version` header; a JSON-RPC batch; a `GET /mcp`.
- **Complication:** the engineer has no customer or staff access by design and still needs to check one record's integrity.
- **Good looks like:** the public page says in plain words the service is working and agrees with the status tool and with `/ops`; `/ops` shows a leader per region and the same business date as `reconciliation`; reconciliation shows no breaks, or names them; no customer's personal data appears in any operations answer; consoles refuse him with a page that says why; no sign-in gets 401 with a pointer to how to sign in; the unknown sign-in is offered at most the public tool and no data; protocol errors are answered as the code documents (400, "send one message at a time", 405).
- **Stories:** US-ST16-1, US-ST16-2.

---

## Things noticed while designing (check these first; each is a possible finding)

1. **No new-business path for a person.** The adviser portal has no quote or application screen, the only quote route is a staff one (`POST /staff/v1/new-business/quote`), and no role template in the cast carries `new_business.quote`, so nobody can use it. Policies exist in the stack only by `POST /v1/policies` on the API. The adviser-to-underwriter-to-customer hand-off (SCN-04) cannot be completed.
2. **The underwriting intake, documents service, bank pump, reinsurance driver and pricing file reader are not in the stack.** Underwriting queues will be empty; documents and letters cannot be produced; `/documents` and `/records` have no scripted journey at all, nor does `/collections`.
3. **MCP tool lists are by sign-in, not by role.** An adviser is listed employer and customer tools; every member of staff is listed all 70 staff tools. The routes still check each call, but "the tool list matches what that person may do and nothing more" does not hold as built. No scripted test calls `/mcp`.
4. **Staff actions nobody in the cast holds:** `new_business.quote`, `policy.core_view`, `customers.view`, `customers.create`, `customers.change`, `customers.merge`, `customers.export`, `customers.audit`, `staff.approve_configuration`, `staff.review_break_glass`. So the customer-record tools (open a record, merge duplicates, Privacy Act export, who-read-what audit) and a privacy officer persona cannot be exercised by anyone who can sign in.
5. **Cast gaps.** `p1` and `p18` are bound sign-ins with no customer-master record and no phone, so they cannot sign or be found by name; `d4`, `d5` and `d10` are bound but not in the distribution structure; `e3` has no scheme. Many raw MCP bodies are described only as `{...}` (`scheme_command`, `payments_command`, `group_command`, `claims_command`), which leaves an assistant guessing.
6. **Seeded payment limits against seeded claims authority.** The payments routes cap an item at $100,000 and a run at $500,000, while a senior assessor may admit $600,000 and the term life template is $500,000: a single death claim cannot go out in one item (SCN-12).
7. **The shipped application templates are the generic persona products** (`term_life`, `income_protection`, `health_family`, `invest_plan`), not the AIA range in `docs/aia-products.md`; no scripted journey issues an `aia-*` product through the stack.
