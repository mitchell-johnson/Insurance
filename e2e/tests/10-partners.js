// Advisers and the people who look after them: the adviser portal, the
// commission console, replacement matches and the underwriting console.
//
// The seed enters two advice providers and three advisers through the
// commission console. A policy reaches an adviser's book the way it does in
// life: its policyholder appoints the adviser. The underwriting intake (the
// service that receives proposals) is not part of this stack, so the
// underwriting tests cover the console, its authority rules and its
// two-person rule on remedies, and say so where the intake would be needed.
"use strict";
const { test } = require("../lib/suite");
const { cast } = require("../lib/cast");

const ADV = "Adviser portal", COM = "Commission console", REP = "Replacement matches", UW = "Underwriting";
const COMMISSION = "/staff/v1/commission/", UNDERWRITING = "/staff/v1/underwriting/";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

/// A term life policy whose policyholder has appointed Tessa Maddox (D1) as their adviser, once it is in her book.
async function advised(t) {
  const p = await t.policy("term_life", cast.p1.party);
  await t.does(cast.p1, "POST", `/app/v1/policies/${p.id}/advisers`, { adviser: cast.d1.party, engagement: "eng-d1" }, "appoints their adviser");
  for (let n = 0; n < 50; n++) {
    const book = await t.sys.portal("GET", "/adviser/v1/book", cast.d1);
    if (book.ok && book.data.policies.some((x) => x.policy === p.id)) return p;
    await pause(200);
  }
  throw new Error(`policy ${p.number} never reached the adviser's book`);
}
/// Clearwater Advice (D9) with its licence suspended by compliance, once its adviser's portal knows.
async function suspended(t) {
  // The suspension is recorded once: a licence has one status for a day, and the two tests that need it share it.
  const now = await t.does(cast.d9a, "GET", "/adviser/v1/book", undefined, "reads the book");
  if (!now.read_only) await t.does(cast.st13, "POST", COMMISSION + "commands", { command: { record_licence: { party: cast.d9.party, state: "suspended", effective: t.stack.opening, source_ref: "FMA-2027-118" } }, purpose: "the regulator's notice" }, "records the suspension");
  for (let n = 0; n < 50; n++) {
    const book = await t.sys.portal("GET", "/adviser/v1/book", cast.d9a);
    if (book.ok && book.data.read_only) return;
    await pause(200);
  }
  throw new Error("the adviser's portal never learned of the suspension");
}
/// The commission console has finished loading: it draws its forms again once it knows the person's roles, and says
/// who they are last. A person waits for that before typing.
const loaded = (s) => s.page.waitForFunction(() => document.querySelector("#me").textContent.trim() !== "");
/// Ask the commission console a question through its Look up form, filling the fields the question shows.
async function asks(s, question, fields = {}) {
  await loaded(s);
  await s.tab("Look up");
  await s.pick("#q", question);
  for (const [key, value] of Object.entries(fields)) await s.fill("#q-" + key, value);
  await s.press("Show", "#ask");
}
/// Make a change through the commission console's Change something form.
async function changes(s, action, fields, purpose) {
  await loaded(s);
  await s.tab("Change something");
  await s.pick("#a", action);
  for (const [key, value] of Object.entries(fields)) {
    if (await s.$("#a-" + key).evaluate((el) => el.tagName === "SELECT")) await s.pick("#a-" + key, value); else await s.fill("#a-" + key, value);
  }
  await s.fill("#a-purpose", purpose);
  await s.press("Do it", "#do");
}
/// Send the commission console one command in the module's own JSON, for the few with no form.
async function sends(s, command, purpose) {
  await loaded(s);
  await s.tab("Another command");
  await s.fill("#cmd", typeof command === "string" ? command : JSON.stringify(command));
  await s.fill("#purpose", purpose);
  await s.press("Send", "#send");
}
/// A remedy a senior underwriter has prepared and nobody has signed. The review named does not exist, which the module
/// only checks when the second senior signs: nobody's cover is touched.
async function remedy(t, who = cast.st2) {
  const r = await t.does(who, "POST", UNDERWRITING + "commands", { record_non_disclosure_outcome: { review: `ndr/${t.ref("CLM")}/L1`, outcome: "no_action", reasons: ["RC-0980"], varied_terms: [], premium: null } }, "prepares a remedy");
  if (!r.prepared) throw new Error(`the remedy was not held for a second senior: ${JSON.stringify(r)}`);
  return r.prepared;
}
const mark = (id) => encodeURIComponent(id);

// --- The adviser portal. ----------------------------------------------------------------------------------------

