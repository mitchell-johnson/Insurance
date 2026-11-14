// The payments console: runs from preparation to the bank and back, the
// accounts that may be paid, reconciliation and the controls around them.
//
// The pump that carries a submission to a bank is not part of this stack, and
// neither are the sanctions screening and customer master services. Where a
// journey needs what one of them reports (a payee screened, a file settled, a
// statement read) the test reports it to the payments module exactly as that
// service would; every decision a person makes goes through the screen.
"use strict";
const { test } = require("../lib/suite");
const { cast } = require("../lib/cast");

const A = "Payments";
const COMMANDS = "/staff/v1/payments/commands";
const tomorrow = (t) => require("../lib/suite").Journey.before(t.stack.opening, -1);

/// What a service reports to the payments module (the bank driver, screening, the customer master).
async function reports(t, command, roles = ["service"]) {
  const r = await t.sys.tell("payments", { key: t.ref("svc"), actor: { id: "bank-driver", roles }, at: `${t.stack.opening}T10:00:00`, command });
  const inner = r.result && r.result.Ok;
  if (!inner || inner.Err) throw new Error(`payments refused ${command.type}: ${JSON.stringify(r).slice(0, 300)}`);
  return inner.Ok;
}
const sends = (t, who, command) => t.does(who, "POST", COMMANDS, command, command.type);

/// An account that may be paid: entered by one maintainer, confirmed by another, screened clear.
async function payee(t, { kind = "customer", confirm = true, screen = true, name_check = "match" } = {}) {
  const id = t.ref("pay").toLowerCase(), party = t.ref("cust").toLowerCase();
  // Each payee has its own account number: the duplicate check looks at the account and the amount, across runs.
  const number = `01-0001-${String(Date.now() % 10_000_000).padStart(7, "0")}-${String(Math.floor(Math.random() * 100)).padStart(2, "0")}`;
  await sends(t, cast.payee, { type: "register_payee", payee: id, party, kind, account: { name: "Account Holder", number }, name_check });
  if (confirm) await sends(t, cast.payee2, { type: "confirm_payee", payee: id });
  if (confirm && screen) await reports(t, { type: "screen_payee", payee: id, clear: true });
  return { id, party, number };
}
const item = (t, to, amount_minor) => ({ item: t.ref("it").toLowerCase(), source: `invoice:${t.ref("src")}`, counterparty: { payee: to }, amount_minor });

/// A run the maker has prepared, filled and (unless told otherwise) checked.
async function run(t, items, { type = "lump_sum_claims", check = true } = {}) {
  const id = t.ref("run").toLowerCase();
  await sends(t, cast.st10, { type: "prepare_run", run: id, run_type: type, fund: "sf1", currency: "NZD", value_date: tomorrow(t), urgent: false });
  if (items.length) await sends(t, cast.st10, { type: "add_run_items", run: id, items });
  if (check) await sends(t, cast.st10, { type: "check_run", run: id });
  return id;
}
async function opened(t, who, id) {
  const s = await t.open("/payments", who);
  await s.fill("#run-id", id);
  await s.press("Open", "#run-form");
  await s.sees("value date", "#run");
  return s;
}
/// The control totals of the run on the screen, by column.
const totals = (s) => s.page.evaluate(() => {
  const table = [...document.querySelectorAll("#run table")].pop();
  const heads = [...table.querySelectorAll("th")].map((e) => e.textContent.trim());
  const cells = [...table.querySelectorAll("tbody td")].map((e) => e.textContent.trim());
  return Object.fromEntries(heads.map((k, i) => [k, cells[i]]));
});
/// Send a command through the console's own "Another command" form.
async function another(s, command) {
  await s.tab("Controls");
  await s.fill("#raw-cmd", JSON.stringify(command));
  await s.press("Send", "#raw-form");
}
/// The bank takes a submission through to settlement.
async function settled(t, id, { rejected = {} } = {}) {
  const submission = `${id}-1`;
  await reports(t, { type: "begin_submission", submission, region: "akl" });
  await reports(t, { type: "record_submitted", submission, content_hash: "e2e", bank_reference: `B-${id}` });
  await reports(t, { type: "record_acknowledgement", submission, rejected });
  await reports(t, { type: "record_settlement", submission, settled_on: tomorrow(t) });
}

// --- Who sees what -----------------------------------------------------------------------------------------------

test({ id: "E2E-166", area: A, persona: "ST10", story: "US-ST10-1", title: "The maker arrives to their role, the payments waiting and the form to prepare a run", matrix: ["PAY-001"],
  purpose: "Sign in to the payments console as the finance operator who prepares runs.",
  why: "The maker's whole job starts from this page; it must say who they are to the module and offer the maker's forms and nothing an approver does." }, async (t) => {
  const s = await t.open("/payments", cast.st10);
  await s.sees("You are st10: maker.", "#me");
  await s.sees("Prepare a run");
  if (!(await s.page.locator("#prep").isVisible())) throw new Error("the maker has no form to prepare a run");
  await s.sees("Open runs");
});

