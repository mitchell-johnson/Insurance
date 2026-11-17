# Round 2 operator report: op2-a (SCN2-01, SCN2-02, SCN2-03)

Operator: op2-a.

**Shared state.** I changed no seeded record and closed no business day. I did not move the date or stop any node. Everything I made is prefixed SCN201, SCN202 or SCN203:
- **SCN2-01:** customer Aroha Scnwiremu, born 1 Jan 1986, party `pty_6031a0855057471d8b7232889cf7ab6f`. Policy SCN201-1, AIA Essentials Life, $400,000, monthly.
- **SCN2-02:** customer Mereana Scntwotwo, born 2 Apr 1986, party `pty_e9cf8d9fdfff459eb02faad0dfcae121`. Proposal `prop-scn202-1`, now issued as policy SCN202-1. Interim cover SCN202-1-I, now ended.
- **SCN2-03:** two customers:
  - Rawiri Scnthreecousin, born 12 May 1979, `pty_25aa93f0d5ab4a158fab1c75da519e2d`, proposal `prop-scn203-1`.
  - Kahu Scnthreeother, born 20 Aug 1982, `pty_44e891d72a804c2d842f219be0b99abb`, proposal `prop-scn203-2`.
  - Both cases are still open and held by st1b.
  - st1 has a conflict declared against Rawiri's party.
  - `prop-scn203-2` has a tele-interview booked in the past, on 2025-03-01 (F2-03-7).

**Set-up I did off-screen** (allowed set-up, listed here):
- Customer-master records as new business, through `master()` in `e2e/lib/customer.js`. The SCN2-01 record was the exception: nb made it over MCP.
- Sign-in binding and phone enrolment through `System.internal("identity", …)`, the same as `customer()` does.
- `node e2e/stack/play.js issue` for SCN201-1.
- `play.js propose` for prop-scn202-1, prop-scn203-1 and prop-scn203-2.
- `play.js evidence` for the GP report (acknowledged, then received, $125) and the medical exam (received, $180).
- **Playing the customer's phone app.** No screen takes health answers or consents, and MCP refuses them (see F2-02-1 and F2-02-2). So I played the phone app on `/app/v1/proposals/...` with a device proof header (`x-device-id`, `x-device-proof`, as `crates/channels/src/http.rs:343` and `identity.rs:165` define it). I used it for:
  - the disclosure
  - the consents `adviser_access` (yes), `employer_sees_loading` (no) and `obtain_records` (yes)
  - the phone's approval when the customer set up her browser for signing

**Set-up corrections to the scenarios:**
- The AIA templates now exist, so "Things noticed" item 6 is refuted.
- `play.js propose` takes `covers` as a map, for example `{"life":30000000}`.
- A customer's disclosure answers have the shape `{"<question>": [<question version>, {"number": 165}]}`, with `yes_no`, `money` (in cents), `choice` and `text` as the other value types. Nothing a user or an assistant can see says this.
- The evidence request ids are `<proposal>/<life>/<kind>`.

---

## SCN2-01: The new business officer quotes AIA Essentials Life through an assistant

**What I did**
1. As nb on MCP, I ran `tools/list` and quoted for the customer: monthly and yearly, a sum typed in dollars by mistake, a sum over the limit, an age-62 case and an age-77 case.
2. I ran `match_customer` and `create_customer`, then `create_customer` again with a new key, then the same key with a different person.
3. I tried every overreach in step 7.
4. As nb on `/records`, I tried "Add a person" for the same customer.
5. I looked for a way to submit the application or issue the policy, then issued it with play.js.
6. As st5b on `/staff`, I found her by name and opened the policy.
7. As the customer on `/customer`, I read the policy and the first bill.

**Outcome:** completed. The quote leads nowhere, as expected.

**What worked**
- **Tools offered:** `quote`, `find`, `find_customers`, `match_customer` and `create_customer`, plus `status`, `staff_me`, `staff_access` and `colleagues`. No claims, payments or finance tools.
- **Prices:**
  - Monthly is $45.06 and yearly is $510.14. Monthly times twelve is $540.72, which is the yearly figure times 1.06 to within rounding, so the 6% monthly loading is right.
  - The policy fee is $0, so Essentials carries no fee, as the product says.
