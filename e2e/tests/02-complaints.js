// Complaints and conduct: frontline staff record a complaint where they hear
// it, the complaints team works it to an outcome under the conduct regime's
// timers, legal handles forums, and the conduct function runs its cases.
"use strict";
const { test } = require("../lib/suite");
const { cast } = require("../lib/cast");

const A = "Complaints and conduct";
// The complaints module dates what happens by the calendar in Auckland, so the tests do too:
// for half of every day the date there is a day ahead of the date in UTC.
const auckland = new Intl.DateTimeFormat("en-CA", { timeZone: "Pacific/Auckland" });
const today = () => auckland.format(new Date());
const plus = (days) => new Date(Date.parse(`${today()}T00:00:00Z`) + days * 86400000).toISOString().slice(0, 10);
const COMMANDS = "/staff/v1/complaints/commands";

/// A complaint as frontline staff record it, sent by the same route the screen uses.
async function heard(t, id, extra = {}) {
  await t.does(cast.front, "POST", COMMANDS, { as: "intake", command: { record_complaint: { complaint: id, channel: "call", complainant: "cust-p1", complainant_kind: "policyholder", links: { policies: [] },
    category: { process: "service" }, summary_document: `doc-${id}`, vulnerable: false, adjustments: [], ...extra } } }, "records a complaint");
  return id;
}
const tells = (t, who, seat, command) => t.does(who, "POST", COMMANDS, { as: seat, command }, Object.keys(command)[0]);
/// A complaint the reviewer already owns.
async function owned(t, extra) {
  const id = await heard(t, t.ref("CMP"), extra);
  await tells(t, cast.st14, "complaints", { assign: { complaint: id, handler: "st14" } });
  return id;
}
/// ... and has decided in the complainant's favour, in writing.
async function upheld(t) {
  const id = await owned(t);
  await tells(t, cast.st14, "complaints", { decide: { complaint: id, outcome: "upheld", reason_codes: ["R-DELAY"], document: `out-${id}` } });
  return id;
}
async function workbench(t, who) { const s = await t.open("/complaints", who); await s.tab("Queue"); return s; }
async function file(s, id) { await s.fill("#o-id", id); await s.press("Open", "#open"); }

test({ id: "E2E-029", area: A, persona: "Frontline", story: "US-ST14-1", title: "Frontline staff record a complaint the moment they hear it", matrix: ["CMP-001"],
  purpose: "Sign in as frontline staff, fill the intake form for a complaint heard on a call, and record it.",
  why: "The conduct regime starts its clock when the insurer first hears a complaint; it must be recordable by whoever hears it, in one form, and say it was recorded." }, async (t) => {
  const s = await t.open("/complaints", cast.front);
  await s.sees("intake", "#seats");
  const id = t.ref("CMP");
  await s.fill("#i-id", id); await s.pick("#i-channel", "call"); await s.fill("#i-who", "cust-p18"); await s.pick("#i-process", "billing"); await s.fill("#i-doc", `doc-${id}`);
  await s.press("Record", "#intake");
  await s.done("#intake-msg", `Recorded ${id}. The complaints team will acknowledge it.`);
  // The form is ready for the next complaint: a fresh reference, and nothing of the last one's.
  const next = await s.page.inputValue("#i-id");
  if (!next || next === id || await s.page.inputValue("#i-doc")) throw new Error("the form kept the last complaint's details");
  const queue = await t.does(cast.st14, "GET", "/staff/v1/complaints/queue");
  const row = queue.find((q) => q.complaint === id);
  if (!row || row.process !== "billing" || row.next_due[0] !== "acknowledge") throw new Error(`the team's queue holds ${JSON.stringify(row)}`);
});

test({ id: "E2E-030", area: A, persona: "Frontline", story: "US-ST13-1", title: "Frontline staff can record a complaint but cannot read the register", matrix: ["CMP-009"],
  purpose: "As frontline staff, look for the queue, ask for the register and open the conduct cases view.",
  why: "Whoever hears a complaint records it, but the files hold health and conduct detail; intake must not become a way to browse them." }, async (t) => {
  const s = await workbench(t, cast.front);
  await s.never("Open", "#queue");
  if (await s.page.locator("#a-seat option").count()) throw new Error("frontline staff are offered a seat to act in");
  await s.tab("Register and report");
  await s.fill("#p-from", today()); await s.fill("#p-to", today());
  await s.press("Show", "#period");
  await s.stopped("#period-msg", "the register is open to the complaints team, legal and the conduct function", 403);
  await s.tab("Conduct cases");
  await s.sees("Conduct cases are open to the conduct function and legal only.", "#case-msg");
});

test({ id: "E2E-031", area: A, persona: "Frontline", story: "US-ST14-1", title: "A complaint reference cannot be used twice",
  purpose: "Record a complaint, then record another under the same reference.",
  why: "Two complaints under one reference would merge two people's files and one of them would lose their timers." }, async (t) => {
  const id = await heard(t, t.ref("CMP"));
  const s = await t.open("/complaints", cast.front);
  await s.fill("#i-id", id); await s.fill("#i-who", "cust-p18"); await s.fill("#i-doc", "doc-other");
  await s.press("Record", "#intake");
  await s.stopped("#intake-msg", "a complaint with that id is already recorded", 409);
});

