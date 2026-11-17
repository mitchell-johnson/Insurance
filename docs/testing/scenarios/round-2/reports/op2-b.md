# Round 2 operator op2-b: SCN2-04, SCN2-05, SCN2-06, SCN2-21

## Shared state I changed (read first)

- **Balanced fund** (the scenario's shared state, mine)
  - It was blocked by an open price error that I did not raise: `err-hq8qn-2226-7`, raised on 2027-02-08 by an e2e journey before I started. To get it moving, I prepared and approved a restated correction, `balanced:2027-02-08.2`.
  - I then priced every weekday from 2027-02-09 to 2027-11-15 in order, as set-up. I played the valuation and core-units facts; st8 prepared and st9 approved each through the portal's route.
  - 2027-11-15 was done on the screens.
  - **Left behind:** `balanced:2027-11-16.1`, prepared by st8 and approved by st9 on screen. It is stuck in "activating", because every partition refused it: "a price cannot be struck for a date that has not come" (F2-06-4). It should deliver once the business date reaches 2027-11-16 and someone republishes it.
  - The error `err-hq8qn-2226-7` is corrected but not closed.
- **Cash fund** (shared state for SCN2-06, mine)
  - Priced in the same way from 2027-02-03 to 2027-11-12.
  - Suspended from 2027-11-15 and **resumed from 2027-11-16**, so it is no longer suspended. Its "next to price" still reads 2027-11-15.
  - Left behind: `cash:2027-11-15.1`, prepared for the suspended day, which no one can approve. Also `cash:2027-04-28.2`, a stale prepared price from my catch-up when it restarted.
- **Growth fund (not mine)**
  - In SCN2-06 step 1, `st9l` suspended growth from 2027-11-15 by mistake, as the scenario asks.
  - It **could not be undone for that day**: a resume must be dated after the suspension's first day (F2-06-1). It is resumed from 2027-11-16.
  - So growth now carries a one-day suspension on 2027-11-15, and that day will be skipped when growth is priced. Growth's next price is 2027-02-08, so nobody is affected today.
- **adv-d1 (Tessa)**
  - `comops` cut statement 1 for her.
  - `dist` set an accrual hold and removed it, then set a payment hold and **released** it. She ends as she started, apart from the statement.
- **My own customers**
  - SCN205's customer still has her own spending hold on (lifting takes a week).
  - SCN205-IP1 keeps a switch sell order, `scn205-switch-sell-1`, waiting for 2027-11-16.
- **Orphan:** SCN221-TL1 was issued with adviser adv-d1 and never paid. I lost its customer's phone key, so I made SCN221-TL3 and TL4 in its place.

## What I made off-screen (set-up and played parts)

**Customers.** All were made with `customer()`, each with a phone unless noted:

| Scenario | Who | Party | Name | Born |
| --- | --- | --- | --- | --- |
| SCN204 | her | pty_df8f… | Cgpdcdm Kgdopdbbfel | 1999-09-17 |
| SCN204 | him | pty_a8d3… | Cedocnl Kbgjalndmnl | 1967-07-12 |
| SCN204 | child (no phone) | pty_ea4c… | | |
| SCN204 | sister | pty_78c3… | | |
| SCN205 | customer | pty_d4ab… | Cpbdcdh Knnabjbdafe | 1989-03-13 |
| SCN221 | client 1 | pty_2daa… | Cngcopj Knhnehgdkka | 1998-04-11 |
| SCN221 | client 2 | pty_a754… | Cmmdbhe Kcnncmcfmhb | 1973-06-14 |

**Policies.**
- **SCN204-TL1:** `term_life`, two lives with their own parties, life cover of $500,000 each.
- **SCN204-HF1:** `health_family` with her, him and the child.
- Both owners were recorded as `add_relationship owner` on each of them in the customer master, playing new business. It relayed to the portal, and both became policyholders.

**Other set-up.**
- **SCN205-IP1:** `invest_plan`. The adviser was appointed through `POST /app/v1/policies/{id}/advisers` with `{adviser, engagement}`.
- **SCN221-TL3 and TL4:** issued with `play.js issue` (adv-d1, fap-d6), inception 42 days back, every premium paid by a receipt.

**Parts I played.**
- The bank receipt.
- Policy servicing: `allocate_cash`, `place_order` (buy, switch sell, withdrawals) and `cancel_order`, on `/v1/commands` as `policy-servicing`.
- Unit-pricing intake: valuations, core units and a `refuse_valuation_file`.
- The end-of-day unit-order pass: `execute_orders` as `unit-orders`, because orders only execute at a close.
- Owner and life-insured relationships in the customer master.

**Corrections to the scenario set-up.**
- Unit orders must have a pricing date on or after the business date, so the funds (priced only to February 2027) had to be priced up to the business date first.
- Orders execute only in the end-of-day pass.
- Playwright cannot carry a browser's signing key across contexts (the key is a non-extractable CryptoKey). Every approval was made in a context whose browser was set up in the same run, and the page showed "Failed to execute 'sign' on 'SubtleCrypto'" when I tried. That is a test limit, not a product defect.

---

## SCN2-04 A couple on one policy — **partly completed**

**What I did.**
- Both owners opened both policies.
- st5b entered a reduce_cover on his life by mistake. There is no way to take it back, so he declined it.
- st5b entered her reduction with an amount, so that it was above his limit. st5l agreed it.
- She signed. He declined. st5b entered it again, and both signed. It was applied.
- She set family consent "no" and named her sister as a viewer, through the app routes, because there is no screen for either.
- The sister looked, and tried a request.
- He asked to cancel the health policy alone, through the app route.
- st5l read the policy as the core holds it, through MCP `core_policy`.

**Results.**
- Her life cover is now $200,000 (core layer 3); his is untouched at $500,000.
- The outstanding bill fell by $12.58. This is the pro-rata share, 16/30 of $23.59, and $23.59 is the oracle's monthly fall from $448.27 to $424.68. It matches.
- Round 1 regression: staff can now reduce cover (F-13-1 holds). Cancellation needs a signature: an unsigned cancellation on a jointly owned policy waits for every owner.

### Findings

**F2-04-1 (major).** The cover to reduce is picked by life code, so staff cannot tell whose life they are cutting.
- Hurts: st5b, and the couple.
- Steps: `/staff` as sub-st5b → "A customer is on the phone" → policy SCN204-TL1 → Change "Reduce cover".
- Happened: the choices read "life on life-1: NZ$500,000.00 now" and "life on life-2: …". The policy rows above show `cover pty_df8f…` with "Life life-1, Layer 1", and "Type life" / "Product term-life".
- Should: offer "Life cover — Cgpdcdm Kgdopdbbfel (life 1): $500,000", with names and products in words.
- Cause: `crates/channels/ui/staff.html` around line 118 builds the option from `c.cover`/`c.life`; party names are not resolved.

**F2-04-2 (major).** Staff cannot withdraw a request they entered by mistake.
- Steps: as above, enter the reduction on life-2, then look for a way to take it back.
- Happened: there is none. `/staff/v1/requests` has POST and GET only (`http.rs:4903`). The only way out was for the customer to decline it.
- Should: the person who entered it can withdraw it until the customer signs.

**F2-04-3 (major).** The customer's prompt does not say whose cover changes, or from what amount.
- Steps: `/customer` as either owner → To approve.
- Happened: both owners read "Reduce life to NZ$200,000.00". He was asked to approve a cut to *his* life (the mistake) with nothing showing it was his, and she is asked the same for hers.
- Should: "Reduce your life cover from $500,000 to $200,000" for the person whose life it is, and "Reduce Cgpdcdm's life cover …" for the other owner.
- Cause: the customer page change wording (`portal.change`).

**F2-04-4 (major).** Nobody is told whose signature is awaited, and a signer's approval disappears.
- Steps: she approves; then both pages are read.
- Happened:
  - Her page keeps the row under "Waiting for you" with Approve and Decline buttons, after saying "Approved… It shows under Your requests". Her "Your requests" says "You have asked for nothing, and approved nothing, yet."
  - His page shows the same row with no sign that she signed.
  - After both signed, the "Done" row appears only in his list, not hers.
- Should: "You signed on …; waiting for Cedocnl to sign", and the other owner should see "Cgpdcdm has signed".
- Cause: the state keeps `signed_by` (`state.rs:1217-1236`), but neither the feed nor `customer.html` uses it. Her request list seems to hold only requests where she is the last approver or a decliner.

**F2-04-5 (major).** A decline by one owner is not explained to the other.
- Happened:
  - When he declined, the request vanished from her page with no word.
  - His own cancellation, declined by her, reads "Declined by the customer" on his page; he *is* a customer.
  - On her page his cancellation was shown as "Asked by another owner" (fine), but on **his** page too it reads "Asked by another owner".
- Should: say "Declined by Cedocnl on …" to both owners, and label the asker's own request as theirs.

**F2-04-6 (major).** The above-limit check ignores the cover given up.
- Steps: st5b enters a $300,000 reduction with no "Amount it moves".
- Happened: it went straight to the customers.
- Should: a reduction of sum insured is measured by the cover given up, or by its premium effect, and a $300,000 cut reaches a team lead.
- Cause: `state.rs:846` takes only the optional `payload.amount_minor`. I typed $1,000 into "Amount it moves" to reach st5l at all. The field's meaning for a reduction is unclear: the premium change? the cover?

**F2-04-7 (minor).** The second-approvals queue row says too little, and shows codes.
- Happened: the row reads "Reduce life to NZ$200,000.00, SCN204 her life only to 200k, NZ$1,000.00". It shows no "from $500,000", no life, the entered-by as `st5b` (a code), and the date from the wall clock (17 Nov 2026 against a business date of 15 Nov 2027).
- Should: show both amounts in dollars, the life in words, and the colleague's name.

**F2-04-8 (major).** The customer page cannot set family consent or name a delegate.
- Happened: the routes exist (`/app/v1/policies/{p}/consent`, `/delegates/{party}`, `http.rs:4872`), but `customer.html` has neither. I used the routes.
- Results through the routes:
  - The sister, as a viewer, sees the health policy and its covers, and her request was refused with "your relationship to this policy does not allow requests". That is right, but it does not say what she may do instead.
  - With no claim on the policy, the effect of consent "no" could not be observed; that check is not done.

**F2-04-9 (major).** The customer page offers no way to ask for any change.
- Happened: no cancel, reduce or change of frequency; only "Tell us about a claim". Step 2's "if the page offers it" fails, and step 6 (he cancels the health policy alone) could only be done through the API route.
- Results through the route: his unsigned cancellation went to "awaiting approval" for both owners, she declined it, and nothing changed. That part is right.

**F2-04-10 (minor).** The customer's policy view has layout noise and codes.
- Happened:
  - The header shows the sign-in subject `sub-pty_df8f…` instead of the name.
  - Hospital cover shows "COVER AMOUNT NZ$0.00".
  - The "HELD AT" column is always empty.
  - "Your investments: No units held yet…" appears on term and health policies.
  - Dates are ISO (2027-11-01).
  - "PAYS –" on a monthly policy (the staff view also shows "Frequency –" for monthly).

**F2-04-11 (minor).** Policy standing reads "paid" while $448.27 is due.
- Happened: the staff and customer views show "Standing paid" with $448.27 due from 2027-11-01 (within the grace period). The core says `standing: paid`.
- Should: "Within grace: $448.27 due 1 Nov".

**F2-04-12 (minor).** Reading the policy as the core holds it.
- Things noticed #2 is **refuted**: st5l now holds the insurance-sensitive class and can read it.
- Happened: there is no screen. MCP `core_policy` and `GET /staff/v1/core/policies/{NUMBER}` give raw JSON, and the route answers 404 for the policy id `scn204-tl1`, only the number works.
- MCP `core_policy` takes `number` and `query.case`. A wrong argument gets "policy is not an argument of core_policy" without naming the right one.
- The core's `policyholder` holds only her, though both own the policy.
- st5b is refused, naming `policy.core_view`, which is right.
- The layers are right: layer 1 ended 2027-11-15, layer 3 is $200,000.

**F2-04-13 (minor).** The success message shows a UUID.
- Happened: "Request e1688187-26d3-… recorded."
- Should: "Sent to Cgpdcdm and Cedocnl to approve."

---

## SCN2-05 Unit-linked: pays in, switches — **partly completed (blocked at the switch's execution)**

**What I did.**
- $5,000 receipt, then allocate, then a buy order in balanced for 2027-11-15.
- The customer saw it waiting.
- Balanced was priced on screens (st8 prepared, st9 approved): mid 1.0820, buy 1.0837, sell 1.0803, **equal to `oracle.prices`**.
- I played the order pass. 4,613.823013 units = $5,000 / 1.0837, rounded down, which is right. Value $4,984.31 = units × sell 1.0803 on 2027-11-15, which is right.
- Switch dead ends tried.
- Switch sell (50%) placed; the next day's price was struck but cannot be delivered (F2-06-4), so the sell and the cash buy never execute.
- Hold placed; withdrawals played.
- MCP read.

### Findings

**F2-05-1 (major).** There is no switch on any channel, and a made-up kind is accepted.
- Things noticed #7 **confirmed**.
- `/customer` has no request form at all. The adviser change form has decline_indexation, change_frequency and cancel_policy only. MCP `make_request` lists decline_indexation, change_frequency, cancel_policy and notify_claim.
- Steps: MCP as the customer, `make_request {kind:"switch_funds", module:"policy-servicing"}`.
- Happened:
  - It was **accepted**, and `/customer` showed "Switch funds — Being carried out".
  - It sat in the staff "sent to a person" list. Carry-out answered "switch_funds is not a change the portal carries to the core; ask the service team", which was said to the service team itself.
  - The customer then sees it rejected with that same text.
- Should: refuse unknown kinds at submission, listing the kinds. Give every dead end a "who can do it" (for example "Ask your adviser to arrange a switch with our investment team on 0800…"), and add a switch.
- Cause: `submit_request` takes any kind (`http.rs:1118`); `servicing::carried_by_sweep` (`servicing.rs:143`) leaves unknown kinds for a person.

**F2-05-2 (major).** The customer's own spending hold does not stop a withdrawal.
- Steps: `/customer` → Your details → "Put a hold on". It asks for a signature first; with signing set up: "A hold is on." Then a withdrawal `place_order` (kind withdrawal, $1,000) as policy-servicing on the API.
- Happened: **applied**. A switch sell was also applied, which is right, because a switch takes no money out.
- Should: the withdrawal is refused naming her hold and how to lift it.
- Cause: nothing outside `crates/customers` and `crates/channels` reads the spending hold. `crates/api/src/servicing.rs` checks only `routing` and `authority`, and payments has no reference to it, though `config/cedar/customers.cedar` gives payments, servicing and claims read access "for everyone who pays money out".
- I cancelled both withdrawal orders afterwards.

**F2-05-3 (minor).** Asking for a protective hold needs a signature.
- Happened: "Changing your details needs your signature. Set up this browser under Signing first."
- Should: a hold that only protects the customer should be quick to place; signing should be kept for lifting it.

**F2-05-4 (major, product limit; blocks the scenario on the stack).** The funds' pricing calendar is nine months behind the business date.
- The funds' next pricing dates were February 2027, while orders must be dated on or after the business date ("the pricing date is today or later, and within 400 days").
- On a fresh stack, no customer's contribution can ever be invested without about 200 days of prices. The stack should seed prices up to its opening date.
- This is a stack issue; round 1 op-e saw the same.

**F2-05-5 (minor).** The customer page shows codes, and does not explain the switch or the spread.
- Happened:
  - It reads "balanced", not "Balanced Fund", and "buy" and "sell" in lower case.
  - The waiting switch sell reads "balanced sell 2,306.911506 units" with no word that it is a switch to cash.
  - After the buy, the value is $4,984.31 against the $5,000 she paid, with no word about the buy/sell spread.
  - After the price was approved, the order still read "Waiting for a price" until the end-of-day pass; it should say "Priced on 15 Nov; units added tonight".
  - The cover row "Account NZ$0.00" means nothing to her.
  - The contribution is never shown as a premium or as overdue, which is good.

**F2-05-6 (major; product, shown by another scenario's state).** An adviser can be appointed under an ended engagement, and the adviser's view could not be tested.
- Happened: the appointment of adv-d2 under eng-d2 was accepted (200), though eng-d2 had ended on 2027-11-15. Another operator's SCN2-20 moved Viliami to Kōwhai under ENG-20271115-B5WMM.
- Viliami's book does not list SCN205-IP1, so his view of the holdings was not tested.
- Should: an appointment under an ended engagement is refused in words, or the customer is told the adviser cannot act.

**F2-05-7 (minor).** MCP leaves the assistant guessing, and shows cancelled orders.
- `my_policy` gives `units_micro: 4613823013`, `value_minor: 498431` and `price: "1.0803"` for fund `balanced`. That is unambiguous only if the assistant knows micro means millionths and minor means cents; nothing in the tool description says so.
- `my_account` lists the policies only: no holdings and no orders.
- Cancelled orders (`scn205-wd-1`, `scn205-wd-2`, `scn205-switch-sell-2`) are still listed as `unit_order` rows in `my_policy`, while the page has dropped them.
- `make_request`'s description lists no `reduce_cover` kind, though the API carries it.

---

## SCN2-06 Pricing lead suspends a fund — **partly completed**

**What I did.**
- Cash 2027-11-12 struck on screens (st8 prepared, st9 approved).
- st9l suspended Growth by mistake, could not undo it for that day, and resumed it from 2027-11-16.
- st9l suspended Cash from 2027-11-15 with a reason.
- st8 tried to prepare cash for 2027-11-15: first refused for a missing valuation. After I played a valuation it was **prepared**, and st9's approval was then refused.
- I played an unreadable file.
- MCP as st9l: status, funds, work, late, errors.
- `resume_fund` with a wrong fund name, then correctly from 2027-11-16.
- The next day's balanced price was prepared and approved, then refused by every partition. Republish was accepted and changes nothing.
- st9l cannot prepare, which is right.
- The auditor was checked.

**Recorded.**
- Suspension needs a reason, and shows who (st9l) but not when.
- Resume is done by the same person with no second signature. The module allows it, and the page asks for nothing more.
- Unreadable files are listed with the row and the reason.
- On the overdue list, the suspended cash day did not appear as late, which is right.

### Findings

**F2-06-1 (major).** A mistaken suspension cannot be undone.
- Steps: `/pricing` as sub-st9l → Funds → Growth, Suspend, from 2027-11-15 → Send. Then Resume with from 2027-11-15.
- Happened: "a fund resumes after the date it was suspended from". The earliest possible resume left growth with a one-day suspension on 2027-11-15, which will be skipped forever.
- Should: an undo, or a resume on the same date, that removes the mistaken suspension; and the refusal should say "resume from 16 Nov or later".

**F2-06-2 (major).** A price is prepared for a suspended fund and date.
- Steps: after the cash suspension from 2027-11-15, a valuation arrives; st8 → Prepare, Cash, 2027-11-15.
- Happened: "Prepared cash:2027-11-15.1. Someone else approves it." st9 then pressed the offered Approve and got "cash is suspended from 2027-11-15: SCN206 custodian cannot value the cash fund today". The prepared price stays on the work list for good, Approve button and all.
- Should: preparation is refused, naming the suspension and who placed it; and the refusal to approve should say who placed it and who can resume.
- Also: before the valuation arrived, the refusal named only the missing valuation, not the suspension.

**F2-06-3 (minor).** The fund's next date ignores the suspension.
- Happened: the Funds tab and `pricing_question funds` still give cash "next 2027-11-15" after that day was suspended and the fund resumed from 2027-11-16.

**F2-06-4 (major).** A price for a date after the business date can be prepared and approved, and then fails silently.
- Steps: valuation for balanced 2027-11-16 (the business date is 2027-11-15); st8 prepares, st9 approves on screen ("Approved. It is on its way to every partition.").
- Happened:
  - Every partition refused it: `invalid_unit_price: a price cannot be struck for a date that has not come`.
  - The work list shows status "activating" with empty "failed checks", and still offers Approve and Reject.
  - `republish` answers "done" and changes nothing.
  - Only `pricing_question price` shows the refusals.
- Should: refuse to prepare a price for a date that has not come, and show partition refusals in words on the work list.
- This is the honest "partition refused" case for step 5. It also blocks step 4 (bring it back cleanly tomorrow) and SCN2-05 step 5 on a stack whose date cannot move.

**F2-06-5 (minor).** A wrong fund name is not caught.
- Steps: MCP st9l `pricing_command {resume_fund:{fund:"cash fund",…}}`.
- Happened: "cash fund is not suspended". It should be "there is no fund 'cash fund'; the funds are balanced, cash, growth".

**F2-06-6 (minor).** The MCP pricing tools leave the assistant guessing.
- `pricing_command`'s body is described only as `{"<command>": {...}}`: no command names and no fields.
- `late` needs `as_at` ("late needs as_at"), but the description lists it among optional keys.
- `late` and the page's "Prices overdue today" list every date individually (41 to 100 rows), not per fund.
- `funds` is raw JSON with spreads in basis points.

**F2-06-7 (minor).** The auditor's refusals and tool list.
- The page refusal says "Your roles do not include unit pricing" and does not name the permission.
- No pricing tool is listed for the auditor, and a call to one by name is refused in words. That is right.
- But the auditor is listed `commission_command`, `payments_command`, `reinsurance_command`, `reinsurance_decide` and `finance_command`. That confirms Things noticed #10.

**Step 6 (the customer's order during the suspension) was not tested.** The switch's cash buy is only placed when the balanced sell executes, which F2-06-4 blocked.

---

## SCN2-21 Cover cut in the first year: whose commission? — **mostly completed**

Things noticed #9 and stack item 4 (reduce_cover attribution):
- **The code carries a reduction; the adviser portal does not.** `docs/distribution.md` is right that the portal has no reduction: the adviser form has decline_indexation, frequency and cancel only.
- The servicing relay does attribute `reduce_cover` (`servicing.rs:47`).
- MCP `make_request` with `kind:"reduce_cover"` was accepted, though it is not listed.

**What I did.**
1. Tessa read her book, commission tab and MCP tools.
2. Over MCP she prepared reduce_cover for SCN221-TL3, life $500,000 to $250,000. The client approved it, signed, on `/customer`, and it was applied.
3. comops read the positions and the exposure, and cut Tessa's statement.
4. Client 2 cancelled SCN221-TL4: st5b entered it from the client's call, and the client signed on `/customer`. The customer page has no cancel, so it could not be the client alone.
5. The dispute path was tried.
6. The hold mistake was made and corrected, then released.

**Figures by hand** (aia-risk upfront: 200% initial, 100% clawback under 12 months; book-d1: adviser 75%, provider 25%, provider bears clawback):
- TL3 annual premiums: life $431.02, trauma $187.28. Initial commission: life $862.04 (adviser $646.53, provider $215.51), trauma $374.56. The statement lines match exactly.
- The reduction gives up half of the life cover: by hand, clawback = 200% × $215.51 = **$431.02**. The system: fap-d6's clawback exposure fell by **$431.03**; its commission fell by $431.32 (the clawback plus $0.30 of renewal); Tessa's fell by $0.88 ("commission reversed").
- TL4 cancellation, by hand: $7,636.56. The system: **$7,636.54**, all on fap-d6.
- Exposure after both: $805.57, against $805.58 by hand. The exposure equals the open lines, to within about 2 cents of rounding per policy.
- The maker could not approve, and nothing was raised for approval (no tasks).
- The customer was never shown commission.

### Findings

**F2-21-1 (major).** The adviser cannot see what a change costs her, before or after.
- No tool or screen says what a change will cost the adviser before she sends it.
- After it, her Commission tab shows "Nothing is being held from you" and no clawback.
- Her statement is empty ("you have no statement number 1") until commission ops cuts one. She cannot list her statements, though the refusal says "the latest on your Commission tab", and the tab shows none.
- The cut statement shows lines as `ce:0-460 scn221-tl3 NZ$646.53` and `cu:0-560 scn221-tl3 -NZ$0.88`: reference codes, a lower-case policy id, no cover, and no reason ("cover reduced").
- Should: "SCN221-TL3 life cover reduced on 15 Nov: your clawback $0.00 (Harbourline bears $431.02)", and a list of statements.

**F2-21-2 (major).** comops cannot see the clawback or the attribution per policy.
- Steps: `/commission` as sub-comops → Look up → Clawback exposure, date 2027-11-15.
- Happened:
  - The answer is "Fap-d6 887314": raw cents, with the code capitalised as a name.
  - Position and statement need party codes typed in (adv-d1, fap-d6), with no picker (`commission.html:22-27`).
  - Neither the reduction nor the cancellation shows which engagement it was attributed to, or under which cause. The two cases looked identical (both borne by fap-d6), so "visibly different" could not be judged.
- Should: dollars per payee, a link to the clawback lines, and the cause shown in words.

**F2-21-3 (major).** An adviser cannot dispute a clawback.
- Steps: MCP as d1, `dispute_replacement {layer:"scn221-tl3/life/1"}`.
- Happened: "that layer is not one of your matches". There is no path to dispute a clawback as an error; only replacements can be disputed.
- Also: compliance (st13) has no report of early reductions across a book. `commission_question` offers position, statement and exposure, nothing by adviser and cause.

**F2-21-4 (major).** Accrual hold and payment hold.
- Steps:
  - As sub-comops, "Another command" `{"set_accrual_hold":…}` → refused, naming the distribution administrator, which is good.
  - As sub-dist, the same → "Done."
- Happened:
  - The accrual hold shows **nowhere**: not in comops' "A party's position", and not to the adviser ("Nothing is being held from you").
  - The payment hold, set on the form, shows to staff as "Payment hold: <reason>" but **not to the adviser** (portal and `adviser_held_balance` both say nothing held).
  - Setting an accrual hold needs hand-written JSON, and the reason must be a code (`missing_payment_details` or `legal_hold`).
- Should: both holds named in words to staff and to the adviser ("still earning, payment paused" against "not earning from …").
- The cast's comops cannot do step 8 at all; the distribution administrator can.

**F2-21-5 (minor).** The adviser portal and MCP leave the assistant guessing.
- The portal shows product "term-life", engagement and provider codes, and the request list's dates from the wall clock (17 Nov 2026).
- `my_policy` wants the policy id (`scn221-tl3`); the number gets "no such resource". `core_policy` wants the number. Nothing says which.
- `adviser_statement` takes `number` as a string, with no way to list numbers.
- `commission_question`'s `query` description names `party, as_at, payee, number, period`; `clawback_exposure` needs `date`, which I had to guess.

**F2-21-6 (minor).** The adviser's statement says payments are held, and the adviser sees no reason.
- The statement says "Payment held: no verified account past its cooling-off" for adv-d1, though the seed gave her an account; it is within its cooling-off period.
- The adviser sees no such reason on her own screen.

---

## Things noticed while designing

| Item | Verdict |
| --- | --- |
| #2 | Refuted: st5l reads the core policy. There is no screen for it (F2-04-12). |
| #7 | Confirmed (F2-05-1). |
| #9 | Both are partly true. The adviser portal carries no reduction, but MCP `make_request` accepts one and servicing attributes it (SCN2-21). |
| #10 | Confirmed for the auditor (F2-06-7). |
| #12 | Wall-clock dates are seen on the customer's and staff's request lists and in the second-approvals queue (17 Nov 2026 against a business date of 15 Nov 2027). |
| #16 | Names of staff, advisers and providers are still shown as codes, everywhere I looked. |

## Test-stack limits, not product defects

- Signing keys cannot be carried across Playwright contexts.
- Orders execute only at a close, so I played that pass.
- The business date cannot move. That blocks next-day pricing (F2-06-4), the switch's execution, and the fund-resume check.
- e2e journeys had left an open price error on balanced before I started.
