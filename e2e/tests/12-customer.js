// The customer page: a policyholder's own cover, bills and claims, the
// changes an adviser or the insurer prepared for them to approve, their
// adviser, their record in the customer master, and the signing that makes
// their approvals their own.
//
// Every journey has a customer of its own (`lib/customer.js`): a record the
// customer master opened, a sign-in bound to it, and a phone enrolled after
// identity verification. The phone app is not part of this stack; its key is
// made by the test and its signatures are made the way the app makes them.
// The browser's own key is made in the page by Web Crypto and never leaves
// it; the phone approves it, as a person would.
"use strict";
const { test } = require("../lib/suite");
const { cast } = require("../lib/cast");
const oracle = require("../lib/oracle");
const { customer, enrolBrowser, phoneSigns, master } = require("../lib/customer");

const A = "Customer";
const pause = (ms) => new Promise((done) => setTimeout(done, ms));
/// Amounts as the page writes them, without the dollar sign and thousands separators.
const plain = (text) => text.replace(/NZ\$|\$|,/g, "");

/// A customer with one term life policy, its first premium owed, on their own page.
async function insured(t, extra = {}) {
  const who = await customer(t, extra);
  const p = await t.policy("term_life", who.party);
  const s = await t.open("/customer", who);
  await s.sees(p.number, "#policies");
  return { who, p, s };
}
/// A change the customer's adviser prepares for them, as the adviser page sends it, once the customer appointed her.
async function prepared(t, who, p, kind = "decline_indexation") {
  await t.does(who, "POST", `/app/v1/policies/${p.id}/advisers`, { adviser: cast.d1.party, engagement: "eng-d1" }, "appoints their adviser");
  const id = t.ref("req").toLowerCase();
  for (let n = 0; ; n++) {
    const r = await t.sys.portal("POST", "/app/v1/requests", cast.d1, { id, policy: p.id, kind, module: "policy-servicing", payload: { note: "asked for by phone" } });
    if (r.ok) return id;
    if (n >= 40) throw new Error(`the adviser could not prepare ${kind}: ${r.status} ${JSON.stringify(r.data)}`);
    await pause(250);
  }
}
/// The customer's own page, freshly loaded, on the To approve view.
async function toApprove(t, who) {
  const s = await t.open("/customer", who);
  await s.tab("To approve");
  return s;
}
/// The state of one of the customer's requests, as the portal records it.
async function stateOf(t, who, id) {
  const p = (await t.does(who, "GET", "/app/v1/me", undefined, "reads their policies")).policies;
  for (const policy of p) {
    const view = await t.does(who, "GET", `/app/v1/policies/${policy.policy}`, undefined, "reads a policy");
    const row = view.rows.find((r) => r.kind === "request" && r.body.id === id);
    if (row) return row.body.state;
  }
  return null;
}

// --- Policies, bills and claims. ---------------------------------------------------------------------------------

test({ id: "E2E-325", area: A, persona: "P17", story: "US-P05-1", title: "A policyholder sees their cover and what they owe, and the amount equals the independent calculation", matrix: ["LED-005"],
  purpose: "Sign in as a new term life policyholder and read the policy the page opens: each cover, its sum insured, and the premium due.",
  why: "This is the first thing a customer checks. The cover must be what they bought and the amount due must be the premium, worked out a second way, to the cent." }, async (t) => {
  const { p, s } = await insured(t);
  await s.sees(`Policy ${p.number}`, "#detail-title");
  const covers = Object.fromEntries((await s.rows("#covers")).map((r) => [r[0], r]));
  for (const [cover, sum] of [["life", 500_000_00], ["trauma", 100_000_00]]) {
    const row = covers[cover];
    if (!row || plain(row[2]) !== oracle.money(sum) || row[4] !== "yes") throw new Error(`the ${cover} cover reads ${JSON.stringify(row)}; expected ${oracle.money(sum)} and in force`);
  }
  const premium = oracle.premium(p.application);
  const bill = plain(await s.$("#bills").innerText());
  if (!bill.includes(`${oracle.money(premium.total)} is due`)) throw new Error(`the bill reads "${bill.slice(0, 200)}"; the calculator gives ${oracle.money(premium.total)} due`);
  if (!bill.includes(p.application.inception)) throw new Error(`the bill does not say the premium fell due on ${p.application.inception}: "${bill.slice(0, 200)}"`);
});