test({ id: "E2E-167", area: A, persona: "ST17", story: "US-ST17-1", title: "The run approver is offered no way to prepare a run", matrix: ["STF-007"],
  purpose: "Sign in as the treasury approver and look for the maker's forms.",
  why: "An approver who can also prepare is one person paying money out alone; the page must not even offer it." }, async (t) => {
  const s = await t.open("/payments", cast.st17);
  await s.sees("You are st17: approver, treasury.", "#me");
  if (await s.page.locator("#prep").isVisible()) throw new Error("the approver is offered the form to prepare a run");
  if (await s.page.locator("#take").isVisible()) throw new Error("the approver is offered the form to take waiting payments");
});

test({ id: "E2E-168", area: A, persona: "Auditor", story: "US-ST17-1", title: "The internal auditor can look at runs and is offered nothing that changes one", matrix: ["STF-007"],
  purpose: "Sign in as the read-only auditor.",
  why: "Audit needs to see every run without being a person who could alter one." }, async (t) => {
  const s = await t.open("/payments", cast.auditor);
  await s.sees("view only", "#me");
  await s.sees("Open runs");
  if (await s.page.locator("#prep").isVisible()) throw new Error("the auditor is offered the form to prepare a run");
  await s.tab("Controls");
  if (await s.page.locator("#raw").isVisible()) throw new Error("the auditor is offered the form that sends commands");
});

test({ id: "E2E-169", area: A, persona: "Anyone", story: "US-ST17-1", title: "A sign-in the insurer does not know is turned away from payments", matrix: ["STF-007"],
  purpose: "Open the payments console with a sign-in that belongs to no member of staff.",
  why: "The payments console lists who is owed money; nothing on it may show to someone who is not staff." }, async (t) => {
  const s = await t.open("/payments", cast.stranger, 403);
  await s.refused("not registered");
  await s.never("Open runs");
});

test({ id: "E2E-170", area: A, persona: "ST5", story: "US-ST05-1", title: "A service officer with no payments role is refused the payments console", matrix: ["STF-003"],
  purpose: "Open the payments console as a service officer whose roles do not include payments.",
  why: "Staff see financial detail only where their role needs it; servicing a policy does not need the bank runs." }, async (t) => {
  const s = await t.open("/payments", cast.st5b, 403);
  await s.refused("do not include payments");
});

test({ id: "E2E-171", area: A, persona: "ST5", story: "US-ST05-1", title: "The collections officer sees payments as collections and is not offered a maker's forms", matrix: ["COL-019"],
  purpose: "Sign in to payments as the collections officer.",
  why: "Collections works dishonours and authorities, not disbursement runs; the page should fit the role." }, async (t) => {
  const s = await t.open("/payments", cast.st5);
  await s.sees("You are st5: collections.", "#me");
  if (await s.page.locator("#prep").isVisible()) throw new Error("the collections officer is offered the form to prepare a run");
  await s.tab("Controls");
  await s.sees("Collections held for a conversation");
});

// --- A run from preparation to the bank --------------------------------------------------------------------------

test({ id: "E2E-172", area: A, persona: "ST10", story: "US-ST10-1", title: "The maker prepares a run and it joins the open runs under their name", matrix: ["PAY-001"],
  purpose: "Fill in the Prepare a run form and read the open runs.",
  why: "A run starts with who prepared it, from which fund and for which date; that record is what the approver later relies on." }, async (t) => {
  const s = await t.open("/payments", cast.st10);
  const id = t.ref("run").toLowerCase();
  await s.fill("#r-id", id);
  await s.pick("#r-type", "benefit_payments");
  await s.fill("#r-date", tomorrow(t));
  await s.press("Prepare", "#prep");
  await s.done("#r-msg", "Prepared. Add items, then check it.");
  await t.open("/payments", cast.st10).then(async (again) => {
    await again.sees(id, "#runs");
    const cells = await again.row("#runs", id).locator("td").allInnerTexts();
    if (!cells.includes("benefit payments") || !cells.includes("prepared") || !cells.includes("st10")) throw new Error(`the open runs show ${JSON.stringify(cells)}`);
  });
});

test({ id: "E2E-173", area: A, persona: "ST10", story: "US-ST10-1", title: "A run with nothing in it cannot be checked", matrix: ["PAY-001"],
  purpose: "Open a run that has no items and press Check.",
  why: "Checking is the maker's statement that the items are right; an empty run has nothing to state." }, async (t) => {
  const id = await run(t, [], { check: false });
  const s = await opened(t, cast.st10, id);
  await s.press("Check", "#run-actions");
  await s.stopped("#run-msg", "there is nothing in the run", 409);
});