test({ id: "E2E-032", area: A, persona: "Frontline", story: "US-ST14-1", title: "A complaint is not recorded without the complainant's own words",
  purpose: "Try to record a complaint with no summary document.",
  why: "The summary in the complainant's words is what the reviewer and any external scheme judge the handling against." }, async (t) => {
  const s = await t.open("/complaints", cast.front);
  const id = t.ref("CMP");
  await s.fill("#i-id", id); await s.fill("#i-who", "cust-p1");
  await s.press("Record", "#intake");
  await s.wait(500);
  if (await s.page.locator("#intake-msg .banner").count()) throw new Error("the form was sent without a summary document");
  if (!(await s.page.locator("#i-doc").evaluate((el) => !el.validity.valid))) throw new Error("the summary document is not marked as needed");
  const found = await t.does(cast.st14, "GET", "/staff/v1/complaints/queue");
  if (found.some((q) => q.complaint === id)) throw new Error("the complaint was recorded anyway");
});

test({ id: "E2E-033", area: A, persona: "ST14", story: "US-ST14-1", title: "The reviewer sees their seats, authority and training at the top of the workbench",
  purpose: "Sign in as the complaints reviewer and read the standing shown beside the page title.",
  why: "What a handler may decide and whether they may take vulnerable complainants comes from staff records; they must be able to see what the system believes." }, async (t) => {
  const s = await t.open("/complaints", cast.st14);
  await s.sees("intake, complaints. Authority level 3, trained to work with vulnerable customers.", "#seats");
  const junior = await t.open("/complaints", cast.st14j);
  await junior.sees("Authority level 1.", "#seats");
  await junior.never("trained to work with vulnerable customers", "#seats");
});

test({ id: "E2E-034", area: A, persona: "ST14", story: "US-ST14-1", title: "A vulnerable complainant is at the top of the queue with the adjustments they need", matrix: [],
  purpose: "Record an ordinary complaint and then a vulnerable complainant's, and read the queue's order and columns.",
  why: "The queue is how the team chooses what to work next; vulnerable complainants come first and the handler must see the adjustments before they call." }, async (t) => {
  const plain = await heard(t, t.ref("CMP"));
  const frail = await heard(t, t.ref("CMP"), { vulnerable: true, adjustments: ["large_print", "call_after_10am"] });
  const s = await workbench(t, cast.st14);
  await s.sees(frail, "#queue");
  const rows = await s.rows("#queue");
  const at = (id) => rows.findIndex((r) => r[0] === id);
  const row = rows[at(frail)];
  if (row[1] !== "unassigned" || row[3] !== "large_print, call_after_10am" || row[4] !== "yes") throw new Error(`the vulnerable complaint's row reads ${JSON.stringify(row)}`);
  if (!row[6].startsWith("acknowledge by ")) throw new Error(`next due reads "${row[6]}"`);
  if (at(plain) >= 0 && at(plain) < at(frail)) throw new Error("an ordinary complaint is listed before a vulnerable complainant's");
  if (rows.slice(0, at(frail)).some((r) => r[4] !== "yes")) throw new Error("a complaint that needs no trained handler is above a vulnerable complainant's");
});

test({ id: "E2E-035", area: A, persona: "ST14", story: "US-ST14-1", title: "The reviewer takes a complaint and opens its file with every timer running", matrix: ["CMP-002"],
  purpose: "Assign a new complaint to yourself from the Act form, open the file, and read its timers.",
  why: "Acknowledge, update, decide and deadlock dates are regulatory deadlines; the handler must see all four the moment the file is theirs." }, async (t) => {
  const id = await heard(t, t.ref("CMP"));
  const s = await workbench(t, cast.st14);
  await s.act("#act", "assign", { "Complaint": id, "Handler (staff reference)": "st14" });
  await s.done("#act-msg", "Done: assign.");
  await file(s, id);
  await s.sees(`${id}: open`, "#file-title");
  const timers = Object.fromEntries(await s.rows("#file-timers"));
  for (const [timer, days] of [["acknowledge", 5], ["update", 10], ["decide", 20], ["deadlock", 40]]) if (timers[timer] !== plus(days)) throw new Error(`${timer} is due ${timers[timer]}, expected ${plus(days)}`);
  await s.sees(`doc-${id}`, "#file-raw");
  await s.sees("handling the complaint", "#file-raw");
});

test({ id: "E2E-036", area: A, persona: "ST14", story: "US-ST13-1", title: "A complaint file opens only for the person it has been given to", matrix: ["CMP-009"],
  purpose: "As a member of the complaints team who is not the complaint's handler, try to open its file.",
  why: "Complaint files hold health and financial detail; being on the team is not a reason to read every file." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.st14j);
  await file(s, id);
  await s.stopped("#open-msg", "the case has not given this person access", 403);
  if (await s.page.locator("#file").isVisible()) throw new Error("the file is on screen for someone it was not given to");
});

test({ id: "E2E-037", area: A, persona: "ST14", story: "US-ST14-1", title: "Recording the acknowledgement stops the acknowledgement timer", matrix: ["CMP-002"],
  purpose: "Record the acknowledgement letter for a complaint and re-open the file.",
  why: "A timer that keeps running after the letter went out raises false breaches and hides real ones." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "record_acknowledgement", { "Complaint": id, "Acknowledgement document": `ack-${id}` });
  await s.done("#act-msg", "Done: record acknowledgement.");
  await file(s, id);
  await s.sees(`ack-${id}`, "#file-raw");
  const timers = (await s.rows("#file-timers")).map((r) => r[0]);
  if (timers.includes("acknowledge")) throw new Error("the acknowledgement timer is still running");
  if (!timers.includes("decide")) throw new Error(`the decision timer is gone: ${timers}`);
  await s.act("#act", "record_acknowledgement", { "Complaint": id, "Acknowledgement document": `ack2-${id}` });
  await s.stopped("#act-msg", "already acknowledged", 409);
});