test({ id: "E2E-326", area: A, persona: "P17", story: "US-P05-1", title: "A customer sees only their own policies, and another's policy cannot be opened by its address", matrix: [],
  purpose: "Give two customers a policy each; read the first one's page, then ask for the second one's policy as the first.",
  why: "A customer's page must never show another person's cover or debts, even to someone who guesses a policy reference." }, async (t) => {
  const other = await customer(t, { phone: false });
  const theirs = await t.policy("term_life", other.party);
  const { who, s } = await insured(t);
  await s.never(theirs.number, "#policies");
  for (const path of [`/app/v1/policies/${theirs.id}`, `/app/v1/policies/${theirs.id}/claims`]) {
    const r = await t.sys.portal("GET", path, who);
    if (r.ok) throw new Error(`another customer was given ${path}: ${JSON.stringify(r.data).slice(0, 200)}`);
  }
});

test({ id: "E2E-327", area: A, persona: "P17", story: "US-P05-1", title: "A customer with no claims is told so, and telling us about a claim records it without asking about their health", matrix: [],
  purpose: "Read the claims of a new policy; then use Tell us about a claim for the life cover with a date, and read the requests list.",
  why: "Claims start with a call or a form. The page must take only what is needed to start, record it as the customer's own request, and say what happens next." }, async (t) => {
  const { s } = await insured(t);
  await s.sees("No claims.", "#claims");
  await s.pick("#nc-cover", { index: 0 });
  await s.fill("#nc-date", t.stack.opening);
  await s.press("Tell us", "#notify");
  await s.done("#nc-msg", "The claims team will be in touch");
  await s.tab("To approve");
  await s.sees("notify claim", "#requests");
  for (const word of ["diagnosis", "symptom", "doctor", "medical"]) if ((await s.$("#notify").innerText()).toLowerCase().includes(word)) throw new Error(`the claim form asks about "${word}"`);
});

// --- Signing. -----------------------------------------------------------------------------------------------------

test({ id: "E2E-328", area: A, persona: "P17", story: "US-P18-1", title: "An approval is refused until the browser can sign, and the customer is told how to set it up", matrix: [],
  purpose: "With a change waiting from the adviser, press Approve on a browser that has not been set up.",
  why: "An approval is the customer's say-so. A browser anyone could be sitting at must not give it without a key the customer's phone has vouched for." }, async (t) => {
  const { who, p } = await insured(t);
  const id = await prepared(t, who, p);
  const s = await toApprove(t, who);
  await s.click(`#approve-${id}`);
  await s.stopped("#wait-msg", "Approving needs your signature");
  if ((await stateOf(t, who, id)).status !== "awaiting_approval") throw new Error("the change went ahead without a signature");
});

test({ id: "E2E-329", area: A, persona: "P17", story: "US-P18-1", title: "The customer's phone approves this browser, and both are listed as their devices", matrix: [],
  purpose: "Set up the browser under Signing, have the phone sign the browser's key, and read Your devices.",
  why: "A new browser is trusted only because a device the customer already holds signed for it. The list lets the customer see every device that can act for them." }, async (t) => {
  const { who, s } = await insured(t);
  const device = await enrolBrowser(t, s, who);
  const rows = await s.rows("#devices");
  const mine = rows.find((r) => r[0].startsWith(device)), phone = rows.find((r) => r[0] === who.phone.device);
  if (!mine || !mine[0].includes("this browser") || mine[1] !== "Browser" || mine[3] !== "Active") throw new Error(`the browser is listed as ${JSON.stringify(mine)}`);
  if (!phone || phone[1] !== "Phone with a hardware key" || phone[3] !== "Active") throw new Error(`the phone is listed as ${JSON.stringify(phone)}`);
});

