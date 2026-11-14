// The staff console: looking up a policy for a stated purpose, entering a
// change for a customer, a second member of staff agreeing it, a person's
// own access, personal connections, and the held balance workbench.
"use strict";
const { test } = require("../lib/suite");
const { cast } = require("../lib/cast");
const claims = require("./03-claims");

const A = "Staff console";

async function lookup(s, policy, { purpose = "CASE-1", scope } = {}) {
  await s.fill("#pol", policy); await s.fill("#case", purpose);
  if (scope) await s.fill("#scope", scope);
  await s.press("Open", "#lookup");
}
const request = (t, who, policy, id, kind, payload) => t.does(who, "POST", "/staff/v1/requests", { id, policy, kind, module: "policy-servicing", payload, reason: "customer rang", case: "CASE-1" }, "enters a change");
/// A customer whose paid health claim can have an overpayment raised on it.
async function paid(t) {
  const party = `cust-${t.ref("hb").toLowerCase()}`;
  const c = await claims.notified(t, { ...claims.HEALTH, party });
  for (const command of [{ assign: { claim: c.claim, to: "st3" } }, { record_cause: { claim: c.claim, cause_code: "CC-0204" } }, { cover_check: { claim: c.claim, relied_on: ["in_force"] } },
    { decide: { claim: c.claim, request: { admit: { amount_minor: null } }, reasons: ["R-OK"] } }, { verify_payee: { claim: c.claim, party: "prov-pr1", kind: "provider", account_ref: "a", name_check_ref: "n", screening_ref: "s", authority_doc: null } }]) await claims.tells(t, cast.st3, command);
  const payable = (await claims.work(t, c.claim, cast.st3)).payable_minor;
  await claims.tells(t, cast.st3, { pay: { claim: c.claim, payment_ref: "p1", amount_minor: payable } });
  return { party, claim: c.claim };
}
const overpaid = (t, claim, reference) => claims.tells(t, cast.st3, { raise_overpayment: { claim, overpayment: reference, amount_minor: 9000, cause: "provider_billing", good_faith: true } });
async function onBench(t, party, seconds = 10) {
  for (let n = 0; n < seconds * 5; n++) {
    const bench = await t.does(cast.st10, "GET", "/staff/v1/workbench");
    if (bench.parties.some((p) => p.party === party && p.net.NZD)) return true;
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}
/// A customer who owes an overpayment back: a party on the held balance workbench.
async function debtor(t) {
  const { party, claim } = await paid(t);
  await overpaid(t, claim, `op-${party}`);
  if (!(await onBench(t, party))) throw new Error(`the workbench never showed ${party}`);
  return party;
}
/// An adviser with commission held from them: a party on the workbench with money that can be acted on. The commission
/// module publishes what it holds to the workbench; this is that publication, sent as the module sends it, for a party
/// of the test's own (commission is only earned and held over months of business, which no single journey has).
async function withheld(t) {
  const party = t.ref("adv").toLowerCase();
  await t.sys.internal("commission", { workbench: { publish: { party, items: [{ id: "held/2027-10/base", kind: "held", amount_minor: 1_250_00, currency: "NZD", period: "2027-10", book: null, reason: "suspended_licence", since: "2027-11-01" }] } } }, `publish-${party}`);
  if (!(await onBench(t, party))) throw new Error(`the workbench never showed ${party}`);
  return party;
}
/// The person's recent access, once it lists what `wanted` looks for (the log is written a moment after the lookup).
async function recent(s, wanted) {
  let rows = [];
  for (let n = 0; n < 12; n++) {
    await s.tab("Policies"); await s.tab("My access");
    await s.page.waitForLoadState("networkidle");
    rows = await s.rows("#recent");
    if (wanted(rows)) return rows;
    await s.wait(1000);
  }
  return rows;
}
async function bench(t, who, party) {
  const s = await t.open("/staff", who);
  await s.tab("Held balances");
  if (party) { await s.row("#parties", party).getByRole("button", { name: "Open" }).click(); await s.sees(`${party}:`, "#pos-title"); }
  return s;
}

test({ id: "E2E-114", area: A, persona: "ST5", story: "US-ST05-1", title: "A service officer opens a policy for a named case and sees its covers and billing", matrix: ["SVC-001"],
  purpose: "Look up an in-force term life policy with a case reference and read what is shown.",
  why: "Every servicing call starts here; the officer must see the policy's standing, covers and what is owed, and nothing they have no purpose for." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const s = await t.open("/staff", cast.st5b);
  await lookup(s, p.id);
  await s.sees(p.number, "#pol-rows");
  const rows = await s.rows("#pol-rows");
  const kinds = rows.map((r) => r[0]);
  for (const kind of ["policy", "cover", "billing"]) if (!kinds.includes(kind)) throw new Error(`no ${kind} row is shown: ${kinds}`);
  await s.sees("in force", "#pol-rows");
  await s.sees("$500,000.00", "#pol-rows");
  await s.sees("Outstanding", "#pol-rows");
  if (rows.some((r) => r[0].includes("sensitive"))) throw new Error("a sensitive row is shown to a service officer");
});

test({ id: "E2E-115", area: A, persona: "ST5", story: "US-ST05-1", title: "A policy is not opened without the case being worked",
  purpose: "Try to look up a policy with the case field empty.",
  why: "Access with no stated purpose is browsing; the purpose is recorded with every lookup and reviewed." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const s = await t.open("/staff", cast.st5b);
  await s.fill("#pol", p.id);
  await s.press("Open", "#lookup");
  await s.wait(400);
  if (await s.page.locator("#pol-view").isVisible()) throw new Error("the policy opened with no case given");
  if (!(await s.page.locator("#case").evaluate((el) => !el.validity.valid))) throw new Error("the case field is not marked as needed");
});

test({ id: "E2E-116", area: A, persona: "ST10", story: "US-ST13-1", title: "A finance operator cannot open a customer's policy", matrix: [],
  purpose: "As the finance operator, look up a policy with a case reference.",
  why: "Finance works with ledgers, not customers' records; the refusal must say what to do if the access is really needed." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const s = await t.open("/staff", cast.st10);
  await lookup(s, p.id);
  await s.stopped("#lookup-msg", "None of your roles allows this action. Ask your manager for the role or a one-off grant.", 403);
  if (await s.page.locator("#pol-view").isVisible()) throw new Error("the policy is on screen");
});

test({ id: "E2E-117", area: A, persona: "Records", story: "US-ST13-1", title: "A role without personal data cannot open a policy", matrix: [],
  purpose: "As the records officer, whose role carries ordinary data only, look up a policy.",
  why: "Data classes are the second lock after the action; holding the action without the class must not open the record." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const s = await t.open("/staff", cast.records);
  await lookup(s, p.id);
  await s.stopped("#lookup-msg", "Your roles do not include the class of data this action needs.", 403);
});

test({ id: "E2E-118", area: A, persona: "ST7", story: "US-ST07-1", title: "Naming a queue you are not assigned to does not open a record", matrix: ["STF-003"],
  purpose: "Look up a policy claiming a queue that is not one of your paths.",
  why: "Access follows assigned work; a typed queue name must not be a way around it." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const s = await t.open("/staff", cast.st5b);
  await lookup(s, p.id, { scope: "queue:claims-life" });
  await s.stopped("#lookup-msg", "You have no path to this record: it is not in a case, queue, team, scheme or book assigned to you.", 403);
});

test({ id: "E2E-119", area: A, persona: "ST5", story: "US-ST05-1", title: "A mistyped policy number is answered in words the officer can act on",
  purpose: "Look up a policy number that does not exist.",
  why: "Mistyped numbers are the commonest slip on a call; the answer must say the policy was not found, not show a system message." }, async (t) => {
  const s = await t.open("/staff", cast.st5b);
  await lookup(s, "pol-does-not-exist");
  const words = await s.stopped("#lookup-msg", "", 404);
  if (/no such resource/i.test(words) || !/nothing was found|not found|no policy/i.test(words)) throw new Error(`the screen answers a mistyped policy number with "${words}"`);
});

test({ id: "E2E-120", area: A, persona: "ST5", story: "US-ST05-1", title: "The collections officer sees what is owed on a policy and no health information", matrix: ["COL-001"],
  purpose: "As the collections officer, open a policy that has a claim on it and read the rows.",
  why: "Collections needs the arrears and the standing; it must never be shown claim or health detail to do that job." }, async (t) => {
  const c = await claims.notified(t);
  const s = await t.open("/staff", cast.st5);
  await lookup(s, c.policy.id, { purpose: "arrears call" });
  await s.sees("Oldest unpaid due", "#pol-rows");
  await s.sees("Outstanding", "#pol-rows");
  const text = (await s.text("#pol-view")).toLowerCase();
  for (const word of ["trauma claim", "cancer", "cause", "diagnos", c.claim.toLowerCase()]) if (text.includes(word)) throw new Error(`the collections officer's view mentions "${word}"`);
});

test({ id: "E2E-121", area: A, persona: "ST5", story: "US-ST05-1", title: "A change is entered for the customer and goes to their app for approval", matrix: ["SVC-004"],
  purpose: "On an open policy, enter a change of payment frequency with the reason, and send it.",
  why: "Staff enter changes on a customer's behalf but never approve them for the customer; the screen must say where the request goes next." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const s = await t.open("/staff", cast.st5b);
  await lookup(s, p.id);
  await s.sees(p.number, "#pol-rows");
  await s.pick("#ckind", "change_frequency"); await s.fill("#cnote", "monthly to annual"); await s.fill("#creason", "customer asked on the phone");
  await s.press("Send to customer's app", "#for-customer");
  const words = await s.done("#cust-msg", "recorded. If it is above your limit or backdated, a colleague agrees it first; then it goes to the customer's app.");
  if (!/^Request [0-9a-f-]{36} recorded/.test(words)) throw new Error(`the confirmation gives no request reference: "${words}"`);
});

test({ id: "E2E-122", area: A, persona: "ST5", story: "US-ST05-1", title: "A change for a customer is not sent without the reason it was made",
  purpose: "Try to send a change for the customer with the reason left empty.",
  why: "The reason is recorded with the officer's name and the case; an unexplained change made on a customer's behalf cannot be defended later." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const s = await t.open("/staff", cast.st5b);
  await lookup(s, p.id);
  await s.sees(p.number, "#pol-rows");
  await s.press("Send to customer's app", "#for-customer");
  await s.wait(400);
  if (await s.page.locator("#cust-msg .banner").count()) throw new Error("the change was sent with no reason");
  if (!(await s.page.locator("#creason").evaluate((el) => !el.validity.valid))) throw new Error("the reason is not marked as needed");
});

test({ id: "E2E-123", area: A, persona: "ST17", story: "US-ST17-1", title: "The officer who entered a change cannot be its second approver", matrix: ["SVC-005"],
  purpose: "Enter a change that moves more than the officer's $500 limit, then try to agree it yourself.",
  why: "A change above an officer's limit needs a second person; the same person agreeing it twice is no second person." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const id = t.ref("REQ");
  await request(t, cast.st5l, p.id, id, "cancel_policy", { note: "cancel and refund", amount_minor: 600_00 });
  const s = await t.open("/staff", cast.st5l);
  await s.fill("#second-id", id);
  await s.press("Agree it", "#second");
  await s.stopped("#second-msg", "the person who entered a change is not its second approver", 422);
});

test({ id: "E2E-124", area: A, persona: "ST5", story: "US-ST17-1", title: "A team lead agrees a colleague's over-limit change, once", matrix: ["SVC-005"],
  purpose: "As the service team lead, agree a change a service officer entered above their limit; then agree it again.",
  why: "The second approval is what releases the change to the customer; it must work for the right person and only once." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const id = t.ref("REQ");
  await request(t, cast.st5b, p.id, id, "cancel_policy", { note: "cancel and refund", amount_minor: 600_00 });
  const s = await t.open("/staff", cast.st5l);
  await s.fill("#second-id", id);
  await s.press("Agree it", "#second");
  await s.done("#second-msg", "Agreed. It now waits for the customer's approval in their app.");
  await s.press("Agree it", "#second");
  await s.stopped("#second-msg", "that request is not waiting for a second member of staff", 409);
});

test({ id: "E2E-125", area: A, persona: "ST5", story: "US-ST17-1", title: "An officer without the approving role cannot agree a colleague's change",
  purpose: "As the collections officer, try to agree a change a service officer entered above their limit.",
  why: "Second approval is a named role, not a favour between colleagues at the same level." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const id = t.ref("REQ");
  await request(t, cast.st5b, p.id, id, "cancel_policy", { note: "cancel", amount_minor: 600_00 });
  const s = await t.open("/staff", cast.st5);
  await s.fill("#second-id", id);
  await s.press("Agree it", "#second");
  await s.stopped("#second-msg", "your roles do not include approving changes others entered", 403);
});

test({ id: "E2E-126", area: A, persona: "ST5", story: "US-ST17-1", title: "A change within the officer's limit does not wait for a second person",
  purpose: "Enter a change with no amount and today's date, then try to second-approve it as the team lead.",
  why: "Routing every small change through a second person would stall the service desk; only over-limit and backdated changes wait." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const id = t.ref("REQ");
  await request(t, cast.st5b, p.id, id, "change_frequency", { note: "to annual" });
  const s = await t.open("/staff", cast.st5l);
  await s.fill("#second-id", id);
  await s.press("Agree it", "#second");
  await s.stopped("#second-msg", "that request is not waiting for a second member of staff", 409);
});

test({ id: "E2E-127", area: A, persona: "ST5", story: "US-ST05-1", title: "A backdated change waits for a second member of staff", matrix: ["SVC-006"],
  purpose: "Enter a change dated a month in the past and agree it as the team lead.",
  why: "Backdating changes what the customer was covered for and what they owed; it always needs a second person." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const s = await t.open("/staff", cast.st5b);
  await lookup(s, p.id);
  await s.sees(p.number, "#pol-rows");
  await s.pick("#ckind", "decline_indexation"); await s.fill("#creason", "letter received late"); await s.fill("#ceffective", "2027-11-02");
  await s.press("Send to customer's app", "#for-customer");
  const id = (await s.done("#cust-msg", "recorded")).match(/Request ([0-9a-f-]{36})/)[1];
  const lead = await t.open("/staff", cast.st5l);
  await lead.fill("#second-id", id);
  await lead.press("Agree it", "#second");
  await lead.done("#second-msg", "Agreed.");
});

test({ id: "E2E-128", area: A, persona: "ST3", story: "US-ST03-1", title: "A claims assessor cannot enter servicing changes for a customer",
  purpose: "As a claims assessor, open a policy and try to send a change for the customer.",
  why: "Seeing a policy for a claim is not authority to change it; servicing changes belong to servicing roles." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const s = await t.open("/staff", cast.st3);
  await lookup(s, p.id, { purpose: "CLM-1" });
  await s.sees(p.number, "#pol-rows");
  await s.fill("#creason", "claimant asked");
  await s.press("Send to customer's app", "#for-customer");
  await s.stopped("#cust-msg", "None of your roles allows this action.", 403);
});

test({ id: "E2E-129", area: A, persona: "ST5", story: "US-ST07-1", title: "A person sees their own roles, authority profile and reach",
  purpose: "Open the My access view as the collections officer.",
  why: "Staff should never have to guess what the system will let them do; it shows them their team, roles, limits and when access is recertified." }, async (t) => {
  const s = await t.open("/staff", cast.st5);
  await s.tab("My access");
  await s.sees("collections", "#mine");
  const [row] = await s.rows("#mine");
  if (row[0] !== "operations" || row[1] !== "collections" || row[2] !== "standard v1" || !row[4]) throw new Error(`my access reads ${JSON.stringify(row)}`);
});

test({ id: "E2E-130", area: A, persona: "ST5", story: "US-ST13-1", title: "Every lookup is listed in the person's recent access, refused ones too", matrix: ["AUD-004"],
  purpose: "Open one policy successfully and be refused another for want of a path, then read Your recent access.",
  why: "Showing people their own access log is the cheapest deterrent to browsing, and how an honest mistake gets noticed." }, async (t) => {
  const p = await t.policy("term_life", "cust-p18");
  const s = await t.open("/staff", cast.st5l);
  await lookup(s, p.id, { purpose: t.ref("CASE") });
  await s.sees(p.number, "#pol-rows");
  // The access log keys a line by person, second, action and record, so a second decision on the same record inside
  // the same second is dropped as a repeat (reported to the portal's owner). A person takes longer than that.
  await s.wait(1100);
  await s.fill("#scope", "queue:not-mine");
  await s.press("Open", "#lookup");
  await s.stopped("#lookup-msg", "You have no path to this record", 403);
  const mine = (await recent(s, (rows) => rows.some((r) => r[2] === `policy:${p.id}` && r[3].startsWith("no:")))).filter((r) => r[2] === `policy:${p.id}`);
  if (!mine.some((r) => r[1] === "policy.view" && r[3] === "yes")) throw new Error(`no allowed lookup is listed: ${JSON.stringify(mine)}`);
  if (!mine.some((r) => r[3].startsWith("no:"))) throw new Error(`the refused lookup is not listed: ${JSON.stringify(mine)}`);
});

test({ id: "E2E-131", area: A, persona: "N7", story: "US-N07-1", title: "A member of staff cannot open their own policy", matrix: ["STF-006"],
  purpose: "As the assessor who is also a customer, look up the policy she holds.",
  why: "Staff are customers too; their own records are handled by colleagues, and the refusal tells them to use the app like any customer." }, async (t) => {
  const p = await t.policy("term_life", "n7");
  const s = await t.open("/staff", cast.n7);
  await lookup(s, p.id, { purpose: "checking my cover" });
  await s.stopped("#lookup-msg", "This record is your own or belongs to someone you have a personal connection with. Use the customer app for your own policies; a colleague must handle this one.", 403);
  const colleague = await t.open("/staff", cast.st5b);
  await lookup(colleague, p.id);
  await colleague.sees(p.number, "#pol-rows");
});

test({ id: "E2E-132", area: A, persona: "N7", story: "US-N07-1", title: "Declaring a personal connection closes that customer's records to the person", matrix: ["STF-006"],
  purpose: "Open a customer's policy, declare a personal connection with them, and try to open it again.",
  why: "A declared relative or former partner must be out of reach from the moment of the declaration, and the attempt must be on the record." }, async (t) => {
  const party = `cust-${t.ref("rel").toLowerCase()}`;
  const p = await t.policy("term_life", party);
  const s = await t.open("/staff", cast.st6);
  await lookup(s, p.id);
  await s.sees(p.number, "#pol-rows");
  await s.tab("My access");
  await s.fill("#conn", party);
  await s.press("Declare", "#connect");
  await s.done("#conn-msg", "Declared.");
  await s.sees(party, "#connections");
  await s.tab("Policies");
  await s.press("Open", "#lookup");
  await s.stopped("#lookup-msg", "This record is your own or belongs to someone you have a personal connection with.", 403);
  const refused = (await recent(s, (rows) => rows.some((r) => r[2] === `policy:${p.id}` && r[3].startsWith("no:")))).filter((r) => r[2] === `policy:${p.id}` && r[3].startsWith("no:"));
  if (!refused.length) throw new Error("the refused lookup of a connected customer is not in the access log");
  if (!refused.some((r) => r[4])) throw new Error(`the lookup of a connected customer is not flagged: ${JSON.stringify(refused)}`);
});

test({ id: "E2E-133", area: A, persona: "ST5", story: "US-ST07-1", title: "Only a staff administrator sees access administration",
  purpose: "Open the Access administration view as a service officer.",
  why: "Who holds which grant is itself sensitive; the view is for administrators and must say so to everyone else." }, async (t) => {
  const s = await t.open("/staff", cast.st5b);
  await s.tab("Access administration");
  await s.stopped("#msg", "you do not administer staff access", 403);
  if ((await s.rows("#grants")).length) throw new Error("grants are listed for a service officer");
});

test({ id: "E2E-134", area: A, persona: "ST7", story: "US-ST07-1", title: "An administrator approves a colleague's request to cover someone's work", matrix: ["STF-004"],
  purpose: "A service officer asks for a grant to cover a colleague for a day; the administrator finds it under Grants and approves it.",
  why: "Cover for leave is the commonest reason for extra access; it must be requested, approved by someone else, time-boxed and visible." }, async (t) => {
  const id = t.ref("GRANT");
  const now = Math.floor(Date.now() / 1000);
  await t.does(cast.st5b, "POST", "/staff/v1/access", { request_grant: { id, holder: "st5b", kind: { kind: "cover", covered: "st5" }, reason: "covering leave", from: now, to: now + 86400 } }, "asks for a grant");
  const s = await t.open("/staff", cast.admin);
  await s.tab("Access administration");
  await s.sees(id, "#grants");
  const row = s.row("#grants", id);
  await s.sees("requested", row);
  await s.stale();
  await row.getByRole("button", { name: "Approve" }).click();
  await s.done("#admin-msg", "Approved.");
  const after = (await s.rows("#grants")).find((r) => r[0] === id);
  if (after[1] !== "st5b" || after[2] === "requested") throw new Error(`after approval the grant reads ${JSON.stringify(after)}`);
  const holder = await t.open("/staff#my-access", cast.st5b);
  await holder.sees(id, "#mine");
});

test({ id: "E2E-135", area: A, persona: "ST10", story: "US-ST10-1", title: "The held balance workbench opens with the daily control and every party", matrix: [],
  purpose: "As the finance operator, open Held balances and read the daily control and the party list for a customer who owes an overpayment.",
  why: "Held and owed money is watched daily; the workbench must show every party with a position and what is open against them." }, async (t) => {
  const party = await debtor(t);
  const s = await bench(t, cast.st10);
  await s.sees("Daily control: held money by age");
  await s.sees(party, "#parties");
  const row = (await s.rows("#parties")).find((r) => r[0] === party);
  if (!row || row[1] !== "normal" || row[2] !== "none" || !/90\.00/.test(row[3]) || row[4] !== "0") throw new Error(`the party row reads ${JSON.stringify(row)}`);
});

test({ id: "E2E-136", area: A, persona: "ST10", story: "US-D06-1", title: "A party's position shows each line, where it came from and since when", matrix: [],
  purpose: "Open a party on the workbench and read its lines.",
  why: "Before releasing or offsetting anything the operator must see what makes up the balance and which module owns each line." }, async (t) => {
  const party = await debtor(t);
  const s = await bench(t, cast.st10, party);
  await s.sees(`${party}: normal`, "#pos-title");
  const [line] = await s.rows("#pos-items");
  if (line[0] !== `memo/claims/op-${party}` || line[1] !== "overpayment" || !line[4].includes("90.00") || line[6] !== t.stack.opening) throw new Error(`the line reads ${JSON.stringify(line)}`);
  await s.sees("Net position:", "#pos-summary");
});

test({ id: "E2E-137", area: A, persona: "ST5", story: "US-ST13-1", title: "A collections officer is refused the held balance workbench", matrix: [],
  purpose: "Open the Held balances view as the collections officer.",
  why: "The workbench moves money between the insurer and third parties; it is closed to roles that do not hold it." }, async (t) => {
  const s = await t.open("/staff", cast.st5);
  await s.tab("Held balances");
  await s.stopped("#msg", "None of your roles allows this action.", 403);
  if ((await s.rows("#parties")).length) throw new Error("parties are listed for a collections officer");
});

test({ id: "E2E-138", area: A, persona: "ST10", story: "US-ST17-1", title: "A hold pending review is prepared by one person and cannot be approved by them", matrix: ["ADV-024"],
  purpose: "Prepare a hold pending review on a party with a legal reference and a review date, then try to approve it yourself.",
  why: "Maker and checker must differ on every held balance action, or one person can freeze or release money alone." }, async (t) => {
  const party = await withheld(t);
  const s = await bench(t, cast.st10, party);
  await s.pick("#act", "hold_pending"); await s.fill("#ref", "LEGAL-77"); await s.fill("#review", "2027-12-15"); await s.fill("#reason", "DISPUTE"); await s.fill("#doc", "doc-hold-1");
  await s.press("Prepare", "#prepare");
  await s.done("#bench-msg", "Prepared. It now needs approval by someone else.");
  const [action] = await s.rows("#pos-actions");
  if (action[1] !== "hold pending" || action[3] !== "DISPUTE" || action[4] !== "st10" || !action[7].includes("awaiting approval")) throw new Error(`the action reads ${JSON.stringify(action)}`);
  await s.stale();
  await s.page.locator("#pos-actions").getByRole("button", { name: "Approve", exact: true }).click();
  const words = await s.stopped("#bench-msg", "", 403, 409, 422);
  if (!/prepared|maker|own|someone else/i.test(words)) throw new Error(`the refusal does not say why: "${words}"`);
});

test({ id: "E2E-139", area: A, persona: "ST10", story: "US-ST17-1", title: "A second person approves a prepared hold and the party's status changes", matrix: ["ADV-024"],
  purpose: "As the finance approver, open a party with a hold waiting and approve it.",
  why: "The approval is what puts the hold into force; the party must then show the review that is due and who approved." }, async (t) => {
  const party = await withheld(t);
  const id = t.ref("ACT");
  await t.does(cast.st10, "POST", "/staff/v1/workbench/actions", { prepare: { id, party, currency: "NZD", action: { action: "hold_pending", review_on: "2027-12-15", legal_reference: "LEGAL-78" }, reason_code: "DISPUTE", document: "doc-hold-2" } }, "prepares a hold");
  const s = await bench(t, cast.st10b, party);
  await s.stale();
  await s.row("#pos-actions", id).getByRole("button", { name: "Approve", exact: true }).click();
  await s.done("#bench-msg", "Approved.");
  const action = (await s.rows("#pos-actions")).find((r) => r[0] === id);
  if (!action[6].includes("st10b") || action[7].includes("awaiting")) throw new Error(`after approval the action reads ${JSON.stringify(action)}`);
  await s.sees("Review due 2027-12-15 (LEGAL-78)", "#pos-summary");
});

test({ id: "E2E-140", area: A, persona: "ST10", story: "US-ST17-1", title: "A prepared action is declined with a reason, asked for in the page's own dialog",
  purpose: "As the finance approver, decline a prepared hold and give the reason when asked.",
  why: "A decline with no reason teaches the maker nothing; the page asks for it and records it with the action." }, async (t) => {
  const party = await withheld(t);
  const id = t.ref("ACT");
  await t.does(cast.st10, "POST", "/staff/v1/workbench/actions", { prepare: { id, party, currency: "NZD", action: { action: "hold_pending", review_on: "2027-12-15", legal_reference: "LEGAL-79" }, reason_code: "DISPUTE", document: "doc-hold-3" } }, "prepares a hold");
  const s = await bench(t, cast.st10b, party);
  await s.stale();
  await s.row("#pos-actions", id).getByRole("button", { name: "Decline", exact: true }).click();
  await s.sees("Decline this action", "dialog[open]");
  await s.answer("no legal reference on file");
  await s.done("#bench-msg", "Declined.");
  const action = (await s.rows("#pos-actions")).find((r) => r[0] === id);
  if (!action[7].includes("declined")) throw new Error(`after the decline the action reads ${JSON.stringify(action)}`);
});

test({ id: "E2E-141", area: A, persona: "ST10", story: "US-ST10-1", title: "A write-off cannot be prepared for more than is held", matrix: [],
  purpose: "Prepare a write-off of $500 on a party from whom nothing is held.",
  why: "An action for more than the held balance would post money that does not exist; it is refused when prepared, not at approval." }, async (t) => {
  const party = await debtor(t);
  const s = await bench(t, cast.st10, party);
  await s.pick("#act", "write_off"); await s.fill("#amt", "500.00"); await s.fill("#reason", "SMALL"); await s.fill("#doc", "doc-wo-1");
  await s.press("Prepare", "#prepare");
  await s.stopped("#bench-msg", "the amount is more than the held balance not already committed to another action", 422);
  if ((await s.rows("#pos-actions")).some((r) => r[1] === "write off")) throw new Error("a write-off is listed");
});

test({ id: "E2E-142", area: A, persona: "ST10", story: "US-D09-1", title: "The proof of debt extract states what the party owes, line by line", matrix: ["ADV-020"],
  purpose: "Produce the proof of debt extract for a party who owes an overpayment.",
  why: "When a party fails, the insurer lodges this with the liquidator; it must agree to the position on screen and carry each line's origin." }, async (t) => {
  const party = await debtor(t);
  const s = await bench(t, cast.st10, party);
  await s.press("Proof of debt extract");
  await s.sees("Owed by party", "#proof-out");
  await s.sees(`commission:memo/claims/op-${party}`, "#proof-out");
  await s.sees(t.stack.opening, "#proof-out");
  await s.sees("90.00", "#proof-out");
});

test({ id: "E2E-143", area: A, persona: "ST10", story: "US-D06-1", title: "Two customers' overpayments both reach the workbench when their claims use the same reference", matrix: [],
  purpose: "Raise an overpayment called op-1 on two different customers' claims at the same moment and look for both customers on the held balance workbench.",
  why: "An overpayment reference is only unique within its claim; if the workbench keys on it alone, one customer's debt silently replaces the other's and is never recovered." }, async (t) => {
  const [first, second] = [await paid(t), await paid(t)];
  await Promise.all([overpaid(t, first.claim, "op-1"), overpaid(t, second.claim, "op-1")]);
  const seen = [await onBench(t, first.party, 8), await onBench(t, second.party, 8)];
  if (!seen[0] || !seen[1]) throw new Error(`only ${seen.filter(Boolean).length} of the two customers' $90.00 overpayments reached the workbench (first: ${seen[0]}, second: ${seen[1]})`);
  const s = await bench(t, cast.st10);
  await s.sees(first.party, "#parties"); await s.sees(second.party, "#parties");
});
