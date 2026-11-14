// Claims: an assessor notifies, takes, assesses, decides and pays a claim as
// themselves, within the authority the claims authority book gives them.
"use strict";
const { test, Journey } = require("../lib/suite");
const { cast } = require("../lib/cast");

const A = "Claims";
const COMMANDS = "/staff/v1/claims/commands";
const event = (t) => Journey.before(t.stack.opening, 3);   // eleven days after the cover began
const early = (t) => Journey.before(t.stack.opening, 12);  // two days after the cover began
const tells = (t, who, command, purpose = "working the claim") => t.does(who, "POST", COMMANDS, { purpose, command }, Object.keys(command)[0]);
const work = async (t, id, who = cast.st4) => (await t.does(who, "GET", `/staff/v1/claims/work?claim=${id}`)).view;
const dollars = (minor) => (minor / 100).toFixed(2);

/// A policy and a claim notified on it by phone.
async function notified(t, { kind = "term_life", party = "cust-p18", cover = "trauma", type = "trauma", on, life = null, who = cast.st4, notifier = "policyholder", claimant = null, invoice } = {}) {
  const policy = await t.policy(kind, party);
  const claim = t.ref("CLM");
  // A provider's invoice arrives with a health claim: the category it is paid under, the amount, and the service billed.
  const health = invoice ? { category: "surgical", invoice_minor: invoice, provider: "prov-pr1", service_code: `SURG-${claim}`, service_date: on || event(t) } : undefined;
  await tells(t, who, { notify: { claim, policy: policy.id, cover, life, claim_type: type, event_date: on || event(t), channel: invoice ? "provider" : "phone", notifier, claimant, ...(health ? { health } : {}) } });
  return { claim, policy };
}
/// ... that the senior assessor owns.
async function owned(t, options = {}) {
  const c = await notified(t, options);
  await tells(t, cast.st4, { assign: { claim: c.claim, to: options.owner || "st4" } });
  return c;
}
/// ... with its cause and cover check recorded, ready to decide.
async function assessed(t, options = {}) {
  const c = await owned(t, options);
  const who = options.owner ? cast[options.owner] : cast.st4;
  await tells(t, who, { record_cause: { claim: c.claim, cause_code: options.cause || "CC-0003" } });
  await tells(t, who, { cover_check: { claim: c.claim, relied_on: ["in_force", "life_on_policy"] } });
  return c;
}
async function admitted(t, options = {}) {
  const c = await assessed(t, options);
  await tells(t, cast.st4, { decide: { claim: c.claim, request: { admit: { amount_minor: null } }, reasons: ["R-OK"] } });
  return c;
}
const HEALTH = { kind: "health_family", party: "cust-p1", cover: "hospital", type: "health", life: "life-1", who: cast.st3, notifier: "provider", invoice: 1_700_00 };
async function healthClaim(t, invoice = HEALTH.invoice) {
  const c = await notified(t, { ...HEALTH, invoice });
  await tells(t, cast.st3, { assign: { claim: c.claim, to: "st3" } });
  await tells(t, cast.st3, { record_cause: { claim: c.claim, cause_code: "CC-0204" } });
  await tells(t, cast.st3, { cover_check: { claim: c.claim, relied_on: ["in_force", "definition"] } });
  return c;
}
async function desk(t, who, claim) {
  const s = await t.open("/claims", who);
  await s.tab("Claim");
  if (claim) { await s.fill("#c-id", claim); await s.press("Show", "#lookup"); await s.sees(claim, "#c-title"); }
  return s;
}
const summary = async (s) => { const [head] = [["Policy", "Cover", "Partition", "Queue", "Owner", "Event", "Notified", "Closed", "Reserve", "Admitted", "Payable", "Paid"]]; const [row] = await s.rows("#c-summary"); return Object.fromEntries(head.map((k, i) => [k, row[i]])); };

test({ id: "E2E-070", area: A, persona: "ST4", story: "US-ST04-1", title: "The assessor is told who they act as and how the ledger is split",
  purpose: "Sign in to the claims console as the senior assessor and read the line beside the title.",
  why: "Every claims action is recorded against the person; the console must say whose name will be on it before they act." }, async (t) => {
  const s = await t.open("/claims", cast.st4);
  await s.sees(`You act as st4. The ledger has ${t.stack.partitions} partitions`, "#me");
});

test({ id: "E2E-071", area: A, persona: "ST4", story: "US-ST03-1", title: "A trauma claim is notified by phone and opens with its reserve and timers", matrix: ["CLM-001"],
  purpose: "Notify a trauma claim on an in-force policy from the Act form and read the claim that opens.",
  why: "Notification is the start of every claim; it must land in the right queue with the sum insured reserved and the acknowledgement timer running." }, async (t) => {
  const policy = await t.policy("term_life", "cust-p18");
  const claim = t.ref("CLM");
  const s = await desk(t, cast.st4);
  await s.act("#act", "notify", { "Claim": claim, "Policy": policy.id, "Cover": "trauma", "Claim type": "trauma", "Event date": event(t), "Channel": "phone", "Notified by": "policyholder" }, { purpose: "claimant rang" });
  await s.done("#act-msg", "Done: notify.");
  await s.sees(`${claim}: trauma, open`, "#c-title");
  const row = await summary(s);
  if (row.Policy !== policy.id || row.Cover !== "trauma" || row.Queue !== "lump sum" || row.Owner !== "unassigned" || row.Event !== event(t) || row.Notified !== t.stack.opening) throw new Error(`the claim reads ${JSON.stringify(row)}`);
  if (row.Reserve !== "100000.00" || row.Admitted !== "0.00" || row.Paid !== "0.00") throw new Error(`the money reads ${JSON.stringify(row)}`);
  await s.sees("acknowledgement", "#c-timers");
  await s.sees("No payee verified.", "#c-money");
});

test({ id: "E2E-072", area: A, persona: "ST4", story: "US-ST04-1", title: "A new claim is in its queue and opens from there", matrix: ["CLM-002"],
  purpose: "Show the lump sum queue, find a newly notified claim, and open it from its row.",
  why: "The queue is where work is picked up; a claim missing from it is a claimant nobody calls." }, async (t) => {
  const c = await notified(t);
  const s = await t.open("/claims", cast.st4);
  await s.pick("#q-name", "lump_sum");
  await s.press("Show", "#queue");
  await s.sees(c.claim, "#q-rows");
  const row = (await s.rows("#q-rows")).find((r) => r[0] === c.claim);
  if (row[1] !== String(c.policy.partition) || row[3] !== "0" || row[4] !== "unassigned") throw new Error(`the queue row reads ${JSON.stringify(row)}`);
  await s.row("#q-rows", c.claim).getByRole("button", { name: "Open" }).click();
  await s.tab("Claim");
  await s.sees(`${c.claim}: trauma, open`, "#c-title");
});

