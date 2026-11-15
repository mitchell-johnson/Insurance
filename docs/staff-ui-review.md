# Staff screens: what people need, and what changed

A review of every staff screen, and the adviser and employer portals, against what a person needs to do the job well and enjoy it (November 2026). Each screen was judged on six questions:

1. **Can I start without knowing a reference?** People know a caller's name and birth date, a policy number from a letter, or nothing at all. They never know a party, case or price reference.
2. **Does it tell me what is waiting?** A screen that opens on an empty form makes people keep their own lists.
3. **Does it carry me to the next step?** After opening something, the likely next actions should be one press away, filled in.
4. **Does it fill in what it already knows?** Today's date, the period being closed, the case in hand, a new reference, the colleagues who may take the work.
5. **Does it speak the person's words?** Policy numbers and names, not internal keys. Dollars, not cents. "Benefit payments", not `benefit_payments`.
6. **When something cannot be done, does it say why and what to do?**

The fixes use the shared design system (`docs/design-system.md`): the picker, find, links between consoles, the case in hand, and the defaults listed under "Picking, finding and defaults". Every page is still driven end to end by `crates/channels/tests/browser/check.js` and by the persona suite in `e2e/`.

## Shared foundations

| Need | Before | Now |
| --- | --- | --- |
| Find a customer | Staff typed a party reference into the policy form. Nobody on a call has one. | `GET /staff/v1/find?q=` reads one box (name, "family, given", birth date in New Zealand order, email, phone, IRD number or policy number) and answers with people (name and birth date only) and the policies each owns or is insured on. The search and each person listed are put to the four questions under the case in hand. A person the member of staff is connected to comes back as "closed to you", never by name. The customer master scores the matches; at most 8 people are read. |
| Find a policy | The policy reference had to be typed in full. | The same box finds a policy by its number or reference typed in part (at most 10). |
| Pick a colleague | Handlers, owners and reviewers were typed as staff references. | `GET /staff/v1/colleagues?action=` lists the people current on the staff directory who hold the action today, with team and roles. Work is never offered to someone who could not take it. |
| Say why once | Every form asked for a case reference. | The case or call in hand is set once per tab (`portal.workOn`) and fills every form; "New call" makes a `CALL-` reference. |
| Make a reference | People made up claim, task, journal and run references. | `portal.reference("CLM")` makes a dated, unique, readable one (`CLM-20261115-7KQ2M`). |
| Move between consoles | Each console was an island. | `portal.link("claims", "claim", { claim })` opens another console on the right view with the record loaded; a sign-in keeps the link. |

## Staff console

**What a person needs.** A caller on the line: find them, open the right policy, see what can be done, enter a change, and hand anything above their limit to a colleague.

**Found.** The console opened on a policy reference field. A second approval needed the request's reference typed in. Declaring a personal connection needed a party reference.

**Changed.** The first card is now "Who are you helping?": the case or call, then one search box. Results show each person by name and birth date with their policies as Open buttons. The policy field is a picker over the same search. An opened policy links to its claims and complaints. Second approvals are a queue (`GET /staff/v1/requests`) with an Agree button on each row; the typed reference stays as a fallback. A personal connection is found by name and confirmed in a dialog.

## Claims console

**What a person needs.** Their own claims first, then the claim in front of them, the next thing to do on it, and the people and policies involved without looking them up elsewhere.

**Found.** The queue had to be asked for. A claim, cover, task, evidence or overpayment had to be typed by reference. New claim, task and payment references were made up by hand. The partition was a number.

**Changed.** The queue loads on arrival, starting with "Claims I own"; Open moves to the claim. The claim field finds claims through their policy. "Notify a new claim" starts one with a new reference. Next steps are offered as buttons from what the claim holds. Every action form fills itself: new references, colleagues who may take claims work, people by name, the policy and only its covers, the claim's open tasks, outstanding evidence and overpayments. Partitions are a list. The purpose defaults to "Working claim X".

## Underwriting console

**What a person needs.** Which queue has work and how much, the cases they hold, and the library of reasons and exclusions in force when they decide.

**Found.** The queues gave no counts and opened empty. Reason and exclusion codes were typed from memory. Case, proposal and life references were typed.

**Changed.** Each queue shows how many cases it holds and the console opens on the first with work. "Cases you hold" lists your own. Opening a case fills case, proposal, life and purpose. Reasons (a new `Reasons { on }` question on the underwriting module) and exclusions are picked from the libraries in force. Colleagues, people, non-disclosure reviews and peer-review samples are picked. Dates start sensibly.

## Complaints and conduct

**What a person needs.** To record a complaint from what the complainant says, link the policies it is about, and work the register by deadline.