test({ id: "E2E-263", area: ADV, persona: "D1", story: "US-D01-1", title: "An adviser sees who they are engaged with and that the engagement is active", matrix: [],
  purpose: "Sign in as Tessa Maddox and read the Engagements view.",
  why: "An adviser may only sell under a licensed provider; the portal must show her which provider the insurer has her under and that she may transact." }, async (t) => {
  const s = await t.open("/adviser", cast.d1);
  await s.tab("Engagements");
  await s.sees("eng-d1", "#engagements");
  const row = await s.row("#engagements", "eng-d1").innerText();
  for (const word of [cast.d6.party, "2025-01-01", "Active"]) if (!row.includes(word)) throw new Error(`the engagement row reads "${row}", without "${word}"`);
  await s.never("Your access is read-only");
});

test({ id: "E2E-264", area: ADV, persona: "D1", story: "US-D01-1", title: "A policy joins an adviser's book when its policyholder appoints her", matrix: ["ADV-013"],
  purpose: "Issue a policy, have its policyholder appoint Tessa Maddox, and read her book.",
  why: "The book is the adviser's working list. A policy must appear there because the customer chose her, with its product and status." }, async (t) => {
  const p = await advised(t);
  const s = await t.open("/adviser", cast.d1);
  await s.sees(p.number, "#book");
  const row = await s.row("#book", p.number).innerText();
  for (const word of ["term-life", "in force", "eng-d1"]) if (!row.includes(word)) throw new Error(`the book row reads "${row}", without "${word}"`);
});

test({ id: "E2E-265", area: ADV, persona: "D1", story: "US-D01-2", title: "An adviser opens a policy in her book and sees its covers, what is owed and its claims by status only", matrix: ["ADV-035"],
  purpose: "Open a policy from the book and read what the page shows.",
  why: "An adviser needs the cover and billing position to advise; what a claim is about stays with the customer and the claims team." }, async (t) => {
  const p = await advised(t);
  const s = await t.open("/adviser", cast.d1);
  await s.sees(p.number, "#book");
  await s.row("#book", p.number).getByRole("button", { name: "Open" }).click();
  await s.sees(`Policy ${p.number}`, "#detail-title");
  await s.sees("cover", "#rows");
  await s.sees("billing", "#rows");
  await s.sees("No claims.", "#claims");
  await s.sees("No units held.", "#invest");
  await s.never("diagnos", "#detail");
});

test({ id: "E2E-266", area: ADV, persona: "D2", story: "US-D02-1", title: "Another adviser cannot see a policy that is not in his book, even by its address", matrix: ["ADV-035"],
  purpose: "As Viliami Fifita, read the book and then ask for another adviser's customer's policy directly.",
  why: "Two advisers at the same provider are still competitors for the customer; one must not read the other's clients." }, async (t) => {
  const p = await advised(t);
  const s = await t.open("/adviser", cast.d2);
  await s.sees("Your book");
  await s.never(p.number, "#book");
  const r = await t.sys.portal("GET", `/app/v1/policies/${p.id}`, cast.d2);
  if (r.status !== 404) throw new Error(`asking for the policy directly answered ${r.status} ${JSON.stringify(r.data).slice(0, 200)}`);
});

test({ id: "E2E-267", area: ADV, persona: "P18", story: "US-D01-1", title: "Only the policyholder can appoint an adviser to a policy", matrix: ["ADV-013"],
  purpose: "Have a different customer try to appoint an adviser to someone else's policy.",
  why: "Appointing an adviser opens the policy to them. Nobody but the policyholder may do that." }, async (t) => {
  const p = await t.policy("term_life", cast.p1.party);
  const r = await t.sys.portal("POST", `/app/v1/policies/${p.id}/advisers`, cast.p18, { adviser: cast.d1.party, engagement: "eng-d1" });
  if (r.status !== 403 || !String(r.data.detail).includes("only the policyholder")) throw new Error(`the appointment answered ${r.status} ${JSON.stringify(r.data)}`);
  const book = await t.sys.portal("GET", "/adviser/v1/book", cast.d1);
  if (book.data.policies.some((x) => x.policy === p.id)) throw new Error("the policy is in the adviser's book all the same");
});

