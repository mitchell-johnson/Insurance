# Round 2: what is still open (18 Nov 2026)

Round 2 fixes are on main, but these items are not done yet. Each area's fix report in [fixes/](fixes) gives every finding a line. Defects that hurt a person's work come first, then missing screens and wording, and last the test-stack gaps. "Partly" means the main fault is fixed and the part named is left.

## Defects and missing behaviour

| Finding | Area | What is wrong | What it needs |
|---|---|---|---|
| F2-09-4 | Claims | A benefit run pays a month ahead. The payments list and the remittance disagree. | A claims decision on paying in arrears or in advance, then a change to how runs pick periods. |
| F2-09-5 | Claims | The partial-benefit overpayment at the end of a benefit ignores the schedule's income offsets. | Rework the arithmetic in `overpay.rs`, with a recalculation test. |
| F2-09-7 | Claims | Nobody can refer a claim for investigation. | A new command, role, route and MCP tool, and a decision on who refers. |
| F2-08-4 | Claims | The cover check once showed "life on the policy: No" for a covered life. | Not reproduced. Re-test now that cover facts name their life (F2-08-3). |
| F2-08-8 | Reinsurance | `ri-life-base@1` has no trauma rate, so trauma cessions cannot be priced. | Treaty configuration from the reinsurance owner. |
| F2-08-10 | Reinsurance | It is not settled whether the reinsurer's share of premium set off against a claim is shared. | A treaty decision. |
| F2-16-2 | Payments | A customer paying their own premium by direct credit looks like a third-party payer. | A customer-master question that matches payer accounts to the person's own bank details. |
| F2-16-4 | Payments | One reconciler can apply unidentified money alone. | Show the receipt-match threshold and require a second reconciler above it. The cast also needs a second reconciler. |
| F2-16-6 | Payments | No legal override of a customer-set hold. | A legal command in payments. |
| F2-22-1 | Group | Rates and salary multiples cannot change at renewal. | A design across group terms, the core premium and cover, and the product engine (recorded in docs/decisions.md). |
| F2-22-4 | Group | The screens do not say which limit applies to automatic cover: the $500k reinsurance retention or the product's $750k. Until the reinsurance cap arrives, a member is billed on $750k and then corrected. | Label the limit on `/employer` and `/group`. Decide whether group should wait for the cap before billing. |
| F2-02-11 | New business | Interim cover issues only the life cover, not accidental TPD, trauma or income. | `settings.interim` to list a cover per product, and the issue flow to take them. |
| F2-02-12 | New business, distribution | An adviser's book shows no applications until the customer appoints them. | The adviser named on a proposal with consent, published to the book. |
| F2-01-10 | New business, documents | No schedule or welcome letter is sent when a policy is issued. | A documents trigger on the core's `PolicyIssued`, and seeded templates. |
| F2-14-2 (part) | Customer master | An issued policy leaves no relationship in the customer master. | The core's issue to be carried into the master. |
| F2-14-3 | Customer master | There is no "what we keep, until when and why" view, and no erasure on request. | A retention schedule across modules. |
| F2-12-1, F2-12-2 | Privacy | There is no privacy-officer report across modules, and the customer cannot see who looked at their file. | A product decision on what is shown, then the reads. |
| F2-13-3 | Customer master | A customer cannot ask for two merged records to be split. | A service flow. |
| F2-23-3, -4, -5, -7 | Complaints, frontline | Frontline cannot check a caller and open the policy. "D for M" and its representative check are missing. Adjustments split at commas. Timers ignore the marker. An attorney cannot see the people they act for. | A frontline template and route; complaints and markers work; a decision on what an attorney sees. |
| F2-21-1 to -6 | Commission | No cost preview before a change. No clawback exposure per payee, and no attribution by cause. No dispute path. Holds are not shown in words. A statement says payment is held during cooling-off. | Commission console and adviser portal work. |
| F2-24-5 (part) | Staff access | `employ` binds a party without checking its sign-in, and needs no second person. | An identity-mapping flow, and a decision on seeding (the first administrator bootstraps alone). |

## Screens, pickers and wording

These leave the work possible but harder than it should be:

- **Staff typing references they cannot know:**
  - payments pickers for policy, party and authority (F2-15-2);
  - partition and run on the claims console (F2-09-10);
  - the treaty on the reinsurance Cut form (F2-08-9);
  - engagements and accreditations on `/commission` (F2-20-5);
  - a scheme list, terms view and money in dollars on `/group` (F2-22-8).
- **Names instead of ids:**
  - staff appear by workforce id because the staff record holds no display name (F2-10-12, F2-03-4, F2-15-8);
  - recipients and payees appear without names (F2-11-7, F2-08-6, F2-08-12, F2-20-11);
  - product display names and option labels are missing on staff pages and the customer page (F2-01-7, F2-11-12).
