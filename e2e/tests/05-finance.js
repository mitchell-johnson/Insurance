// The finance console: the month-end close and its checks, journals, the
// books, aged items and set-up, each open to the role that holds it.
//
// The finance ledger is fed by the core's end-of-day close, which runs as a
// workflow. Where the suite runs without the workflow service no business day
// reaches the ledger, so these tests cover what a finance persona meets on
// the console before the first close: the checks and why each is red, who
// may do what, and that nothing can be signed, journalled or closed early.
"use strict";
const { test } = require("../lib/suite");
const { cast } = require("../lib/cast");

const A = "Finance";
const PERIOD = (t) => t.stack.opening.slice(0, 7);
async function close(t, who) {
  const s = await t.open("/finance", who);
  await s.fill("#p-id", PERIOD(t));
  await s.press("Show", "#period");
  await s.sees(PERIOD(t), "#dash-title");
  return s;
}
const steps = async (s) => Object.fromEntries((await s.rows("#dash-steps")).map((r) => [r[0], { name: r[1], owner: r[2], due: r[3], blocking: r[4], status: r[5], detail: r[6] }]));

test({ id: "E2E-144", area: A, persona: "ST10", story: "US-ST10-1", title: "The finance operator sees their role and the state of the books on arrival", matrix: ["LED-001"],
  purpose: "Sign in to the finance console as the finance operator and read the role chip and the status line.",
  why: "Before anything else the operator needs to know the chart and close configuration in force and how far the books have been fed." }, async (t) => {
  const s = await t.open("/finance", cast.st10);
  await s.sees("finance operator", "#roles");
  await s.sees("Chart version 1, close configuration 1", "#status");
  await s.sees("No day is waiting for a root.", "#work");
  await s.never("A feed has stalled", "#status");
});

test({ id: "E2E-145", area: A, persona: "ST11", story: "US-ST11-1", title: "A period's close lists every check with its owner, due date and why it is red", matrix: ["FIN-001"],
  purpose: "As the finance lead, show the close for the current period and read the checks.",
  why: "The close is run from this list; each red check must say in words what is missing, or the lead cannot chase it." }, async (t) => {
  const s = await close(t, cast.st11);
  const checks = await steps(s);
  for (const id of ["control_check", "commission_sweep", "unit_pricing", "fund_cash_settlement", "subledger_reconciliations", "valuation_extracts", "tax_reconciliation", "analytical_review", "trial_balance", "accounting_groups"]) if (!checks[id]) throw new Error(`the close has no ${id} check`);
  if (checks.control_check.status !== "red" || checks.control_check.blocking !== "yes" || !checks.control_check.detail.includes("no business day has been received from the core")) throw new Error(`the control check reads ${JSON.stringify(checks.control_check)}`);
  if (checks.trial_balance.status !== "green" || checks.unapproved_journals.status !== "green") throw new Error("an empty ledger does not balance");
  if (checks.reinsurance_sweep.status !== "not due") throw new Error(`the reinsurance sweep reads ${JSON.stringify(checks.reinsurance_sweep)}`);
  for (const [id, c] of Object.entries(checks)) if (!c.owner || !/^\d{4}-\d{2}-\d{2}/.test(c.due)) throw new Error(`check ${id} has no owner or due date`);
});

test({ id: "E2E-146", area: A, persona: "ST11", story: "US-ST11-1", title: "The close says plainly what is stopping it and which sign-offs are still needed", matrix: ["FIN-002"],
  purpose: "Read the summary and sign-off section of the period's close.",
  why: "The lead needs the list of blockers in one sentence and to know whose signature is outstanding: only theirs in a month that is not a quarter end." }, async (t) => {
  const s = await close(t, cast.st11);
  await s.sees("Stopping the close:", "#dash-summary");
  await s.never("Nothing is stopping the close.", "#dash-summary");
  // November is not a quarter end, so only the finance lead signs; the actuary signs quarter ends.
  await s.sees("Still needed: finance lead.", "#dash-signed");
  await s.never("appointed actuary", "#dash-signed");
});