test({ id: "E2E-268", area: ADV, persona: "D1", story: "US-D01-3", title: "A change an adviser prepares waits for the customer's approval", matrix: [],
  purpose: "Open a policy, prepare a decline of indexation for the customer, and read Requests you have made.",
  why: "An adviser prepares; the customer decides. The request must be recorded as waiting and nothing on the policy may change." }, async (t) => {
  const p = await advised(t);
  const s = await t.open("/adviser", cast.d1);
  await s.sees(p.number, "#book");
  await s.row("#book", p.number).getByRole("button", { name: "Open" }).click();
  await s.sees(`Policy ${p.number}`, "#detail-title");
  await s.pick("#kind", "decline_indexation");
  await s.fill("#note", "Asked for by phone on " + t.stack.opening);
  await s.press("Send to customer");
  await s.done("#change-msg", "The customer has been asked to approve it");
  // The request names the policy by the number the adviser knows it by, the change in words and where it stands,
  // and the adviser may still withdraw it.
  await s.sees(p.number, "#requests");
  const cells = await s.row("#requests", p.number).locator("td").allInnerTexts();
  if (cells[2] !== "Decline this year's indexation increase" || cells[3] !== "Waiting for the customer to approve" || cells[4] !== "Withdraw") throw new Error(`the request row reads ${JSON.stringify(cells)}`);
});

test({ id: "E2E-269", area: ADV, persona: "D6", story: "US-D06-1", title: "An advice provider sees the advisers engaged with it and the policies they look after", matrix: ["ADV-035"],
  purpose: "Sign in as Harbourline Advice and read Advisers under you; then check a single adviser is shown no such section.",
  why: "The provider is accountable for its advisers' business and must see it; an adviser has nobody under her." }, async (t) => {
  const p = await advised(t);
  const s = await t.open("/adviser", cast.d6);
  await s.sees("Advisers under you");
  await s.sees(cast.d1.party, "#practice");
  const row = await s.row("#practice", cast.d1.party).innerText();
  if (!row.includes(p.number) || !row.includes("Active")) throw new Error(`the provider's row for the adviser reads "${row.slice(0, 300)}"`);
  await s.sees(cast.d2.party, "#practice");
  const own = await t.open("/adviser", cast.d1);
  await own.sees(p.number, "#book");
  await own.never("Advisers under you");
});

test({ id: "E2E-270", area: ADV, persona: "D1", story: "US-D01-3", title: "An adviser with nothing held and no replacement matches is told so in words", matrix: [],
  purpose: "Read the Commission view as an adviser in good standing.",
  why: "An empty table leaves an adviser wondering whether the page has loaded. Held commission is money: the page must say plainly there is none." }, async (t) => {
  const s = await t.open("/adviser", cast.d1);
  await s.tab("Commission");
  await s.sees("Nothing is being held from you.", "#held");
  await s.sees("None of your business has been treated as a replacement.", "#matches");
});

test({ id: "E2E-271", area: ADV, persona: "D1", story: "US-D01-3", title: "Asking for a commission statement that does not exist is answered plainly", matrix: [],
  purpose: "Ask for statement number 999 in the Commission view.",
  why: "Advisers reconcile their income to statements; a wrong number must be answered with a message, not a blank or a broken page." }, async (t) => {
  const s = await t.open("/adviser", cast.d1);
  await s.tab("Commission");
  await s.fill("#st-number", 999);
  await s.press("Show", "#st-form");
  await s.stopped("#st-msg", "you have no statement number 999: your statements are numbered from 1, the latest on your Commission tab", 404);
});

test({ id: "E2E-272", area: ADV, persona: "D9", story: "US-D09-1", title: "When a provider's licence is suspended its adviser's portal becomes read-only and says why", matrix: ["ADV-019"], lock: "licence:d9",
  purpose: "Have compliance record the suspension of Clearwater Advice's licence, then open the portal as its adviser.",
  why: "Decision 4 of the addendum: an adviser whose provider loses its licence may see their book and may not transact. The page must tell them which licence and since when." }, async (t) => {
  await suspended(t);
  const s = await t.open("/adviser", cast.d9a);
  await s.sees(`Your access is read-only. The licence of ${cast.d9.party} has been suspended since ${t.stack.opening}. You can see your book. Nothing can be quoted, submitted or changed until that licence is restored or you move to another provider.`, "#readonly");
  await s.tab("Engagements");
  await s.sees(`Read-only since ${t.stack.opening}: the licence of ${cast.d9.party} is suspended`, "#engagements");
});

test({ id: "E2E-273", area: ADV, persona: "D9", story: "US-D09-1", title: "The suspended provider sees its adviser as read-only, and the suspension opens a conduct case", matrix: ["ADV-019"], lock: "licence:d9",
  purpose: "After the suspension, read Advisers under you as the provider, and the conduct cases as commission operations.",
  why: "Licence loss starts work for the insurer: customers to write to and advice files to review. A case must exist naming the advisers and payees affected." }, async (t) => {
  await suspended(t);
  const s = await t.open("/adviser", cast.d9);
  await s.sees(cast.d9a.party, "#practice");
  const row = await s.row("#practice", cast.d9a.party).innerText();
  if (!row.includes("Read-only since") || !row.includes("suspended")) throw new Error(`the provider's row for its adviser reads "${row}"`);
  const cases = await t.does(cast.comops, "GET", COMMISSION + "conduct_cases", undefined, "reads the conduct cases");
  const opened = cases.find((c) => c.party === cast.d9.party && c.status === "suspended");
  if (!opened) throw new Error(`no conduct case names the provider: ${JSON.stringify(cases)}`);
  if (!opened.advisers.includes(cast.d9a.party) || !opened.payees.includes(cast.d9.party)) throw new Error(`the case does not name the adviser and the provider as payee: ${JSON.stringify(opened)}`);
});