test({ id: "E2E-330", area: A, persona: "P17", story: "US-P18-1", title: "A browser vouched for by a key that is not the customer's phone is refused", matrix: [],
  purpose: "Make the browser's key, then send its enrolment with a signature from some other key, naming the customer's phone.",
  why: "Anyone can make a key. Only a signature by the customer's own enrolled phone may add a device, or one stolen password would be enough to act for them." }, async (t) => {
  const { who, s } = await insured(t);
  await s.tab("Signing");
  await s.press("Set up this browser");
  await s.done("#sg-msg", "Approve it from your phone");
  const device = (await s.$("#sg-device").innerText()).trim(), key = (await s.$("#sg-key").innerText()).trim();
  const impostor = await customer(t);
  const r = await t.sys.portal("POST", "/app/v1/devices", who, { device, public_key: key, hardware_backed: false, approver: who.phone.device, signature: phoneSigns(impostor, `enrol\n${device}\n${key}`) });
  if (r.ok || !/did not sign this key/.test(JSON.stringify(r.data))) throw new Error(`an enrolment signed by another key answered ${r.status} ${JSON.stringify(r.data)}`);
  await s.press("Check again");
  await s.sees("This browser has a key but is not set up yet.", "#sign-state");
});

test({ id: "E2E-331", area: A, persona: "P17", story: "US-D01-3", title: "A change the adviser prepared is approved by the customer with a signature, and nothing is left waiting", matrix: [],
  purpose: "Have the adviser prepare a decline of indexation, set up the browser, approve the change, and read what is waiting.",
  why: "This is the whole point of the adviser's request: it goes ahead only once the customer has signed for it, and then it leaves their list." }, async (t) => {
  const { who, p } = await insured(t);
  const id = await prepared(t, who, p);
  const s = await t.open("/customer", who);
  await enrolBrowser(t, s, who);
  await s.tab("To approve");
  await s.sees("decline indexation", "#waiting");
  await s.click(`#approve-${id}`);
  await s.done("#wait-msg", "Approved: decline indexation.");
  await s.sees("Nothing is waiting for you.", "#waiting");
  const state = await stateOf(t, who, id);
  if (!state || state.status === "awaiting_approval") throw new Error(`after approval the request is ${JSON.stringify(state)}`);
});

test({ id: "E2E-332", area: A, persona: "P17", story: "US-D01-3", title: "A customer declines a change after confirming, and it does not go ahead", matrix: [],
  purpose: "Have the adviser prepare a cancellation, press Decline, confirm in the page's own dialog, and read what is waiting.",
  why: "A customer must be able to say no as easily as yes, with one confirmation so a slip does not decide it." }, async (t) => {
  const { who, p } = await insured(t);
  const id = await prepared(t, who, p, "cancel_policy");
  const s = await toApprove(t, who);
  await s.sees("cancel policy", "#waiting");
  await s.click(`#decline-${id}`);
  await s.page.click("dialog[open] button.danger");
  await s.done("#wait-msg", "Declined: cancel policy.");
  await s.sees("Nothing is waiting for you.", "#waiting");
  const state = await stateOf(t, who, id);
  if (state && state.status === "awaiting_approval") throw new Error("the declined change is still waiting");
  const policy = (await t.does(who, "GET", "/app/v1/me", undefined, "reads their policies")).policies.find((x) => x.policy === p.id);
  if (!policy || policy.status !== "in_force") throw new Error(`after declining a cancellation the policy is ${policy && policy.status}`);
});