test({ id: "E2E-174", area: A, persona: "ST10", story: "US-ST10-1", title: "The maker adds an item, checks the run and sees the control total", matrix: ["PAY-001"],
  purpose: "Add an item to a prepared run, check it, and read the run and its control totals.",
  why: "The count and control total at check are what every later stage is reconciled to." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [], { check: false });
  const s = await opened(t, cast.st10, id);
  await another(s, { type: "add_run_items", run: id, items: [item(t, to.id, 1234_56)] });
  await s.done("#raw-msg", "Done.");
  await s.tab("Runs");
  await s.press("Check", "#run-actions");
  await s.done("#run-msg", "Done: check run.");
  await s.sees(": checked. Prepared by st10.", "#run");
  await s.sees("pending", "#run");
  const sums = await totals(s);
  if (sums.Unsent !== "1234.56" || sums.Settled !== "0.00") throw new Error(`the control totals read ${JSON.stringify(sums)}`);
});

test({ id: "E2E-175", area: A, persona: "ST17", story: "US-ST17-1", title: "The maker is offered no Approve and the approver no Check", matrix: ["STF-007"],
  purpose: "Open the same checked run as its maker and as the approver and compare the buttons.",
  why: "Segregation starts on the screen: each person is shown only their own half of the control." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 50_00)]);
  const maker = await opened(t, cast.st10, id);
  await maker.sees("Check", "#run-actions");
  await maker.never("Approve", "#run-actions");
  const approver = await opened(t, cast.st17, id);
  await approver.sees("Approve", "#run-actions");
  await approver.never("Check", "#run-actions");
});

test({ id: "E2E-176", area: A, persona: "ST10", story: "US-ST17-1", title: "A maker who sends an approval by hand is refused", matrix: ["STF-007"],
  purpose: "As the maker, send the approve command for their own checked run through the command form.",
  why: "Hiding the button is not the control; the module must refuse the preparer whatever they send." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 60_00)]);
  const s = await opened(t, cast.st10, id);
  await another(s, { type: "approve_run", run: id });
  await s.stopped("#raw-msg", "approver", 403);
});

test({ id: "E2E-177", area: A, persona: "ST17", story: "US-ST17-1", title: "A second person approves a checked run and it goes to the bank", matrix: ["PAY-001"],
  purpose: "As the approver, open a checked run and press Approve.",
  why: "Approval by someone other than the preparer is the control that releases money; the run must then show who approved and what is at the bank." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 2500_00)]);
  const s = await opened(t, cast.st17, id);
  await s.press("Approve", "#run-actions");
  await s.done("#run-msg", "Done: approve run.");
  await s.sees(": approved. Prepared by st10; approved by st17.", "#run");
  const sums = await totals(s);
  if (sums.Unsent !== "0.00" || sums["At the bank"] !== "2500.00") throw new Error(`the control totals read ${JSON.stringify(sums)}`);
});

test({ id: "E2E-178", area: A, persona: "ST17", story: "US-ST17-1", title: "A run cannot be approved before its maker has checked it", matrix: ["PAY-001"],
  purpose: "As the approver, press Approve on a run that is only prepared.",
  why: "An approver signs what the maker has stated is complete; an unchecked run may still be changing." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 70_00)], { check: false });
  const s = await opened(t, cast.st17, id);
  await s.press("Approve", "#run-actions");
  await s.stopped("#run-msg", "a run is approved after it has been checked", 409);
});

test({ id: "E2E-179", area: A, persona: "ST17", story: "US-ST17-1", title: "An approver who also maintains payees is refused the approval", matrix: ["STF-007"],
  purpose: "As a member of staff holding both the approver and payee maintenance roles, press Approve on a checked run.",
  why: "Someone who can set up an account and release money to it can pay themselves; the roles must not combine." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 80_00)]);
  const s = await opened(t, cast.paybad, id);
  await s.press("Approve", "#run-actions");
  await s.stopped("#run-msg", "an approver does not maintain payees", 403);
});

test({ id: "E2E-180", area: A, persona: "ST17", story: "US-ST17-1", title: "An ex gratia run needs two different approvers", matrix: ["PAY-008"],
  purpose: "Approve an ex gratia run once, try to approve it again as the same person, then as a second approver.",
  why: "Payments outside the contract carry a second signature; one person pressing twice must not count as two." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 500_00)], { type: "ex_gratia" });
  const first = await opened(t, cast.st17, id);
  await first.press("Approve", "#run-actions");
  await first.done("#run-msg", "Done: approve run.");
  await first.sees(": checked.", "#run");
  await first.press("Approve", "#run-actions");
  await first.stopped("#run-msg", "each approval comes from a different person", 409);
  const second = await opened(t, cast.st17b, id);
  await second.press("Approve", "#run-actions");
  await second.done("#run-msg", "Done: approve run.");
  await second.sees(": approved.", "#run");
  await second.sees("approved by st17, st17b", "#run");
});

test({ id: "E2E-181", area: A, persona: "ST10", story: "US-ST10-1", title: "The maker cancels a checked run and it leaves the open runs", matrix: ["PAY-001"],
  purpose: "Open a checked run as the maker, press Cancel and read the open runs.",
  why: "A run prepared in error must be withdrawn cleanly so its items can be paid in another." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 90_00)]);
  const s = await opened(t, cast.st10, id);
  await s.press("Cancel", "#run-actions");
  await s.done("#run-msg", "Done: cancel run.");
  await s.sees(": cancelled.", "#run");
  await s.never(id, "#runs");
});