test({ id: "E2E-038", area: A, persona: "ST14", story: "US-ST06-1", title: "A vulnerable complainant's file cannot be given to an untrained handler", matrix: [],
  purpose: "Try to assign a complaint with a vulnerability marker to the junior handler, who is not trained, then to the trained reviewer.",
  why: "The training requirement is read from staff records, not typed; an untrained handler on a vulnerable complainant's file is a conduct breach." }, async (t) => {
  const id = await heard(t, t.ref("CMP"), { vulnerable: true, adjustments: ["speak_slowly"] });
  const s = await workbench(t, cast.st14);
  await s.act("#act", "assign", { "Complaint": id, "Handler (staff reference)": "st14j" });
  await s.stopped("#act-msg", "a complaint with a vulnerability marker goes to trained staff", 409);
  await s.act("#act", "assign", { "Complaint": id, "Handler (staff reference)": "st14" });
  await s.done("#act-msg", "Done: assign.");
  await s.sees("st14", s.row("#queue", id));
});

test({ id: "E2E-039", area: A, persona: "ST14", story: "US-ST14-1", title: "A complaint cannot be given to someone who does not work complaints",
  purpose: "Try to assign a complaint to the finance operator.",
  why: "Assignment grants access to the file; it must go only to people the staff records seat in the complaints team." }, async (t) => {
  const id = await heard(t, t.ref("CMP"));
  const s = await workbench(t, cast.st14);
  await s.act("#act", "assign", { "Complaint": id, "Handler (staff reference)": "st10" });
  await s.stopped("#act-msg", "st10 does not work complaints", 403);
  await s.sees("unassigned", s.row("#queue", id));
});

test({ id: "E2E-040", area: A, persona: "ST14", story: "US-ST14-1", title: "Only the handler a complaint is assigned to may decide it",
  purpose: "As a team member who does not own the complaint, try to record its outcome.",
  why: "One named person is accountable for each outcome; a colleague deciding someone else's file breaks the audit trail." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.st14j);
  await s.act("#act", "decide", { "Complaint": id, "Outcome": "not_upheld", "Reason codes (comma separated)": "R-NONE", "Written outcome document": `out-${id}` });
  await s.stopped("#act-msg", "only the person the complaint is assigned to may do this", 409);
});

test({ id: "E2E-041", area: A, persona: "ST14", story: "US-ST14-1", title: "An outcome must be in writing", matrix: ["CMP-003"],
  purpose: "Decide a complaint without naming the written outcome, then with it.",
  why: "The written outcome carries the reasons and the external scheme's details; without it the complainant cannot escalate." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "decide", { "Complaint": id, "Outcome": "partly_upheld", "Reason codes (comma separated)": "R-DELAY" });
  await s.stopped("#act-msg", "an outcome is written, with the reasons and the external scheme's details", 409);
  await s.act("#act", "decide", { "Complaint": id, "Outcome": "partly_upheld", "Reason codes (comma separated)": "R-DELAY", "Written outcome document": `out-${id}` });
  await s.done("#act-msg", "Done: decide.");
  await file(s, id);
  await s.sees("partly upheld", "#file-raw");
  await s.sees(`out-${id}`, "#file-raw");
});

test({ id: "E2E-042", area: A, persona: "ST14", story: "US-ST14-1", title: "A decided complaint cannot be decided again",
  purpose: "Record a second outcome on a complaint that already has one.",
  why: "The outcome the complainant was sent must be the outcome on file; a silent second decision would rewrite it." }, async (t) => {
  const id = await upheld(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "decide", { "Complaint": id, "Outcome": "not_upheld", "Reason codes (comma separated)": "R-NONE", "Written outcome document": "out-2" });
  await s.stopped("#act-msg", "the complaint already has its outcome", 409);
});

test({ id: "E2E-043", area: A, persona: "ST14", story: "US-ST14-1", title: "A complaint cannot be closed until the outcome letter is proven delivered", matrix: ["CMP-003"],
  purpose: "Try to close a decided complaint before, and then after, recording that the outcome was delivered.",
  why: "Closing on an undelivered letter leaves a complainant who never heard the answer and a register that says they did." }, async (t) => {
  const id = await upheld(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "close", { "Complaint": id, "Cause code": "delay" });
  await s.stopped("#act-msg", "the outcome letter has no proof of delivery", 409);
  await s.act("#act", "record_outcome_delivered", { "Complaint": id, "Delivered on": today() });
  await s.done("#act-msg", "Done: record outcome delivered.");
  await s.act("#act", "close", { "Complaint": id, "Cause code": "delay" });
  await s.done("#act-msg", "Done: close.");
  await s.never(id, "#queue");
});

test({ id: "E2E-044", area: A, persona: "ST14", story: "US-ST14-1", title: "A complaint is closed only with a cause code from the controlled list", matrix: ["CMP-006"],
  purpose: "Close a complaint with a cause code that is not on the list.",
  why: "Root cause reporting to the board and the regulator is built from these codes; free text makes the report meaningless." }, async (t) => {
  const id = await upheld(t);
  await tells(t, cast.st14, "complaints", { record_outcome_delivered: { complaint: id, on: today() } });
  const s = await workbench(t, cast.st14);
  await s.act("#act", "close", { "Complaint": id, "Cause code": "bad-luck" });
  await s.stopped("#act-msg", "`bad-luck` is not a cause code", 409);
  await s.sees(id, "#queue");
});