test({ id: "E2E-073", area: A, persona: "ST4", story: "US-ST04-1", title: "The queue lead gives a claim to an assessor, with the reason recorded",
  purpose: "Assign a notified claim to the senior assessor from the Act form with a purpose.",
  why: "Ownership decides who may act on a claim and who the claimant hears from; the hand-over must be explicit and attributable." }, async (t) => {
  const c = await notified(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "assign", { "Claim": c.claim, "To (staff reference)": "st4b" }, { purpose: "allocating lump sum work" });
  await s.done("#act-msg", "Done: assign.");
  if ((await summary(s)).Owner !== "st4b") throw new Error("the claim does not show its new owner");
});

test({ id: "E2E-074", area: A, persona: "ST3", story: "US-ST01-1", title: "A claim is not given to an assessor who is not accredited for its line", matrix: ["CLM-003"],
  purpose: "Assign a trauma claim to the health claims assessor.",
  why: "Accreditation by line is the first authority control; a health assessor deciding a trauma claim is outside their training and their limits." }, async (t) => {
  const c = await notified(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "assign", { "Claim": c.claim, "To (staff reference)": "st3" }, { purpose: "allocating" });
  await s.stopped("#act-msg", "st3 is not accredited for this line and queue", 409);
  if ((await summary(s)).Owner !== "unassigned") throw new Error("the claim changed owner");
});

test({ id: "E2E-075", area: A, persona: "ST4", story: "US-ST04-1", title: "Only the claim's owner records its cause",
  purpose: "As an assessor who does not own the claim, record its cause.",
  why: "The owner is accountable for the assessment; a colleague's entry on someone else's file would be unattributable work." }, async (t) => {
  const c = await owned(t);
  const s = await desk(t, cast.st4b, c.claim);
  await s.act("#act", "record_cause", { "Claim": c.claim, "Cause code": "CC-0003" }, { purpose: "helping out" });
  await s.stopped("#act-msg", `claim ${c.claim} is not assigned to st4b`, 403);
});

test({ id: "E2E-076", area: A, persona: "ST4", story: "US-ST03-1", title: "A cause of claim comes from the controlled list",
  purpose: "Record a cause code that is not on the list, then one that is.",
  why: "Exclusions, reinsurance and experience reporting all key on the cause code; a free-text cause breaks each of them." }, async (t) => {
  const c = await owned(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "record_cause", { "Claim": c.claim, "Cause code": "CC-9999" }, { purpose: "assessment" });
  await s.stopped("#act-msg", "that cause code is not on the controlled list", 409);
  await s.act("#act", "record_cause", { "Claim": c.claim, "Cause code": "CC-0003" }, { purpose: "assessment" });
  await s.done("#act-msg", "Done: record cause.");
});

test({ id: "E2E-077", area: A, persona: "ST3", story: "US-ST03-1", title: "A claim cannot be admitted before the cover check is recorded", matrix: ["CLM-004"],
  purpose: "Try to admit an owned claim with no cover check, record the check with the facts relied on, and try again without a cause.",
  why: "The cover check gives the assessor facts, not a verdict; the decision must rest on a recorded check and a recorded cause." }, async (t) => {
  const c = await owned(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "decide_admit", { "Claim": c.claim, "Reason codes (comma separated)": "R-OK" }, { purpose: "decision" });
  await s.stopped("#act-msg", "record the cover check before deciding", 409);
  await s.act("#act", "cover_check", { "Claim": c.claim, "Facts relied on (comma separated)": "in_force, life_on_policy, definition" }, { purpose: "assessment" });
  await s.done("#act-msg", "Done: cover check.");
  await s.act("#act", "decide_admit", { "Claim": c.claim, "Reason codes (comma separated)": "R-OK" }, { purpose: "decision" });
  await s.stopped("#act-msg", "record the cause of claim before admitting", 409);
});

test({ id: "E2E-078", area: A, persona: "ST4", story: "US-ST03-1", title: "An admitted trauma claim shows what is payable after unpaid premium", matrix: ["CLM-005"],
  purpose: "Admit a trauma claim for its sum insured and read the admitted and payable amounts and the payment timer.",
  why: "Unpaid premium is taken from the claim, not chased from a claimant; the assessor must see the net figure the customer will receive." }, async (t) => {
  const c = await assessed(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "decide_admit", { "Claim": c.claim, "Reason codes (comma separated)": "R-OK" }, { purpose: "decision" });
  await s.done("#act-msg", "Done: decide admit.");
  await s.sees(`${c.claim}: trauma, admitted`, "#c-title");
  const row = await summary(s);
  if (row.Admitted !== "100000.00" || row.Reserve !== "0.00") throw new Error(`after admission the claim reads ${JSON.stringify(row)}`);
  const payable = Number(row.Payable);
  if (!(payable < 100000 && payable > 99000)) throw new Error(`payable is ${row.Payable}; the unpaid premium should have been taken from the sum insured`);
  await s.sees("payment", "#c-timers");
});

test({ id: "E2E-079", area: A, persona: "ST4", story: "US-ST17-1", title: "Nothing is paid on a claim until the payee is verified", matrix: ["PAY-004"],
  purpose: "Try to pay an admitted claim with no verified payee.",
  why: "Paying a claim to an unverified account is the classic claims fraud; verification must be a hard gate, not a habit." }, async (t) => {
  const c = await admitted(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "pay", { "Claim": c.claim, "Payment reference": `pay-${c.claim}`, "Amount": "1000.00" }, { purpose: "payment" });
  await s.stopped("#act-msg", "verify the payee before the first payment", 409);
  if ((await summary(s)).Paid !== "0.00") throw new Error("money shows as paid");
});