test({ id: "E2E-333", area: A, persona: "P17", story: "US-D01-1", title: "The customer appoints an adviser from the page, the policy reaches her book, and removing her takes it away", matrix: ["ADV-013"],
  purpose: "Appoint Tessa Maddox with her engagement reference, read the adviser's book, then remove her after confirming.",
  why: "The customer decides who advises them. Appointing must give the adviser the policy at once, and removing must take it away just as surely." }, async (t) => {
  const { p, s } = await insured(t);
  await s.sees("No adviser on this policy.", "#advisers");
  await s.fill("#ap-adviser", cast.d1.party); await s.fill("#ap-engagement", "eng-d1");
  await s.press("Appoint", "#appoint");
  await s.done("#ap-msg", "Your adviser was appointed.");
  await s.sees(cast.d1.party, "#advisers");
  const inBook = async () => (await t.does(cast.d1, "GET", "/adviser/v1/book", undefined, "reads her book")).policies.some((x) => x.policy === p.id);
  let seen = false;
  for (let n = 0; n < 40 && !seen; n++) { seen = await inBook(); if (!seen) await pause(250); }
  if (!seen) throw new Error("the appointed adviser never had the policy in her book");
  await s.stale();
  await s.row("#advisers", cast.d1.party).getByRole("button", { name: "Remove" }).click();
  await s.page.click("dialog[open] button.danger");
  await s.done("#ap-msg", "Your adviser was removed.");
  await s.sees("No adviser on this policy.", "#advisers");
  for (let n = 0; n < 40 && seen; n++) { seen = await inBook(); if (seen) await pause(250); }
  if (seen) throw new Error("the removed adviser still has the policy in her book");
});

test({ id: "E2E-334", area: A, persona: "P17", story: "US-D01-1", title: "An adviser cannot be appointed under an engagement that is not hers", matrix: [],
  purpose: "Appoint Tessa Maddox giving another adviser's engagement reference.",
  why: "The engagement is what says which provider stands behind the adviser and is paid for the advice. A wrong one must be refused, not recorded." }, async (t) => {
  const { p, s } = await insured(t);
  await s.fill("#ap-adviser", cast.d1.party); await s.fill("#ap-engagement", "eng-d2");
  await s.press("Appoint", "#appoint");
  await s.stopped("#ap-msg", "", 400, 403, 404, 409, 422);
  await s.sees("No adviser on this policy.", "#advisers");
  const book = await t.does(cast.d1, "GET", "/adviser/v1/book", undefined, "reads her book");
  if (book.policies.some((x) => x.policy === p.id)) throw new Error("the policy reached the adviser under someone else's engagement");
});

// --- The customer's own record. ------------------------------------------------------------------------------------

test({ id: "E2E-335", area: A, persona: "P17", story: "US-P18-1", title: "A customer reads their own record and changes their email once the browser can sign", matrix: [],
  purpose: "Open Your details, try to save a new email before setting up the browser, then set it up and save; read the customer master's record afterwards.",
  why: "Contact details decide where notices and codes go. Changing them is what someone taking over an account does first, so it needs the customer's signature." }, async (t) => {
  const { who, s } = await insured(t);
  await s.tab("Your details");
  await s.sees(who.family, "#record");
  if ((await s.$("#ct-email").inputValue()) !== who.email) throw new Error(`the email field holds "${await s.$("#ct-email").inputValue()}", not the record's ${who.email}`);
  const fresh = `new.${who.email}`;
  await s.fill("#ct-email", fresh);
  await s.press("Save", "#contact");
  await s.stopped("#ct-msg", "Changing your details needs your signature");
  await enrolBrowser(t, s, who);
  await s.tab("Your details");
  await s.fill("#ct-email", fresh);
  await s.press("Save", "#contact");
  await s.done("#ct-msg", "Your details were saved.");
  const held = await master(t.stack, "GET", `/v1/parties/${who.party}`);
  const email = held.ok && held.data.facts && held.data.facts.email && held.data.facts.email.value;
  if (email !== fresh) throw new Error(`the customer master holds ${JSON.stringify(email)}, not ${fresh}`);
});

test({ id: "E2E-336", area: A, persona: "P17", story: "US-P18-1", title: "A customer puts a hold on money going out, and the customer master records it", matrix: ["SVC-022"],
  purpose: "With the browser set up, press Put a hold on, then read the hold as the page and as the customer master see it.",
  why: "The hold protects a customer who is being pressured to cash in. It must take effect when they press the button, in the system that enforces it." }, async (t) => {
  const { who, s } = await insured(t);
  await enrolBrowser(t, s, who);
  await s.tab("Your details");
  await s.sees("No hold is on.", "#hold");
  await s.press("Put a hold on");
  await s.done("#hold-msg", "A hold is on.");
  await s.sees("A hold is on.", "#hold");
  const held = await t.does(who, "GET", "/app/v1/profile/spending-hold", undefined, "reads their hold");
  if (held.held !== true) throw new Error(`the customer master's hold reads ${JSON.stringify(held)}`);
});