test({ id: "E2E-045", area: A, persona: "ST14", story: "US-ST14-1", title: "A delivery date in the future is not proof of delivery",
  purpose: "Record the outcome as delivered tomorrow.",
  why: "Proof of delivery is evidence; a date that has not happened is a typing mistake that would let the file close early." }, async (t) => {
  const id = await upheld(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "record_outcome_delivered", { "Complaint": id, "Delivered on": plus(2) });
  await s.stopped("#act-msg", "the delivery date is before the outcome or in the future", 409);
});

test({ id: "E2E-046", area: A, persona: "ST14", story: "US-ST14-1", title: "A goodwill payment within the reviewer's limit is recorded against the complaint", matrix: ["CMP-004"],
  purpose: "Record an ex gratia payment of $250 on an upheld complaint, then mark it done and close the complaint.",
  why: "Remediation is what the complainant actually receives; the complaint must not close while it is outstanding, and must close once it is done." }, async (t) => {
  const id = await upheld(t);
  await tells(t, cast.st14, "complaints", { record_outcome_delivered: { complaint: id, on: today() } });
  const s = await workbench(t, cast.st14);
  await s.act("#act", "record_remediation", { "Complaint": id, "Kind": "ex_gratia", "Reference in the owning module": `pay-${id}`, "Amount": "250.00" });
  await s.done("#act-msg", "Done: record remediation.");
  await s.act("#act", "close", { "Complaint": id, "Cause code": "delay" });
  await s.stopped("#act-msg", "remediation is still outstanding", 409);
  await s.act("#act", "complete_remediation", { "Complaint": id, "Reference": `pay-${id}` });
  await s.done("#act-msg", "Done: complete remediation.");
  await s.act("#act", "close", { "Complaint": id, "Cause code": "delay" });
  await s.done("#act-msg", "Done: close.");
});

test({ id: "E2E-047", area: A, persona: "ST14", story: "US-ST14-1", title: "A goodwill payment above the authoriser's limit is refused", matrix: ["CMP-004"],
  purpose: "As the reviewer (limit $5,000) record a $6,000 goodwill payment; then name the junior handler (limit $500) as authoriser of $600.",
  why: "Goodwill limits are the control on money leaving through complaints; the limit applied must be the authoriser's from staff records, whoever types the command." }, async (t) => {
  const id = await upheld(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "record_remediation", { "Complaint": id, "Kind": "ex_gratia", "Reference in the owning module": `pay-${id}`, "Amount": "6000.00" });
  await s.stopped("#act-msg", "the goodwill payment is above the authoriser's limit", 409);
  await s.act("#act", "record_remediation", { "Complaint": id, "Kind": "ex_gratia", "Reference in the owning module": `pay-${id}`, "Amount": "600.00", "Authorised by (staff reference; blank for you)": "st14j" });
  await s.stopped("#act-msg", "the goodwill payment is above the authoriser's limit", 409);
  await s.act("#act", "record_remediation", { "Complaint": id, "Kind": "ex_gratia", "Reference in the owning module": `pay-${id}`, "Amount": "450.00", "Authorised by (staff reference; blank for you)": "st14j" });
  await s.done("#act-msg", "Done: record remediation.");
});

test({ id: "E2E-048", area: A, persona: "ST14", story: "US-ST14-1", title: "Remediation needs an outcome in the complainant's favour",
  purpose: "Record a refund on a complaint that has no outcome yet.",
  why: "Paying before deciding hides the decision; remediation follows a recorded outcome so the reason for the money is on file." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "record_remediation", { "Complaint": id, "Kind": "refund", "Reference in the owning module": `ref-${id}`, "Amount": "80.00" });
  await s.stopped("#act-msg", "remediation follows an outcome in the complainant's favour or a binding decision", 409);
});

test({ id: "E2E-049", area: A, persona: "ST14", story: "US-ST14-1", title: "A deadlocked complaint cannot be closed quietly and carries its notice", matrix: ["CMP-005"],
  purpose: "Try to deadlock a complaint before its time, then deadlock one whose written outcome the complainant has received, try to close it, record the deadlock notice with the scheme's details, and read the file.",
  why: "Deadlock is the complainant's gateway to the external scheme, given once the insurer's final answer has been received or its time has run out; the file must stay open and show the notice that tells them how to use it." }, async (t) => {
  const early = await owned(t);
  const id = await upheld(t);
  await tells(t, cast.st14, "complaints", { record_outcome_delivered: { complaint: id, on: today() } });
  const s = await workbench(t, cast.st14);
  await s.act("#act", "declare_deadlock", { "Complaint": early });
  await s.stopped("#act-msg", "is deadlocked only once the date to decide it by", 409);
  await s.act("#act", "declare_deadlock", { "Complaint": id });
  await s.done("#act-msg", "Done: declare deadlock.");
  await s.act("#act", "declare_deadlock", { "Complaint": id });
  await s.stopped("#act-msg", "this complaint cannot be deadlocked: it is already deadlocked", 409);
  await s.act("#act", "close", { "Complaint": id, "Cause code": "delay" });
  await s.stopped("#act-msg", "the deadlock notice has not been sent", 409);
  await s.act("#act", "record_deadlock_notice", { "Complaint": id, "Deadlock notice document": `dl-${id}` });
  await s.done("#act-msg", "Done: record deadlock notice.");
  await s.act("#act", "record_deadlock_notice", { "Complaint": id, "Deadlock notice document": `dl2-${id}` });
  await s.stopped("#act-msg", "no deadlock is waiting for its notice", 409);
  await file(s, id);
  await s.sees(`${id}: deadlocked`, "#file-title");
  await s.sees(`dl-${id}`, "#file-raw");
});