test({ id: "E2E-080", area: A, persona: "ST4", story: "US-ST17-1", title: "A payment cannot exceed what is admitted and unpaid",
  purpose: "With a verified payee, try to pay the full sum insured when unpaid premium has reduced what is payable.",
  why: "The payable figure is the control total for a claim; a payment above it is money the insurer does not owe." }, async (t) => {
  const c = await admitted(t);
  await tells(t, cast.st4, { verify_payee: { claim: c.claim, party: "cust-p18", kind: "policyholder", account_ref: "acct-p18", name_check_ref: "nc-1", screening_ref: "sc-1", authority_doc: null } });
  const s = await desk(t, cast.st4, c.claim);
  await s.sees("Payee: cust-p18 (policyholder).", "#c-money");
  await s.act("#act", "pay", { "Claim": c.claim, "Payment reference": `pay-${c.claim}`, "Amount": "100000.00" }, { purpose: "payment" });
  await s.stopped("#act-msg", "the amount is not within what is admitted and unpaid", 409);
});

test({ id: "E2E-081", area: A, persona: "ST4", story: "US-ST04-1", title: "A verified payee is paid the whole payable amount and the claim is settled", matrix: ["CLM-006"],
  purpose: "Verify the policyholder as payee on screen, pay the payable amount, and read the payments and totals.",
  why: "This is the moment the insurer keeps its promise; the payment must be recorded once, for the right amount, to the verified person." }, async (t) => {
  const c = await admitted(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "verify_payee", { "Claim": c.claim, "Payee (customer reference)": "cust-p18", "Kind": "policyholder", "Account reference": "acct-p18", "Name check reference": "nc-77", "Screening reference": "sc-77" }, { purpose: "payment" });
  await s.done("#act-msg", "Done: verify payee.");
  const payable = (await summary(s)).Payable;
  await s.act("#act", "pay", { "Claim": c.claim, "Payment reference": `pay-${c.claim}`, "Amount": payable }, { purpose: "payment" });
  await s.done("#act-msg", "Done: pay.");
  const row = await summary(s);
  if (row.Paid !== payable || row.Payable !== "0.00") throw new Error(`after payment the claim reads ${JSON.stringify(row)}`);
  const paid = await s.rows("#c-money");
  if (!paid.some((r) => r[0] === `pay-${c.claim}` && r[1] === payable && r[2] === t.stack.opening)) throw new Error(`the payments read ${JSON.stringify(paid)}`);
  await s.act("#act", "pay", { "Claim": c.claim, "Payment reference": `pay2-${c.claim}`, "Amount": "10.00" }, { purpose: "payment" });
  await s.stopped("#act-msg", "", 409);
});

test({ id: "E2E-082", area: A, persona: "ST4", story: "US-BK1-1", title: "A payment reference is used once on a claim", matrix: ["PAY-006"],
  purpose: "Pay part of a claim, then send a second payment under the same reference.",
  why: "A repeated reference is how a double payment starts; the second must be refused, not paid again." }, async (t) => {
  const c = await admitted(t);
  await tells(t, cast.st4, { verify_payee: { claim: c.claim, party: "cust-p18", kind: "policyholder", account_ref: "acct-p18", name_check_ref: "nc-1", screening_ref: "sc-1", authority_doc: null } });
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "pay", { "Claim": c.claim, "Payment reference": "part-1", "Amount": "40000.00" }, { purpose: "part payment" });
  await s.done("#act-msg", "Done: pay.");
  await s.act("#act", "pay", { "Claim": c.claim, "Payment reference": "part-1", "Amount": "20000.00" }, { purpose: "part payment" });
  await s.stopped("#act-msg", "that payment reference has been used on this claim", 409);
  if ((await summary(s)).Paid !== "40000.00") throw new Error("the paid total moved on a refused payment");
});

test({ id: "E2E-083", area: A, persona: "ST4", story: "US-ST02-1", title: "A large death claim waits for a second signature and shows who proposed it", matrix: ["CLM-008"],
  purpose: "Admit a $500,000 death claim as the senior assessor and read the waiting notice.",
  why: "Admissions above the two-signature amount must not take effect on one person's word; the file must show what is waiting and on whom." }, async (t) => {
  const c = await assessed(t, { cover: "life", type: "death", cause: "CC-0101", notifier: "executor", claimant: "cust-p1" });
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "decide_admit", { "Claim": c.claim, "Reason codes (comma separated)": "R-OK" }, { purpose: "decision" });
  await s.done("#act-msg", "Done: decide admit.");
  await s.sees("Waiting for a signature: admit proposed by st4", "#c-waiting");
  await s.sees("Still needed: 1.", "#c-waiting");
  await s.sees(`${c.claim}: death, open`, "#c-title");
  if ((await summary(s)).Admitted !== "0.00") throw new Error("the claim shows as admitted on one signature");
});

test({ id: "E2E-084", area: A, persona: "ST4", story: "US-ST02-1", title: "The proposer of a decision cannot give its second signature", matrix: ["CLM-008"],
  purpose: "As the assessor who proposed a large admission, approve it yourself.",
  why: "A second signature from the same person is no control at all." }, async (t) => {
  const c = await assessed(t, { cover: "life", type: "death", cause: "CC-0101", notifier: "executor", claimant: "cust-p1" });
  await tells(t, cast.st4, { decide: { claim: c.claim, request: { admit: { amount_minor: null } }, reasons: ["R-OK"] } });
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "approve", { "Claim": c.claim }, { purpose: "second signature" });
  await s.stopped("#act-msg", "each signature must come from a different person", 409);
  await s.sees("Waiting for a signature", "#c-waiting");
});

test({ id: "E2E-085", area: A, persona: "ST4", story: "US-ST02-1", title: "A second assessor countersigns a large admission and it takes effect", matrix: ["CLM-008"],
  purpose: "As a second senior assessor, open a death claim waiting for a signature and approve it.",
  why: "The countersignature is what releases half a million dollars; both identities must be on the decision and the claim must then be admitted in full." }, async (t) => {
  const c = await assessed(t, { cover: "life", type: "death", cause: "CC-0101", notifier: "executor", claimant: "cust-p1" });
  await tells(t, cast.st4, { decide: { claim: c.claim, request: { admit: { amount_minor: null } }, reasons: ["R-OK"] } });
  const s = await desk(t, cast.st4b, c.claim);
  await s.sees("proposed by st4", "#c-waiting");
  await s.act("#act", "approve", { "Claim": c.claim }, { purpose: "second signature with the file read" });
  await s.done("#act-msg", "Done: approve.");
  await s.sees(`${c.claim}: death, admitted`, "#c-title");
  if ((await summary(s)).Admitted !== "500000.00") throw new Error("the admitted amount is not the sum insured");
  await s.never("Waiting for a signature", "#c-waiting");
});