test({ id: "E2E-147", area: A, persona: "ST11", story: "US-ST11-1", title: "The finance lead cannot sign off a period that has not started", matrix: ["FIN-003"],
  purpose: "Press Sign off as finance lead on a period no business day has reached.",
  why: "A sign-off is a statement that the books for the period are right; it cannot be given before there are books." }, async (t) => {
  const s = await close(t, cast.st11);
  await s.press("Sign off as finance lead");
  await s.stopped("#close-msg", `${PERIOD(t)} has not started`, 409);
  // November is not a quarter end, so only the finance lead signs; the actuary signs quarter ends.
  await s.sees("Still needed: finance lead.", "#dash-signed");
  await s.never("appointed actuary", "#dash-signed");
});

test({ id: "E2E-148", area: A, persona: "ST11", story: "US-ST11-1", title: "A period with red blocking checks cannot be closed", matrix: ["FIN-003"],
  purpose: "Press Close the period while blocking checks are red.",
  why: "This is the control the finance lead's story is about: the close means something only if it is refused while a blocking check is red." }, async (t) => {
  const s = await close(t, cast.st11);
  await s.press("Close the period");
  await s.stopped("#close-msg", "", 409);
  await s.sees("Stopping the close:", "#dash-summary");
});

test({ id: "E2E-149", area: A, persona: "ST10", story: "US-ST17-1", title: "The operator is not offered the lead's or the actuary's buttons",
  purpose: "As the finance operator, look at the sign-off and close buttons; as the finance lead and the actuary, look again.",
  why: "Each signature belongs to one office; the console shows a person only the buttons their role can press." }, async (t) => {
  const off = async (s, id) => s.page.locator(id).isDisabled();
  const operator = await close(t, cast.st10);
  for (const id of ["#sign-lead", "#sign-actuary", "#close-period"]) if (!(await off(operator, id))) throw new Error(`${id} is offered to the finance operator`);
  const lead = await close(t, cast.st11);
  if ((await off(lead, "#sign-lead")) || (await off(lead, "#close-period")) || !(await off(lead, "#sign-actuary"))) throw new Error("the finance lead's buttons are wrong");
  const actuary = await close(t, cast.st12);
  if ((await off(actuary, "#sign-actuary")) || !(await off(actuary, "#sign-lead")) || !(await off(actuary, "#close-period"))) throw new Error("the actuary's buttons are wrong");
  await actuary.sees("appointed actuary", "#roles");
});

test({ id: "E2E-150", area: A, persona: "ST12", story: "US-ST12-1", title: "The appointed actuary cannot sign off a period early",
  purpose: "As the appointed actuary, press Sign off as appointed actuary on a period that has not started.",
  why: "The actuary's signature covers the valuation for the period; it is refused until the period's books exist." }, async (t) => {
  const s = await close(t, cast.st12);
  await s.press("Sign off as appointed actuary");
  await s.stopped("#close-msg", `${PERIOD(t)} has not started`, 409);
});

test({ id: "E2E-151", area: A, persona: "ST11", story: "US-ST11-1", title: "A waiver needs its reason, owner and remediation date before it is sent", matrix: ["FIN-004"],
  purpose: "Try to waive a check with nothing filled in, then with everything filled in on a period that has not started.",
  why: "A waiver is the only way past a red check; it must carry who will fix the cause and by when, and cannot be granted for a period with no books." }, async (t) => {
  const s = await close(t, cast.st11);
  await s.press("Waive", "#waive");
  await s.wait(300);
  for (const id of ["#w-check", "#w-reason", "#w-owner", "#w-due"]) if (!(await s.page.locator(id).evaluate((el) => !el.validity.valid))) throw new Error(`${id} is not marked as needed`);
  await s.fill("#w-check", "analytical_review"); await s.fill("#w-reason", "reviewer on leave"); await s.fill("#w-owner", "st10"); await s.fill("#w-due", "2027-12-10");
  await s.press("Waive", "#waive");
  await s.stopped("#close-msg", `${PERIOD(t)} has not started`, 409);
  await s.sees("Nothing to show.", "#dash-waivers");
});