test({ id: "E2E-050", area: A, persona: "ST14", story: "US-ST14-1", title: "Deadlock waits until the complainant has received the written outcome",
  purpose: "Declare deadlock on a complaint decided in writing whose outcome has not yet been delivered.",
  why: "The scheme's deadlock letter follows the insurer's final answer; sent before the complainant has it, it would cut short the internal process and misstate the register the regulator reads." }, async (t) => {
  const id = await upheld(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "declare_deadlock", { "Complaint": id });
  await s.stopped("#act-msg", "deadlock follows a written outcome the complainant has received and rejected; record the outcome's delivery first", 409);
});

test({ id: "E2E-051", area: A, persona: "ST14", story: "US-ST14-1", title: "The complaints team refers a complaint to the external scheme but not to court", matrix: ["CMP-007"],
  purpose: "Refer a complaint to court as the complaints team, which is refused, then to the dispute scheme.",
  why: "Court proceedings and privacy complaints are legal's to record; the scheme referral is the team's daily work and starts the forum's deadlines." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "refer", { "Complaint": id, "Forum": "court", "Forum's reference": "CIV-2027-1", "Response due": plus(20) });
  await s.stopped("#act-msg", "court proceedings and privacy complaints are recorded by legal", 409);
  await s.act("#act", "refer", { "Complaint": id, "Forum": "scheme", "Forum's reference": `IFSO-${id}`, "Response due": plus(20) });
  await s.done("#act-msg", "Done: refer.");
  await file(s, id);
  await s.sees(`IFSO-${id}`, "#file-raw");
  await s.act("#act", "refer", { "Complaint": id, "Forum": "scheme", "Forum's reference": "IFSO-again", "Response due": plus(20) });
  await s.stopped("#act-msg", "the complaint is already with an external forum", 409);
});

test({ id: "E2E-052", area: A, persona: "Legal", story: "US-ST14-1", title: "Legal records court proceedings against a complaint", matrix: ["CMP-007"],
  purpose: "Sign in as legal counsel and refer a complaint to court from the legal seat.",
  why: "A complaint in court is handled differently from then on; only legal may put it there." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.legal);
  await s.sees("legal", "#seats");
  await s.act("#act", "refer", { "Complaint": id, "Forum": "court", "Forum's reference": `CIV-${id}`, "Response due": plus(25) });
  await s.done("#act-msg", "Done: refer.");
  const owner = await workbench(t, cast.st14);
  await file(owner, id);
  await owner.sees(`CIV-${id}`, "#file-raw");
  await owner.sees("court", "#file-raw");
});

test({ id: "E2E-053", area: A, persona: "Legal", story: "US-R02-1", title: "A bundle for an external forum is reviewed by someone other than its releaser", matrix: ["CMP-008"],
  purpose: "Release a document bundle to the scheme naming yourself as reviewer, then naming a colleague.",
  why: "A bundle leaves the insurer with customer records in it; a second pair of eyes is the control against sending the wrong person's documents." }, async (t) => {
  const id = await owned(t);
  await tells(t, cast.st14, "complaints", { refer: { complaint: id, forum: "scheme", reference: `IFSO-${id}`, response_due: plus(20), documents_due: null } });
  const s = await workbench(t, cast.legal);
  await s.act("#act", "release_bundle", { "Complaint": id, "Bundle": `b-${id}`, "Documents (comma separated)": `doc-${id}, ack-${id}`, "Reviewed by (someone else)": "legal" });
  await s.stopped("#act-msg", "a bundle is reviewed by someone other than the person releasing it", 409);
  await s.act("#act", "release_bundle", { "Complaint": id, "Bundle": `b-${id}`, "Documents (comma separated)": `doc-${id}, ack-${id}`, "Reviewed by (someone else)": "st14" });
  await s.done("#act-msg", "Done: release bundle.");
});

test({ id: "E2E-054", area: A, persona: "ST14", story: "US-ST14-1", title: "A binding forum decision keeps the complaint open until it is carried out", matrix: ["CMP-007"],
  purpose: "Record the scheme's binding decision for the complainant, try to close, record the remediation as done, and close.",
  why: "A binding decision the insurer has not implemented is the most serious state a complaint can be in; the system must hold it open." }, async (t) => {
  const id = await owned(t);
  await tells(t, cast.st14, "complaints", { refer: { complaint: id, forum: "scheme", reference: `IFSO-${id}`, response_due: plus(20), documents_due: null } });
  const s = await workbench(t, cast.st14);
  await s.act("#act", "close", { "Complaint": id, "Cause code": "wrong_decision" });
  await s.stopped("#act-msg", "the external forum has not decided", 409);
  await s.act("#act", "record_forum_decision", { "Complaint": id, "Outcome": "upheld", "Binding": "true", "Remediation due (optional)": plus(14) });
  await s.done("#act-msg", "Done: record forum decision.");
  await s.act("#act", "close", { "Complaint": id, "Cause code": "wrong_decision" });
  await s.stopped("#act-msg", "the forum's decision has not been implemented", 409);
  await s.act("#act", "complete_forum_remediation", { "Complaint": id });
  await s.stopped("#act-msg", "the remediation is not recorded as done", 409);
  await s.act("#act", "record_remediation", { "Complaint": id, "Kind": "decision_reopened", "Reference in the owning module": `clm-${id}`, "Amount": "0" });
  await s.done("#act-msg", "Done: record remediation.");
  await s.act("#act", "complete_remediation", { "Complaint": id, "Reference": `clm-${id}` });
  await s.done("#act-msg", "Done: complete remediation.");
  await s.act("#act", "complete_forum_remediation", { "Complaint": id });
  await s.done("#act-msg", "Done: complete forum remediation.");
  await s.act("#act", "close", { "Complaint": id, "Cause code": "wrong_decision" });
  await s.done("#act-msg", "Done: close.");
});