- **Missing views:**
  - what is ceded on one life and to whom (F2-08-1);
  - payee change history, closed runs and a journal list for auditors (F2-18-4);
  - an "Access administration" screen (F2-24-8);
  - renewal date and terms on `/employer` and `/group` (F2-22-2);
  - what an employer invoice covers (F2-22-5);
  - old and new salary in a file preview (F2-22-6);
  - an "Add a payment instruction" form (F2-20-9);
  - an explanation of an empty adviser book (F2-20-12).
- **Wording:**
  - refusals in payments and finance should name who acts (F2-18-9);
  - unlabelled `aged_minor` buckets and Unix-second times in `staff_me` and `staff_access` (F2-18-5, F2-24-4);
  - a grant example and a `withdrawn` status (F2-18-8);
  - a statement number in the commission confirmation (F2-20-10);
  - review tasks under Tasks (F2-09-2);
  - the overpayment picker (F2-09-6);
  - the prompt on a sharp reserve move (F2-08-7);
  - the preparer seeing their own prepared settlement (F2-08-11);
  - the claim closing on release (F2-08-13);
  - the customer's end date (F2-09-8);
  - `/records` dates (F2-16-8, F2-12-5);
  - `/collections` arrears on the wall clock (F2-15-3);
  - smaller items in F2-24-10, F2-23-10 and F2-12-4.
- **Routing:** a case is not routed to a senior queue by amount; the refusal says who can act (F2-02-10).

## Servicing and unit pricing (SCN2-04, -05, -06)

This fixer was stopped early. Its fixes are on main: [fixes/fix2-servicing.md](fixes/fix2-servicing.md) lists 16 fixed, 1 partly fixed and 9 left. Left:

- **Staff page:**
  - F2-04-1: the cover picker does not yet use the life names the policy view now gives.
  - F2-04-2: there is no Withdraw button. The route and MCP tool exist.
  - F2-04-7: the second-approvals row shows codes and the clock time.
  - F2-04-13: a request's UUID is shown.
- **Customer page:**
  - F2-04-8: no family consent or delegate forms.
  - F2-04-9: no signed change-request form.
  - F2-04-10: layout noise and codes.
  - F2-04-11: "paid" is shown while a premium is due within grace.
  - F2-05-1: no switch form. The `switch_funds` kind exists.
  - F2-05-5: fund names and the switch, spread and pricing wording.
- **Core policy view (F2-04-12):** no screen shows a policy as the core holds it, and MCP does not name the right argument.
- **MCP (F2-05-7):** `my_policy` field descriptions, cancelled orders still listed, holdings in `my_account`.
- **Decisions not yet recorded in docs/decisions.md** (the code is done):
  - unknown request kinds are refused, with the kinds listed;
  - the switch pricing date;
  - the `assisted_cover_reduction` limit;
  - a same-day resume withdraws a suspension;
  - the pricing console sends the business date;
  - protective holds need no device.
  - docs/channels.md also does not yet describe the new withdraw route, the switch kind or the pricing commands.
- **e2e tests behind these fixes:**
  - E2E-229 expects Approve to be refused for a suspended fund; now the prepared price itself is refused.
  - E2E-230 expects a same-day resume to be refused; now it withdraws the suspension.
  - Both tests need updating.
- **Stack:**
  - seed fund prices up to the stack's opening date (F2-05-4);
  - give team leads an `assisted_cover_reduction` limit.
- **Group life rule set (F2-22-3):** it is now shipped (`rules_group*.star`). If executives on the stack still raise `underwriting_not_reached`, the cause is probably `member_not_linked`: the member has no customer-master person.

## Not yet tested

- **SCN2-07, SCN2-17 and SCN2-19** (moving the business date, closing days, losing a region) were not run. Each needs a fresh stack to itself.
- **Scenarios blocked in the first run** have not been re-run since the fixes.

## Test stack gaps

- **Payment relays:** the core-to-payments relay (`ic-payments-service`) does not run on the e2e stack. So payments never hears `policy_owner`, `record_customer_hold` and the other relayed events; tests tell payments directly.
- **Document templates:** none are seeded, so documents do not issue on the stack (F2-10-1).
- **`documents.remediation`:** no one in the cast holds it besides the person who raises a defect (F2-11-6).
- **E2E-261 (reinsurance month-end check):** it passes alone but failed once in a full run. Either the cession took longer than its 10-second wait, or another test had already cut the retail statement.
- **Full e2e run:** the last full run was stopped during the business-date tests: 334 of 336 passed, and the other failure (E2E-282) is fixed. Tests after E2E-247 in the date-moving group have not run since the merge.
- **Failure drill:** the drill workflow (`drill.yml`) has failed on every push since before round 1. It was not looked at in this round.
- **Browser checks:** the round 2 screen changes have no automated browser check. `crates/channels/tests/browser/check.js` was updated but not run.