- **Same person twice:** the second `create_customer` was refused `possible_duplicate`. The `/records` screen refused it too, with good wording: "This person may already be on file. Search for them under Record; if they are someone else, choose 'Add even if they may already be known'."
- **Key reuse (F-SCN02-4):** reusing `idempotency_key` SCN201-cust-1 for a different person is now refused: `409 idempotency_conflict: this idempotency key was used for a different request`. The fix holds for the customer master.
- **First bill:** $45.06 on `/customer` and on `/staff`, the same as the quote to the cent.
- **Overreach:** `merge_customers`, `core_policy`, `staff_policy` and `underwriting_command` are each refused in words: "this tool is not open to you…".
- **Dead ends:** no screen or tool submits an application or issues a policy. `issue_policy` and `open_proposal` answer "there is no tool". nb has no quote screen. "Things noticed" item 5 is confirmed.

### Findings

**F2-01-1 (major): The quote prices cover the product does not allow for the customer's age.**
- **Who it hurts:** the new-business officer and the customer.
- **Reproduce:** as `sub-nb` on MCP, call `quote` with the `aia_essentials_life` template, date of birth 1965-06-01 (age 62) and `sum_insured_minor` 60000000 ($600,000), with `query.case` set.
- **What happens:** it is priced at $343.54 a month.
- **What should happen:** Essentials allows at most $500,000 from age 61 to 70, so the quote should refuse and name that limit. The officer is quoting a price for cover the customer cannot have.
- **Where:** the age-banded limit lives only in underwriting (`products/aia_essentials_life.star` header comment; `docs/aia-products.md` line 149, "Essentials at most $500,000 from 61 to 70"). The core quote, `staff_quote` at `crates/channels/src/http.rs:4110`, never checks it.

**F2-01-2 (minor): The amount refusals do not name the limit.**
- **Who it hurts:** the new-business officer, and the assistant acting for them.
- **Reproduce:** quote with `sum_insured_minor` 400000 (meaning $400,000, typed in dollars), or with 120000000 ($1.2m).
- **What happens:** both answer `409 sum_insured: cover life: sum insured outside the cover's limits`.
- **What should happen:** the refusal should say "below the $50,000 minimum" or "above the $1,000,000 maximum". It gives neither the limit nor which side was crossed, so a dollars-for-cents slip is not obvious.
- **Note:** the age refusal is good: "cover life starts for lives aged 16 to 70; life life-1 is 77".

**F2-01-3 (major): The quote tool makes the officer invent identifiers and a purpose.**
- **Who it hurts:** the new-business officer's assistant.
- **Reproduce:** call `quote` without `query.case`, then read the `body` description.
- **What happens:**
  - Without a case it is refused: "A purpose is required: the case or task you are working on, or a reason code this data accepts". No accepted reason code is listed.
  - The body must be "as the core's IssuePolicy", with `policy`, `number`, `group` and `policyholder`. A walk-in has none of these. I had to use the template's `pol-00000015` and `party-00000015`. The access check was then recorded against `party-00000015`, a party that does not exist.
  - Nothing describes `lives[].factors`, cover codes, or the `excess_option` code (`basis=simplified`).
- **What should happen:** the quote takes product, cover, amount, frequency, birth date, sex and smoker. The ids are made by the system, and a purpose such as "new business quote" is accepted.
- **Where:** `crates/channels/src/http.rs:4110-4128` and the MCP tool definition in `crates/channels/src/mcp.rs`.

**F2-01-4 (minor): The quote answer carries an internal field and no total.**
- **Who it hurts:** the officer's assistant, and through it the customer.
- **What happens:** the answer is `{"partition":3,"premium":{"by_cover":{"life":{"minor":4506}},"policy_fee":{"minor":0},"gst":{},"levies":{}}}`.
- **What should happen:**
  - `partition` is internal and should not be in the answer.
  - There is no total for the period, so the assistant has to add it up.
  - `gst` is an empty object; "GST: none (life cover is exempt)" would say what it means.
- **Also:** the schema says `additionalProperties: false`, but an extra `idempotency_key` argument is accepted silently.