test({ id: "E2E-274", area: ADV, persona: "D1", story: "US-D01-3", title: "An adviser cannot dispute a replacement match that is not hers", matrix: ["ADV-029"],
  purpose: "Send a dispute for a layer of a policy the rule never matched.",
  why: "A dispute holds commission until the conduct officer decides. It must only be possible on a match the adviser actually has." }, async (t) => {
  const p = await advised(t);
  const r = await t.sys.portal("POST", "/adviser/v1/replacements/dispute", cast.d1, { layer: `${p.id}/life/1`, reason: "It was new cover" });
  if (r.status !== 400 || !String(r.data.detail).includes("not one of your matches")) throw new Error(`the dispute answered ${r.status} ${JSON.stringify(r.data)}`);
});

test({ id: "E2E-275", area: ADV, persona: "P1", story: "US-D01-1", title: "A customer who opens the adviser portal is refused, and is shown nobody's book", matrix: ["ADV-035"],
  purpose: "Open the adviser portal as a retail customer who has a policy with an adviser.",
  why: "The adviser portal and the customer's sign-in share a realm. A customer who finds the page must be told it is not theirs and see no book and no other customer." }, async (t) => {
  const p = await advised(t);
  const s = await t.open("/adviser", cast.p1, 403);
  await s.refused("the adviser portal is for advisers and advice providers");
  await s.never(p.number);
  const book = await t.sys.portal("GET", "/adviser/v1/book", cast.p1);
  if (book.status !== 403) throw new Error(`asking for the book directly answered ${book.status} ${JSON.stringify(book.data).slice(0, 200)}`);
});

// --- The commission console. ------------------------------------------------------------------------------------

test({ id: "E2E-276", area: COM, persona: "Distribution", story: "US-D10-1", title: "The distribution administrator arrives with the role that maintains the structure", matrix: ["STF-014"],
  purpose: "Sign in to the commission console as the distribution administrator.",
  why: "The console sends commands in the module's own terms; the person must see which role the module will judge them by." }, async (t) => {
  const s = await t.open("/commission", cast.dist);
  await s.sees("dist: distribution admin", "#me");
});

test({ id: "E2E-277", area: COM, persona: "Auditor", story: "US-D10-1", title: "The auditor may ask the commission module anything and is told they can change nothing", matrix: ["STF-014"],
  purpose: "Sign in as the read-only auditor, read the role line, ask for the status, and send a command.",
  why: "Audit must be able to read commission and held money without being able to move it; the module must refuse, not just the page." }, async (t) => {
  const s = await t.open("/commission", cast.auditor);
  await s.sees("may look, holds no role that changes anything", "#me");
  await asks(s, "status");
  await s.sees(t.stack.opening, "#answer");
  await changes(s, "add_party", { party: t.ref("adv"), kind: "adviser" }, "trying a change");
  await s.stopped("#do-msg", "none of your roles allows", 403);
});

test({ id: "E2E-278", area: COM, persona: "ST5", story: "US-D10-1", title: "Staff with no commission role are refused the console", matrix: [],
  purpose: "Open the commission console as a collections officer.",
  why: "What advisers earn and what is held from them is commercially sensitive and personal; only commission roles see it." }, async (t) => {
  const s = await t.open("/commission", cast.st5, 403);
  await s.refused("do not include the commission console");
});

test({ id: "E2E-279", area: COM, persona: "Commission", story: "US-D10-1", title: "A party's position shows its licence history and that nothing is held", matrix: [],
  purpose: "As commission operations, ask for Harbourline Advice's position at the month end.",
  why: "The position is what operations read before releasing or holding money: the licence history must be there with its source." }, async (t) => {
  const s = await t.open("/commission", cast.comops);
  await asks(s, "position", { party: cast.d6.party, as_at: "2027-11-30" });
  await s.sees("FSPR-6", "#answer");
  await s.sees("2020-01-01", "#answer");
  await s.sees("Summary", "#answer");
  const position = await t.does(cast.comops, "GET", `${COMMISSION}position?party=${cast.d6.party}&as_at=2027-11-30`, undefined, "reads the position");
  // What the party is owed net is the sum of its balances: nothing has been earned or held on a new structure.
  const { net_minor: net, ...parts } = position.summary;
  if (net !== 0 || Object.values(parts).some((v) => v !== 0) || position.held.length) throw new Error(`the position of a provider with no business is not nil: ${JSON.stringify(position.summary)}`);
});