test({ id: "E2E-055", area: A, persona: "ST14", story: "US-ST14-1", title: "The register and the report for a period agree with the complaints worked", matrix: ["CMP-006"],
  purpose: "Take a complaint from receipt to closure, then show the register and report for today.",
  why: "The register is what the regulator inspects and the report is what the board reads; both must show the complaint with its outcome, days taken and cause." }, async (t) => {
  const id = await upheld(t);
  await tells(t, cast.st14, "complaints", { record_acknowledgement: { complaint: id, document: `ack-${id}` } });
  await tells(t, cast.st14, "complaints", { record_outcome_delivered: { complaint: id, on: today() } });
  await tells(t, cast.st14, "complaints", { close: { complaint: id, detail: { cause: "poor_explanation", product_version: null, rule_version: null, template_version: null } } });
  const s = await t.open("/complaints", cast.st14);
  await s.tab("Register and report");
  await s.fill("#p-from", today()); await s.fill("#p-to", today());
  await s.press("Show", "#period");
  await s.sees(id, "#register");
  const row = (await s.rows("#register")).find((r) => r[0] === id);
  if (row[1] !== today() || row[2] !== "call" || row[3] !== "service" || row[4] !== "closed" || row[5] !== "upheld" || row[6] !== "0" || row[7] !== "0" || row[8] !== today()) throw new Error(`the register row reads ${JSON.stringify(row)}`);
  await s.sees("Acknowledged on time", "#report");
  await s.sees("Poor explanation", "#report");
  const received = Number((await s.text("#report")).match(/Received\s+(\d+)/)[1]);
  const listed = (await s.rows("#register")).length;
  if (received !== listed) throw new Error(`the report counts ${received} received, the register lists ${listed}`);
});

test({ id: "E2E-056", area: A, persona: "ST14", story: "US-ST14-1", title: "The register refuses a period it cannot read",
  purpose: "Ask for the register with the dates the wrong way round.",
  why: "A regulator's request names a period; a period that silently returns nothing looks like a clean month." }, async (t) => {
  const s = await t.open("/complaints", cast.st14);
  await s.tab("Register and report");
  await s.fill("#p-from", plus(1)); await s.fill("#p-to", plus(-30));
  await s.press("Show", "#period");
  const o = await s.outcome("#period-msg").catch(() => null);
  s.expect(400, 403, 409, 422);
  if (!o || o.kind !== "bad") throw new Error("a period that ends before it starts was accepted without a word");
  if (!/before it starts/i.test(o.words)) throw new Error(`the refusal does not say what is wrong with the period: "${o.words}"`);
});

test({ id: "E2E-057", area: A, persona: "ST14", story: "US-ST14-1", title: "Every complaint a customer has made is found from their reference",
  purpose: "Record two complaints from the same customer and look the customer up.",
  why: "A repeat complainant is a signal of harm; the handler must see the history before they call." }, async (t) => {
  const who = `cust-${t.ref("rep")}`;
  const a = await heard(t, t.ref("CMP"), { complainant: who }), b = await heard(t, t.ref("CMP"), { complainant: who, channel: "email" });
  const s = await t.open("/complaints", cast.st14);
  await s.tab("Register and report");
  await s.fill("#of-party", who);
  await s.press("Find", "#of");
  await s.sees(a, "#of-out"); await s.sees(b, "#of-out");
  const rows = await s.rows("#of-out");
  if (rows.length !== 2) throw new Error(`the customer has ${rows.length} complaints listed, expected 2`);
});

test({ id: "E2E-058", area: A, persona: "ST14", story: "US-ST14-1", title: "The complainant's own record leaves out who handled the file and what staff wrote",
  purpose: "Open the complainant's own record for a complaint and compare it with the working file.",
  why: "A complainant may ask for what the insurer holds on their complaint; the extract must carry their record and not internal access grants or staff names." }, async (t) => {
  const id = await upheld(t);
  const s = await workbench(t, cast.st14);
  await s.fill("#o-id", id);
  await s.press("Complainant's own record", "#open");
  await s.sees("The complainant's own record", "#file-title");
  await s.sees(`out-${id}`, "#file-raw");
  await s.sees("upheld", "#file-raw");
  await s.never("handling the complaint", "#file-raw");
  await s.never("Owner authority", "#file-raw");
  await s.never("Recorded by", "#file-raw");
});