**F2-01-5 (minor): A duplicate refusal names no candidate and gives no next step.**
- **Who it hurts:** the new-business officer.
- **Reproduce:** call `create_customer` a second time with the same name and birth date.
- **What happens:** `409 possible_duplicate: this person may already be known`.
- **What should happen:** the refusal should name the candidate record, or at least say "search with find_customers, or send allow_duplicate: true if they are someone else". `match_customer` does return the candidate, so the information exists.
- **Description guessing:** the `facts` list in `create_customer` (`"facts": [...]`) has no description. The assistant has to guess `[{field, value}]` and the field names.

**F2-01-6 (minor): Unlisted tools give two different kinds of refusal.**
- **Who it hurts:** the assistant, and anyone reading its refusals.
- **What happens:**
  - `customer` and `change_customer` are not in nb's list, but calling them reaches the route. They are refused "None of your roles allows this action… The action you need is customers.view", which is a code, not words.
  - `merge_customers`, `core_policy` and `staff_policy` are refused at the MCP layer with "this tool is not open to you".
- **What should happen:** one consistent refusal for every unlisted tool, naming the permission in words.

**F2-01-7 (minor): The product and its terms show as codes on the staff and customer screens.**
- **Who it hurts:** service staff and the customer.
- **Reproduce:** as st5b, open SCN201-1 on `/staff`; as the customer, open it on `/customer`.
- **What happens:**
  - `/staff` search results and the policy panel show the product as `aia-essentials-life`. The cover row's "About" column shows the raw party id `pty_6031a…`.
  - The policy panel shows "Life life-1", "Layer 1" and "Receivable 1".
  - "Frequency –" is shown although the policy pays monthly.
  - Standing shows "paid" while $45.06 is outstanding.
  - Dates are ISO (2027-11-15).
  - The "Change payment frequency" list offers Quarterly and Half-yearly, which Essentials does not allow (fortnightly, monthly and yearly only).
  - `/customer` shows the product as "Aia essentials life", not "AIA Essentials Life Cover". Its options column shows "basis simplified".
  - `/customer` shows an empty "Your investments" section on a life policy.
- **What should happen:** the product's name, frequencies in words, NZ dates, and only the frequencies the product allows.
- **Where:** the screens build their labels from the codes (`words()` in `customer.html`) instead of the product's name.

**F2-01-8 (minor): Search calls an exact match "not a strong match".**
- **Who it hurts:** service staff.
- **Reproduce:** as st5b on `/staff`, search "Aroha Scnwiremu 1/1/1986", which is the exact name and birth date.
- **What happens:** the result says "Not a strong match: check the name and birth date with the caller".
- **What should happen:** an exact name and birth date should read as a match.

**F2-01-9 (minor): The `/records` "Add a person" tab has a mislabelled list and no case field.**
- **Who it hurts:** the new-business officer.
- **What happens:**
  - The person-or-organisation list is labelled "Action".
  - The case field sits on the Record tab, so "Send" on Add a person first fails with "Say what you are working on first…".
- **What should happen:** label the list "Person or organisation", and show the case on the Add tab too.

**F2-01-10 (minor): The new policy has no documents.**
- **Who it hurts:** the customer.
- **What happens:** after issue, `/customer` shows "No documents yet". There is no policy schedule or welcome letter.

---

## SCN2-02: An AIA Living application with evidence, a loading, a second look and interim cover

**What I did**
1. I opened the proposal as new business: AIA Living, life $800,000, critical conditions $200,000, income $4,000 a month, adviser adv-d1 / fap-d6.
2. The customer opened `/customer`. Then:
   - No screen let her answer the questions, so I tried MCP, then played the phone app: a partial disclosure, then the full disclosure, plus the consents.
3. st1 took the case on `/underwriting`, read the disclosure, tried a +50% offer (refused), asked for a GP report, and handed the case to st2 with the picker.
4. I played the provider: acknowledged, then received with a cost of $125.
5. st2 tried the screen's single-cover decision form (refused), then made the decision on MCP: life and critical conditions at +50%, income at standard rates.
6. The customer asked for another look on `/customer`. st2 asked for a medical exam ($180) and re-offered at +25%.
7. The customer asked for another look a second time, which was accepted. st2 re-offered at +25% again.
8. The customer accepted on screen with her signature. The policy issued and the interim cover ended.
9. On MCP as the customer I ran `my_proposals`, `offer_to_sign`, and `answer_offer` without a signature and with a made-up one.