test({ id: "E2E-086", area: A, persona: "ST4", story: "US-ST02-1", title: "A second assessor refuses a proposed admission and the claim stays open",
  purpose: "As a second senior assessor, refuse a proposed large admission.",
  why: "A countersigner who disagrees must be able to stop the decision, and the claim must return to assessment, not vanish." }, async (t) => {
  const c = await assessed(t, { cover: "life", type: "death", cause: "CC-0101", notifier: "executor", claimant: "cust-p1" });
  await tells(t, cast.st4, { decide: { claim: c.claim, request: { admit: { amount_minor: null } }, reasons: ["R-OK"] } });
  const s = await desk(t, cast.st4b, c.claim);
  await s.act("#act", "refuse", { "Claim": c.claim }, { purpose: "evidence of death incomplete" });
  await s.done("#act-msg", "Done: refuse.");
  await s.sees(`${c.claim}: death, open`, "#c-title");
  await s.never("Waiting for a signature", "#c-waiting");
  await s.act("#act", "approve", { "Claim": c.claim }, { purpose: "second signature" });
  await s.stopped("#act-msg", "no decision on this claim is waiting for a signature", 409);
});

test({ id: "E2E-087", area: A, persona: "ST3", story: "US-ST03-1", title: "The health assessor takes a hospital claim from the health queue and admits it within their limit", matrix: ["HLT-003"],
  purpose: "As the health assessor, find a provider's $1,700 hospital invoice in the health queue, take it, record cause and cover check, and admit it less the $500 excess.",
  why: "Most claims are small health claims decided by one assessor; the whole path must work at the screen for the person whose authority is smallest." }, async (t) => {
  const c = await notified(t, HEALTH);
  const s = await t.open("/claims", cast.st3);
  await s.pick("#q-name", "health");
  await s.press("Show", "#queue");
  await s.row("#q-rows", c.claim).getByRole("button", { name: "Open" }).click();
  await s.tab("Claim");
  await s.sees(`${c.claim}: health, open`, "#c-title");
  await s.act("#act", "assign", { "Claim": c.claim, "To (staff reference)": "st3" }, { purpose: "taking it" });
  await s.done("#act-msg", "Done: assign.");
  await s.act("#act", "record_cause", { "Claim": c.claim, "Cause code": "CC-0204" }, { purpose: "assessment" });
  await s.done("#act-msg", "Done: record cause.");
  await s.act("#act", "cover_check", { "Claim": c.claim, "Facts relied on (comma separated)": "in_force, definition" }, { purpose: "assessment" });
  await s.done("#act-msg", "Done: cover check.");
  await s.act("#act", "decide_admit", { "Claim": c.claim, "Amount (blank for the sum insured)": "1200.00", "Reason codes (comma separated)": "R-OK" }, { purpose: "decision" });
  await s.done("#act-msg", "Done: decide admit.");
  await s.sees(`${c.claim}: health, admitted`, "#c-title");
  // The invoice was $1,700 and the plan's excess is $500.
  if ((await summary(s)).Admitted !== "1200.00") throw new Error("the admitted amount is not the invoice less the excess");
});

test({ id: "E2E-088", area: A, persona: "ST3", story: "US-ST01-1", title: "A claim above the health assessor's authority is not theirs to take", matrix: ["CLM-003"],
  purpose: "As the health assessor (limit $5,000), take a $9,000 hospital invoice from the queue; then try to admit more than the invoice on a claim within the limit.",
  why: "Only cases an assessor can decide should reach them; an over-limit claim must route up at assignment, not after the work is done." }, async (t) => {
  const big = await notified(t, { ...HEALTH, invoice: 9_000_00 });
  const s = await desk(t, cast.st3, big.claim);
  await s.act("#act", "assign", { "Claim": big.claim, "To (staff reference)": "st3" }, { purpose: "taking it" });
  await s.stopped("#act-msg", "st3 has no authority for a claim of this type and amount", 403);
  if ((await summary(s)).Owner !== "unassigned") throw new Error("the over-limit claim changed owner");
  const small = await healthClaim(t);
  await s.fill("#c-id", small.claim); await s.press("Show", "#lookup"); await s.sees(small.claim, "#c-title");
  await s.act("#act", "decide_admit", { "Claim": small.claim, "Amount (blank for the sum insured)": "4000.00", "Reason codes (comma separated)": "R-OK" }, { purpose: "decision" });
  await s.stopped("#act-msg", "the amount is more than the cover pays for this event", 409);
});

test({ id: "E2E-089", area: A, persona: "ST3", story: "US-ST01-1", title: "A reserve above the health assessor's limit waits for a senior",
  purpose: "As the health assessor (reserve limit $10,000), set a $20,000 reserve and read the claim.",
  why: "Reserves drive the insurer's reported liabilities; a large reserve needs a senior's eye before it stands." }, async (t) => {
  const c = await healthClaim(t);
  const s = await desk(t, cast.st3, c.claim);
  await s.act("#act", "decide_reserve", { "Claim": c.claim, "Reserve": "20000.00", "Reason codes (comma separated)": "R-EVID" }, { purpose: "reserve review" });
  await s.done("#act-msg", "Done: decide reserve.");
  await s.sees("Waiting for a signature: reserve proposed by st3", "#c-waiting");
  if ((await summary(s)).Reserve === "20000.00") throw new Error("the reserve stands on the health assessor's word");
});

test({ id: "E2E-090", area: A, persona: "ST3", story: "US-ST01-1", title: "The health assessor cannot decline a claim",
  purpose: "As the health assessor, whose profile does not include declines, decline a hospital claim.",
  why: "A decline is the decision most likely to be complained about; it is reserved to profiles that carry it." }, async (t) => {
  const c = await healthClaim(t);
  await tells(t, cast.st3, { open_task: { claim: c.claim, task: "talk", kind: "claimant_conversation", due: null } });
  await tells(t, cast.st3, { complete_task: { claim: c.claim, task: "talk", outcome: "explained", attendees: ["st3"], vault_ref: null } });
  const s = await desk(t, cast.st3, c.claim);
  await s.act("#act", "decide_decline", { "Claim": c.claim, "Reason codes (comma separated)": "R-EXCL" }, { purpose: "decision" });
  await s.stopped("#act-msg", "your authority profile does not let you propose this decision", 409, 403);
  await s.sees(`${c.claim}: health, open`, "#c-title");
});