test({ id: "E2E-059", area: A, persona: "ST14", story: "US-ST13-1", title: "Access to a complaint file is granted by its handler, for a purpose, and never to oneself", matrix: ["CMP-009"],
  purpose: "As the handler, grant yourself access (refused), grant the junior handler access for a stated purpose, and check the junior can now open the file; then revoke it.",
  why: "Every extra reader of a complaint file is a privacy decision; it must be made by the owner, recorded with its purpose, and reversible." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "grant_access", { "Complaint": id, "To (staff reference)": "st14", "Data classes (personal, financial, insurance_sensitive, clinical)": "personal", "Purpose": "curiosity" });
  await s.stopped("#act-msg", "nobody grants themselves access", 409);
  await s.act("#act", "grant_access", { "Complaint": id, "To (staff reference)": "st14j", "Data classes (personal, financial, insurance_sensitive, clinical)": "personal", "Purpose": "covering leave" });
  await s.done("#act-msg", "Done: grant access.");
  const junior = await workbench(t, cast.st14j);
  await file(junior, id);
  await junior.sees(`${id}: open`, "#file-title");
  await junior.sees("covering leave", "#file-raw");
  await s.act("#act", "revoke_access", { "Complaint": id, "From (staff reference)": "st14j" });
  await s.done("#act-msg", "Done: revoke access.");
  await file(junior, id);
  await junior.stopped("#open-msg", "the case has not given this person access", 403);
});

test({ id: "E2E-060", area: A, persona: "ST14", story: "US-ST14-1", title: "Waiting on the complainant is recorded once and ends when they answer",
  purpose: "Ask the complainant for information, ask again (refused), and record their response.",
  why: "Time spent waiting on the complainant is treated differently by the timers; it must be recorded exactly, not inferred." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "ask_complainant", { "Complaint": id });
  await s.done("#act-msg", "Done: ask complainant.");
  await s.act("#act", "ask_complainant", { "Complaint": id });
  await s.stopped("#act-msg", "the complainant has already been asked", 409);
  await s.act("#act", "record_complainant_response", { "Complaint": id });
  await s.done("#act-msg", "Done: record complainant response.");
  await s.act("#act", "record_complainant_response", { "Complaint": id });
  await s.stopped("#act-msg", "nothing was asked of the complainant", 409);
});

test({ id: "E2E-061", area: A, persona: "ST14", story: "US-ST14-1", title: "A representative is added to a complaint with the complainant's authority", matrix: ["CMP-001"],
  purpose: "Add a representative with an authority document, then add the same representative again.",
  why: "Letters and calls go to the representative from then on; it must rest on a recorded authority and never be duplicated." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.st14);
  const fields = { "Complaint": id, "Representative (customer reference)": "cust-p18", "Relationship": "daughter", "Authority document": `auth-${id}` };
  await s.act("#act", "add_representative", fields);
  await s.done("#act-msg", "Done: add representative.");
  await s.act("#act", "add_representative", fields);
  await s.stopped("#act-msg", "that representative is already recorded", 409);
  await file(s, id);
  await s.sees("daughter", "#file-raw");
});

test({ id: "E2E-062", area: A, persona: "ST14", story: "US-ST06-1", title: "A vulnerability marker added later changes who may handle the complaint", matrix: [],
  purpose: "As the trained reviewer, mark an owned complaint's complainant as vulnerable with adjustments and read the queue.",
  why: "Vulnerability is often learned mid-complaint; from that moment the queue must show the adjustments and require a trained handler." }, async (t) => {
  const id = await owned(t);
  const s = await workbench(t, cast.st14);
  await s.act("#act", "set_vulnerability", { "Complaint": id, "Vulnerable": "true", "Adjustments (comma separated)": "written_only" });
  await s.done("#act-msg", "Done: set vulnerability.");
  const row = (await s.rows("#queue")).find((r) => r[0] === id);
  if (row[3] !== "written_only" || row[4] !== "yes") throw new Error(`the queue row reads ${JSON.stringify(row)}`);
});

test({ id: "E2E-063", area: A, persona: "ST14", story: "US-ST14-1", title: "A month's root cause review is recorded once", matrix: ["CMP-006"],
  purpose: "Record this month's root cause review with an action, then try to record it again.",
  why: "The monthly review is the evidence that complaints change the business; a second record for the same month would overwrite the first." }, async (t) => {
  const s = await workbench(t, cast.st14);
  const month = today().slice(0, 7);
  const fields = { "Month (YYYY-MM)": month, "Actions, as JSON: [{\"kind\":\"training\",\"cause\":\"delay\",\"reference\":\"T-1\"}]": JSON.stringify([{ kind: "training", cause: "delay", reference: `T-${t.options.run}` }]) };
  await s.act("#act", "record_root_cause_review", fields);
  s.expect(409);
  const first = await s.outcome("#act-msg");
  if (first.kind === "bad" && !first.words.includes("that month's review is already recorded")) throw new Error(`the screen refused: ${first.words}`);
  await s.act("#act", "record_root_cause_review", fields);
  await s.stopped("#act-msg", "that month's review is already recorded", 409);
});

test({ id: "E2E-064", area: A, persona: "Conduct", story: "US-ST13-1", title: "The conduct officer opens a breach case and finds it among the open cases", matrix: ["CMP-009"],
  purpose: "Sign in as the conduct officer, open a conduct case for a breach that the regulator must hear of, and open it from the Conduct cases view.",
  why: "A breach starts regulatory clocks; the case must exist from the moment the conduct function records it, with its owner and what is outstanding." }, async (t) => {
  const id = t.ref("CON");
  const s = await workbench(t, cast.conduct);
  await s.sees("conduct", "#seats");
  await s.act("#act", "open_conduct_case", { "Conduct case": id, "Kind": "breach", "Owner (staff reference)": "conduct", "Regulator to be notified": "true" });
  await s.done("#act-msg", "Done: open conduct case.");
  await s.tab("Conduct cases");
  await s.row("#cases", id).getByRole("button", { name: "Open" }).click();
  await s.sees(`${id}: breach`, "#case-title");
  await s.sees("Owner conduct.", "#case-summary");
  await s.sees("population not complete", "#case-summary");
});