**Outcome:** partly completed. The policy issued, but only because MCP and the played phone app stood in where the screens have nothing.

**What worked**
- The Multi-Benefit Discount of 12.5% (`mbd-12-5`) was on the quote.
- Interim cover SCN202-1-I was in force at once and was cancelled when the policy issued.
- A partial disclosure was refused, listing each missing question (as ids, in the structured answer).
- `obtain_records` consent was enforced before any evidence was asked for.
- The case returned for another look reached a queue.
- A junior's loading was refused.
- On MCP, `answer_offer` with no signature or a made-up signature was refused `device_required`.
- The hand-over picker lists only underwriters.
- After issue, the first bill ($289.86) differs from the original quote ($264.38), consistent with a +25% loading on life and critical conditions. I could not compare it with "the offered premium", because no screen shows one (F2-02-7).

### Findings

**F2-02-1 (blocker): A customer cannot complete her application on any screen.**
- **Who it hurts:** every applicant.
- **Reproduce:** sign in on `/customer` as the applicant and open "To approve" → "Your applications".
- **What happens:** the row reads "prop-scn202-1 · awaiting disclosure · waiting on applicant" with nothing to click. `customer.html` has no health-and-lifestyle form and no consent controls.
- **What should happen:** the customer answers the questions there, sees what is missing, gives or refuses each consent, and signs.
- **Where:** `crates/channels/ui/customer.html` around lines 29 and 228-236. Only offers and requotes have controls.

**F2-02-2 (major): MCP lists disclosure and consent tools the customer can never complete there.**
- **Who it hurts:** the customer's assistant.
- **Reproduce:** call `submit_disclosure` or `give_consent` (even with `granted:false`) over MCP with a valid signature in the body.
- **What happens:** both are always refused `401 device_required … do it in the app on your phone`, because MCP calls carry no device proof (`crates/channels/src/mcp.rs:18-20`, by design).
- **What should happen:** list them only where they can succeed, or say in the description that the phone app finishes them. Refusing a consent arguably should not need the phone at all.

**F2-02-3 (major): The disclosure tools give an assistant nothing to go on.**
- **Who it hurts:** the customer's assistant.
- **Reproduce:** call `disclosure_to_sign` following its own description.
- **What happens:**
  - The description says `"answers": {...}` and `"adviser_entered": false`.
  - `adviser_entered: false` is in fact wrong: the intake wants a list of question ids. Answers are `[version, {"number": 165}]`.
  - Every wrong shape is answered "the underwriting intake refused this without saying why (422)". axum's JSON rejection is plain text, so `passed()` at `crates/channels/src/http.rs:483` finds no message to show.
  - No tool lists the questions or their wording.
- **What should happen:** a tool that lists the questions with their wording, and an answer shape the description states.
- **Where:** `crates/underwriting/src/disclosures.rs:87-96` and `crates/channels/src/disclosures.rs:43-55`.

**F2-02-4 (major): The question set cannot take what the customer has to disclose.**
- **Who it hurts:** the customer, and underwriting.
- **Reproduce:** submit a disclosure that includes `anything_else`.
- **What happens:** `personal-statement@2` has no question on blood pressure, medication or asthma, and no free text. `anything_else` existed in @1 and was dropped in @2, so `anything_else` is refused `unknown_question`. "I take a blood pressure tablet" can only be squeezed into "heart_condition: yes".
- **What should happen:** a free-text question, or a question for current medication and conditions.