test({ id: "E2E-280", area: COM, persona: "Commission", story: "US-D10-1", title: "A question asked without what it needs is answered with what is missing", matrix: [],
  purpose: "Ask for a party's position without naming the party, on the form and then by hand.",
  why: "The console is a form over the module; a person who leaves a field out must be told which, in the module's words." }, async (t) => {
  const s = await t.open("/commission", cast.comops);
  await asks(s, "position", { as_at: "2027-11-30" });
  // The form marks the party as required and does not send the question; asked by hand, the module says what is missing.
  if (!(await s.$("#q-party").evaluate((el) => el.validity.valueMissing))) throw new Error("the form does not mark the party as missing");
  if ((await s.text("#answer")) !== "") throw new Error("the page shows an answer to a question that lacks its party");
  const r = await t.sys.portal("GET", COMMISSION + "position", cast.comops);
  if (r.status !== 400 || !String(r.data.detail).includes("position needs party")) throw new Error(`asked by hand the question answered ${r.status} ${JSON.stringify(r.data)}`);
});

test({ id: "E2E-281", area: COM, persona: "Commission", story: "US-D10-1", title: "A command that is not valid JSON never leaves the page", matrix: [],
  purpose: "Type a malformed command and send it.",
  why: "A mistyped command must be caught where it was typed, with a message, and must not reach the module." }, async (t) => {
  const s = await t.open("/commission", cast.comops);
  await sends(s, "{ set_payment_hold: ", "a typing mistake");
  await s.stopped("#send-msg", "The command is not JSON");
});

test({ id: "E2E-282", area: COM, persona: "Commission", story: "US-D10-1", title: "Commission operations cannot change the distribution structure", matrix: [],
  purpose: "As commission operations, send a command that adds an adviser.",
  why: "Who is an adviser, and under whom, decides who is paid. The people who run payments must not be able to change it." }, async (t) => {
  const party = t.ref("adv");
  const s = await t.open("/commission", cast.comops);
  await changes(s, "add_party", { party, kind: "adviser" }, "trying to add an adviser");
  await s.stopped("#do-msg", "none of your roles allows", 403);
  const position = await t.does(cast.comops, "GET", `${COMMISSION}position?party=${party}&as_at=2027-11-30`, undefined, "reads the position");
  if (position.licence_history.length) throw new Error("the refused party has a history");
});

test({ id: "E2E-283", area: COM, persona: "Distribution", story: "US-D10-1", title: "The distribution administrator adds a party once; adding it again is refused", matrix: [],
  purpose: "Add a new adviser through the form, then send the same addition with a new purpose.",
  why: "A party entered twice would split an adviser's commission between two records." }, async (t) => {
  const party = t.ref("adv");
  const s = await t.open("/commission", cast.dist);
  await changes(s, "add_party", { party, kind: "adviser" }, "a new adviser joins");
  await s.done("#do-msg", `Done: add an adviser, company or other party for ${party} (kind adviser).`);
  await changes(s, "add_party", { party, kind: "adviser" }, "entered again by mistake");
  await s.stopped("#do-msg", "already in the structure", 409);
});

test({ id: "E2E-284", area: COM, persona: "Distribution", story: "US-D10-1", title: "A policy cannot be given to an adviser from the commission console", matrix: ["ADV-013"],
  purpose: "Look for the action among those the console offers, then send the module's own appoint-adviser command by hand.",
  why: "Only the policyholder appoints an adviser. The console must not be a way round that, even for an administrator." }, async (t) => {
  const s = await t.open("/commission", cast.dist);
  await s.tab("Change something");
  const offered = await s.$("#a").locator("option").evaluateAll((options) => options.map((o) => o.value));
  if (offered.includes("appoint_adviser")) throw new Error("the Change something form offers to appoint a policy's adviser");
  await sends(s, { appoint_adviser: { policy: "pol-any", engagement: "eng-d1", book: null } }, "trying to assign a policy");
  await s.stopped("#send-msg", "not a command the console sends", 400);
});