test({ id: "E2E-182", area: A, persona: "ST10", story: "US-ST10-1", title: "A run cannot be closed before it has been approved and paid", matrix: ["PAY-001"],
  purpose: "Press Close on a run that is only checked.",
  why: "Closing says the bank has done its part; a run closed early would hide money that never left." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 95_00)]);
  const s = await opened(t, cast.st10, id);
  await s.press("Close", "#run-actions");
  await s.stopped("#run-msg", "a run closes after it has been approved and paid", 409);
});

test({ id: "E2E-183", area: A, persona: "ST10", story: "US-ST10-1", title: "Once the bank settles a run its totals say so and the maker closes it", matrix: ["PAY-001"],
  purpose: "Have a run approved, let the bank acknowledge and settle it, then read the totals and press Close.",
  why: "Submitted equals acknowledged equals settled is the story's own test of a run that reconciles itself." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 3100_00)]);
  await sends(t, cast.st17, { type: "approve_run", run: id });
  await settled(t, id);
  const s = await opened(t, cast.st10, id);
  const sums = await totals(s);
  if (sums.Settled !== "3100.00" || sums.Unsent !== "0.00" || sums["At the bank"] !== "0.00" || sums.Rejected !== "0.00") throw new Error(`the control totals read ${JSON.stringify(sums)}`);
  await s.sees("settled", "#run");
  await s.press("Close", "#run-actions");
  await s.done("#run-msg", "Done: close run.");
  await s.sees(": closed.", "#run");
});

test({ id: "E2E-184", area: A, persona: "ST10", story: "US-ST10-1", title: "An item the bank rejects shows as rejected and the rest of the run settles", matrix: ["PAY-003"],
  purpose: "Let the bank reject one of two items in a run and settle the other, then read the run.",
  why: "Accepted items must proceed and rejected ones must be visible at once, or a payee waits while nobody knows." }, async (t) => {
  const to = await payee(t);
  const [good, bad] = [item(t, to.id, 410_00), item(t, to.id, 620_00)];
  const id = await run(t, [good, bad]);
  await sends(t, cast.st17, { type: "approve_run", run: id });
  await settled(t, id, { rejected: { [bad.item]: "account closed" } });
  const s = await opened(t, cast.st10, id);
  const sums = await totals(s);
  if (sums.Settled !== "410.00" || sums.Rejected !== "620.00") throw new Error(`the control totals read ${JSON.stringify(sums)}`);
  const cells = await s.row("#run", bad.item).locator("td").allInnerTexts();
  if (!cells.join(" ").includes("rejected")) throw new Error(`the rejected item reads ${JSON.stringify(cells)}`);
});

test({ id: "E2E-185", area: A, persona: "ST10", story: "US-ST10-1", title: "A payment the bank sends back after settlement shows as returned", matrix: ["PAY-003"],
  purpose: "Settle a run, have the bank return its item the next day, and read the run.",
  why: "A returned payment is money the payee never received; it must come back onto the run's totals, not vanish as paid." }, async (t) => {
  const to = await payee(t);
  const one = item(t, to.id, 730_00);
  const id = await run(t, [one]);
  await sends(t, cast.st17, { type: "approve_run", run: id });
  await settled(t, id);
  await reports(t, { type: "record_return", item: one.item, reason: "account_closed", returned_on: tomorrow(t) });
  const s = await opened(t, cast.st10, id);
  const sums = await totals(s);
  if (sums.Returned !== "730.00") throw new Error(`the control totals read ${JSON.stringify(sums)}`);
  await s.sees("returned", "#run");
});

test({ id: "E2E-186", area: A, persona: "ST10", story: "US-ST10-1", title: "The maker sends a run the bank refused whole to the bank again, on a value date they give", matrix: ["INC-007"],
  purpose: "After the bank refuses a submitted file, press Send what is unpaid again on the run and give the new value date.",
  why: "The runbook for a refused file is a new submission of what was not paid, under the approval that stands; the button is how the maker does it." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 150_00), item(t, to.id, 260_00)]);
  await sends(t, cast.st17, { type: "approve_run", run: id });
  // The bank driver's part: the file went, and the bank answered that it holds none of it.
  const submission = `${id}-1`;
  await reports(t, { type: "begin_submission", submission, region: "akl" });
  await reports(t, { type: "record_submitted", submission, content_hash: "e2e", bank_reference: `B-${id}` });
  await reports(t, { type: "record_acknowledgement", submission, refused: "the file could not be read", rejected: {} });
  const s = await opened(t, cast.st10, id);
  await s.press("Send what is unpaid again", "#run-actions");
  // The page asks which day the bank should pay the new submission.
  await s.page.locator("dialog[open]").waitFor({ state: "visible" });
  await s.answer(tomorrow(t));
  await s.done("#run-msg", "A new submission is queued.");
});

