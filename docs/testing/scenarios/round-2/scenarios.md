# User scenarios for the insurance core, round 2 (local e2e stack)

Twenty-four new scenarios for an operator who acts as the user against the running stack (`node e2e/stack/stack.js`, portal at http://127.0.0.1:17090, business date 2027-11-15 at start). Round 1 (`docs/testing/scenarios/round-1/scenarios.md`) covered servicing requests, the employer's monthly file, health and death claims, a payment run, the month-end dashboard, a price held for review, held commission and a four-team complaint. Round 2 goes where round 1 went lightly or not at all: the AIA range from quote to issue, underwriting with evidence, conflicts and reinstatement, joint and multi-life policies, unit-linked money, reinsurance recoveries, document templates and notices, privacy requests, duplicate customer records, direct debits and money that arrives wrongly, regulatory returns, region fail-over, adviser onboarding and clawback, scheme renewal, a complaint made by an attorney, and assistants (MCP) acting for staff roles. Several scenarios re-test areas fixed after round 1 from another angle; these are marked `regression` and name the round 1 finding.

## Index

| ID | Title | Persona (cast key) | Channel | Flags |
|---|---|---|---|---|
| SCN2-01 | New business officer quotes and issues AIA Essentials Life through an assistant | New business (`nb`), own customer, ST5 (`st5b`) | both | probe, regression |
| SCN2-02 | AIA Living application: evidence, a loading, a second look, interim cover | D1 (`d1`), ST1 (`st1`), ST2 (`st2`), own customer | both | hand-off |
| SCN2-03 | An underwriter's cousin applies: conflict, hand-over and peer review | ST1 (`st1`, `st1b`), ST2 (`st2`, `st2b`) | both | hand-off, probe |
| SCN2-04 | A couple on one policy: one wants less cover, the other must agree | two own customers, ST5 (`st5b`, `st5l`) | screens | hand-off, regression |
| SCN2-05 | A unit-linked customer pays in and switches funds | own customer, D2 (`d2`), ST8 (`st8`), ST9 (`st9`) | both | probe, shared-state (balanced fund) |
| SCN2-06 | Pricing lead suspends a fund on a bad market day | ST9 lead (`st9l`), ST8 (`st8`), ST9 (`st9`), Auditor (`auditor`) | both | shared-state (cash fund) |
| SCN2-07 | A lapsed policy comes back: free of evidence, then with it | ST5 (`st5`), ST1 (`st1`), ST2 (`st2`), own customer | screens | hand-off, needs-date-move |
| SCN2-08 | A large trauma claim and the reinsurer's share | ST4 (`st4`, `st4b`), Reinsurance (`ri1`, `ri2`), ST17 (`st17`), Auditor (`auditor`) | both | hand-off, regression |
| SCN2-09 | An income claim in payment: part-time return, an overpayment, a write-off | ST4 (`st4`, `st4b`), ST3 (`st3`), ST13 (`st13`), own customer | both | hand-off, regression |
| SCN2-10 | The lapse notice wording is wrong: a new template version and its three approvals | ST15 (`st15`), Legal (`legal`), ST13 (`st13`), Records (`records`) | screens | hand-off, probe |
| SCN2-11 | Notices that went out wrong and a letter that came back | Records (`records`), Legal (`legal`), ST5 (`st5b`), own customer | screens | hand-off |
| SCN2-12 | A Privacy Act access request and a correction the customer and insurer disagree on | Privacy (`privacy`), own customer, ST5 (`st5b`) | both | hand-off, regression |
| SCN2-13 | The same woman twice: merge, regret, unmerge | Privacy (`privacy`), New business (`nb`), ST7 (`st7`), own customer | screens | hand-off |
| SCN2-14 | "Delete everything you hold on me" with a dispute still open | Privacy (`privacy`), Legal (`legal`), Records (`records`), ST16 (`st16`) | both | hand-off, regression |
| SCN2-15 | A direct debit that bounces twice | Payments (`payee`, `payee2`, `recon`), ST10 (`st10`), ST17 (`st17`), ST5 (`st5`) | screens | hand-off |
| SCN2-16 | Mum pays the premium, money lands on the wrong policy, and a refund meets a hold | ST13 (`st13`), Payments (`recon`), ST10 (`st10`), ST17 (`st17b`), Legal (`legal`), own customer | screens | hand-off |
| SCN2-17 | The quarterly return, a late entry and a proposed rate table | ST10 (`st10`, `st10b`), ST11 (`st11`), ST12 (`st12`) | screens | needs-date-move |
| SCN2-18 | The internal auditor's assistant does the quarter-end walk-through | Auditor (`auditor`) | mcp | probe, regression |
| SCN2-19 | Sydney drops out in the middle of the afternoon | ST16 (`st16`), ST17 (`st17`), own customer, Anyone (`stranger`) | both | shared-state (run alone) |
| SCN2-20 | An adviser changes advice provider | Distribution (`dist`), D2 (`d2`), D6 (`d6`), D4 (`d4`), D10 (`d10`), Commission (`comops`, `comapp`) | screens | hand-off, shared-state, regression |
| SCN2-21 | Cover cut in the first year: whose commission comes back? | D1 (`d1`), own customer, Commission (`comops`, `comapp`), ST13 (`st13`) | both | hand-off |
| SCN2-22 | A scheme renewal with a rate rise and an executive above the automatic limit | ST7 (`st7`), E1 (`e1`, `e1b`), ST2 (`st2`), own customer | screens | hand-off |
| SCN2-23 | A daughter with a power of attorney complains about a privacy breach | Frontline (`front`), ST14 (`st14`, `st14j`), ST6 (`st6`), Legal (`legal`), Privacy (`privacy`), Conduct (`conduct`) | screens | hand-off, regression |
| SCN2-24 | The staff administrator's assistant sets up a starter and tidies access | Administrators (`admin`, `admin2`), ST15 (`st15`), ST5 (`st5b`), ST10 (`st10`) | both | probe, regression |

MCP is used in SCN2-01, 02, 03, 05, 06, 08, 09, 12, 14, 18, 19, 21 and 24. SCN2-07 and SCN2-17 move the business date. SCN2-19 needs the lead to stop and start ledger nodes and must be run alone.

## Ground rules for the operator

**Act only as the user.** Every decision a person makes goes through a portal screen or `/mcp` with that person's own sign-in. Never call the nodes, the customer master, the group service, the documents service or the intake directly to do a person's work, and never write to a store. The set-up the scenarios list under "Start" (a customer who can sign, an issued policy, a scheme) is the only thing made off-screen, the way round 1 did it; say in your report what you made.

**Signing in.** On any page, the development sign-in card: subject `sub-<key>` from `e2e/lib/cast.js`, roles empty, except the platform engineer (`sub-st16`, roles `operations`). `sub-nobody` is a sign-in the insurer does not know.

**MCP.** `POST http://127.0.0.1:17090/mcp`, one JSON-RPC message per call (`initialize`, `tools/list`, `tools/call`), header `x-dev-subject: sub-<key>` (and `x-dev-roles: operations` for `st16`). Since round 1 the tool list is narrowed to what the caller's roles and relationships reach. In every MCP scenario save the list and record: (a) any tool listed that the person's job does not reach, (b) any tool the job needs that is missing, (c) that a call to an unlisted tool by name is refused in words, (d) every place the description left the assistant guessing a field, a unit or a command name. Money over MCP is in cents; record any answer where that is unclear.

**Parts you may play, and must report.** The bank (acknowledgement, settlement, rejection, statement), sanctions screening, the unit-pricing file reader (a valuation, units on issue), the virus scan on an uploaded file, an evidence provider's report coming back, new business opening a proposal (no channel opens one), and a contribution or fund order arriving for a unit-linked policy. Play them as the scripted tests do (`System.tell`, the API's `/v1/payments` and `/v1/commands`, the intake's `/v1/evidence`), state it in the result, and do nothing more with those identities.

**Data you must make yourself** (as round 1): customers with a phone key (`customer()` and `enrolBrowser()` in `e2e/lib/customer.js`; write down name and birth date), retail policies from `e2e/.run/shipped.json` templates by `POST /v1/policies` as `new-business`, schemes as `register()` in `e2e/lib/group.js`. Prefix every reference `SCN2nn-`. The seed gives staff, three advice providers (Harbourline `fap-d6` with `adv-d1`, `adv-d2`; Clearwater `fap-d9` with `adv-d9a`; Kōwhai `fap-kowhai` with `adv-d4`, `adv-d5`), Harbourline under the Northlight dealer group (`grp-d10`), and the schemes `kaifresh` and `southline`. Do not change `kaifresh` or `southline`.

**Findings.** Record each as `F2-<nn>-<n>` (scenario number, then a count): who it hurts, what you did, what happened, what good looks like, and a severity. A scenario that cannot start because nobody in the cast may do it is itself a finding; name the missing action.

**Shared stack.** Other operators use the same stack. Restore what you change that others depend on (a suspended fund, an ended engagement, a licence, a disabled template) and say so at the top of your report. Do not close business days except in SCN2-07 and SCN2-17, which run last, one at a time, with the stack restarted afterwards; note every date move with the dates. SCN2-19 runs alone, after every other operator has finished and before the date-move scenarios.

**Record everywhere.** An internal reference a person had to type that a list could have offered; an amount without dollars or in cents on a screen; a code where words belong; a date not in New Zealand form or not from the books' business date; a refusal that does not say what to do next or who can; a console error the page did not explain; anything a person could see or do that their role should not allow.

---

## New business and underwriting

### SCN2-01 New business officer quotes and issues AIA Essentials Life through an assistant (probe, regression)
- **Persona / channel:** new business officer (`nb`), a customer you create afterwards, service officer (`st5b`); MCP first, then `/records`, `/staff` and `/customer`.
- **Goal:** "A walk-in wants Essentials Life, $400,000, she is 41 and does not smoke. Tell me the price by month and by year, then for her husband who is 62, and get her record opened."
- **Start:** Nothing. **Needs from the stack:** application templates for the AIA range in `shipped.json` (at least `aia_essentials_life`, `aia_living`, `aia_cover_for_life`, `aia_private_health`), or the operator cannot form the application body a quote needs; say what you used.
- **Steps:**
  1. As `sub-nb`: `tools/list`. Expect `quote`, `find`, `find_customers`, `match_customer`, `create_customer`; no claims, payments or finance tools.
  2. `match_customer` for her name and birth date, then `create_customer`. Repeat `create_customer` with the same details and a new key (she comes back next day to a colleague).
  3. `quote` for `aia-essentials-life`, $400,000, monthly, then yearly. Then with the sum insured typed in dollars by mistake (400000, read as $4,000), then $1,200,000 (above the product's $1m), then age 62 at $600,000 (Essentials allows at most $500,000 from 61 to 70).
  4. `quote` again for the first case with the same `idempotency_key` but a yearly body.
  5. She agrees. The officer looks for a way to submit the application or issue the policy by screen or tool. (Expected: none. Record what each dead end says.) You then issue it on the API as new business, on the quoted application.
  6. `st5b` finds her by name on `/staff` and opens the policy; she signs in on `/customer` and reads the first bill.
  7. Overreach as `nb`: `customer` (open the record she just created; `customers.view` is not hers), `change_customer`, `merge_customers`, `core_policy`, `staff_policy`.
- **Complication:** dollars against cents; age and amount limits; the same person created twice; no path from a quote to a policy.
- **Good looks like:** the quote gives the first period's bill with premium, fee and GST apart, and the monthly figure times twelve differs from the yearly by exactly the product's monthly loading (6%); Essentials carries no policy fee (`docs/aia-products.md`) and the quote shows none; the $4,000 quote is refused as below the $50,000 minimum naming the minimum in dollars, not priced; the over-limit and age refusals name the limit; the second `create_customer` answers "possible duplicate" with the first record as the candidate and creates nothing; the first bill on `/customer` and on `/staff` equals the quote to the cent; a reused key with a different body is refused or at least not answered as the first quote (round 1 left F-SCN02-4 open: record exactly what happens); every overreach is refused naming the permission in words; the product shows on screens as "AIA Essentials Life Cover", not `aia-essentials-life`.
- **Stories:** US-ST05-1, US-D04-1, US-P01-1.

### SCN2-02 AIA Living application: evidence, a loading, a second look, interim cover
- **Persona / channel:** D1 Tessa Maddox (`d1`), junior underwriter (`st1`), senior underwriter (`st2`), a customer with a phone; `/underwriting`, `/customer`, `/adviser`, and MCP for the customer's signing steps.
- **Goal (customer):** "Life $800,000, Critical Conditions $200,000 and income cover for me. I had asthma as a child and I take a blood pressure tablet." (underwriter): "Get the GP report, decide, and tell her plainly."
- **Start:** A customer with a phone who has appointed Tessa. You play new business: open a proposal for `aia-living` (life, accelerated Critical Conditions, agreed value income with a 4-week wait to 65, occupation class 2) for that party, with Tessa's engagement `eng-d1`. Keep the proposal reference out of the underwriters' hands: they must find it from a queue.
- **Steps:**
  1. The customer opens `/customer`, "Your applications": answers the health and lifestyle questions, first leaving a required answer out, then fully (blood pressure disclosed), and signs. She gives consent for her adviser to see progress, and refuses the consent to share with her employer (not relevant, and she says no).
  2. While it is being underwritten she asks on `/customer`: "Am I covered if I'm hit by a bus tomorrow?" Interim cover should be visible with its limits.
  3. `st1` takes the case from the queue, reads the disclosure (purpose asked), requests a GP report with a due date, and sets the case to wait. He tries to offer terms with a +50% loading (not a junior's to give).
  4. You play the provider: acknowledged, then the report received with its cost. The case comes back to the top of `st1`'s work.
  5. The amount or the loading puts it beyond `st1`: it routes to `st2`, who offers life and trauma at +50% and excludes nothing, with income cover at standard rates, reasons picked from the library.
  6. The customer sees the offer, asks us to look again once (she says the reading was a one-off), `st2` asks for a second blood pressure reading as evidence, then re-offers at +25%. The customer accepts with her signature.
  7. The policy issues. Interim cover ends. Tessa sees the policy in her book with the covers and what each is held at, and nothing about the loading's reason. The first bill equals the offered premium.
  8. MCP as the customer: `my_proposals`, `offer_to_sign`, then `answer_offer` without a signature and with a made-up one.
- **Complication:** a missing answer; a junior over his authority; a counter-ask; a provider who is slow.
- **Good looks like:** the question set refuses the incomplete answers naming each missing question; the interim cover shows as free cover with its caps ($500,000 life, $150,000 accidental TPD and trauma) and ends on issue; the evidence request is an open task with a due date and its cost is on the case; a junior is not offered the loading or is refused naming his limit and who takes it; the offer letter is under the customer's documents and never reaches the adviser (clinical class); "look again" is offered once and says so; the accepted premium, the quote on the offer and the first bill are the same figure; the Multi-Benefit Discount shows on the bill if the minimums are met (life $100,000 plus trauma $75,000 and income $2,000 a month gives the tier for two other categories, 12.5%); every disclosure read is in the access log with a purpose; the adviser sees progress in words and no health detail.
- **Stories:** US-ST01-1, US-ST02-1, US-D01-2, US-P01-1.

### SCN2-03 An underwriter's cousin applies: conflict, hand-over and peer review (probe)
- **Persona / channel:** junior underwriters (`st1`, `st1b`), seniors (`st2`, `st2b`); `/underwriting` and MCP.
- **Goal (st1):** "That's my cousin's application in my queue. I shouldn't touch it." (st2): "Draw this month's peer review sample and record what I find."
- **Start:** Two proposals opened by you as new business for two customers on `term_life` ($300,000 and $750,000). At least one decided case from SCN2-02 helps the peer sample.
- **Steps:**
  1. `st1` takes both cases, then realises one is his cousin's. He declares a conflict on it from the case, with a reason. What happens to the case he already holds?
  2. He tries to open the proposal again, read the disclosure, and find it through MCP (`underwriting_question` `proposal`, `read_disclosures`).
  3. `st1b` finds the case back in the queue and takes it, with `st1`'s notes up to the declaration but nothing after.
  4. `st1` hands his other case to `st1b` by picking a colleague (not typing an ID), then changes his mind and tries to take it back.
  5. `st2` draws the peer review sample, records a finding against one of `st2b`'s (or her own, which should not be offered) decisions, and reads the report for the month.
  6. `st1` tries the senior-only items: draw a sample, record a finding, prepare a non-disclosure remedy; by screen and by MCP (`underwriting_command`, `underwriting_decide`).
  7. `st2` moves a case to another queue and records a tele-interview booking for a date in the past by mistake.
- **Complication:** a conflict declared after taking the case; a hand-over taken back.
- **Good looks like:** after the declaration the case leaves `st1` at once, he sees "closed to you" and nothing else by screen, search or MCP, and the attempt after declaring is logged and flagged; the colleague picker lists only people who may take the case; a hand-over cannot be undone by the giver, only by the holder handing back; nobody peer-reviews their own decision; the junior is not shown senior forms and MCP refuses them naming the role; a booking in the past is refused beside the field; the monthly report's counts equal what the queue views show.
- **Stories:** US-ST01-1, US-ST02-1, US-N07-1.

### SCN2-04 A couple on one policy: one wants less cover, the other must agree (regression)
- **Persona / channel:** two customers you create (partners), service officer (`st5b`), service lead (`st5l`); `/customer`, `/staff`; screens.
- **Goal (her):** "We're both on this policy. Money is tight: drop my cover to $200,000 but leave his alone. And I don't want him seeing my health details."
- **Start:** One `term_life` policy with two lives, each life a different party (pass `lives` through the template as round 1's operator did), both recorded as owners in the customer master so both must sign. Both have phones. A `health_family` policy for the same couple with a child as third life.
- **Steps:**
  1. Each signs in: each sees the policy, "You" against their own life and "Life 2" or the partner's name against the other.
  2. She asks for the reduction herself from `/customer` if the page offers it; otherwise she rings and `st5b` enters `reduce_cover` for her life only, picking the cover and life from the policy and typing $200,000. `st5b` first picks his life by mistake and has to take it back.
  3. The change is above the officer's limit: `st5l` agrees it. Then it waits for the customers. She approves. He has not: what does each page say, and for how long?
  4. He declines. She is told. `st5b` enters it again after they talk; both approve.
  5. She sets the family consent so the policyholder cannot see her health details, and names her sister as a delegate who may look at the policy. The sister (a third customer, or `p1` if it has a record) signs in.
  6. On the health policy he tries to cancel the whole policy alone.
  7. `st5l` opens the policy "as the core holds it" (`core_policy` or its screen) to check the layers after the reduction.
- **Complication:** two owners; the wrong life picked; one declines; consent between partners.
- **Good looks like:** nothing changes until both owners sign, and each page says whose signature is awaited; a decline by one ends the request for both with who declined; after both sign, her life shows $200,000, his is untouched, and the next bill falls by her part only (compare `e2e/lib/oracle.js`); the staff queue row says the change in words with both amounts in dollars; the delegate sees the policy and no health rows, and cannot ask for changes; a cancellation by one owner of a jointly owned policy waits for the other and says so; the partner never sees the other's claim causes after the family consent is set to no; `st5l` holds `policy.core_view` but the route asks for the insurance-sensitive class his role lacks: record what he is told. Round 1 regression: staff can reduce cover (F-13-1) and cancellation needs a device signature on every channel.
- **Stories:** US-P18-1, US-P44-1, US-ST05-1.

---

## Unit-linked investments

### SCN2-05 A unit-linked customer pays in and switches funds (probe, shared-state)
- **Persona / channel:** a customer you create with an `invest_plan` policy, D2 Viliami Fifita (`d2`) as her adviser, pricing maker (`st8`) and checker (`st9`); `/customer`, `/adviser`, `/pricing`, MCP as the customer. **Shared-state:** the balanced fund only; tell the other operators.
- **Goal (customer):** "I put $5,000 in last week. Is it invested yet, at what price, and can I move half from balanced into cash before Christmas?"
- **Start:** The policy issued; the customer appoints `adv-d2` (`eng-d2`). You play the bank and the contribution: a $5,000 receipt quoting the policy number, set aside and placed as buy orders for the balanced fund. You play the administrator's file for the balanced fund's next pricing date.
- **Steps:**
  1. Before any price: the customer reads "Your investments". The order should be "Waiting for a price" with the fund, the amount and the date it will be priced.
  2. `st8` prepares the balanced fund's price from the valuation; `st9` approves.
  3. The customer and Viliami refresh: units held, the price used and its date, the value today.
  4. She looks for a way to switch half to cash: on `/customer`, by `make_request` over MCP (which kinds does it list?), by asking Viliami (`/adviser` change form). (Expected: no kind carries a switch. Record every dead end and what it says.)
  5. You play servicing: place the switch on the API. Both pages show a sell waiting in balanced and, once it executes, a buy in cash. Strike the next prices for both funds (`st8`, `st9`).
  6. She places her own hold on money going out (`/customer`), then asks (through you, as servicing) to withdraw $1,000.
  7. MCP as the customer: `my_policy`, and `my_account`; check the holdings and orders are there with money and units unambiguous.
- **Complication:** money in flight between a sell and a buy; a hold placed mid-way; no customer path to switch.
- **Good looks like:** the value shown equals units times the sell price in force, with the price date; buy uses the buy price and sell the sell price (compare `oracle`); between the sell and the buy the page says where the money is, and the total never silently drops; the fund reads "Balanced fund", not a code; a switch is never held by the customer's own hold (it leaves no money) but the withdrawal is refused naming the hold and how to lift it; the adviser sees the same holdings and orders as the customer; the contribution is not shown as a premium or as "overdue" at any point; each dead end for a switch tells her who can do it.
- **Stories:** US-P10-1, US-FA1-1, US-ST08-1.

### SCN2-06 Pricing lead suspends a fund on a bad market day (shared-state)
- **Persona / channel:** pricing lead (`st9l`), maker (`st8`), checker (`st9`), auditor (`auditor`); `/pricing` and MCP as `st9l`. **Shared-state:** the cash fund only; resume it before you finish and say so.
- **Goal (lead):** "The custodian cannot value the cash fund today. Suspend it, make sure nothing is priced or traded on a stale figure, and bring it back cleanly tomorrow."
- **Start:** The cash fund with at least one price in force (strike one first as `st8` and `st9`). An order waiting in the cash fund (from SCN2-05, or play one).
- **Steps:**
  1. `st9l` suspends the cash fund with a reason. She first picks the growth fund by mistake and must undo it at once.
  2. `st8` tries to prepare a cash price for today; a valuation file for cash arrives anyway (you play it) and one that cannot be read (you play `refuse_valuation_file`).
  3. `st9l` asks her assistant (MCP): `pricing_question` `status`, `funds`, `work`, `late`, `errors`; then `pricing_command` to resume the fund with the wrong fund name, then correctly.
  4. Next day's valuation arrives: `st8` prepares; `st9l`, who holds approve as well as lead, approves. Then try the reverse on another date: `st9l` cannot prepare (she has no prepare role).
  5. One partition "refused" the price (if you can produce that honestly; otherwise note it): `st9l` offers the price again (`republish`).
  6. The customer from SCN2-05 looks at her waiting order during the suspension and after.
  7. `auditor` opens `/pricing` and asks MCP `pricing_question`: the auditor's roles carry no unit pricing action.
- **Complication:** the wrong fund suspended; files arriving for a suspended fund; a lead with two roles.
- **Good looks like:** suspension needs a reason, shows who and when, and is undone by the same person without a second signature only if the module says so (record which); while suspended, prepare is refused saying the fund is suspended and by whom; unreadable files are listed with the row and reason; the overdue list shows the cash fund as suspended, not as late; the customer's order says the fund is not being priced for now, in words, and executes at the first price after resumption, not a stale one; an approver is never the preparer whatever roles one person holds; the MCP refusal for a wrong fund lists the funds; the auditor gets "not open to you" naming the permission and is listed no pricing tool.
- **Stories:** US-ST09-1, US-ST08-1, US-FA1-1.

---

## Arrears and reinstatement

### SCN2-07 A lapsed policy comes back: free of evidence, then with it (`needs-date-move`)
- **Persona / channel:** collections officer (`st5`), underwriters (`st1`, `st2`), two customers you create; `/collections`, `/underwriting`, `/customer`, `/staff`; screens. Run last, alone; restart the stack afterwards.
- **Goal (customer A):** "I was overseas, the policy lapsed three weeks ago, I'll pay everything now." (customer B): "Mine lapsed four months ago and I've had a heart scare since. Can I have it back?"
- **Start:** Two monthly `term_life` policies with inceptions set so that, after the days you close, A is about 20 days past lapse and B about 100 days past lapse (term life: lapse after 90 days unpaid, reinstatement window 180 days, evidence-free for the first 60). **Needs from the stack:** role templates carrying `collections.officer`, `collections.team_lead`, `collections.hardship` and `collections.configuration` (the cast's `collections` template holds only `payments.collections`, so `/collections` opens to nobody), and the dunning sequences set before days are closed.
- **Steps:**
  1. Close days one at a time (`ic-e2e-close`), checking each close reconciles, until both policies have lapsed as above. Note every date.
  2. Each customer signs in: what does the page say about a lapsed policy, what is owed, and until when it can come back?
  3. `st5` finds A in the queue or by name, asks for reinstatement; the arrears needed are shown; A pays (you play the receipt for the exact amount, then first for $10 less).
  4. `st5` asks for B's reinstatement: beyond the evidence-free days it goes to underwriting. `st1` finds it under reinstatements, asks for a health declaration; B declares the heart scare (signed on the device); `st1` cannot decide it; `st2` offers reinstatement with a cardiac exclusion picked from the library.
  5. B accepts and pays the arrears. Both policies are in force again.
  6. A third policy is left to pass 180 days (if time allows): `st5` asks for reinstatement and is refused.
  7. A claim for an event dated inside the lapsed gap is notified on A's policy by `st4`.
- **Complication:** a short payment; health changed while lapsed; the window closing.
- **Good looks like:** arrears equal the independent calculation (`oracle.arrears`) and are shown in dollars with the date they are good until; a short payment does not reinstate and the page says how much is still needed; A comes back with no health questions and the same cover, terms and policy number; B's reinstatement cannot complete without underwriting, and the exclusion shows on the policy for the customer in plain words; the customer pages never show "lapsed" and "in force" at once and say the reinstatement date; the gap claim's cover check shows the policy was not in force on the event date as a fact; after 180 days the refusal says a new application is needed; reminders stop once reinstated.
- **Stories:** US-ST05-1, US-ST01-1, US-P01-3.

---

## Claims and reinsurance

### SCN2-08 A large trauma claim and the reinsurer's share (regression)
- **Persona / channel:** senior assessors (`st4`, `st4b`), reinsurance operations (`ri1`) and lead (`ri2`), treasury (`st17`), auditor (`auditor`); `/claims`, `/reinsurance`, `/payments`, and MCP as `ri1` and `auditor`.
- **Goal (assessor):** "Heart attack, 49, $900,000 trauma cover. Admit it and pay him." (reinsurance): "Most of that is the reinsurer's. Did we tell them, did they consent, and when do we get it back?"
- **Start:** A customer with a `term_life`-style policy carrying trauma cover at $900,000 on one life (override the template), in force more than 14 days before the event (the trauma survival period) and more than 7 days (early-claim rule). The reinsurance driver runs in the stack: cessions should exist before the claim.
- **Steps:**
  1. `ri1` finds the life on the Lives view by name (his role now has `customers.find`) and reads retained and ceded before the claim.
  2. `st4` notifies the claim from a call, cause picked (`CC-0101`), event 20 days ago; requests the cardiologist's report; records it; reserves $900,000. A reserve move this sharp may need a reason: record what the console asks.
  3. `st4` admits $900,000: above $250,000 it needs a second signature, and above her $600,000 limit she cannot admit it at all. Who can? (Nobody in the seeded authority book has more than $600,000.) Record the refusal's words, then continue with a $600,000 cover on a second policy if the first is stuck.
  4. On the claim that can be admitted: `st4b` countersigns. `ri1` sees the claim fact arrive; an `admitted_without_consent` exception shows if the treaty wanted consent first: he records the reinsurer's answer and where it is filed.
  5. `st4` pays the customer: the item is above the $100,000 route limit, so the run holds it for treasury; `st17` releases it (round 1's decision) and approves the run. You play the bank.
  6. `ri1` cuts the reinsurer's statement for the treaty and period, agrees it, prepares the settlement instruction; tries to sign it; `ri2` signs.
  7. `auditor` (MCP): `reinsurance_question` `exposure`, `statements`, `statement`, `balance`, `consent_status` for the claim; then `reinsurance_command` (refused).
- **Complication:** a claim above every assessor's limit; consent after the fact; an item above the route limit.
- **Good looks like:** the life's retained plus ceded equals the sum insured before the claim, and the recovery due equals the ceded share of what was admitted, in dollars; the over-authority refusal names her limit, the amount and who may take it, and if nobody may, says that instead of pointing at a role nobody holds; the survival period is on the cover check as a fact; the statement's recovery line equals the claim's ceded share and its totals equal the sum of its lines; the settlement cannot be signed by its preparer; the over-limit item is named with payee, dollars and "treasury releases this"; the auditor reads all and is offered nothing that writes. Round 1 regression: F-12-4, F-SCN16-1, F-SCN22-1.
- **Stories:** US-ST04-1, US-R01-1, US-R02-1, US-ST17-1.

### SCN2-09 An income claim in payment: part-time return, an overpayment, a write-off (regression)
- **Persona / channel:** senior assessors (`st4`, `st4b`), health assessor (`st3`) through an assistant, investigator (`st13`), the customer; `/claims`, `/customer`, MCP as `st3`.
- **Goal (assessor):** "He's been on claim since his back went. He's now back three mornings a week and didn't tell us for a month. Put the benefit right, get back what we overpaid without hurting him, and set his next review."
- **Start:** A customer with an `income_protection` policy ($6,000 a month), inception far enough back that the waiting period has been served by the event date; a claim notified from `/customer`, cause `CC-0204`, admitted by `st4` with a monthly benefit and the customer's bank account registered and confirmed as payee (two maintainers).
- **Steps:**
  1. `st4` sets up the monthly benefit and pays the first month; checks the medical review (6 months) and income declaration (3 months) dates the console set.
  2. The customer reports part-time earnings on a call: `st4` records the income, the partial benefit is worked out, and a month already paid in full is now an overpayment.
  3. She proposes recovering it by offset from future benefits: first 50% (the rules cap offset at 25%), then 25%.
  4. He misses his income declaration by more than the 14-day grace: what happens to the benefit, and what does `/customer` say?
  5. A small remainder ($180) is uncollectable: `st4` writes it off (within her $1,000 limit); a second write-off of $1,200 needs `st4b` as well.
  6. `st13` (investigator) opens the claim for a referral about the unreported work: what can an investigator see and do, and does `st4` see that an investigation is open?
  7. `st3`'s assistant (MCP as `sub-st3`): `claims_question` for the queue and this claim, `claims_definition` for the wording the cover is bound to, then `claims_command` to end the benefit (income protection is not a line a health assessor holds), to decline, and to pay.
  8. The customer recovers: `st4` ends the benefit with reason `R-RECOV`. The policy carries on and premiums restart if they were waived.
- **Complication:** late news of earnings; an offset over the cap; a missed declaration; a line the assistant's principal does not hold.
- **Good looks like:** the partial benefit and the overpayment are worked out and shown with their arithmetic in dollars; the offset refusal names the 25% cap; a missed declaration suspends, with a task and a date, and the customer page says what to send and by when without the cause of claim; write-offs above $1,000 wait for a different senior; the investigator's access is its own role and is logged; `st3`'s MCP calls outside the health line are refused naming her lines and limit (round 1 F-11-5) and nothing is recorded on the claim; the definition tool returns the bound version's words, not the latest; ending the benefit stops the next payment and the customer sees "ended" with the date.
- **Stories:** US-ST04-1, US-ST03-1, US-ST13-1, US-P17-1.

---

## Documents, notices and privacy

### SCN2-10 The lapse notice wording is wrong: a new template version and its three approvals (probe)
- **Persona / channel:** product owner (`st15`), legal counsel (`legal`), compliance (`st13`), records officer (`records`); `/documents`; screens.
- **Goal (product owner):** "The lapse notice gives the old contact centre number. Put up a corrected version, get it approved and switched on, and stop the old one."
- **Start:** The documents service runs in the stack since round 1. The templates listed on the Templates view (placeholder wordings shipped with the service).
- **Steps:**
  1. `st15` opens `/documents`: which views load, what her callers and approvals are. She finds the lapse notice template and its version in force.
  2. She submits a new version: first with a field name the template does not know (`{{ policy.numbr }}`), then with wording above the reading grade (one very long sentence), then properly, still carrying the line "[Placeholder wording for legal review. Not for issue.]".
  3. She runs the rendering test. She tries to approve it for product herself.
  4. `legal` approves for legal; tries to give the compliance approval as well. `st13` approves for compliance.
  5. `st15` tries to activate it: the placeholder line should stop it. She removes the line in a further version; approvals start again for that version.
  6. With the new version active, she disables the old version. `records` checks "notices by template" for the old version over the last month and raises a defect if any went out (hand-off to SCN2-11).
  7. `records` and `legal` each try what is not theirs: `records` submits a template; `legal` disables one.
- **Complication:** the author is the only person in the cast who holds the product approval; the placeholder mark.
- **Good looks like:** validation refusals name the unknown field and the reading grade found against the limit; a version is never edited, only superseded, and the list shows which is in force from when; the author cannot approve her own version and the screen says who can; one person cannot give two approvals; activation is refused while the legal-review mark is in the wording, saying so in plain words; activation is refused without a passed rendering run; disabling asks for a reason and takes effect at once; buttons a role may not use are not offered. If the product approval can be given by nobody but the author, record it as a cast gap (see "Needs from the stack"). **Needs from the stack:** a second person holding `documents.approve_product` (or `documents.configuration`) so author and product approver differ.
- **Stories:** US-ST15-1, US-ST13-1.

### SCN2-11 Notices that went out wrong and a letter that came back
- **Persona / channel:** records officer (`records`), legal counsel (`legal`), service officer (`st5b`), a customer with a policy; `/documents`, `/staff`, `/customer`; screens.
- **Goal (records):** "Forty lapse warnings went out with the wrong number on them, and Mr T's came back 'gone, no address'. Treat the bad ones as not given, send good ones, and get his to him."
- **Start:** A customer with a `term_life` policy whose customer-master email is one that cannot be delivered (or a postal address only); documents produced for the policy at issue (schedule, welcome). If no notice with a notice period exists without moving the date, work the defect on whatever notices the policy has and say so.
- **Steps:**
  1. `records` finds the policy's documents from the policy number (her role has `policy.view`): schedule, welcome letter, any notice; who each went to, when, and whether delivery is proven.
  2. She raises a notice defect against the template version from SCN2-10 (picked from the list), with what was wrong; fills it with the affected notices; excludes one that was already corrected.
  3. She tries to close the defect while notices are uncorrected.
  4. The undeliverable one: the Work view shows it. `st5b` rings the customer and enters the new email as a change the customer approves on `/customer` (or the customer changes it himself). `records` presses "Deliver again".
  5. The customer opens the corrected notice under his documents; the old one should read as replaced.
  6. A letter was requested twice by mistake: `records` cancels one under "Letters requested".
  7. `legal` places a legal hold on the policy's documents with a reason; `records` tries to release it; `legal` releases it at the end.
  8. `st5b` tries to download the schedule for the customer on the phone (`documents.download` is not his): what is he told?
- **Complication:** a defect that cannot be closed yet; a contact detail that has to change before re-delivery; who may hold and who may release.
- **Good looks like:** a document row says kind, recipient, channel, sent, and delivered or not, in words and New Zealand dates; a defective notice is shown as "treated as not given" and whatever it warned of cannot take effect until the corrected one is proven delivered (the gate reads "waiting"); closing the defect is refused listing the notices still uncorrected, and closing asks for the control that should have caught it; "Deliver again" uses the new contact point and the page shows the new attempt, not a second document; the customer sees one current notice; a hold shows who placed it and why, and only legal releases it; a download asks for a purpose, is logged, and a clinical document is refused to non-claims staff.
- **Stories:** US-ST15-1, US-P01-3; no story exists for the records officer (gap noted in round 1).

### SCN2-12 A Privacy Act access request and a correction the customer and insurer disagree on (regression)
- **Persona / channel:** privacy officer (`privacy`), a customer with a phone, service officer (`st5b`); `/records`, `/customer`, `/staff`, MCP as `privacy` and as the customer.
- **Goal (customer):** "Send me everything you hold on me. And your record says I was born in 1979; it's 1978. And who has been looking at my file?"
- **Start:** A customer with a policy, a claim notified in an earlier scenario or a fresh one, at least two staff reads of the policy (`st5b` on a call, `st4` on the claim), and one changed contact detail so there is history.
- **Steps:**
  1. The customer takes his own copy on `/customer` (signed), and by MCP `export_my_data` (refused without the device).
  2. He rings: `st5b` cannot produce the staff export (`customers.export` is not his) and passes it to privacy.
  3. `privacy` opens `/records`: finds him by name and birth date under "New call", opens the record with the reason for opening, and reads history, relationships and consents.
  4. She produces the staff export for the access request and the "who read or changed this record" audit; then "what one person did" for `st5b`.
  5. The birth date: he sends no proof. She declines the correction, recording his statement of what he says is right. A week later he sends a passport: she records the corrected date. The premium is rated on age: what happens to the policy?
  6. He changes his mobile himself to `021 555 0123`; she sees it arrive in history with who changed it.
  7. MCP as `sub-privacy`: `customer`, `customer_history`, `customer_export`, `customer_audit`, `actions_by`; then tools outside her job: `staff_policy` (no `policy.view`), `claims_question`, `payments_question`.
  8. `st5b` and `nb` try `customer_audit` and `customer_export` by MCP.
- **Complication:** a correction first declined, then made; a rating fact changes; a customer asking who looked.
- **Good looks like:** the customer's own copy and the staff export hold the same facts, including records merged into his, and both say as at when; the export is a file a person could send, with headings in words; the audit lists each reader with date, purpose and what they saw, includes refusals, and includes the privacy officer's own reads; a declined correction keeps his statement attached to the record and shows in his own copy; the corrected birth date shows in history with both values and who recorded each, and either re-rates the policy from a stated date or raises a task for someone to do so (record which, and whether anyone is told); the local-format mobile is stored with +64 (F-08-2); privacy cannot open policies or claims and the refusal says so; non-privacy staff are refused the export and audit by name of permission.
- **Stories:** US-P18-1, US-ST13-1.

### SCN2-13 The same woman twice: merge, regret, unmerge
- **Persona / channel:** privacy officer (`privacy`), new business officer (`nb`), group administrator (`st7`), a customer with a phone; `/records`, `/group`, `/customer`; screens.
- **Goal (privacy):** "Hana Mason and Hana Te Rangi look like one person: married name. Check, merge, and make sure she still sees both her policies. Then deal with the pair that only looks alike."
- **Start:** Customer A (with phone and sign-in) under her maiden name holding a `term_life` policy. Record B created by `nb` with `allow_duplicate` under her married name, same birth date and mobile, owning an `income_protection` policy. A second pair C and D: two different people (twins: same family name, same birth date, different given names), each with a policy. A member row in a scheme of your own (not `kaifresh`) with A's name and birth date and no strong identifier, so a possible match waits for `st7`.
- **Steps:**
  1. `nb` creates B: the master answers "possible duplicate" with A as candidate; she goes ahead (the customer insists she has no other policy), which opens a review.
  2. `privacy` opens "Likely duplicates": the A/B review and, if raised, C/D. She opens both records side by side, checks relationships, and merges B into A with a reason.
  3. The customer signs in as A: both policies show; her name is as she last gave it; her devices and requests are intact.
  4. `st7` decides the scheme's possible match: the candidate should now be one person, not two.
  5. `privacy` dismisses C/D as "not a duplicate" with a reason. Then, by mistake, she merges C into D anyway from the lookup.
  6. D rings confused about a policy that is not his. She undoes the merge. C and D each see only their own policy again.
  7. She tries to merge a record that has a legal hold or an open request waiting (if you have one from SCN2-14), and a record into itself.
- **Complication:** a merge that should not have happened; a group match depending on the merge; live sign-ins on the absorbed record.
- **Good looks like:** the duplicate refusal shows candidates by name, birth date and matching strength, never internal scores alone; a merge needs a reason, lists what will move (policies, relationships, consents, sign-in), and warns it is hard to undo; after it, searching either name finds one person and the absorbed reference answers for the survivor; the customer loses nothing she could see before and gains only her own; a dismissed review does not come back next day; the unmerge restores each record with its own history and both carry the merge and unmerge in their history; a wrong merge never lets D read C's policy on `/customer` (check while merged: this is the serious case, record exactly what D could see); merging a record into itself is refused in words.
- **Stories:** US-ST07-1, US-P18-1, US-N01-1.

### SCN2-14 "Delete everything you hold on me" with a dispute still open (regression)
- **Persona / channel:** privacy officer (`privacy`), legal counsel (`legal`), records officer (`records`), platform engineer (`st16`); `/records`, `/documents`, `/ops`, MCP as `privacy` and `st16`.
- **Goal (privacy):** "He cancelled last month and wants to be forgotten. Legal has a dispute open on his old claim. Do what we must, keep what we must, and tell him which is which."
- **Start:** A customer whose policy has been cancelled (customer cancels on `/customer`, signed), with documents on file and a declined or withdrawn claim. `legal` has placed a legal hold on the party's documents with a reason.
- **Steps:**
  1. `privacy` opens the record and looks for what can be erased now and what is kept, until when, and why (the retention view by kind on `/documents`, the record's own retention on `/records`).
  2. She tries to shred his documents (`documents.privacy`): the legal hold should stop the held ones.
  3. `legal` releases the hold after the dispute settles. `privacy` shreds again.
  4. `records` looks for his schedule: gone or kept? The customer signs in and asks for a document: what does he see?
  5. She tries to exit or erase the customer-master record while a relationship is still within its seven years.
  6. A prospect who never took a policy (created by `nb` in SCN2-01 style, no relationship) asks the same: what differs?
  7. `st16` (MCP, operations): `verify_customer` for the party reference `privacy` reads out to him, and for one that does not exist; confirm `reshred_customers` and `rewrap_customers` are marked destructive or write and refuse an empty body. Do not run them.
  8. MCP as `sub-privacy`: `documents_command` with a misspelt `type`.
- **Complication:** a hold that outranks the request; retention the insurer must keep; telling the person honestly.
- **Good looks like:** the screens say, per kind of document and per class of fact, kept until a date and why, or erased now, so the officer can write to him without reading source; shredding under a hold is refused naming the hold and who placed it, and shreds only what is free; a shredded document answers "destroyed" (410) to staff and "not found" to the customer, never a broken PDF; the master refuses erasure inside retention saying when it becomes possible, and the erasure register entry is made when it happens; nothing in operations' answers shows personal data; `verify_customer` answers plainly for a missing party (F-23-1, F-23-2); a misspelt documents command is answered with the commands there are.
- **Stories:** US-P18-1, US-ST16-1, US-ST13-1.

---

## Payments and finance

### SCN2-15 A direct debit that bounces twice
- **Persona / channel:** payee maintainers (`payee`, `payee2`), finance operator (`st10`), treasury approver (`st17`), reconciler (`recon`), collections officer (`st5`); `/payments`, `/staff`, `/customer`; screens.
- **Goal (maintainer):** "Set up Mr R's direct debit from the form he signed." (maker): "Run this morning's collection." (collections): "It bounced again. Ring him before we stop trying."
- **Start:** Two customers with monthly `term_life` policies and premiums due. You play the bank throughout.
- **Steps:**
  1. `payee` creates a direct debit authority for customer 1: account number first with a digit missing, then correctly; who must confirm it before it can be used?
  2. `st10` prepares a collection run (reference and date made for her), fills it, checks it; `st17` approves before the 16:00 file cut-off. Try once after the cut-off time if the console's clock allows: which channel does it say it will use?
  3. Bank: acknowledged, customer 2 settled, customer 1 dishonoured (insufficient funds).
  4. The dishonour shows where? `st5` (who holds `payments.collections`) records a conversation with the customer and marks hardship for a fortnight; the next collection should not present him.
  5. After the mark ends, the item is presented again (`represent`) and dishonours again: after two the authority is suspended by rule.
  6. `st10` tries to add him to a third run. `payee` reinstates the authority after he gives a new account: changed account, cooling-off of 24 hours, second maintainer `payee2` confirms.
  7. `recon` reads the day's statement against the collection account: settled items matched, the dishonour reversed, no open break.
  8. The customer's `/customer` page throughout: what he owes and why the payment did not go.
  9. `st17` tries to redirect the route to the second bank (connectivity role: nobody in the cast holds it).
- **Complication:** a bad account number; two dishonours; a changed account inside the cooling-off window.
- **Good looks like:** an account number that fails the bank's check digit is refused beside the field; an unconfirmed authority cannot be collected on and the refusal says who confirms; the run's control totals show presented equals settled plus dishonoured, in dollars; the dishonour reaches the policy (premium still due, not shown as paid) and collections sees it with the bank's reason in words; a hardship mark stops presentation and shows its end date; the second dishonour suspends the authority and says so on the row, and a suspended authority cannot be added to a run; an account changed within 24 hours cannot be used until the cooling-off ends, and the refusal gives the time; the customer page never says "paid" for a dishonoured premium; the redirect refusal names the connectivity role.
- **Stories:** US-ST10-1, US-ST17-1, US-BK1-1, US-ST05-1.

### SCN2-16 Mum pays the premium, money lands on the wrong policy, and a refund meets a hold
- **Persona / channel:** compliance (`st13`), reconciler (`recon`), finance operator (`st10`), treasury approver (`st17b`), legal (`legal`), two customers; `/payments`, `/customer`; screens.
- **Goal (compliance):** "Someone who isn't the owner is paying this premium. Find out who and whether that's fine." (reconciler): "A customer says she paid and we show nothing: find her $240."
- **Start:** Customer 1 (daughter) and customer 2 (unrelated) each with a monthly policy. You play the bank: a receipt of one premium for customer 1's policy from an account in her mother's name; a receipt of $240 from customer 2 quoting a reference one character off customer 1's policy number; a duplicate receipt of customer 2's premium.
- **Steps:**
  1. `st13` sees a payer case for the mother's payment: who paid, for which policy, what is known. `recon` and `st10` try to open payer cases (compliance only).
  2. The mother is not in the customer master. `st13` asks for her to be recorded as a `premium_payer` with basis "parent" (who does that: privacy, on `/records`?) and clears the case with that relationship's reference; first with a made-up reference.
  3. `recon` finds the $240: is it in aged unidentified receipts, or applied to customer 1's policy by a near-match? He proposes a match to customer 2's policy; tries to approve his own proposal; a second person approves.
  4. If it was applied to the wrong policy: `st10` reports misdirected money; the correction moves it; customer 1's page stops showing a credit she did not pay.
  5. Customer 2's duplicate premium: she asks for it back. `st10` requests a refund to the account it came from; `st17b` releases it.
  6. Customer 2 had placed her own hold on money going out (`/customer`) during a scam scare: the refund is held. `legal` is asked to override (court order only): there is none. She asks to lift her hold: the page says to call and that it takes seven days.
  7. `st10` writes nothing off; an approver who did not report the misdirection may write off a $2 residue.
- **Complication:** a third-party payer; a near-miss reference; the customer's own hold blocking her own refund.
- **Good looks like:** a payer case never blocks the cover (the premium counts as paid) but is open until cleared, and clearing needs a real relationship reference checked with the customer master; account details are never shown to roles without the right, on any view; the unidentified receipt is listed with amount and age and is matched by two people; a refund goes only to the account the money came from, and a held refund reads "held by the customer's own hold", says who can lift it and when, and is not lost; legal's override asks for a reference and is refused or recorded with it, never silent; nobody both reports and writes off misdirected money; every amount is in dollars.
- **Stories:** US-ST13-1, US-ST10-1, US-ST17-1, US-P18-1.

### SCN2-17 The quarterly return, a late entry and a proposed rate table (`needs-date-move`)
- **Persona / channel:** finance operator (`st10`), approver (`st10b`), lead (`st11`), actuary (`st12`); `/finance`; screens. Run on a stack where at least two business days have been closed (after SCN2-07, or close two days yourself and note the dates); round 1 could not exercise any finance write because the books had been fed no day.
- **Goal (lead):** "File the solvency return for the last period on the definition we have, check one odd figure, and tell me what Friday's backdated cancellation did to a period we thought was finished." (actuary): "Price next year's proposed term life table against the book before I sign it."
- **Start:** Books fed to at least one closed day, with new business and receipts from other scenarios in them. A policy cancelled with an effective date before the last closed day (enter it as `st5b` with a past effective date so it needs second approval, then the customer signs).
- **Steps:**
  1. `st11` opens Returns: the definitions offered (`return_definitions`), picks one, files it for the period. Then tries a definition typed by hand.
  2. She opens one figure on the filed return (`return_line`) and follows it to the accounts and movements behind it.
  3. The backdated cancellation: Backdated entries shows it with the period it belongs to and the period it was posted in; the trial balance for the earlier period is unchanged if that period is closed, and the difference shows as a prior-period item.
  4. `st11` restates the return with a reason; the first filing stays visible.
  5. `st10` proposes an actuarial journal; `st10b` approves it as an ordinary approver (should that be enough?); `st12` signs it as actuary.
  6. Tax reconciliation and conduct figures for the period: do they agree with the policies and payments the operators made?
  7. `st12` opens "Rate tables": pastes the current `term_life` table and a proposed one with one rate mistyped ten times too large; reads totals, the movement by rating cell and the covers that move most; corrects it and runs again. `st10` tries the same view.
  8. `st12` asks for an `in_force` extract and a `claims_by_cause` extract for the date.
- **Complication:** a typed definition; a closed period touched by a backdated event; a mistyped rate.
- **Good looks like:** a return is filed only on a loaded definition, named by id and version, and the refusal for a typed one says why; each return line opens to what is behind it and the parts add to the line; a backdated event never changes a closed period's balances and is listed as prior-period with both dates; a restatement keeps both versions with who and why; an actuarial journal needs the actuary's signature as well as a different approver; the dry run shows loaded totals only (no per-person loading), flags the mistyped cell as the largest mover, says `loadings_read` plainly, and is refused to the operator; extracts open as tables with a row count, and in-force count equals the policies in force on the date; all dates default to the books' date.
- **Stories:** US-ST11-1, US-ST12-2, US-ST10-1.

### SCN2-18 The internal auditor's assistant does the quarter-end walk-through (probe, regression)
- **Persona / channel:** internal auditor (`auditor`: finance, payments, reinsurance and commission read roles); MCP only, then one look at each console to compare.
- **Goal:** "For my working papers: the state of the books, any payment run approved and prepared by the same person, payees changed in the last week, reinsurer balances, commission held and aged, and prove to me I can't change anything."
- **Start:** Whatever the other scenarios have left: runs, payees, statements, journals.
- **Steps:**
  1. `tools/list` as `sub-auditor`: expect the question tools of finance, payments, reinsurance and commission, `staff_me`, `colleagues`, and no command tools she cannot use. Note whether `finance_command`, `payments_command`, `reinsurance_command`, `commission_command` are listed (the reach is by module prefix, so a read-only action may list the write tool).
  2. `finance_question`: `status`, `dashboard`, `trial_balance`, `journal` for one journal, `aged_suspense`, `extract` (`movement`). `rate_table_dry_run` (lead or actuary only).
  3. `payments_question`: `open_runs`, `run` and `control_totals` for one, `payee` for one (account details must not come back), `config` (connectivity only), `payer_cases` (compliance only), `notices` for a role she does not hold.
  4. `reinsurance_question`: `exposure`, `statements`, `balance`, `sweep_check`; `commission_question`: `held_control`, `aged_held`, `month_end_blocks`, `clawback_exposure`, `history`.
  5. Every write: propose a journal, approve a run, register a payee, prepare an abandonment, set a payment hold, `staff_access` to grant herself `finance.operate`.
  6. Not hers at all: `find`, `staff_policy`, `claims_question`, `customer_audit`, `complaints_question`, `documents_question`, `regions`.
  7. Protocol: a `tools/call` with no `arguments`; a path value as a number; a query field at top level; the same read 50 times quickly.
  8. Open `/finance`, `/payments`, `/reinsurance`, `/commission`: no action buttons; the figures equal the MCP answers.
- **Complication:** an assistant with only descriptions to go on; tools listed by module, not by read or write.
- **Good looks like:** every read answers with units and dates unambiguous; no account number, health fact or customer name she has no reach to appears anywhere; every write is refused naming the role needed, and a self-grant waits for an administrator and gives nothing meanwhile; unlisted tools answer "not open to you" before anything about arguments (F-SCN02-5); the argument mistakes are answered as round 1's fix describes (F-SCN04-4); a large extract comes back whole or says how to page; what the consoles show equals what the tools said. Record as a finding any write tool listed to a read-only person, and any question that needs an internal reference she cannot find by another question.
- **Stories:** US-ST11-1, US-ST17-1, US-R03-1.

---

## Operations

### SCN2-19 Sydney drops out in the middle of the afternoon (shared-state, run alone)
- **Persona / channel:** platform engineer (`st16`), treasury approver (`st17`), a customer with a phone, a stranger (`stranger`); `/ops`, `/status`, `/payments`, `/customer`, MCP as `st16`.
- **Goal (engineer):** "We've lost a region. Who is leading, is anything stuck, what are customers being told, and are we whole again afterwards?"
- **Start:** The stack runs three ledger nodes: two in `auckland` (home) and one in `sydney`. **Needs from the stack:** the lead stops and restarts nodes on request (the operator must not); a way to stop node 3 (Sydney) and, separately, one Auckland node as well so no majority remains. Run after every other operator has finished.
- **Steps:**
  1. Baseline: `/status` signed out, `/ops` as `st16`, MCP `regions`, `reconciliation`, `customer_index`.
  2. The lead stops the Sydney node. Within a minute: `/status`, `/ops`, `regions`. Meanwhile `st17` approves a checked payment run and the customer approves a waiting request with her signature.
  3. The lead stops one Auckland node as well (no majority). The customer presses Approve on a second request; `st17` opens `/payments`; a stranger loads `/status`; someone tries to sign in fresh.
  4. Leave it two and a half minutes: the staff and customer pages should say their data is stale rather than show it as current.
  5. The lead starts both nodes. The customer's second approval: did it apply once, not at all, or twice? `st17`'s run: one submission?
  6. `st16`: `regions` (leaders at home again?), `reconciliation` (no breaks), `customer_index` (caught up). He reads "What customers are told" on `/ops` against `/status`.
  7. MCP as `st16` during the outage: `status` (public tool) and `regions`.
- **Complication:** writes in flight when the majority is lost; pages that keep showing old data.
- **Good looks like:** with one region out, `/status` reads "Degraded" in plain words and everything still works; with no majority it reads "Outage", says cover is not affected and nothing sent is lost, and keeps answering; `/ops` shows which nodes are reachable, which region leads, and whether leaders are at home, and agrees with the `regions` tool; a write during the outage is refused or waits with words that say to try again, never a blank page or a raw 503 body, and never reports success; after recovery each in-flight write has applied exactly once and the page shows it; stale pages say so within two minutes; no personal data appears in any operations answer; the public page and the engineer's page never disagree about the state.
- **Stories:** US-ST16-1, US-ST16-2.

---

## Distribution and commission

### SCN2-20 An adviser changes advice provider (shared-state, regression)
- **Persona / channel:** distribution administrator (`dist`), Viliami Fifita (`d2`), Harbourline (`d6`), Anika Sharma at Kōwhai (`d4`), Northlight (`d10`), commission operations and approver (`comops`, `comapp`); `/commission`, `/adviser`, `/customer`; screens. **Shared-state:** `d2`'s engagement with Harbourline is ended and restarted; put it back and say so.
- **Goal (distribution):** "Viliami leaves Harbourline for Kōwhai at the end of the month. End one engagement, start the other, keep his clients served in between, and make sure he's paid by the right provider."
- **Start:** Two customers with policies who have appointed `adv-d2` under `eng-d2`. Kōwhai (`fap-kowhai`) has no sign-in in the cast: note what that stops.
- **Steps:**
  1. `d10` (Northlight) opens `/adviser`: sees Harbourline's advisers and books beneath it and nothing of Kōwhai or Clearwater. `d6` sees `d1` and `d2`.
  2. `dist` ends `eng-d2` from a date: first a date before it started (refused), then today. She picks parties and the engagement from lists.
  3. `d2` signs in between engagements: what can he see and do? His clients' `/customer` pages: who is their adviser now? One client prepares to appoint Anika (`adv-d4`, `eng-d4`) instead.
  4. `dist` starts a new engagement for `adv-d2` with Kōwhai, accreditation `life`; then by mistake a second engagement overlapping the first.
  5. The other client re-appoints Viliami under the new engagement. `d6` no longer sees that client; `d10` does not either (Kōwhai is not under Northlight).
  6. `dist` registers Kōwhai as a payee for commission and adds its bank account; `comops` adds a payment instruction; who must approve a change of account? `comops` cuts a statement for the old provider.
  7. `dist` records Kōwhai's licence as "suspended" in error and puts it right the same day (round 1 fix).
  8. Put back: end the Kōwhai engagement, restart `eng-d2`-equivalent with Harbourline, re-appoint as needed.
- **Complication:** a gap between engagements with clients in it; an overlapping engagement; a licence recorded in error.
- **Good looks like:** an adviser with no engagement in force sees why in words and can prepare nothing, and his clients are told their adviser is unavailable with how to appoint another (F-SCN03-7); the engagement reference is generated and picked, never typed (F-20-2); overlapping engagements for one adviser and accreditation are refused or flagged; the book follows the engagement: Harbourline and Northlight lose sight of moved clients at the end date and keep history up to it; a payee account change waits for a second person; the statement names the provider and period and its total equals its lines; the licence correction on the same day leaves no read-only banner behind; history shows who did each step.
- **Stories:** US-D02-1, US-D06-1, US-D10-1, US-D04-1.

### SCN2-21 Cover cut in the first year: whose commission comes back?
- **Persona / channel:** Tessa Maddox (`d1`), her client (customer with phone), commission operations (`comops`), approver (`comapp`), compliance (`st13`); `/adviser`, `/customer`, `/commission`, MCP as `d1`.
- **Goal (adviser):** "My client wants to halve her cover six weeks in. I'll put it through, but tell me what it does to my commission before I press send." (commission): "Show me the clawback, who bears it, and the exposure across the book."
- **Start:** **Needs from the stack:** commission books, schedules and new-business attribution seeded (`config/distribution/aia-schedules.json` or a rule with `clawback_by_cause`), which round 1 left out (F-20-1); without them nothing accrues and this scenario stops at step 2: record that and do the steps that remain. A `term_life` policy issued with Tessa's engagement as the writing adviser, first premium paid (you play the receipt), business date unchanged.
- **Steps:**
  1. Tessa reads her statement and held balance: the upfront commission on the policy, when it is paid, and anything held.
  2. She asks her assistant (MCP): `adviser_book`, `adviser_held_balance`, `adviser_statement` for the latest number, `adviser_replacements`.
  3. She prepares `reduce_cover` to $250,000 for the client, with a note. The client approves with her signature on `/customer`.
  4. `comops` opens `/commission`: the alteration is attributed to Tessa's engagement; the clawback is worked out; `clawback_exposure` for today includes it. `comapp` approves whatever needs approving; `comops` tries to.
  5. Tessa refreshes: the clawback shows against the policy with the reason "cover reduced" and the amount; her next statement nets it.
  6. A second client cancels on `/customer` herself, without Tessa: is that clawback attributed to Tessa the same way, or by another cause?
  7. Tessa disputes the first clawback as an error (is there a path, or only for replacements?). `st13` looks at the pattern of early reductions across her book.
  8. `comops` sets an accrual hold on Tessa by mistake, meaning a payment hold, and corrects it.
- **Complication:** an alteration by the adviser against one by the customer; a hold of the wrong kind.
- **Good looks like:** the adviser sees, before or at least immediately after, what the change cost her, in dollars with the policy number; the clawback equals the schedule's share of the commission on the cover given up (work it by hand from the schedule and record both figures); an adviser-submitted reduction is attributed to her engagement and the customer's own is treated as the rule says, visibly different; exposure equals the sum of the open clawback lines; maker cannot approve; an accrual hold and a payment hold read differently in words to staff and to the adviser ("still earning, payment paused" against "not earning"); the customer is never shown commission. Note: `docs/distribution.md` still says the portal carries no reduction of cover, while `servicing.rs` attributes one; say which is true.
- **Stories:** US-D01-3, US-D01-2, US-ST13-1.

---

## Group schemes

### SCN2-22 A scheme renewal with a rate rise and an executive above the automatic limit
- **Persona / channel:** group administrator (`st7`), employer administrators (`e1`, `e1b`), senior underwriter (`st2`), a customer who is a scheme member; `/group`, `/employer`, `/underwriting`, `/customer`; screens.
- **Goal (group administration):** "Their renewal is due: rates go up 8%, the executive multiple goes from 5 to 6 times salary. Agree it with the employer, bill on the new rates from the right day, and get the one director who is now over the automatic limit underwritten."
- **Start:** A scheme of your own for `hr-e1` (not `kaifresh`), `e1` and `e1b` administrators, five members applied by file: three staff, two executives, one executive on a salary where 6 times passes the automatic acceptance limit but 5 times did not. One member is a customer with a master record who has linked the cover from `/customer`.
- **Steps:**
  1. `st7` opens the scheme: terms in force, renewal date, lead days (90), what happens with no agreement (old rates for 30 days).
  2. She amends the terms for renewal on the Act view (`agree_renewal` takes the terms as JSON: record how much she had to know to write it). First with the executive multiple typed as 60.
  3. `e1` opens `/employer`: is the renewal offer visible with old and new rates and the date? Can she agree it there, or only by telling `st7`? `e1b` reads the same.
  4. After agreement: the next invoice. Members, cover and premium per category in dollars; the director's cover is capped at the automatic limit with the excess shown as waiting for underwriting.
  5. `st2` finds the excess case in underwriting (entry `group_excess`), asks the director for health answers (he signs on `/customer` if he is the linked member), and offers the excess with a loading.
  6. The loading: terms bill it to the employer as one total. With only one loaded member, `st7` sees the "loading over too few members" exception, opens the members in the total, and asks underwriting to offer him the loading on his own cover (`reoffer`). He agrees on his device.
  7. `e1` looks at the invoice and the member record: she must not be able to work out who is loaded or why.
  8. `e1` uploads next month's file with a salary rise for the director: the excess moves again.
- **Complication:** a mistyped multiple; one loaded member in a total; cover above the automatic limit.
- **Good looks like:** the renewal is refused or questioned for an implausible multiple; the employer sees the renewal in words and dollars, and nothing changes before the renewal date; the invoice equals the independent calculation at the new rates and multiples for the members on cover (`oracle`), with cover capped at the automatic limit until underwriting accepts; the excess case reaches underwriting once, named by member and amount, with no typing of the proposal reference; the employer's views show category, salary and cover only: no health, no loading by person, and a total that cannot be traced to one member; the member's own page shows his loading and what he pays; the exception closes itself once he agrees.
- **Stories:** US-E01-1, US-ST07-1, US-ST02-1, US-E04-1.

---

## Complaints and care

### SCN2-23 A daughter with a power of attorney complains about a privacy breach (regression)
- **Persona / channel:** frontline (`front`), complaints reviewer (`st14`), junior handler (`st14j`), hardship specialist (`st6`), legal (`legal`), privacy officer (`privacy`), conduct officer (`conduct`); `/complaints`, `/staff`, `/records`, `/customer`; screens.
- **Goal (daughter):** "You posted Mum's claim letter to her old flat. The neighbour opened it. Mum has dementia; I hold her power of attorney. I want to know what you'll do." (reviewer): "Is she entitled to complain for her mother, what did we send where, and do we have to notify anyone?"
- **Start:** Customer M (the mother) with a policy and a claim; customer D (the daughter) with her own sign-in and phone. `privacy` records D as `attorney` for M (basis enduring power of attorney, scope `view` and `complain`) on `/records`. M's postal address was changed a month ago (history shows both).
- **Steps:**
  1. `front` takes the call: finds M by name, then must record that the caller is D, not M. Is D picked as a person and her authority checked, or typed as free text? "Check a caller" on the policy for D.
  2. He records the complaint in D's words, cause "privacy breach", linking the policy and the claim (the claim link was added after round 1).
  3. D mentions her mother can no longer manage her affairs. `front` (or `st5b`) records a concern; `st6` finds it under "Markers to review" and sets a marker with adjustments (more time, speak to the attorney), review date in six months.
  4. `st14j` had taken the complaint: with the marker he can no longer hold it; `st14` takes it. Timers show the extra days.
  5. `st14` needs to know what was sent where: she asks `records` for the letter's delivery record (address used, date) and `privacy` for the address history. Each reads only what their role allows.
  6. Capacity: `st14` questions M's capacity on the record (who may: staff, claims or servicing). While it is in question, D's acts are routed to a person: D tries to view M's policy on `/customer` ("someone I act for"). `legal` settles capacity as "lacks capacity"; D's view works again.
  7. `st14` writes the outcome, apologises, offers $1,500 ex gratia, and flags the regulator notification (10 days). `conduct` sees the privacy-breach cause in its counts; `st14` sees no conduct case.
  8. D reads the outcome on `/customer` as M's representative, and tries to change M's bank details from there (acting is not offered; viewing only).
- **Complication:** a complainant who is not the customer; capacity in question; a breach with a deadline.
- **Good looks like:** the complaint records who complained and for whom, with the authority's kind and scope, picked not typed; the caller check tells `front` he may speak to D and logs it; the marker takes the file from the untrained handler with words saying who can hold it (F-21-4); due dates are shown as dates including the extra days; the claim opens from the complaint by a link for someone who may open claims and is refused in words for someone who may not (F-21-10 was partly fixed); while capacity is in question D gets "this is being reviewed" and not a bare 403, and nothing else about M's cover stops; after legal settles it the attorney's view returns; the letter to the outcome goes to D as representative, and M is not sent a clinical letter at the old address; the regulator deadline shows as a date with an owner; the representative cannot change bank details and is told staff will check her authority for that.
- **Stories:** US-ST14-1, US-ST06-1, US-N10-1, US-P46-1.

---

## Staff access

### SCN2-24 The staff administrator's assistant sets up a starter and tidies access (probe, regression)
- **Persona / channel:** staff administrator (`admin`), second administrator who approves configuration (`admin2`), product owner (`st15`), service officer (`st5b`), finance operator (`st10`); MCP as `admin`, `admin2` and `st5b`, then `/staff` to compare.
- **Goal (administrator):** "A new claims assessor starts Monday, a service officer needs to cover collections for a fortnight, finance wants one of theirs to approve runs while someone is on leave, and I need last week's break-glass use reviewed."
- **Start:** **Needs from the stack:** one bound sign-in that is not yet staff (for example `sub-newstarter`, party `newstarter`) for the administrator to employ; the cast has none, and employing an existing person changes shared state. Otherwise use the stranger's subject only to show the refusal.
- **Steps:**
  1. As `sub-admin`: `tools/list` (expect `staff_me`, `staff_admin`, `staff_access`, `colleagues`, `second_approval` and `changes_for_second_approval` since the template carries `service.approve_change`; nothing of claims, customers or payments). `staff_admin`: grants, alignment report, templates.
  2. `staff_access` to employ the starter with the `claims` template and a manager picked from `colleagues`; then with a template name that does not exist; then again with the same details and a new key.
  3. `st5b` asks for a one-off grant of `payments.collections` for 14 days with a reason (MCP as `sub-st5b`); tries to approve it himself; `admin` approves. `st5b`'s `tools/list` before and after.
  4. `st10` (maker) asks for `payments.approver` to cover leave: this joins maker and approver. It must be refused when asked (F-SCN16-7), and `admin` must not be able to approve it by another route. Then `st10` asks for `finance.approve` (operator plus approver in the ledger).
  5. `admin` grants herself `customers.view`. `admin` removes `n7`'s declared connection to `cust-p60` by mistake, and puts it back.
  6. `st15` (product owner, `staff.configure`) proposes a change to an authority profile (junior `assisted_change` from $100 to $250); `admin2` (`staff.approve_configuration`) approves; `st15` tries to approve her own; `admin` (no configuration action) tries.
  7. Break glass: `st5b` uses break-glass to open a policy outside his reach with a reason (if the role allows; the seeded templates have `break_glass: false`, so expect a refusal and record its words); `admin2` reviews break-glass use (`staff.review_break_glass`).
  8. The grant from step 3 is recertified, then revoked early; `st5b`'s list narrows again. The starter is suspended at the end.
  9. Compare `/staff` "Access administration" with what the tools said.
- **Complication:** a grant that would join duties kept apart; an administrator granting herself; nobody able to approve an administrator's own request.
- **Good looks like:** employing is one call with templates picked from the list, and a repeat is "already staff", not a second person; a grant always needs someone else, expires on its date, and the requester's tool list widens only after approval and narrows on revoke (F-SCN02-2 `grants_widen_the_tools_offered`); the maker-approver grant is refused at request with the two duties named, and the ledger keeps maker and checker apart whatever is granted; a self-grant by the only administrator waits visibly for someone who can approve and names who; a configuration change takes effect only after `admin2` approves and shows both names and the old and new limit in dollars; removing a declared connection asks for a reason and is logged; break-glass, if refused, says why and who can enable it, and the review list is empty in words; every refusal names which of the four questions failed. Record whether `staff.approve_profile_uplift` is needed anywhere: nobody holds it.
- **Stories:** US-ST16-2, US-ST13-1, US-N07-1.

---

## Needs from the stack (collected)

| For | What |
|---|---|
| SCN2-01, 02 | Application templates for the AIA products in `shipped.json` (`aia_essentials_life`, `aia_living` with option codes, `aia_cover_for_life`, `aia_private_health`), and confirmation that the nodes have the `aia-*` products and rates active. |
| SCN2-02, 03, 07, 22 | A documented way for the operator to play new business opening a proposal (the command and caller the scripted tests would use), and the evidence provider's report (`POST /v1/evidence` with the `evidence` role). |
| SCN2-07, 15 | Role templates carrying `collections.officer`, `collections.team_lead`, `collections.hardship`, `collections.configuration`, and people holding them. The `collections` template holds only `payments.collections`, so `/collections` opens to nobody. Dunning sequences set from the shipped best estimate. |
| SCN2-10 | A second person holding `documents.approve_product` (and ideally a second `documents.configuration`) so that a template's author and its product approver can differ. |
| SCN2-15 | Optionally someone holding `payments.connectivity`; otherwise the redirect step is a refusal check only. |
| SCN2-17, 18 | A stack on which at least one business day has closed, so the finance ledger has a period (round 1 F-SCN17 was blocked on this). Either a second stack for date-move scenarios or an agreed order. |
| SCN2-19 | The lead stops and starts ledger nodes on request; the run is alone. |
| SCN2-20 | Optionally a sign-in for Kōwhai (`fap-kowhai`), which is licensed in the seed but has no cast member. |
| SCN2-21 | Commission books, schedules and new-business attribution seeded (left after round 1 as F-20-1). |
| SCN2-24 | A bound sign-in that is not yet staff, for the administrator to employ. |
| SCN2-04, 13, 23 | Nothing new, but note that `p1` and `p18` still have no customer-master record or phone; these scenarios make their own customers. |

## Things noticed while designing (check these first; each is a possible finding)

1. **Nobody can open the collections console.** `crates/channels/src/collections.rs` asks for `collections.view`, `.officer`, `.team_lead`, `.hardship` or `.configuration`. No template in `e2e/lib/cast.js` carries any of them; the `collections` and `hardship` templates carry `payments.collections` and `service.vulnerability_kind` instead. Round 1's SCN-14 does not appear in any operator report, so `/collections` has still never been opened by a person.
2. **The service lead holds `policy.core_view` but not the class its route asks for.** `staff_core_policy` checks `DataClass::InsuranceSensitive`; the `service-lead` template has ordinary and personal only. Nobody in the cast can read a policy as the core holds it (SCN2-04 step 7).
3. **A template's author is the only product approver.** `st15` alone holds `documents.configuration` and `documents.approve_product`, and the documents module refuses an author approving their own template, so no template can be activated by this cast (SCN2-10).
4. **Actions no cast member holds:** `collections.*`, `wellness.view` (the AIA Vitality view on a policy and the `wellness_question` tool), `payments.connectivity` (redirect a route to the second bank, read the configuration), `held_balance.configure`, `staff.approve_profile_uplift`.
5. **Still no way for a person to start new business.** `quote` now has a holder (`nb`), but there is no quote screen (only the route and the MCP tool), no route opens a proposal ("whoever opens the proposal (no channel does yet)", `docs/underwriting.md`), and the adviser portal has no application. A quote leads nowhere (SCN2-01 step 5).
6. **No AIA application template is shipped to the stack.** `e2e/seed/src/main.rs` builds `term_life`, `income_protection`, `health_family`, `invest_plan` and `group_scheme` only; the AIA range is exercised in `crates/underwriting/tests/aia_journeys.rs` and never through the portal.
7. **A unit-linked customer cannot switch, withdraw or redirect contributions.** The portal carries four request kinds (`decline_indexation`, `change_frequency`, `cancel_policy`, `reduce_cover`); the customer page shows holdings and waiting orders but offers nothing to act on them, and the staff console likewise (SCN2-05).
8. **The senior claims limit is below shipped sums insured.** The seeded authority book tops out at $600,000 for every claim type, with nobody above; a $900,000 trauma or a $1,500,000 life claim (round 1 issued one) can be admitted by no one. The refusal says who can take it: check it does not name a role nobody holds (SCN2-08).
9. **`docs/distribution.md` and the code disagree** on whether a reduction of cover is attributed to the adviser's engagement: the doc says the portal carries none; `crates/channels/src/servicing.rs` lists `reduce_cover` in `ATTRIBUTED`.
10. **MCP reach is by module prefix.** `reach_of` offers `finance_command`, `payments_command`, `reinsurance_command`, `commission_command`, `documents_command` and the `*_decide` tools to anyone holding any action of that module, including view-only ones, so the read-only auditor is likely listed write tools she can never use (SCN2-18). The same applies to `group_command` for the read-only group administrator.
11. **The stack's regions differ from the shipped ones.** `stack.js` sets `auckland` (two nodes, home) and `sydney` (one node), while `docs/channels.md` describes Sydney and Melbourne leading with a witness; the payments seed names `akl`, `syd`, `mel` as voter regions. What `/ops` calls "at home" on the stack is not what production would show (SCN2-19).
12. **Complaint timers and the collections console use the wall clock** (calendar days, "today in New Zealand"), while the books' business date on the stack is 2027-11-15, a year ahead. Due dates on `/complaints` and dates on `/collections` will look a year behind the policy screens; round 1 recorded this for complaints as "not a defect" (F-21-13), but the customer and staff still see two "todays".
13. **An administrator's own request has no approver.** `admin` is the only holder of `staff.approve_grant`; `admin2` holds configuration approval only. A grant `admin` asks for herself can be approved by nobody (SCN2-24 step 5).
14. **Both payment approvers also hold treasury** (`payments-approver` carries `payments.approver` and `payments.treasury`), so the over-item-limit release that round 1 decided "treasury releases" is given by the same person who approves the run. Check whether the module wants them to differ.
15. **The privacy officer cannot see policies** (`customers.*` and `documents.*` but no `policy.view`), and the records officer can see policies but cannot find a customer (`policy.view`, no `customers.find`). An access request or a document search that starts from a name needs two people (SCN2-11, SCN2-12).
16. **Left from round 1 and still worth a look in passing:** a reused idempotency key with a different body is answered as accepted (F-SCN02-4); the claims cover check does not show the core's cover facts (F-11-4); names of advisers, providers and staff are shown as references because channels holds no names; a rejected or returned claim payment never reaches the claim in the stack (no payments outbox relay, F-15-3).