test({ id: "E2E-091", area: A, persona: "ST4", story: "US-ST06-1", title: "A claim is not declined until the claimant has been spoken to", matrix: ["CLM-010"],
  purpose: "Decline a trauma claim with no conversation recorded; then record the conversation as a task and decline.",
  why: "Nobody should learn of a decline from a letter; the conversation is a required, recorded step." }, async (t) => {
  const c = await assessed(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "decide_decline", { "Claim": c.claim, "Reason codes (comma separated)": "R-EXCL" }, { purpose: "decision" });
  await s.stopped("#act-msg", "talk to the claimant before declining, and record it as a task", 409);
  await s.act("#act", "open_task", { "Claim": c.claim, "Task reference": "talk-1", "Kind": "claimant_conversation" }, { purpose: "pre-decline call" });
  await s.done("#act-msg", "Done: open task.");
  await s.act("#act", "complete_task", { "Claim": c.claim, "Task reference": "talk-1", "Outcome": "explained the exclusion", "Attendees (comma separated)": "st4" }, { purpose: "pre-decline call" });
  await s.done("#act-msg", "Done: complete task.");
  await s.act("#act", "decide_decline", { "Claim": c.claim, "Reason codes (comma separated)": "R-EXCL" }, { purpose: "decision" });
  await s.done("#act-msg", "Done: decide decline.");
  await s.sees("Waiting for a signature: decline proposed by st4", "#c-waiting");
});

test({ id: "E2E-092", area: A, persona: "ST4", story: "US-ST02-1", title: "A decline is countersigned before it stands", matrix: ["CLM-010"],
  purpose: "As a second senior assessor, approve a proposed decline and read the claim.",
  why: "Declines carry two signatures; the claim must close as declined only when the second is given, with nothing admitted or reserved." }, async (t) => {
  const c = await assessed(t);
  await tells(t, cast.st4, { open_task: { claim: c.claim, task: "talk", kind: "claimant_conversation", due: null } });
  await tells(t, cast.st4, { complete_task: { claim: c.claim, task: "talk", outcome: "explained", attendees: ["st4"], vault_ref: null } });
  await tells(t, cast.st4, { decide: { claim: c.claim, request: "decline", reasons: ["R-EXCL"] } });
  const s = await desk(t, cast.st4b, c.claim);
  await s.act("#act", "approve", { "Claim": c.claim }, { purpose: "countersigning the decline" });
  await s.done("#act-msg", "Done: approve.");
  await s.sees(`${c.claim}: trauma, declined`, "#c-title");
  const row = await summary(s);
  if (row.Reserve !== "0.00" || row.Admitted !== "0.00" || !row.Closed) throw new Error(`the declined claim reads ${JSON.stringify(row)}`);
});

test({ id: "E2E-093", area: A, persona: "ST4", story: "US-ST03-1", title: "A decision's reason code must belong to that kind of decision",
  purpose: "Decline a claim with the reason code reserved for withdrawals.",
  why: "Reason codes feed the decline letter and the regulator's return; a withdrawal code on a decline misreports why the customer was turned down." }, async (t) => {
  const c = await assessed(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "decide_decline", { "Claim": c.claim, "Reason codes (comma separated)": "R-WDRAW" }, { purpose: "decision" });
  await s.stopped("#act-msg", "R-WDRAW is not a reason code for this decision", 409);
});

test({ id: "E2E-094", area: A, persona: "ST13", story: "US-ST13-1", title: "A claim days after cover began is held for investigation without telling the assessor why", matrix: ["CLM-020"],
  purpose: "Admit and try to pay a claim whose event was two days after the cover began, as its assessor; look for any sign of an investigation.",
  why: "An early claim is quietly referred; payment must stop, and the assessor and through them the claimant must see no trace of the referral." }, async (t) => {
  const c = await admitted(t, { on: early(t) });
  await tells(t, cast.st4, { verify_payee: { claim: c.claim, party: "cust-p18", kind: "policyholder", account_ref: "acct-p18", name_check_ref: "nc-1", screening_ref: "sc-1", authority_doc: null } });
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "pay", { "Claim": c.claim, "Payment reference": `pay-${c.claim}`, "Amount": "500.00" }, { purpose: "payment" });
  await s.stopped("#act-msg", "payment on this claim is held; ask a claims manager", 409);
  await s.never("investigation", "#claim");
  await s.never("early claim", "#claim");
  await s.never("investigation", "#act-msg");
  await s.tab("Investigations");
  await s.press("Show my cases");
  await s.sees("No cases are open to you.", "#inv");
});

test({ id: "E2E-095", area: A, persona: "ST13", story: "US-ST13-1", title: "The investigator sees the restricted case and the claim on it", matrix: ["CLM-020"],
  purpose: "As the compliance investigator, show your cases and find the early claim's case with its rule and claim.",
  why: "The investigator is the only person who works these cases; the list must show each case, why it opened and what it covers." }, async (t) => {
  const c = await notified(t, { on: early(t) });
  const s = await t.open("/claims", cast.st13);
  await s.tab("Investigations");
  await s.press("Show my cases");
  await s.sees(`investigation/${c.claim}`, "#inv");
  const row = (await s.rows("#inv")).find((r) => r[0] === `investigation/${c.claim}`);
  if (row[1] !== "early claim" || row[2] !== t.stack.opening || row[3] !== "claims" || !row[4].includes(`${c.claim} (open, 100000.00`) || row[5] !== "none") throw new Error(`the case reads ${JSON.stringify(row)}`);
});

test({ id: "E2E-096", area: A, persona: "N7", story: "US-N07-1", title: "An assessor is never given the claim of someone they are related to", matrix: ["STF-006"],
  purpose: "Assign a claim on a relative's policy to the assessor who declared the relationship, as the queue lead and then as the assessor herself.",
  why: "A staff member's family must be handled by people who do not know them; the declared relationship must block the assignment whoever attempts it." }, async (t) => {
  const c = await notified(t, { party: "cust-p60" });
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "assign", { "Claim": c.claim, "To (staff reference)": "n7" }, { purpose: "allocating" });
  await s.stopped("#act-msg", "n7 underwrote the life or is related to someone on the claim", 409);
  // To her, the claim does not exist: she cannot open it, and cannot take it.
  const own = await desk(t, cast.n7);
  await own.fill("#c-id", c.claim);
  await own.press("Show", "#lookup");
  await own.stopped("#c-msg", "", 404);
  if (await own.page.locator("#claim").isVisible()) throw new Error("the relative's claim is on her screen");
  await own.act("#act", "assign", { "Claim": c.claim, "To (staff reference)": "n7" }, { purpose: "I will take it" });
  await own.stopped("#act-msg", "related to someone on the claim", 409);
});