test({ id: "E2E-065", area: A, persona: "ST14", story: "US-ST13-1", title: "The complaints team cannot see conduct cases or record a breach", matrix: ["CMP-009"],
  purpose: "As the complaints reviewer, open the Conduct cases view and try to open a breach case from the complaints seat.",
  why: "Conduct cases can concern colleagues and advisers under investigation; the complaints team raises systemic findings but must not see or create breach cases." }, async (t) => {
  const s = await workbench(t, cast.st14);
  await s.act("#act", "open_conduct_case", { "Conduct case": t.ref("CON"), "Kind": "breach", "Owner (staff reference)": "st14", "Regulator to be notified": "false" });
  await s.stopped("#act-msg", "a breach or incident is recorded by the conduct function", 409);
  await s.tab("Conduct cases");
  await s.sees("Conduct cases are open to the conduct function and legal only.", "#case-msg");
  if ((await s.rows("#cases")).length) throw new Error("conduct cases are listed for the complaints team");
});

test({ id: "E2E-066", area: A, persona: "Conduct", story: "US-ST16-1", title: "A conduct case cannot be closed while the regulator has not been told", matrix: ["CMP-009"],
  purpose: "Open a breach case that needs regulator notification and try to close it with findings; record the notification; read the case.",
  why: "Closing a breach before the regulator is notified is itself a breach; the system must hold the case open and say what is outstanding." }, async (t) => {
  const id = t.ref("CON");
  await tells(t, cast.conduct, "conduct", { open_conduct_case: { case: id, kind: "breach", owner: "conduct", summary_document: null, notify_regulator: true } });
  const s = await workbench(t, cast.conduct);
  await s.act("#act", "close_conduct_case", { "Conduct case": id, "Findings document": `find-${id}` });
  const words = await s.stopped("#act-msg", "", 409);
  if (!/not complete|regulator has not been notified/.test(words)) throw new Error(`the refusal does not say what is outstanding: "${words}"`);
  await s.act("#act", "record_regulator_notification", { "Conduct case": id, "Regulator's reference": `FMA-${id}`, "Notification document": `not-${id}` });
  await s.done("#act-msg", "Done: record regulator notification.");
  await s.tab("Conduct cases");
  await s.row("#cases", id).getByRole("button", { name: "Open" }).click();
  await s.sees(`FMA-${id}`, "#case-raw");
});

test({ id: "E2E-067", area: A, persona: "Conduct", story: "US-ST16-1", title: "A regulator's request is tracked against the case until it is answered", matrix: ["CMP-009"],
  purpose: "Record a request from the regulator with a due date on a conduct case, answer it, and try to answer it again.",
  why: "A missed regulator deadline is the most visible failure a conduct function can have; each request must be tracked singly to its answer." }, async (t) => {
  const id = t.ref("CON");
  await tells(t, cast.conduct, "conduct", { open_conduct_case: { case: id, kind: "incident", owner: "conduct", summary_document: null, notify_regulator: false } });
  const s = await workbench(t, cast.conduct);
  await s.act("#act", "record_regulator_request", { "Conduct case": id, "Request": "RQ-1", "Due": plus(10), "Request document": `rq-${id}` });
  await s.done("#act-msg", "Done: record regulator request.");
  await s.act("#act", "respond_to_regulator", { "Conduct case": id, "Request": "RQ-1", "Response document": `rs-${id}` });
  await s.done("#act-msg", "Done: respond to regulator.");
  await s.act("#act", "respond_to_regulator", { "Conduct case": id, "Request": "RQ-1", "Response document": `rs2-${id}` });
  await s.stopped("#act-msg", "no such request is waiting for a response", 409);
});

test({ id: "E2E-068", area: A, persona: "Conduct", story: "US-ST16-1", title: "A complaint is linked to the conduct case it is evidence for",
  purpose: "Link a recorded complaint to a conduct case, link it again, and link one that does not exist.",
  why: "A pattern across complaints is how a conduct case is proven; links must be to real complaints and never doubled." }, async (t) => {
  const cmp = await heard(t, t.ref("CMP"), { category: { process: "adviser_conduct" } });
  const id = t.ref("CON");
  await tells(t, cast.conduct, "conduct", { open_conduct_case: { case: id, kind: "adviser_pattern", owner: "conduct", summary_document: null, notify_regulator: false } });
  const s = await workbench(t, cast.conduct);
  await s.act("#act", "link_complaint", { "Conduct case": id, "Complaint": cmp });
  await s.done("#act-msg", "Done: link complaint.");
  await s.act("#act", "link_complaint", { "Conduct case": id, "Complaint": cmp });
  await s.stopped("#act-msg", "that complaint is already linked", 409);
  await s.act("#act", "link_complaint", { "Conduct case": id, "Complaint": "CMP-never" });
  await s.stopped("#act-msg", "there is no complaint CMP-never", 409);
});

test({ id: "E2E-069", area: A, persona: "ST10", story: "US-ST13-1", title: "A finance operator is refused the complaints workbench", matrix: [],
  purpose: "Sign in to the complaints workbench as the finance operator.",
  why: "Complaint files are need-to-know; a member of staff with no complaints role must see a refusal and no form." }, async (t) => {
  const s = await t.open("/complaints", cast.st10, 403);
  await s.refused();
  await s.never("Record a complaint");
});