test({ id: "E2E-285", area: COM, persona: "ST13", story: "US-D09-1", title: "Compliance records a licence event and operations cannot", matrix: ["ADV-019"],
  purpose: "Add a provider, then record its licence as revoked first as commission operations and then as compliance; read the conduct cases.",
  why: "A licence event stops an adviser trading and holds their money. It must come from compliance, with the regulator's reference, and open a case." }, async (t) => {
  const party = t.ref("fap");
  await t.does(cast.dist, "POST", COMMISSION + "commands", { command: { add_party: { party, kind: "fap" } }, purpose: "a new provider" }, "adds the provider");
  const event = { party, state: "revoked", effective: t.stack.opening, source_ref: "FMA-2027-204" };
  const ops = await t.open("/commission", cast.comops);
  await changes(ops, "record_licence", event, "the regulator's notice");
  await ops.stopped("#do-msg", "none of your roles allows", 403);
  const s = await t.open("/commission", cast.st13);
  await s.sees("st13: compliance", "#me");
  await changes(s, "record_licence", event, "the regulator's notice");
  await s.done("#do-msg", `Done: record a licence state for ${party} (state revoked, effective ${t.stack.opening}, source ref FMA-2027-204).`);
  await asks(s, "conduct_cases");
  await s.sees(party, "#answer");
  await s.sees("FMA-2027-204", "#answer");
});

test({ id: "E2E-286", area: COM, persona: "Commission", story: "US-D10-1", title: "The daily held money control agrees with the ledger", matrix: ["ADV-034"],
  purpose: "Ask for the held control of the business date and for the month-end sweep check.",
  why: "Held commission is other people's money. The module's total must agree with the ledger every day, and the month cannot close while it does not." }, async (t) => {
  const control = await t.does(cast.comops, "GET", `${COMMISSION}held_control?date=${t.stack.opening}`, undefined, "reads the control");
  const s = await t.open("/commission", cast.comops);
  await asks(s, "held_control", { date: t.stack.opening });
  await s.sees("Agrees with ledger", "#answer");
  if (control.agrees_with_ledger !== true) throw new Error(`the control does not agree with the ledger: ${JSON.stringify(control)}`);
  // The four age bands add up to the total held.
  if (control.aged_minor.reduce((a, b) => a + b, 0) !== control.held_total_minor) throw new Error(`the age bands ${control.aged_minor} do not add to ${control.held_total_minor}`);
  await asks(s, "sweep_check");
  await s.sees("Green", "#answer");
});

test({ id: "E2E-287", area: COM, persona: "Commission", story: "US-D10-1", title: "Asking for a case that does not exist is answered plainly", matrix: [],
  purpose: "Ask the console for a case by an identifier nobody has used.",
  why: "People paste identifiers from email; a wrong one must be answered with a message and nothing else on the page may change." }, async (t) => {
  const s = await t.open("/commission", cast.comops);
  await asks(s, "case", { id: t.ref("case") });
  await s.stopped("#ask-msg", "Nothing was found with that reference", 404);
});

// --- Replacement matches. ---------------------------------------------------------------------------------------

test({ id: "E2E-288", area: REP, persona: "Conduct", story: "US-D05-1", title: "The conduct officer arrives able to decide disputed matches", matrix: ["ADV-029"],
  purpose: "Open Replacement matches as the conduct officer.",
  why: "A disputed match holds an adviser's commission until someone decides; the page must show that this person is the one who does." }, async (t) => {
  const s = await t.open("/replacements", cast.conduct);
  await s.sees("You decide disputed matches as the conduct officer.", "#me");
  await s.sees("The rule has matched nothing.", "#matches");
});

test({ id: "E2E-289", area: REP, persona: "ST10", story: "US-D05-1", title: "Finance reads the matches and cannot decide one", matrix: ["ADV-029"],
  purpose: "Open the page as a finance operator, then send a decision by hand.",
  why: "Finance needs to see why commission is held; the decision belongs to conduct alone." }, async (t) => {
  const s = await t.open("/replacements", cast.st10);
  await s.sees("You may read the matches.", "#me");
  const r = await t.sys.portal("POST", "/staff/v1/replacements/decide", cast.st10, { policy: "pol-any", cover: "life", layer: 1, replacement: false, purpose: "trying" });
  if (r.status !== 403 || !String(r.data.detail).includes("decided by the conduct officer")) throw new Error(`the decision answered ${r.status} ${JSON.stringify(r.data)}`);
});

test({ id: "E2E-290", area: REP, persona: "Commission", story: "US-D05-1", title: "Commission operations are refused the replacement matches", matrix: [],
  purpose: "Open the page as commission operations.",
  why: "The people who pay commission must not see or influence the decisions that decide whether it is paid." }, async (t) => {
  const s = await t.open("/replacements", cast.comops, 403);
  await s.refused("your roles do not include");
});