// --- Controls on what is in a run --------------------------------------------------------------------------------

test({ id: "E2E-187", area: A, persona: "ST10", story: "US-ST17-1", title: "An item to an account nobody has confirmed is held and stops the approval", matrix: ["ADV-037"],
  purpose: "Check a run that pays an account entered by one person and confirmed by no one, then try to approve it.",
  why: "An unconfirmed account is the commonest payment fraud; the run must hold the item and refuse approval while it is held." }, async (t) => {
  const to = await payee(t, { confirm: false });
  const id = await run(t, [item(t, to.id, 100_00)]);
  const s = await opened(t, cast.st17, id);
  await s.sees("held: payee not confirmed", "#run");
  await s.press("Approve", "#run-actions");
  await s.stopped("#run-msg", "held items are released or removed before approval", 409);
});

test({ id: "E2E-188", area: A, persona: "ST10", story: "US-ST17-1", title: "An item to an account not yet screened is held", matrix: ["ADV-037"],
  purpose: "Check a run that pays a confirmed account that sanctions screening has not cleared.",
  why: "Paying a sanctioned party is an offence; no screening result means no payment." }, async (t) => {
  const to = await payee(t, { screen: false });
  const id = await run(t, [item(t, to.id, 100_00)]);
  const s = await opened(t, cast.st10, id);
  await s.sees("held: payee not screened", "#run");
});

test({ id: "E2E-189", area: A, persona: "ST17", story: "US-ST17-1", title: "The same payee and amount twice is held as a possible duplicate until the approver releases it", matrix: ["PAY-006"],
  purpose: "Check a run with two items of the same amount to the same payee; as the approver, release both and approve.",
  why: "Paying twice is easier than getting the money back; the duplicate check must stop the run until a second person has looked." }, async (t) => {
  const to = await payee(t);
  const [one, two] = [item(t, to.id, 777_00), item(t, to.id, 777_00)];
  const id = await run(t, [one, two]);
  const s = await opened(t, cast.st17, id);
  await s.sees("held: possible duplicate", "#run");
  await s.press("Approve", "#run-actions");
  await s.stopped("#run-msg", "held items are released or removed before approval", 409);
  for (const it of [one, two]) { await another(s, { type: "release_item", run: id, item: it.item }); await s.done("#raw-msg", "Done."); }
  await s.tab("Runs");
  await s.press("Approve", "#run-actions");
  await s.done("#run-msg", "Done: approve run.");
  await s.sees(": approved.", "#run");
});

test({ id: "E2E-190", area: A, persona: "ST10", story: "US-ST17-1", title: "The maker cannot release a possible duplicate themselves", matrix: ["PAY-006"],
  purpose: "As the maker, send the release for a held duplicate in their own run.",
  why: "The duplicate check is only a control if the person who made the run cannot wave it through." }, async (t) => {
  const to = await payee(t);
  const [one, two] = [item(t, to.id, 888_00), item(t, to.id, 888_00)];
  const id = await run(t, [one, two]);
  const s = await opened(t, cast.st10, id);
  await another(s, { type: "release_item", run: id, item: one.item });
  await s.stopped("#raw-msg", "approver", 403);
});

test({ id: "E2E-191", area: A, persona: "ST10", story: "US-ST10-1", title: "An item above the limit for one payment is held", matrix: ["PAY-007"],
  purpose: "Check a run holding one item above the per-item limit of its route.",
  why: "Limits bound the damage of any one mistake or fraud; an item above the limit must not travel in an ordinary run." }, async (t) => {
  const to = await payee(t);
  const id = await run(t, [item(t, to.id, 150_000_00)]);
  const s = await opened(t, cast.st10, id);
  await s.sees("held: over item limit", "#run");
});

test({ id: "E2E-192", area: A, persona: "ST10", story: "US-ST10-1", title: "The maker removes a held item and the rest of the run can be approved", matrix: ["PAY-007"],
  purpose: "Remove the held item from a checked run, check again, and have it approved.",
  why: "One doubtful item must not hold up everyone else's money; taking it out is the ordinary remedy." }, async (t) => {
  const [ok, unconfirmed] = [await payee(t), await payee(t, { confirm: false })];
  const [fine, held] = [item(t, ok.id, 45_00), item(t, unconfirmed.id, 55_00)];
  const id = await run(t, [fine, held]);
  const s = await opened(t, cast.st10, id);
  await another(s, { type: "remove_run_item", run: id, item: held.item });
  await s.done("#raw-msg", "Done.");
  await s.tab("Runs");
  await s.press("Check", "#run-actions");
  await s.done("#run-msg", "Done: check run.");
  await s.never(held.item, "#run");
  const approver = await opened(t, cast.st17, id);
  await approver.press("Approve", "#run-actions");
  await approver.done("#run-msg", "Done: approve run.");
});