test({ id: "E2E-152", area: A, persona: "ST10", story: "US-ST10-1", title: "Reporting a check as wrong requires saying what is wrong",
  purpose: "Report the tax reconciliation as red with nothing written in What is wrong.",
  why: "A red check with no detail leaves the lead with a blocker nobody can act on." }, async (t) => {
  const s = await close(t, cast.st10);
  await s.pick("#r-check", "tax_reconciliation"); await s.pick("#r-result", "red");
  await s.press("Report", "#report");
  await s.stopped("#close-msg", "Say what is wrong.");
});

test({ id: "E2E-153", area: A, persona: "ST10", story: "US-ST10-1", title: "A journal is not accepted for a period that has not started", matrix: ["LED-006"],
  purpose: "Propose a balanced accrual journal for the current period before any business day has reached the ledger.",
  why: "A journal dated into a period the ledger has not opened would sit outside the control check; the refusal must name the period." }, async (t) => {
  const s = await t.open("/finance", cast.st10);
  await s.tab("Journals");
  const id = t.ref("JNL");
  await s.fill("#j-id", id); await s.pick("#j-kind", "accrual"); await s.fill("#j-period", PERIOD(t)); await s.fill("#j-date", t.stack.opening); await s.fill("#j-reason", "audit fee accrual"); await s.fill("#j-evidence", "doc-accrual-1");
  await s.fill("#j-lines", "6500, 250.00, SF1-life\n2500, -250.00, SF1-life");
  await s.press("Propose", "#journal");
  await s.stopped("#journal-msg", `${PERIOD(t)} has not started`, 409);
  await s.fill("#jl-id", id);
  await s.press("Show", "#jlookup");
  await s.stopped("#jl-msg", `there is no journal ${id}`, 400);
});

test({ id: "E2E-154", area: A, persona: "ST10", story: "US-ST10-1", title: "A journal line with a mistyped amount is caught before it is sent",
  purpose: "Propose a journal whose first line has an amount with a currency sign and three decimals.",
  why: "Amounts typed in the wrong shape are the commonest journal error; catching them on the page stops a wrong posting reaching an approver." }, async (t) => {
  const s = await t.open("/finance", cast.st10);
  await s.tab("Journals");
  await s.fill("#j-id", t.ref("JNL")); await s.fill("#j-period", PERIOD(t)); await s.fill("#j-date", t.stack.opening); await s.fill("#j-reason", "accrual"); await s.fill("#j-evidence", "doc-1");
  await s.fill("#j-lines", "6500, $250.005, SF1-life\n2500, -250.00, SF1-life");
  await s.press("Propose", "#journal");
  await s.stopped("#journal-msg", "\"$250.005\" is not an amount: use 250.00 or -250.00.");
});

test({ id: "E2E-155", area: A, persona: "Auditor", story: "US-ST12-2", title: "The internal auditor reads the finance console and can change nothing", matrix: ["AUD-006"],
  purpose: "Sign in as the read-only auditor, read the close, and try to propose a journal.",
  why: "Audit needs to see everything finance sees; a read-only role that could post would not be read-only." }, async (t) => {
  const s = await close(t, cast.auditor);
  await s.sees("View only.", "#roles");
  await s.sees("control_check", "#dash-steps");
  for (const id of ["#sign-lead", "#sign-actuary", "#close-period"]) if (!(await s.page.locator(id).isDisabled())) throw new Error(`${id} is offered to the auditor`);
  await s.tab("Journals");
  await s.fill("#j-id", t.ref("JNL")); await s.fill("#j-period", PERIOD(t)); await s.fill("#j-date", t.stack.opening); await s.fill("#j-reason", "x"); await s.fill("#j-evidence", "doc-1");
  await s.fill("#j-lines", "6500, 1.00, SF1-life\n2500, -1.00, SF1-life");
  await s.press("Propose", "#journal");
  await s.stopped("#journal-msg", "you may read the finance console but hold no role to act in it", 403);
});