**F2-02-5 (major): The decision form on screen gives terms for one cover only.**
- **Who it hurts:** the underwriters.
- **Reproduce:** as st2 on `/underwriting` → Cases, open the AIA Living case and choose "Propose a decision": cover life, accept, loading 50, reason RC-0101.
- **What happens:** `409 terms: a decision gives terms for each cover on the life, once`. The form builds `terms: [{cover: f.cover, …}]` with a single cover (`crates/channels/ui/underwriting.html`, the `ACTIONS.propose` entry, about line 96). Every multi-cover life (AIA Living, term_life with trauma) can be decided only through MCP's raw `underwriting_command`.
- **Also:** the "Cover" picker is empty. `coversHere()` reads `proposal.lives[life].covers`, which the proposal answer does not carry.
- **What should happen:** one row per cover on the life, each with its own outcome and loading.

**F2-02-6 (major): The underwriter cannot see the evidence they asked for.**
- **Who it hurts:** the underwriters.
- **Reproduce:** after the provider played the report as received, open the case and the proposal on `/underwriting` as st1 or st2.
- **What happens:**
  - Nothing shows the evidence request, its status, its due date, the report, or its cost.
  - The case does not come back to the top or show that anything arrived.
  - The "Ask for evidence" form has no due-date field. The deadline was set from the wall clock to 2026-12-08, a year before the books' date.
  - The "set the case to wait" action does not exist.
  - The monthly report shows evidence cost as "305.00", without dollars.
- **What should happen:** an evidence list on the case with status, due date, provider reference and cost in dollars, the report itself (read with a purpose), and a due date the underwriter chooses.

**F2-02-7 (major): The customer accepts an offer without ever seeing its terms or price.**
- **Who it hurts:** the customer, and conduct.
- **Reproduce:** with an offer open, look at "To approve" and Documents on `/customer`, then press Accept.
- **What happens:**
  - "Your applications" shows "Open until …" and Accept / Ask us to look again / Decline.
  - The text says "the offer letter is in your documents", but Documents shows "No documents yet". MCP `my_proposals` carries no terms either.
  - Accept goes straight through with no confirmation of loading or premium: "Accepted. We will let you know when your cover starts."
- **What should happen:** the offer letter in her documents; the loading, exclusions and new premium on the row; and a confirmation that names the premium before she signs.