test({ id: "E2E-193", area: A, persona: "ST10", story: "US-ST10-1", title: "A claim payment waiting with no bank account for its payee cannot be taken into a run", matrix: ["PAY-001"],
  purpose: "Tick a payment that claims has released for a provider payments has no account for, and take it into a run.",
  why: "Claims decides who is paid; payments decides which account. A payment must not enter a run until the account exists and has passed its checks." }, async (t) => {
  const waiting = await t.does(cast.st10, "GET", "/staff/v1/payments/waiting");
  const one = Object.values(waiting).find((w) => String(w.source).startsWith("claim:"));
  if (!one) { t.note("no claim payment is waiting on this stack; run the claims area first"); throw new Error("no claim payment is waiting: the claims journeys that pay a provider have not run on this stack"); }
  const id = await run(t, [], { type: "provider_payments", check: false });
  const s = await t.open("/payments", cast.st10);
  await s.sees(one.source, "#waiting");
  await s.row("#waiting", one.source).locator('input[name="take"]').check();
  await s.fill("#t-run", id);
  await s.press("Take the ticked payments", "#take");
  await s.stopped("#w-msg", "there is no payee for", 409);
});

// --- Accounts that may be paid -----------------------------------------------------------------------------------

test({ id: "E2E-194", area: A, persona: "Payments", story: "US-ST17-1", title: "One maintainer enters an account and a second confirms it from the payee page", matrix: ["ADV-037"],
  purpose: "Enter a payee as one maintainer; as a second, open the payee and press Confirm the details.",
  why: "Two people on every new account is the control against a redirected payment." }, async (t) => {
  const to = await payee(t, { confirm: false });
  const s = await t.open("/payments", cast.payee2);
  await s.tab("Payees and refunds");
  await s.fill("#payee-id", to.id);
  await s.press("Open", "#payee-form");
  await s.sees(`${to.party}, customer`, "#payee");
  await s.press("Confirm the details", "#payee-actions");
  await s.done("#payee-msg", "Done: confirm payee.");
});

test({ id: "E2E-195", area: A, persona: "Payments", story: "US-ST17-1", title: "The person who entered an account cannot confirm it", matrix: ["ADV-037"],
  purpose: "As the maintainer who entered a payee, open it and press Confirm the details.",
  why: "A confirmation by the same person is no confirmation." }, async (t) => {
  const to = await payee(t, { confirm: false });
  const s = await t.open("/payments", cast.payee);
  await s.tab("Payees and refunds");
  await s.fill("#payee-id", to.id);
  await s.press("Open", "#payee-form");
  await s.press("Confirm the details", "#payee-actions");
  await s.stopped("#payee-msg", "the person who entered the details cannot confirm them", 403);
});

test({ id: "E2E-196", area: A, persona: "Payments", story: "US-ST17-1", title: "An account the bank says is in another name cannot be confirmed", matrix: ["ADV-037"],
  purpose: "Enter a payee whose name check came back as no match and try to confirm it.",
  why: "The bank's name check is the one independent fact about an account; a mismatch must stop it being paid." }, async (t) => {
  const to = await payee(t, { confirm: false, name_check: "no_match" });
  const s = await t.open("/payments", cast.payee2);
  await s.tab("Payees and refunds");
  await s.fill("#payee-id", to.id);
  await s.press("Open", "#payee-form");
  await s.press("Confirm the details", "#payee-actions");
  await s.stopped("#payee-msg", "the bank says the account is in another name", 409);
});

test({ id: "E2E-197", area: A, persona: "ST10", story: "US-ST10-1", title: "A party's payees are listed with why each is blocked and without account numbers", matrix: ["ADV-037"],
  purpose: "Show a party with one unconfirmed payee and read the list.",
  why: "Staff need to know why a payee cannot be paid yet; nobody browsing a party needs the account number." }, async (t) => {
  const to = await payee(t, { confirm: false });
  const s = await t.open("/payments", cast.st10);
  await s.tab("Payees and refunds");
  await s.fill("#party-id", to.party);
  await s.press("Show", "#party-form");
  await s.sees("No hold of their own is recorded.", "#party");
  const cells = await s.row("#party", to.id).locator("td").allInnerTexts();
  if (!cells.includes("payee not confirmed")) throw new Error(`the payee row reads ${JSON.stringify(cells)}`);
  await s.never(to.number, "#party");
});

test({ id: "E2E-198", area: A, persona: "Payments", story: "US-D01-1", title: "Changing a payee's account sends it back for confirmation with a cooling-off period", matrix: ["ADV-037"],
  purpose: "Change the account of a confirmed payee and show the party.",
  why: "A changed account is how redirected payments start; the change must wait a day and be confirmed again before money moves." }, async (t) => {
  const to = await payee(t);
  const s = await t.open("/payments", cast.payee);
  await another(s, { type: "change_payee_account", payee: to.id, account: { name: "Account Holder", number: "02-0002-0000009-00" }, name_check: "match" });
  await s.done("#raw-msg", "Done.");
  await s.tab("Payees and refunds");
  await s.fill("#party-id", to.party);
  await s.press("Show", "#party-form");
  await s.sees(to.id, "#party");
  const cells = await s.row("#party", to.id).locator("td").allInnerTexts();
  if (!cells.includes("payee not confirmed") || !/^\d{4}-\d{2}-\d{2}/.test(cells[3] || "")) throw new Error(`after the change the payee row reads ${JSON.stringify(cells)}`);
});