test({ id: "E2E-156", area: A, persona: "ST10", story: "US-ST12-2", title: "The trial balance of an empty ledger is empty, not an error", matrix: ["LED-003"],
  purpose: "Show the trial balance, movements and sources for the current period before anything has posted.",
  why: "The first day of a new insurer is an empty ledger; the books must say so calmly rather than fail." }, async (t) => {
  const s = await t.open("/finance", cast.st10);
  await s.tab("Books");
  for (const what of ["trial_balance", "movements", "sources", "backdated"]) {
    await s.fill("#tb-period", PERIOD(t)); await s.pick("#tb-what", what);
    await s.press("Show", "#tb");
    await s.sees("Nothing to show.", "#tb-out");
  }
  if (await s.page.locator("#tb-msg .banner.bad").count()) throw new Error("the books answered with an error");
});

test({ id: "E2E-157", area: A, persona: "ST12", story: "US-ST12-2", title: "Every policy is placed in an accounting group, with none left over", matrix: ["LED-010"],
  purpose: "Issue a term life policy, classify policies, and read the accounting groups.",
  why: "Insurance contract accounting is done by group; a policy left ungrouped is a policy missing from the liability." }, async (t) => {
  await t.policy("term_life", "cust-p18");
  const s = await t.open("/finance", cast.st10);
  await s.tab("Books");
  // Other journeys issue policies while this one runs, and a policy issued after a classification waits for the next:
  // the classification is asked for again until one of them leaves nothing over.
  let text = "";
  for (let n = 0; n < 6; n++) {
    await s.press("Classify policies");
    await s.done("#ag-msg", "Classified.");
    await s.sees("individual-life/FY2028/other", "#ag-out");
    text = await s.text("#ag-out");
    if (/ungrouped/i.test(text) && /(^|\s)0\s*$/.test(text.trim())) return;
    await s.wait(500);
  }
  throw new Error(`after six classifications the groups read "${text.slice(0, 200)}"`);
});

test({ id: "E2E-158", area: A, persona: "ST12", story: "US-ST12-2", title: "The actuary can read the accounting groups but cannot reclassify policies",
  purpose: "As the appointed actuary, open Books and look at the Classify policies button.",
  why: "Classification changes which group a contract's results fall in; it is done by finance operations, and reviewed, not done, by the actuary." }, async (t) => {
  const s = await t.open("/finance", cast.st12);
  await s.tab("Books");
  await s.sees("policies", "#ag-out");
  if (!(await s.page.locator("#ag-classify").isDisabled())) throw new Error("Classify policies is offered to the actuary");
});

test({ id: "E2E-159", area: A, persona: "ST10", story: "US-ST10-1", title: "The conduct figures for the period count the day's new business",
  purpose: "Show the conduct figures for the current period.",
  why: "Accepted and declined counts, overturns and complaints go to the board each month from this view." }, async (t) => {
  await t.policy("term_life", "cust-p18");
  const s = await t.open("/finance", cast.st10);
  await s.tab("Books");
  await s.fill("#tb-period", PERIOD(t)); await s.pick("#tb-what", "conduct");
  await s.press("Show", "#tb");
  await s.sees("accepted", "#tb-out");
  const [row] = await s.rows("#tb-out");
  if (!(Number(row[0]) >= 1)) throw new Error(`accepted reads "${row[0]}" after a policy was issued`);
});