test({ id: "E2E-291", area: REP, persona: "Conduct", story: "US-D05-1", title: "The conduct officer cannot decide a layer the rule never matched", matrix: ["ADV-029"],
  purpose: "Send a decision for the life cover of an ordinary new policy.",
  why: "A decision takes commission back or releases it. It must only be possible where the rule has actually made a match." }, async (t) => {
  const p = await t.policy("term_life", cast.p1.party);
  // The commission module hears of a new policy's layers from the core's feed a moment after issue.
  let r;
  for (let n = 0; n < 40; n++) {
    r = await t.sys.portal("POST", "/staff/v1/replacements/decide", cast.conduct, { policy: p.id, cover: "life", layer: 1, replacement: true, purpose: "testing the rule" });
    if (!String(r.data && r.data.type).endsWith("unknown_layer")) break;
    await new Promise((done) => setTimeout(done, 250));
  }
  if (r.status !== 409 || !String(r.data.detail).includes("not treated as a replacement")) throw new Error(`the decision answered ${r.status} ${JSON.stringify(r.data)}`);
});

// --- The underwriting console. ----------------------------------------------------------------------------------

test({ id: "E2E-292", area: UW, persona: "ST1", story: "US-ST01-1", title: "A junior underwriter arrives with their workload and without the remedy forms", matrix: ["STF-014"],
  purpose: "Sign in to underwriting as a junior underwriter.",
  why: "The page must say who the module takes this person to be and the date their decisions carry, and must not offer what their grade cannot do." }, async (t) => {
  const s = await t.open("/underwriting", cast.st1);
  await s.sees(`You are st1. You hold 0 open cases. Commands are made on ${t.stack.opening}.`, "#me");
  await s.tab("Remedies and review");
  await s.sees("Waiting for a second senior");
  await s.never("Non-disclosure remedy");
});

test({ id: "E2E-293", area: UW, persona: "ST2", story: "US-ST02-1", title: "A senior underwriter is shown the remedy and peer review forms", matrix: [],
  purpose: "Sign in as a senior underwriter and open Remedies and review.",
  why: "Remedies and peer review are senior work; the page must offer them to a senior, with the warning that a remedy changes someone's cover." }, async (t) => {
  const s = await t.open("/underwriting", cast.st2);
  await s.sees("You are st2, a senior underwriter.", "#me");
  await s.tab("Remedies and review");
  await s.sees("Non-disclosure remedy");
  await s.sees("a second senior signs it before it is sent");
  await s.sees("Peer review");
});

test({ id: "E2E-294", area: UW, persona: "ST5", story: "US-ST01-1", title: "Staff who are not underwriters are refused the underwriting console", matrix: [],
  purpose: "Open underwriting as a collections officer.",
  why: "Underwriting holds health disclosures, the most sensitive data the insurer has." }, async (t) => {
  const s = await t.open("/underwriting", cast.st5, 403);
  await s.refused("do not include underwriting");
});

test({ id: "E2E-295", area: UW, persona: "ST1", story: "US-ST01-1", title: "Every queue can be shown, and an empty one says so", matrix: [],
  purpose: "Show each of the six queues in turn.",
  why: "An underwriter starts the day from the queues; an empty queue must read as empty, not as a page that failed." }, async (t) => {
  const s = await t.open("/underwriting", cast.st1);
  for (const queue of ["medical", "financial", "occupational", "tele_interview", "senior_review", "reinsurer_liaison"]) {
    await s.pick("#q-name", queue);
    await s.press("Show", "#q-form");
    await s.sees("Nothing is waiting in this queue.", "#queue");
  }
});

test({ id: "E2E-296", area: UW, persona: "ST2", story: "US-ST02-1", title: "The underwriting report for a period with no decisions shows zeros and no average", matrix: [],
  purpose: "Ask for the report for the month before the opening date.",
  why: "The report feeds management figures; with nothing decided it must show zeros and must not divide by nothing." }, async (t) => {
  const s = await t.open("/underwriting", cast.st2);
  await s.tab("Conflicts and reports");
  await s.fill("#m-from", "2027-10-01");
  await s.fill("#m-to", "2027-10-31");
  await s.press("Show", "#m-form");
  await s.sees("Average hours to decide", "#report");
  const [row] = await s.rows("#report");
  if (JSON.stringify(row) !== JSON.stringify(["0", "0", "0", "0", "0.00", "-"])) throw new Error(`the report row reads ${JSON.stringify(row)}`);
});