test({ id: "E2E-199", area: A, persona: "Legal", story: "US-ST17-1", title: "An estate's account is held until legal approves it", matrix: ["CLM-038"],
  purpose: "Check a run paying a confirmed, screened estate account; as legal counsel, approve the payee; check the run again.",
  why: "Paying an estate means paying whoever proves they may receive for it; that proof is legal's to accept." }, async (t) => {
  const to = await payee(t, { kind: "estate" });
  const id = await run(t, [item(t, to.id, 900_00)]);
  const maker = await opened(t, cast.st10, id);
  await maker.sees("held: payee needs legal approval", "#run");
  const s = await t.open("/payments", cast.legal);
  await s.tab("Payees and refunds");
  await s.fill("#payee-id", to.id);
  await s.press("Open", "#payee-form");
  await s.press("Approve for an estate or liquidator", "#payee-actions");
  await s.done("#payee-msg", "Done: approve payee.");
});

test({ id: "E2E-200", area: A, persona: "ST5", story: "US-ST05-1", title: "A customer's own hold on money going out shows on their party and stops an account change", matrix: ["COL-026"],
  purpose: "With a hold recorded for a customer, show the party and try to change their payee's account.",
  why: "A customer who fears coercion can freeze money going out; staff must see the hold and the system must refuse to redirect payment under it." }, async (t) => {
  const to = await payee(t);
  await reports(t, { type: "record_customer_hold", party: to.party, held: true });
  const s = await t.open("/payments", cast.payee);
  await s.tab("Payees and refunds");
  await s.fill("#party-id", to.party);
  await s.press("Show", "#party-form");
  await s.sees("They have a hold on their own money going out", "#party");
  await another(s, { type: "change_payee_account", payee: to.id, account: { name: "Someone Else", number: "03-0003-0000003-00" }, name_check: "match" });
  await s.stopped("#raw-msg", "has a hold on money going out", 409);
});

test({ id: "E2E-201", area: A, persona: "Payments", story: "US-ST17-1", title: "A direct debit authority is set up and shows on the party", matrix: ["COL-019"],
  purpose: "Create a direct debit authority for a policy and show the party.",
  why: "Collections relies on the authority record: whose account, which policy, and whether the account is in one name." }, async (t) => {
  const party = t.ref("cust").toLowerCase(), authority = t.ref("dd").toLowerCase();
  const p = await t.policy("term_life", party);
  const s = await t.open("/payments", cast.payee);
  await another(s, { type: "create_authority", authority, policy: p.id, party, account: { name: "Account Holder", number: "01-0001-0000005-00" }, sole_name: true });
  await s.done("#raw-msg", "Done.");
  await s.tab("Payees and refunds");
  await s.fill("#party-id", party);
  await s.press("Show", "#party-form");
  await s.sees(authority, "#party");
  const cells = await s.row("#party", authority).locator("td").allInnerTexts();
  if (!cells.includes(p.id) || !cells.includes("active") || cells[6] !== "yes") throw new Error(`the authority row reads ${JSON.stringify(cells)}`);
});

test({ id: "E2E-202", area: A, persona: "ST5", story: "US-ST05-1", title: "The collections officer cannot set up or suspend a direct debit authority", matrix: ["COL-020"],
  purpose: "As the collections officer, send a command suspending an authority.",
  why: "Bank authorities are maintained by the payee team; collections works with them but does not change whose account is debited." }, async (t) => {
  const s = await t.open("/payments", cast.st5);
  await another(s, { type: "suspend_authority", authority: t.ref("dd").toLowerCase(), reason: "customer asked" });
  await s.stopped("#raw-msg", "", 403, 409);
});

test({ id: "E2E-203", area: A, persona: "ST10", story: "US-ST10-1", title: "A requested refund can be opened and read before it is released", matrix: ["PAY-001"],
  purpose: "Request a premium refund to a verified account and open it on the refunds form.",
  why: "A refund is the customer's money; the maker must see how much, for which policy and where it is going before releasing it." }, async (t) => {
  const to = await payee(t);
  const refund = t.ref("rf").toLowerCase();
  await sends(t, cast.st10, { type: "request_refund", refund, policy: t.ref("pol").toLowerCase(), amount_minor: 5000, currency: "NZD", destination: { payee: { payee: to.id } } });
  const s = await t.open("/payments", cast.st10);
  await s.tab("Payees and refunds");
  await s.fill("#refund-id", refund);
  await s.press("Open", "#refund-form");
  await s.sees("requested", "#refund");
  await s.sees(to.id, "#refund");
  await s.sees("Release", "#refund-actions");
});

// --- Reconciliation and controls ---------------------------------------------------------------------------------