**F2-02-8 (major): Proposal and issue dates come from the wall clock, so the new policy is backdated a year.**
- **Who it hurts:** the customer, billing and finance.
- **Reproduce:** issue prop-scn202-1, then open SCN202-1 on `/customer` or `/staff`.
- **What happens:**
  - The policy incepted on 2026-11-17, the wall-clock date, not on 2027-11-15, the books' date.
  - On issue the customer owes $869.58 for three periods from 2026-11-17.
  - The first offer expired 2026-12-17 (wall clock), but later offers expired 2027-12-15 (books' date).
  - Disclosure `submitted_at` and case times are wall-clock ISO strings with nanoseconds.
- **What should happen:** the policy starts on the books' business date (or the quoted inception, 2027-11-15), and the offer dates use the books' calendar.
- **Where:** the underwriting driver's `today()` is `(self.clock)().date_naive()` (`crates/underwriting/src/driver.rs:250-252`, also `:263`), and `flow::application` takes that as the inception (`crates/underwriting/src/flow.rs:631`).
- **Stack or product:** on production the two clocks are closer together, so this is partly a stack effect. But issuing on a date the books are not on is a product defect.

**F2-02-9 (major): "Ask us to look again" is not limited to once.**
- **Who it hurts:** the underwriters, through endless re-work.
- **Reproduce:** as the customer on `/customer`, press "Ask us to look again" on the second offer.
- **What happens:** it was accepted ("Sent. An underwriter will be in touch.") and opened a third case. Neither the screen nor `offer_to_sign` says how many looks are left.
- **What should happen:** one look offered, with the screen saying so.
- **Also:** right after a counter or an accept, the row still shows "offer open" with all three buttons until the page is reloaded.

**F2-02-10 (major): Junior refusals do not say why or who can act.**
- **Who it hurts:** the junior underwriter.
- **Reproduce:**
  - as st1 on MCP, call `underwriting_command` `propose_decision` with a +50% loading
  - on screen, try to read a disclosure after declaring a conflict (see SCN2-03)
  - try to hand a case back to yourself
- **What happens:** the answer is always "st1 is not authorised for this".
- **What should happen:** name the limit and who takes it, for example "an assistant underwriter may not load (maximum 0%) and life above $500,000 needs a senior: hand it to a senior underwriter". The $800,000 case also sat in the junior medical queue although it is above st1's $500,000 life limit, so it was not routed to a senior by amount.

**F2-02-11 (major): The interim cover is shown wrongly to the customer.**
- **Who it hurts:** the customer.
- **Reproduce:** as the customer on `/customer`, open SCN202-1-I.
- **What happens:**
  - It shows as "Aia interim", with only "Life NZ$500,000.00".
  - Accidental TPD and trauma ($150,000 each) and accidental disablement income ($2,000 a month) are missing.
  - Nothing says it is free, why it exists, or that it ends when the policy issues. "Pays –" is shown.
  - After issue it shows as "cancelled" rather than "ended when your policy started".
  - It offers an "Appoint an adviser" form and a claims form like any policy.

**F2-02-12 (major): The adviser sees nothing of the application, or of the policy after issue.**
- **Who it hurts:** the adviser (D1, Tessa Maddox).
- **Reproduce:** as d1 on `/adviser`.
- **What happens:** the customer gave `adviser_access` consent, and the proposal named adv-d1, but the book lists neither the application nor SCN202-1 after issue. `customer.html` shows "No adviser on this policy". The adviser screen only shows proposal rows inside an appointed policy.
- **Note:** stack-additions section 5 says the book fills only after the customer appoints. That is confirmed here, and it is a product gap for advised business.

**F2-02-13 (minor): The disclosure on screen is collapsed and full of codes.**
- **Who it hurts:** the underwriter.
- **Reproduce:** as st1 on `/underwriting` → Proposals, read the disclosure.
- **What happens:**
  - The reading is collapsed under a summary line of `2026-11-17T09:34:42.495808118Z (e85fd8…)`.
  - Inside are question ids made into words ("Heart condition", "Height cm") and "Answer Yes no yes".
  - Money is in raw cents ("Annual income … Money 9500000").
  - The vault reference and hashes come first.
  - The "Read" button gives no confirmation message.
- **Elsewhere on the page:** `/underwriting` shows the queue's "Why" as reason codes ("RC-0101, RC-0201, RC-0301"), "You are st1", and case ids. The Proposal, Life, Review, Decision and "Person you are connected to" fields are typed references; pickers exist for some, but not for the party (F2-03-2).

**F2-02-14 (minor): I could not confirm that disclosure reads reach the staff access log.**
- **Reproduce:** as st1, read a disclosure, then ask MCP `staff_me`.
- **What happens:** `recent` is empty. Reads may be logged by the intake rather than the channels access log.

**F2-02-15 (minor, test stack): `play.js proposal` shows case holders that do not match the screen.**
- **What happens:** `play.js proposal` showed `"holder": null` for cases that `/underwriting` showed as held by st1 and st2. This is a stack summary issue, not a product defect.

---

## SCN2-03: An underwriter's cousin applies: conflict, hand-over and peer review

**What I did**
1. I opened two `term_life` proposals with play.js: Rawiri at $300,000 and Kahu at $750,000. I played their disclosures and the `obtain_records` consent.
2. st1 took both cases on screen. He declared a conflict on Rawiri on the "Conflicts and reports" tab.
3. After declaring, st1 tried the proposal and disclosure on screen and on MCP (`underwriting_question` `proposal`, `read_disclosures`, `take_case`).
4. st1b took the released case.
5. st1 handed Kahu's case to st1b with the picker, then tried to take it back from the queue and by handing it to himself.
6. st2 drew peer review samples on screen and on MCP, for both periods. st2b drew them on MCP with several seeds. Both tried recording a finding, and st2 read the report.
7. st1 tried the senior-only items on screen and on MCP.
8. st2 tried to move a case and booked a tele-interview in the past.

**Outcome:** partly completed. The conflict, the hand-over and the junior's limits were exercised. Peer review produced no sample.

**What worked**
- On declaring, the case left st1 at once and went back to the queue unheld.
- st1 was refused taking it again, reading the disclosure on screen and on MCP, and acting on Kahu's case after the hand-over.
- st1b could take the released case.
- A giver cannot take a handed-over case back: there was no Take button, and handing it to himself was refused.
- The hand-over picker lists only underwriters.
- A senior cannot record a finding on her own decision (refused).
- The senior forms are hidden from st1. MCP refuses him `draw_peer_review_sample` and `record_non_disclosure_outcome` with "… is for a senior underwriter", which names the role.

### Findings

**F2-03-1 (major): A conflicted underwriter can still read the whole proposal.**
- **Who it hurts:** the applicant (privacy) and compliance.
- **Reproduce:** as st1, after `declare_conflict` on the party, call MCP `underwriting_question` with `{"question":"proposal","query":{"proposal":"prop-scn203-1"}}`.
- **What happens:**
  - The whole proposal comes back: the policyholder's party, birth date and sex, the disclosure record (hash, vault reference, time), the case with its reasons (RC-0101 medical history, RC-0105 family history), consents and offers.
  - On screen, Proposals → Open shows the proposal summary.
  - The queue still lists the case with an Open button.
- **What should happen:** "closed to you", and nothing else. Only `read_disclosures` checks the conflict.
- **Where:** the channels' underwriting proposal question passes no conflict check. Disclosures are refused by the module; the proposal question is answered without it (`crates/channels/src/underwriting.rs`).

**F2-03-2 (major): An underwriter cannot declare a conflict without knowing a party id.**
- **Who it hurts:** every underwriter.
- **Reproduce:** as st1 on `/underwriting` → Conflicts and reports, type "Rawiri" into "Person you are connected to".
- **What happens:**
  - The field's picker, `portal.people` over `/staff/v1/find`, needs `customers.find`, which underwriters do not hold. The list stays empty and silent.
  - The only way through is to type `pty_25aa…`, which I copied from the MCP proposal answer.
  - There is no "declare a conflict on this case" action on the case.
  - There is no reason field, although `declare_conflict` is recorded with no reason.
- **What should happen:** declare from the case, with a reason, and no typed reference (docs/staff-ui-review.md).
- **Where:** `crates/channels/ui/underwriting.html`, lines 41 and 204.

**F2-03-3 (minor): The refusal after a conflict does not say why.**
- **Reproduce:** as st1 after declaring, read the disclosure or take the case again.
- **What happens:** "st1 is not authorised for this", the same wording as a limit refusal (F2-02-10).
- **What should happen:** "You declared a conflict with this applicant on …; the case is closed to you."
- **Not confirmed:** I could not see whether the attempts after declaring were logged and flagged.

**F2-03-4 (minor): The hand-over picker offers the giver, and shows ids, not names.**
- **Reproduce:** as st1, open "Hand the case over" and click "Hand to".
- **What happens:** the list includes st1 himself. Colleagues are shown as `st1`, `st2` and so on, not by name.

**F2-03-5 (major): Peer review draws nothing and says nothing.**
- **Who it hurts:** the senior underwriters, and quality assurance.
- **Reproduce:** as st2 on `/underwriting` → Remedies and review, draw a sample for period 2027-11 (2027-11-01 to 2027-11-30), then 2026-11. On MCP, st2b drew 2026-11 with seeds 1 to 9.
- **What happens:**
  - Every draw answers `{"sample":{"decisions":[]}}`. The screen says only "Done." and the Decision picker stays empty.
  - The period can be drawn again any number of times with a new seed, and the screen picks a random seed. So a reviewer can redraw until the sample suits them.
  - Recording a finding on SCN202's effective decision as st2b is refused `not_sampled`.
  - The month's report contradicts itself: "Decisions 1" while "by outcome: standard 1, terms 2".
- **What should happen:** state the sample size and the candidates considered, allow one draw per period, and give report counts that agree with each other.
- **Where:** sampling is by per-mille rate (`crates/underwriting/src/special.rs:809-850`); with only a few effective decisions, zero is likely and unexplained.

**F2-03-6 (minor): Refusing a senior a finding on her own decision does not say why.**
- **Reproduce:** as st2 on MCP, `record_peer_finding` on `prop-scn202-1/life-1/d3`, which is st2's own decision.
- **What happens:** "st2 is not authorised for this".
- **What should happen:** "You made this decision; another senior reviews it." The decision is also offered nowhere, because nothing was drawn.

**F2-03-7 (major): A tele-interview can be booked in the past.**
- **Who it hurts:** the underwriters and the applicant.
- **Reproduce:** as st2 on `/underwriting` → Cases, open the `prop-scn203-2` case, choose "Book a tele-interview", enter When = 2025-03-01 10:00, and Send.
- **What happens:** "Done." It was accepted.
- **What should happen:** refused beside the field.
- **Also:** the booking takes the proposal and life from the Proposals tab's fields, not from the case.

**F2-03-8 (minor): A queue lead cannot move a case another underwriter holds.**
- **Reproduce:** as st2, "Move the case to another queue" on a case held by st1b.
- **What happens:** refused "st2 does not hold case prop-scn203-2/life-1/case/1". The wording is clear.
- **The question:** whether a queue lead (grade-senior, role `queue_lead`) should be able to move a case they do not hold. If not, the screen should not offer Move.

**F2-03-9 (minor): `underwriting_decide` is listed to a junior, and its description leaves out a required field.**
- **Who it hurts:** the junior's assistant.
- **What happens:**
  - st1's tool list includes `underwriting_decide`, described "As a second senior…". This partly confirms "Things noticed" item 10.
  - Its `body` description is `"{}"`, but calling it answers "verb is required". The verb is not described.

---

## "Things noticed while designing": items I touched

- **Item 5 (no way to start new business): confirmed.**
  - There is no quote screen.
  - No tool or route opens a proposal or issues a policy.
  - The adviser portal has no application screen.
  - The customer page cannot take a disclosure either (F2-02-1).
- **Item 6 (no AIA templates): refuted.** The `aia_*` templates are in `shipped.json` and they price.
- **Item 10 (MCP reach by module prefix): partly confirmed.** A junior underwriter is listed `underwriting_decide`, a senior-only tool.
- **Item 12 (wall clock against the books' date): confirmed, and it reaches further.** Underwriting uses the wall clock for:
  - offer expiry
  - evidence deadlines
  - disclosure times
  - **policy inception**, which backdated SCN202-1 a year and billed three periods at once (F2-02-8)
- **Item 16, F-SCN02-4 (key reuse answered as accepted): fixed for the customer master.** A reused key with a different body is refused `idempotency_conflict`. `quote` writes nothing, so a key there has no effect, and it is silently accepted although the schema forbids it.
- **Item 16, names as references: confirmed in underwriting.** Colleagues show as `st1`/`st2`, "You are st1", and parties as `pty_…`.
- **Stack-additions section 5 (an adviser's book stays empty after issue): confirmed** for an advised AIA Living policy (F2-02-12).

## Findings by severity

- **Blocker:**
  - F2-02-1: no screen for the customer's disclosure and consents.
- **Major:**
  - F2-01-1: the quote prices cover the age limit forbids
  - F2-01-3: the quote tool needs invented ids and a purpose
  - F2-02-2: MCP lists disclosure and consent tools that always fail
  - F2-02-3: the answer format is unguessable and errors say nothing
  - F2-02-4: no question for medication or asthma, and no free text
  - F2-02-5: the screen decides one cover only
  - F2-02-6: evidence is invisible to the underwriter
  - F2-02-7: the customer accepts without seeing the terms
  - F2-02-8: the wall clock drives inception and offer dates
  - F2-02-9: "look again" is not limited to once
  - F2-02-10: refusals say "not authorised" with no limit or who can act
  - F2-02-11: interim cover is shown incompletely
  - F2-02-12: the adviser sees no application and no policy
  - F2-03-1: a conflicted underwriter can still read the proposal
  - F2-03-2: a conflict needs a typed party id, with no reason and no case action
  - F2-03-5: peer review draws nothing, can be redrawn, and the report contradicts itself
  - F2-03-7: a tele-interview can be booked in the past
- **Minor:**
  - F2-01-2, F2-01-4, F2-01-5, F2-01-6, F2-01-7, F2-01-8, F2-01-9, F2-01-10
  - F2-02-13, F2-02-14, F2-02-15 (test stack)
  - F2-03-3, F2-03-4, F2-03-6, F2-03-8, F2-03-9