test({ id: "E2E-160", area: A, persona: "ST10", story: "US-ST10-1", title: "Aged suspense as at today is shown with its total", matrix: ["FIN-010"],
  purpose: "Show aged suspense as at the business date.",
  why: "Unidentified money ages into a regulatory problem; the operator checks this list daily and it must total." }, async (t) => {
  const s = await t.open("/finance", cast.st10);
  await s.tab("Aged items and set-up");
  await s.fill("#ag-date", t.stack.opening);
  await s.press("Show", "#aged");
  await s.sees("As at", "#aged-out");
  await s.sees(t.stack.opening, "#aged-out");
  await s.sees("Total", "#aged-out");
});

test({ id: "E2E-161", area: A, persona: "ST10", story: "US-ST10-1", title: "An aged item can only be documented if the suspense report lists it",
  purpose: "Record what is being done about an aged item for a party the latest report does not list.",
  why: "Notes against items that do not exist would make the aged report look managed when it is not." }, async (t) => {
  const s = await t.open("/finance", cast.st10);
  await s.tab("Aged items and set-up");
  await s.fill("#d-kind", "suspense"); await s.fill("#d-party", "cust-nobody"); await s.fill("#d-since", "2027-09-01"); await s.fill("#d-action", "calling the payer"); await s.fill("#d-owner", "st10");
  await s.press("Record", "#doc");
  await s.stopped("#other-msg", "the latest suspense report lists nothing held for cust-nobody since 2027-09-01", 409);
});

test({ id: "E2E-162", area: A, persona: "ST10", story: "US-ST17-1", title: "Only the finance lead may record a migration cut-over total", matrix: ["MIG-004"],
  purpose: "As the finance operator, look at the cut-over Record button; as the lead, record a total against an account that is not a migration control account.",
  why: "Cut-over totals are what a migration is reconciled to; they are entered by the lead, and only against the accounts the close configuration names." }, async (t) => {
  const operator = await t.open("/finance", cast.st10);
  await operator.tab("Aged items and set-up");
  if (!(await operator.page.locator("#co-send").isDisabled())) throw new Error("the cut-over Record button is offered to the operator");
  const s = await t.open("/finance", cast.st11);
  await s.tab("Aged items and set-up");
  await s.fill("#co-account", "1000"); await s.fill("#co-total", "100"); await s.fill("#co-evidence", "old-system-tb");
  await s.press("Record", "#co-form");
  await s.stopped("#co-msg", "1000 is not a migration control account in the close configuration", 409);
});

test({ id: "E2E-163", area: A, persona: "ST11", story: "US-ST15-1", title: "A configuration that was never proposed cannot be approved",
  purpose: "As the finance lead, approve a configuration reference that nothing was proposed under.",
  why: "Chart and close configuration change by propose-then-approve; an approval must point at a real proposal made by someone else." }, async (t) => {
  const s = await t.open("/finance", cast.st11);
  await s.tab("Aged items and set-up");
  await s.fill("#c-id", "chart-never-proposed");
  await s.press("Approve", "#config");
  await s.stopped("#other-msg", "nothing is proposed under chart-never-proposed", 409);
});

test({ id: "E2E-164", area: A, persona: "ST10", story: "US-ST10-1", title: "A return that was never filed is reported as not filed",
  purpose: "Look up a regulatory return by a reference that has not been filed.",
  why: "Asked by an auditor whether a return went in, the operator needs a plain no, not a blank page." }, async (t) => {
  const s = await t.open("/finance", cast.st10);
  await s.tab("Books");
  await s.fill("#ret-id", "RET-2027-Q3");
  await s.press("Show", "#ret");
  await s.stopped("#tb-msg", "return RET-2027-Q3 has not been filed", 400);
});

test({ id: "E2E-165", area: A, persona: "ST5", story: "US-ST13-1", title: "A collections officer is refused the finance console", matrix: [],
  purpose: "Sign in to the finance console as the collections officer.",
  why: "The ledger and the close are finance's; other staff see a refusal and no figures." }, async (t) => {
  const s = await t.open("/finance", cast.st5, 403);
  await s.refused("your roles do not include finance");
  await s.never("Work waiting");
});