test({ id: "E2E-337", area: A, persona: "P17", story: "US-P18-1", title: "A customer downloads a copy of everything held about them, and only after signing", matrix: [],
  purpose: "Press Download my record before and after setting up the browser, and read the file that downloads.",
  why: "A Privacy Act copy holds everything about the person. Giving it to whoever holds a session would make the right to it a way to steal it." }, async (t) => {
  const { who, s } = await insured(t);
  await s.tab("Your details");
  await s.press("Download my record");
  await s.stopped("#ex-msg", "A copy of your record needs your signature");
  await enrolBrowser(t, s, who);
  await s.tab("Your details");
  const [download] = await Promise.all([s.page.waitForEvent("download"), s.press("Download my record")]);
  await s.done("#ex-msg", "Downloaded my-record.json.");
  if (download.suggestedFilename() !== "my-record.json") throw new Error(`the file is called ${download.suggestedFilename()}`);
  const text = await new Promise((done, fail) => download.createReadStream().then((st) => { let b = ""; st.on("data", (c) => (b += c)); st.on("end", () => done(b)); st.on("error", fail); }, fail));
  for (const fact of [who.given, who.family, who.born]) if (!text.includes(fact)) throw new Error(`the copy does not hold ${fact}`);
});

test({ id: "E2E-338", area: A, persona: "P17", story: "US-P18-1", title: "A forgotten browser is revoked and can no longer approve", matrix: [],
  purpose: "Set up the browser, forget it after confirming, then read Your devices and try to approve a change by the route with the old browser's name.",
  why: "A customer leaving a shared computer must be able to take its power to act for them away, there and then." }, async (t) => {
  const { who, p, s } = await insured(t);
  const device = await enrolBrowser(t, s, who);
  await s.press("Forget this browser");
  await s.page.click("dialog[open] button.danger");
  await s.done("#sg-msg", "This browser was forgotten.");
  await s.sees("This browser is not set up for signing.", "#sign-state");
  const listed = (await t.does(who, "GET", "/app/v1/devices", undefined, "reads their devices")).find((d) => d.id === device);
  if (!listed || !listed.revoked) throw new Error(`the forgotten browser is listed as ${JSON.stringify(listed)}`);
  const id = await prepared(t, who, p);
  const r = await t.sys.http("POST", t.stack.portal, `/app/v1/requests/${id}/approve`, { headers: { "x-dev-subject": who.subject, "x-device-id": device, "x-device-proof": "0.AAAA" }, body: { signed: { challenge: "none", signature: "AAAA" } } });
  if (r.ok) throw new Error("a forgotten browser approved a change");
  if ((await stateOf(t, who, id)).status !== "awaiting_approval") throw new Error("the change went ahead");
});

test({ id: "E2E-339", area: A, persona: "P17", story: "US-P18-1", title: "A customer cannot approve or decline a change on someone else's policy", matrix: [],
  purpose: "Have the adviser prepare a change for one customer; as a second customer, send the approve and decline routes for it.",
  why: "Request IDs travel in links and messages. Knowing one must not let anyone else say yes or no to it." }, async (t) => {
  const { who, p } = await insured(t);
  const id = await prepared(t, who, p);
  const stranger = await customer(t, { phone: false });
  for (const [method, path] of [["POST", `/app/v1/requests/${id}/approve`], ["DELETE", `/app/v1/requests/${id}`]]) {
    const r = await t.sys.portal(method, path, stranger, method === "POST" ? {} : undefined);
    if (r.ok) throw new Error(`a stranger's ${method} ${path} was accepted`);
  }
  if ((await stateOf(t, who, id)).status !== "awaiting_approval") throw new Error("a stranger's answer changed the request");
});