test({ id: "E2E-097", area: A, persona: "ST4", story: "US-ST04-1", title: "Evidence is requested, received and its cost recorded on the claim", matrix: ["CLM-012"],
  purpose: "Request a medical report on a claim, see it outstanding, then record it received with its vault reference and cost.",
  why: "Outstanding evidence is the usual reason a claim is slow; the file must show what was asked of whom and when it arrived." }, async (t) => {
  const c = await owned(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.sees("No evidence requested.", "#c-evidence");
  await s.act("#act", "request_evidence", { "Claim": c.claim, "Evidence reference": "ev-gp", "Kind": "medical_report", "From": "gp" }, { purpose: "assessment" });
  await s.done("#act-msg", "Done: request evidence.");
  let row = (await s.rows("#c-evidence")).find((r) => r[0] === "ev-gp");
  if (row[1] !== "medical_report" || row[2] !== "gp" || row[4] !== "outstanding") throw new Error(`the evidence row reads ${JSON.stringify(row)}`);
  await s.act("#act", "receive_evidence", { "Claim": c.claim, "Evidence reference": "ev-gp", "Document in the vault": "vault-ev-gp", "Cost to pay the provider": "150.00" }, { purpose: "assessment" });
  await s.done("#act-msg", "Done: receive evidence.");
  row = (await s.rows("#c-evidence")).find((r) => r[0] === "ev-gp");
  if (row[4] !== t.stack.opening) throw new Error(`the evidence shows received "${row[4]}"`);
});

test({ id: "E2E-098", area: A, persona: "ST4", story: "US-ST04-1", title: "A rehabilitation task has a due date and shows complete once done", matrix: ["INC-006"],
  purpose: "Open a rehabilitation task due in a month on a claim, read it, and complete it with its outcome.",
  why: "Reviews come to the case manager as tasks on their dates so nobody has to remember; the file must show each task's date and whether it was done." }, async (t) => {
  const c = await owned(t);
  const due = Journey.before(t.stack.opening, -30);
  const s = await desk(t, cast.st4, c.claim);
  await s.sees("No tasks.", "#c-tasks");
  await s.act("#act", "open_task", { "Claim": c.claim, "Task reference": "rev-1", "Kind": "rehabilitation", "Due (optional)": due }, { purpose: "review plan" });
  await s.done("#act-msg", "Done: open task.");
  let row = (await s.rows("#c-tasks")).find((r) => r[0] === "rev-1");
  if (row[1] !== "rehabilitation" || row[2] !== due || row[3] !== "" || row[4] !== "") throw new Error(`the task reads ${JSON.stringify(row)}`);
  await s.act("#act", "complete_task", { "Claim": c.claim, "Task reference": "rev-1", "Outcome": "no change", "Attendees (comma separated)": "st4" }, { purpose: "review" });
  await s.done("#act-msg", "Done: complete task.");
  row = (await s.rows("#c-tasks")).find((r) => r[0] === "rev-1");
  if (row[3] !== t.stack.opening) throw new Error(`the task shows completed "${row[3]}"`);
});

test({ id: "E2E-099", area: A, persona: "ST4", story: "US-ST04-1", title: "A new claim's acknowledgement is due in two days and stays due until the letter is delivered",
  purpose: "Read the service timers on a new claim, record a call to the claimant, and read them again.",
  why: "Acknowledgement is met by proof the letter was delivered, not by an assessor saying so; a call must not clear the timer." }, async (t) => {
  const c = await owned(t);
  const s = await desk(t, cast.st4, c.claim);
  let timer = (await s.rows("#c-timers")).find((r) => r[0] === "acknowledgement");
  if (timer[1] !== Journey.before(t.stack.opening, -2) || timer[2] !== "") throw new Error(`the acknowledgement timer reads ${JSON.stringify(timer)}`);
  await s.act("#act", "record_contact", { "Claim": c.claim }, { purpose: "first call" });
  await s.done("#act-msg", "Done: record contact.");
  timer = (await s.rows("#c-timers")).find((r) => r[0] === "acknowledgement");
  if (timer[2] !== "") throw new Error("a phone call cleared the acknowledgement timer");
});

test({ id: "E2E-100", area: A, persona: "ST4", story: "US-ST04-1", title: "A claimant withdraws and the claim closes with nothing reserved",
  purpose: "Withdraw an open claim with the withdrawal reason code.",
  why: "A withdrawn claim must release its reserve and close, or the insurer carries a liability for a claim nobody is making." }, async (t) => {
  const c = await owned(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "decide_withdraw", { "Claim": c.claim, "Reason codes (comma separated)": "R-WDRAW" }, { purpose: "claimant asked" });
  await s.done("#act-msg", "Done: decide withdraw.");
  await s.sees(`${c.claim}: trauma, withdrawn`, "#c-title");
  const row = await summary(s);
  if (row.Reserve !== "0.00" || row.Closed !== t.stack.opening) throw new Error(`the withdrawn claim reads ${JSON.stringify(row)}`);
});

test({ id: "E2E-101", area: A, persona: "ST4", story: "US-ST14-1", title: "A closed claim is reopened on new evidence; an open one cannot be", matrix: ["CLM-015"],
  purpose: "Try to reopen an open claim, withdraw it, then reopen it on new evidence with a reserve.",
  why: "Reopening is how a complaint or new evidence reaches a closed claim; it must restore the reserve and never duplicate an open claim." }, async (t) => {
  const c = await owned(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "reopen", { "Claim": c.claim, "Basis": "new_evidence", "Reserve": "50000.00" }, { purpose: "new report" });
  await s.stopped("#act-msg", "the claim is still open", 409);
  await tells(t, cast.st4, { decide: { claim: c.claim, request: "withdraw", reasons: ["R-WDRAW"] } });
  await s.act("#act", "reopen", { "Claim": c.claim, "Basis": "new_evidence", "Reserve": "50000.00" }, { purpose: "new report" });
  await s.done("#act-msg", "Done: reopen.");
  await s.sees(`${c.claim}: trauma, open`, "#c-title");
  if ((await summary(s)).Reserve !== "50000.00") throw new Error("the reopened claim has no reserve");
});

test({ id: "E2E-102", area: A, persona: "ST4", story: "US-ST03-1", title: "A claim cannot be notified for an event that has not happened",
  purpose: "Notify a claim with an event date after today.",
  why: "A future event date is a typing error that would otherwise pass the cover check and reserve money for nothing." }, async (t) => {
  const policy = await t.policy("term_life", "cust-p18");
  const s = await desk(t, cast.st4);
  await s.act("#act", "notify", { "Claim": t.ref("CLM"), "Policy": policy.id, "Cover": "trauma", "Claim type": "trauma", "Event date": Journey.before(t.stack.opening, -20), "Channel": "phone", "Notified by": "policyholder" }, { purpose: "claimant rang" });
  await s.stopped("#act-msg", "the event date is after today", 409);
  if (await s.page.locator("#claim").isVisible()) throw new Error("a claim opened for a future event");
});

test({ id: "E2E-103", area: A, persona: "ST4", story: "US-ST03-1", title: "A claim reference is registered once",
  purpose: "Notify a second claim under a reference already in use.",
  why: "Two claims under one reference would merge two events; the second notification must be refused in words the assessor understands." }, async (t) => {
  const c = await notified(t);
  const s = await desk(t, cast.st4);
  await s.act("#act", "notify", { "Claim": c.claim, "Policy": c.policy.id, "Cover": "life", "Claim type": "death", "Event date": event(t), "Channel": "letter", "Notified by": "executor" }, { purpose: "letter received" });
  await s.stopped("#act-msg", `claim ${c.claim} is already registered`, 409);
});

test({ id: "E2E-104", area: A, persona: "ST4", story: "US-ST03-1", title: "A claim on a policy the insurer does not hold is refused plainly",
  purpose: "Notify a claim against a policy number that does not exist, and look up a claim that does not exist.",
  why: "A mistyped policy number must be caught at once, in words, and not open an orphan claim." }, async (t) => {
  const s = await desk(t, cast.st4);
  await s.act("#act", "notify", { "Claim": t.ref("CLM"), "Policy": "pol-never-issued", "Cover": "trauma", "Claim type": "trauma", "Event date": event(t), "Channel": "phone", "Notified by": "policyholder" }, { purpose: "claimant rang" });
  await s.stopped("#act-msg", "no partition holds the policy pol-never-issued", 404);
  await s.fill("#c-id", "CLM-never-notified");
  await s.press("Show", "#lookup");
  const words = await s.stopped("#c-msg", "", 404);
  if (/no such resource/i.test(words)) throw new Error(`the screen answers a mistyped claim reference with "${words}", which tells the assessor nothing`);
});

test({ id: "E2E-105", area: A, persona: "ST4", story: "US-ST04-1", title: "An income protection claim is admitted as a monthly benefit from the console", matrix: ["INC-001"],
  purpose: "Take an income protection claim through cause and cover check and admit it as a benefit in payment from the Act form.",
  why: "Income protection is paid monthly for years; if the console cannot admit a benefit in payment, case managers cannot do their main job at the screen." }, async (t) => {
  const c = await assessed(t, { kind: "income_protection", party: "cust-p1", cover: "income", type: "income_protection", cause: "CC-0204" });
  const s = await desk(t, cast.st4, c.claim);
  await s.sees(`${c.claim}: income protection, open`, "#c-title");
  const offered = await s.page.locator("#a-command option").allInnerTexts();
  const monthly = offered.find((o) => /benefit|monthly|periodic/i.test(o) && /admit/i.test(o));
  if (!monthly) {
    await s.act("#act", "decide_admit", { "Claim": c.claim, "Reason codes (comma separated)": "R-OK" }, { purpose: "decision" });
    const words = await s.stopped("#act-msg", "", 409);
    throw new Error(`the console offers no way to admit a benefit in payment (actions: ${offered.filter((o) => o.startsWith("decide")).join(", ")}); a plain admission answers "${words}"`);
  }
});

test({ id: "E2E-106", area: A, persona: "ST4", story: "US-ST04-1", title: "Premium is not waived on a claim that is not in payment",
  purpose: "Start a premium waiver on an income protection claim that has not been admitted.",
  why: "A waiver stops premium collection; granted before the benefit is in payment it would quietly lapse the insurer's income on a claim not yet accepted." }, async (t) => {
  const c = await assessed(t, { kind: "income_protection", party: "cust-p1", cover: "income", type: "income_protection", cause: "CC-0204" });
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "start_waiver", { "Claim": c.claim, "Waive from": event(t) }, { purpose: "waiver" });
  await s.stopped("#act-msg", "premiums are waived beside a benefit in payment", 409);
});