/// A statement line nothing explains, on the second bank's account for `purpose`.
async function mystery(t, purpose, amount) {
  const account = `bnz-sf1-${purpose}`, reference = t.ref("mystery");
  const events = await reports(t, { type: "record_statement", account, date: t.stack.opening, opening_minor: 10_000_000_00, closing_minor: 10_000_000_00 + amount, lines: [{ id: t.ref("line").toLowerCase(), amount_minor: amount, reference }] });
  const opened = events.find((e) => e.type === "exception_opened");
  if (!opened) throw new Error(`the statement raised no exception: ${JSON.stringify(events)}`);
  return { account, reference, exception: opened.exception };
}

test({ id: "E2E-204", area: A, persona: "ST10", story: "US-ST10-1", title: "A credit on a bank statement that nothing explains is raised as an open exception", matrix: ["PAY-009"],
  purpose: "Have a statement arrive with an unexplained credit and read the open reconciliation exceptions.",
  why: "Every statement line is matched or raised; an unexplained line must reach a person the same day." }, async (t) => {
  const m = await mystery(t, "refunds", 123_45);
  const s = await t.open("/payments", cast.recon);
  await s.tab("Reconciliation");
  const cells = await s.row("#exceptions", m.exception).locator("td").allInnerTexts();
  if (!cells.includes("unexplained credit") || !cells.includes("123.45") || !cells.includes(m.reference)) throw new Error(`the exception row reads ${JSON.stringify(cells)}`);
});

test({ id: "E2E-205", area: A, persona: "Payments", story: "US-ST10-1", title: "The reconciler sees an account's open exceptions with Take and Resolve beside each", matrix: ["PAY-009"],
  purpose: "Show the reconciliation of an account that has an open exception.",
  why: "The account view is where the reconciler works an exception; it must list them and offer to take and resolve each." }, async (t) => {
  const m = await mystery(t, "operating", 67_89);
  const s = await t.open("/payments", cast.recon);
  await s.tab("Reconciliation");
  await s.fill("#rec-account", m.account);
  await s.press("Show", "#rec-form");
  await s.sees("Statement of", "#rec");
  await s.sees(m.exception, "#rec");
  await s.sees("Take", "#rec");
});

test({ id: "E2E-206", area: A, persona: "Payments", story: "US-ST10-1", title: "The reconciler takes an exception, resolves it with a reason, and it leaves the open list", matrix: ["PAY-009"],
  purpose: "Assign an exception to the reconciler and resolve it, then read the open exceptions.",
  why: "An exception needs an owner and a recorded resolution; closed ones must stop cluttering the list." }, async (t) => {
  const m = await mystery(t, "third", 45_67);
  const s = await t.open("/payments", cast.recon);
  await another(s, { type: "assign_exception", exception: m.exception, owner: cast.recon.party });
  await s.done("#raw-msg", "Done.");
  await another(s, { type: "resolve_exception", exception: m.exception, resolution: "Interest credited by the bank; journalled." });
  await s.done("#raw-msg", "Done.");
  const again = await t.open("/payments", cast.recon);
  await again.tab("Reconciliation");
  await again.sees("Open reconciliation exceptions");
  await again.never(m.exception, "#exceptions");
});

test({ id: "E2E-207", area: A, persona: "ST10", story: "US-ST10-1", title: "The maker cannot resolve a reconciliation exception", matrix: ["PAY-009"],
  purpose: "As the maker, send a resolution for an open exception.",
  why: "The person who makes payments must not be the one who explains away what the bank statement shows." }, async (t) => {
  const m = await mystery(t, "collection", 11_22);
  const s = await t.open("/payments", cast.st10);
  await another(s, { type: "resolve_exception", exception: m.exception, resolution: "it is fine" });
  await s.stopped("#raw-msg", "reconciler", 403);
});

test({ id: "E2E-208", area: A, persona: "ST13", story: "US-ST13-1", title: "Compliance sees the cases of premiums paid by someone else; the maker does not", matrix: ["INV-019"],
  purpose: "Open the Controls view as the compliance investigator and as the maker.",
  why: "Third-party payers are a money laundering signal worked by compliance alone; nobody else should see who is being looked at." }, async (t) => {
  const s = await t.open("/payments", cast.st13);
  await s.tab("Controls");
  await s.sees("Premium paid by someone else");
  await s.sees("No open cases.", "#aml");
  const maker = await t.open("/payments", cast.st10);
  await maker.tab("Controls");
  await maker.never("Premium paid by someone else");
});

test({ id: "E2E-209", area: A, persona: "ST16", story: "US-ST16-2", title: "Region readiness says plainly that no evacuation drill has passed", matrix: ["INC-007"],
  purpose: "Read Region readiness on the Controls view of a system where no drill has run.",
  why: "Paying from another region is only safe if a drill proved it; the page must not imply readiness nobody has tested." }, async (t) => {
  const s = await t.open("/payments", cast.st17);
  await s.tab("Controls");
  await s.sees("no drill has passed from akl", "#ready");
  await s.sees("no drill has passed from syd", "#ready");
});