test({ id: "E2E-297", area: UW, persona: "ST1", story: "US-ST01-1", title: "A junior underwriter cannot prepare a non-disclosure remedy or draw a peer review sample", matrix: ["UW-024"],
  purpose: "Send both senior commands by hand as a junior.",
  why: "Hiding a form is not a control. The module must refuse a junior who sends the command anyway." }, async (t) => {
  const outcome = await t.sys.portal("POST", UNDERWRITING + "commands", cast.st1, { record_non_disclosure_outcome: { review: "ndr/CLM-1/L1", outcome: "avoid", reasons: ["RC-0980"], varied_terms: [], premium: "retain" } });
  if (outcome.status !== 403 || !String(outcome.data.detail).includes("is for a senior underwriter")) throw new Error(`the remedy answered ${outcome.status} ${JSON.stringify(outcome.data)}`);
  const sample = await t.sys.portal("POST", UNDERWRITING + "commands", cast.st1, { draw_peer_review_sample: { period: "2027-10", from: "2027-10-01T00:00:00.000Z", to: "2027-10-31T23:59:59.000Z", seed: 7 } });
  if (sample.status !== 403 || !String(sample.data.detail).includes("is for a senior underwriter")) throw new Error(`the sample answered ${sample.status} ${JSON.stringify(sample.data)}`);
});

test({ id: "E2E-298", area: UW, persona: "ST2", story: "US-ST02-1", title: "A remedy a senior prepares waits for a second senior, who is offered Sign and send", matrix: ["UW-021"], lock: "underwriting:pending",
  purpose: "Prepare a remedy through the form; read Waiting for a second senior as the preparer and as another senior; withdraw it.",
  why: "A remedy can end someone's cover. One person must not be able to send it, and its preparer must be able to take it back." }, async (t) => {
  const review = `ndr/${t.ref("CLM")}/L1`;
  const s = await t.open("/underwriting", cast.st2);
  await s.tab("Remedies and review");
  await s.fill("#n-review", review);
  await s.fill("#n-reasons", "RC-0980");
  await s.press("Prepare");
  await s.done("#n-msg", "Prepared. A second senior signs it before it is sent.");
  await s.sees(review, "#pending");
  const mine = await s.row("#pending", review).getByRole("button").allInnerTexts();
  if (JSON.stringify(mine) !== JSON.stringify(["Withdraw"])) throw new Error(`the preparer is offered ${JSON.stringify(mine)}`);
  const other = await t.open("/underwriting", cast.st2b);
  await other.tab("Remedies and review");
  await other.sees(review, "#pending");
  const theirs = await other.row("#pending", review).getByRole("button").allInnerTexts();
  if (JSON.stringify(theirs) !== JSON.stringify(["Sign and send"])) throw new Error(`the second senior is offered ${JSON.stringify(theirs)}`);
  await s.stale();
  await s.row("#pending", review).getByRole("button", { name: "Withdraw" }).click();
  await s.done("#w-msg", "Withdrawn.");
  await s.never(review, "#pending");
});

test({ id: "E2E-299", area: UW, persona: "ST2", story: "US-ST02-1", title: "Neither the preparer nor a junior can sign a prepared remedy", matrix: ["UW-021"], lock: "underwriting:pending",
  purpose: "Prepare a remedy, then try to sign it as its preparer and as a junior underwriter.",
  why: "The second signature is the whole control: it must be a different person and a senior." }, async (t) => {
  const id = await remedy(t);
  const own = await t.sys.portal("POST", `${UNDERWRITING}sign/${mark(id)}`, cast.st2);
  if (own.status !== 409 || !String(own.data.detail).includes("someone else signs what you prepared")) throw new Error(`the preparer's signature answered ${own.status} ${JSON.stringify(own.data)}`);
  const junior = await t.sys.portal("POST", `${UNDERWRITING}sign/${mark(id)}`, cast.st1);
  if (junior.status !== 403) throw new Error(`the junior's signature answered ${junior.status} ${JSON.stringify(junior.data)}`);
  await t.does(cast.st2, "POST", `${UNDERWRITING}withdraw/${mark(id)}`, undefined, "withdraws the remedy");
});

test({ id: "E2E-300", area: UW, persona: "ST1", story: "US-ST01-1", title: "Working a case or opening a proposal that does not exist is answered plainly", matrix: [],
  purpose: "Ask for evidence on a case nobody has, then open a proposal nobody sent.",
  why: "Case and proposal references are typed by hand. A wrong one must be answered in words, and nothing may be recorded against a real case." }, async (t) => {
  const s = await t.open("/underwriting", cast.st1);
  await s.tab("Cases");
  await s.fill("#c-case", `${t.ref("P")}/L1/c1`);
  await s.pick("#c-what", "evidence");
  await s.page.getByLabel("gp report").check();
  await s.press("Send", "#c-form");
  await s.stopped("#c-msg", "no case", 409);
  await s.tab("Proposals");
  await s.fill("#p-id", t.ref("prop"));
  await s.press("Open", "#p-form");
  await s.stopped("#p-msg", "Nothing was found with that reference", 404);
});