**Found.** The complaint reference, complainant party and each policy were typed. Adjustments were codes.

**Changed.** A CMP reference is made for you. The complainant is found by name and shown by name. Their policies are offered to tick. Adjustments are picked from the list. Handler and staff fields pick colleagues. The register opens on this month and every complaint has an Open button. A party's complaints are found by name.

## Finance console

**What a person needs.** Which period is closing and what still stops it, then reporting, waiving, journals and sign-off for that period.

**Found.** "Work waiting" was a raw table. The period, check to waive, owners, journal, year-end and return references, and dates were typed. A stalled feed was retried by typing a partition number. An accounting group needed a partition number and a policy reference. The cut-over total was in cents.

**Changed.** Work waiting lists each period still closing with what it needs in words, and "Open its close" shows it. The likely period (the oldest still closing, or the month before the books' date) is filled in everywhere a period is asked and opened on arrival when one is closing. The check to waive is picked from the checks not green. Owners are picked from colleagues. References and dates are made for you; the remediation date starts a fortnight out. A stalled feed has "Try it again" on its banner, and the retry form lists the policy stores. An accounting group is asked by policy number alone; the console finds where it is kept (`accounting_group` without `partition`). The cut-over total is in dollars.

## Payments

**What a person needs.** Payments waiting to be taken, the open runs, an exception worked from its row, a payee reached from the person.

**Found.** Runs, exceptions, accounts and payer cases were typed by reference. Run types were codes. The take form showed with nothing to take.

**Changed.** Runs, exceptions and bank accounts are picked from what is open. Each exception opens its account's reconciliation or is applied from its row. A payer case is cleared from its row. A party's payees each open. A new run gets a reference and today's date, and opens once prepared. Run types read as words. Taking payments is offered only when some wait, and says so when none are ticked.

## Group schemes

**What a person needs.** Work waiting, each item opened where it is worked.

**Found.** Every form needed the scheme and the file or employee typed; possible matches needed a party reference; a scheme's first administrator was a party reference.

**Changed.** Each item of work has Open (or Decide, for a possible match) that fills the scheme and file or member and goes to the right view; a possible match shows the candidates straight away. Schemes the page has seen are offered in every scheme field. The first administrator is found by name and birth date (group administrators may now search the customer master: the `group-admin` template gains `customers.find` and the Personal class in the test and persona casts).

## Unit pricing

**Found.** A price error needed the wrong prices' references typed, comma separated. Dates started empty.

**Changed.** Each price shown has "It is wrong", which adds it to the error; the field picks from prices shown. Dates start today, and the lookup starts 30 days back. Empty lists say so.

## Commission

**Found.** The console opened on a question list with no sense of what was waiting. Dates and parties were typed.

**Changed.** Open tasks show on arrival, soonest due first. Date fields start today. Party, payee and relation fields offer the parties the module has named.

## Reinsurance exceptions and replacement matches

Both were already queue driven: every exception and match is worked from its row. The month-end check now starts at last month's end.

## Operations console

A read-only view that refreshes itself. Nothing to type; nothing changed.

## Adviser portal

**Found.** Requests named the policy by its internal reference and the change as a code.

**Changed.** Requests show the policy number and the change in words, and say so when there are none.

## Employer portal

**Found.** An employer with one scheme had to press Open. A colleague's change was approved by typing its reference. The changes needing a second administrator were typed as codes.

**Changed.** The only scheme opens on arrival. A change waiting for a second administrator has Approve on its row. The changes needing a second administrator are ticked. The effective date starts today.

## Still typed, and why

| Field | Why it is still typed | What would fix it |
| --- | --- | --- |
| Document references (claims evidence, journal and return evidence) | Staff have no upload route to the documents vault; documents arrive from other channels. | A staff upload to the vault, returning the reference into the form. |
| Journal account codes | The chart of accounts is not served to the console. | A `chart` question on the finance console, then a picker. |
| Payments: a party, a payee or a refund by reference | Payments roles hold the Financial class only, so they cannot search people; the payments module has no list of refunds or payees. | Either give payments makers `customers.find` with the Personal class (an access decision), or a refunds and payees list on the payments module. |
| Group: a scheme's policy number, an employee ID | These are the employer's own identifiers, on the file and the call. | Nothing: people do know them. |
| Employer portal: a new administrator | The customer realm cannot search the customer master. | An invitation by email that the colleague accepts once signed in. |
| Adviser portal: a statement number | The commission module serves one statement at a time. | A list of statements for the payee. |
| Commission: a new party's reference | A new adviser's reference comes from the register they are added from. | Nothing. |
| Raw command boxes (payments, group, commission) | For the rare command with no form; labelled as such. | A form for each as it becomes common. |