test({ id: "E2E-107", area: A, persona: "ST3", story: "US-PR1-1", title: "A provider is verified and paid a health claim in two payments", matrix: ["HLT-005"],
  purpose: "On an admitted hospital claim, verify the provider as payee and pay the admitted amount in two parts.",
  why: "Providers are paid directly so patients are not out of pocket; part payments must add up to the admitted amount and no further." }, async (t) => {
  const c = await healthClaim(t);
  await tells(t, cast.st3, { decide: { claim: c.claim, request: { admit: { amount_minor: null } }, reasons: ["R-OK"] } });
  const s = await desk(t, cast.st3, c.claim);
  await s.act("#act", "verify_payee", { "Claim": c.claim, "Payee (customer reference)": "prov-pr1", "Kind": "provider", "Account reference": "acct-pr1", "Name check reference": "nc-pr1", "Screening reference": "sc-pr1" }, { purpose: "payment" });
  await s.done("#act-msg", "Done: verify payee.");
  await s.sees("Payee: prov-pr1 (provider).", "#c-money");
  const payable = Number((await summary(s)).Payable);
  const first = (payable / 2).toFixed(2), rest = (payable - Number(first)).toFixed(2);
  await s.act("#act", "pay", { "Claim": c.claim, "Payment reference": "prov-1", "Amount": first }, { purpose: "payment" });
  await s.done("#act-msg", "Done: pay.");
  await s.act("#act", "pay", { "Claim": c.claim, "Payment reference": "prov-2", "Amount": rest }, { purpose: "payment" });
  await s.done("#act-msg", "Done: pay.");
  const row = await summary(s);
  if (Number(row.Paid) !== payable || row.Payable !== "0.00") throw new Error(`after both payments the claim reads ${JSON.stringify(row)}`);
});

test({ id: "E2E-108", area: A, persona: "ST3", story: "US-PR2-1", title: "An overpayment to a provider is raised and its repayment tracked", matrix: ["CLM-030"],
  purpose: "On a paid health claim, raise an overpayment for provider billing, read it as outstanding, and record part of it repaid.",
  why: "Overpayments are recovered, not forgotten; the claim must carry what is still owed until it is repaid or written off." }, async (t) => {
  const c = await healthClaim(t);
  await tells(t, cast.st3, { decide: { claim: c.claim, request: { admit: { amount_minor: null } }, reasons: ["R-OK"] } });
  await tells(t, cast.st3, { verify_payee: { claim: c.claim, party: "prov-pr2", kind: "provider", account_ref: "acct-pr2", name_check_ref: "nc", screening_ref: "sc", authority_doc: null } });
  const payable = (await work(t, c.claim, cast.st3)).payable_minor;
  await tells(t, cast.st3, { pay: { claim: c.claim, payment_ref: "prov-1", amount_minor: payable } });
  const s = await desk(t, cast.st3, c.claim);
  await s.sees("None.", "#c-over");
  await s.act("#act", "raise_overpayment", { "Claim": c.claim, "Overpayment reference": "op-1", "Amount": "100.00", "Cause": "provider_billing", "Received in good faith": "true" }, { purpose: "audit finding" });
  await s.done("#act-msg", "Done: raise overpayment.");
  let row = (await s.rows("#c-over")).find((r) => r[0] === "op-1");
  if (row[1] !== "100.00") throw new Error(`the overpayment reads ${JSON.stringify(row)}`);
  await s.act("#act", "record_repayment", { "Claim": c.claim, "Overpayment reference": "op-1", "Amount repaid": "40.00" }, { purpose: "provider repaid" });
  await s.done("#act-msg", "Done: record repayment.");
  row = (await s.rows("#c-over")).find((r) => r[0] === "op-1");
  if (row[1] !== "60.00") throw new Error(`after repayment the overpayment reads ${JSON.stringify(row)}`);
  await s.act("#act", "raise_overpayment", { "Claim": c.claim, "Overpayment reference": "op-2", "Amount": "999999.00", "Cause": "insurer_error", "Received in good faith": "true" }, { purpose: "audit finding" });
  await s.stopped("#act-msg", "an overpayment is new, positive and no more than was paid on the claim", 409);
});

test({ id: "E2E-109", area: A, persona: "ST4", story: "US-ST10-1", title: "A payment run with nothing due is not prepared",
  purpose: "Prepare a benefit run on a partition for a date on which no benefit is due.",
  why: "An empty run sent for approval wastes an approver's attention and hides the run that matters." }, async (t) => {
  const s = await desk(t, cast.st4);
  await s.act("#act", "prepare_run", { "Partition": "3", "Run reference": t.ref("RUN"), "Kind": "provider", "Benefit date": Journey.before(t.stack.opening, 400) }, { purpose: "monthly run" });
  await s.stopped("#act-msg", "no payment is due for this run", 409);
  await s.act("#act", "approve_run", { "Partition": "3", "Run reference": "run-never" }, { purpose: "monthly run" });
  await s.stopped("#act-msg", "there is no run run-never", 409);
});

test({ id: "E2E-110", area: A, persona: "ST4", story: "US-ST04-1", title: "The management report counts what was notified and decided, by assessor",
  purpose: "Notify and reserve a claim, then show the management report.",
  why: "Team leads steer by this report; it must reflect work done today across every partition of the ledger." }, async (t) => {
  const c = await owned(t);
  await tells(t, cast.st4, { decide: { claim: c.claim, request: { reserve: { amount_minor: 8000000 } }, reasons: ["R-EVID"] } });
  const s = await t.open("/claims", cast.st4);
  await s.tab("Reports");
  await s.pick("#b-what", "report");
  await s.press("Show", "#books");
  await s.sees("Notified", "#b-out");
  await s.sees("By assessor", "#b-out");
  await s.sees("st4", "#b-out");
  const parts = await s.page.locator("#b-out").getByText("Partition", { exact: false }).count();
  if (!parts) throw new Error("the report does not say which partition each part is for");
});

test({ id: "E2E-111", area: A, persona: "ST4", story: "US-ST12-2", title: "The book of claims in payment is shown for every partition",
  purpose: "Show the claims in payment book from the Reports view.",
  why: "The book of benefits in payment is the actuary's and the case managers' shared picture of the long-tail liability." }, async (t) => {
  const s = await t.open("/claims", cast.st4);
  await s.tab("Reports");
  await s.pick("#b-what", "book");
  await s.press("Show", "#books");
  await s.sees("In payment", "#b-out");
  await s.sees("Reviews overdue", "#b-out");
});

test({ id: "E2E-112", area: A, persona: "ST10", story: "US-ST13-1", title: "A finance operator is refused the claims console", matrix: ["SEC-004"],
  purpose: "Sign in to the claims console as the finance operator.",
  why: "Claims files hold health information; staff without the claims role must see a refusal and nothing behind it." }, async (t) => {
  const s = await t.open("/claims", cast.st10, 403);
  await s.refused("your roles do not include claims");
  await s.never("Work queue");
});

test({ id: "E2E-113", area: A, persona: "ST4", story: "US-ST04-1", title: "An action on a claim carries the reason the assessor gave",
  purpose: "Escalate a claim with a stated purpose, then try an action with no purpose at all.",
  why: "Every touch on a claim file is recorded with why; an action with no purpose is an access the insurer cannot later explain." }, async (t) => {
  const c = await owned(t);
  const s = await desk(t, cast.st4, c.claim);
  await s.act("#act", "escalate", { "Claim": c.claim }, { purpose: "claimant in hardship, needs a senior today" });
  await s.done("#act-msg", "Done: escalate.");
  await s.act("#act", "record_contact", { "Claim": c.claim }, { purpose: "" });
  const o = await s.outcome("#act-msg");
  s.expect(400, 409, 422);
  if (o.kind !== "bad") throw new Error("an action with no stated purpose was accepted");
});

module.exports = { notified, owned, assessed, admitted, healthClaim, tells, work, event, early, HEALTH };
