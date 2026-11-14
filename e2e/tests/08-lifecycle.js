// A policy's life, run through the real system day by day, with every
// amount and date checked against an independent calculator.
//
// Each journey issues its own policy on the business date of the moment,
// moves money through cash matching as a bank file would, and closes business
// days with the same end-of-day close the scheduled workflow runs. What the
// collections officer then sees on the staff console (what is owed, each
// premium notice, the policy's standing, whether cover is in force) is
// compared with `lib/oracle.js`, which works the same figures out from the
// shipped product and rate files with its own arithmetic.
//
// These journeys move the business date, so they hold `business-date` and
// the runner leaves them until every other test has finished.
"use strict";
const fs = require("fs");
const path = require("path");
const { test } = require("../lib/suite");
const { cast } = require("../lib/cast");
const oracle = require("../lib/oracle");

const A = "Policy lifecycle";
const date = { lock: "business-date", limit: 600 };
const cents = (shown) => Math.round(Number(String(shown).replace(/[^0-9.-]/g, "")) * 100);

/// Issue a policy today from a shipped application, changed by `change(application)`.
async function issued(t, kind, change = () => {}) {
  const shipped = JSON.parse(fs.readFileSync(path.join(t.stack.run, "shipped.json"), "utf8"));
  const party = `cust-${t.ref("life").toLowerCase()}`;
  const a = JSON.parse(JSON.stringify(shipped.applications[kind]));
  for (const life of a.lives) if (life.kind.party) life.kind.party = party;
  a.inception = await t.today();
  change(a);
  const p = await t.policy(kind, party, { inception: a.inception, frequency: a.frequency, lives: a.lives, covers: a.covers });
  return { ...p, party, inception: a.inception, premium: oracle.premium(p.application) };
}
/// The policy as the staff console's own route gives it, once it satisfies `ready` (the read side follows the ledger by a moment).
async function known(t, id, ready = () => true, what = "the expected state") {
  let view = null;
  for (let n = 0; n < 60; n++) {
    const r = await t.sys.portal("GET", `/staff/v1/policies/${id}?case=lifecycle`, cast.st5);
    if (r.ok && r.data.found) {
      const row = (kind) => r.data.rows.filter((x) => x.key.split("/")[1] === kind).map((x) => x.body);
      view = { outstanding: row("billing")[0].outstanding_minor, oldest: row("billing")[0].oldest_unpaid_due, notices: row("notice"), status: row("policy")[0].status, standing: row("policy")[0].standing,
        covers: Object.fromEntries(row("cover").map((c) => [c.cover, c])) };
      if (ready(view)) return view;
    }
    await new Promise((res) => setTimeout(res, 250));
  }
  throw new Error(`the policy never reached ${what}; it reads ${JSON.stringify(view)}`);
}
/// What the collections officer reads on the staff console for a policy, in cents and plain words.
async function onScreen(t, id) {
  const s = await t.open("/staff", cast.st5);
  await s.fill("#pol", id); await s.fill("#case", "lifecycle check");
  await s.press("Open", "#lookup");
  await s.sees("Outstanding", "#pol-rows");
  const rows = await s.page.evaluate(() => [...document.querySelectorAll("#pol-rows tbody tr")].filter((tr) => tr.cells.length === 3 && tr.cells[2].querySelector("dl")).map((tr) => ({
    kind: tr.cells[0].innerText.trim().toLowerCase(),
    body: Object.fromEntries([...tr.cells[2].querySelectorAll("dt")].map((dt) => [dt.textContent.trim().toLowerCase(), dt.nextElementSibling.textContent.trim()])) })));
  const billing = rows.find((r) => "outstanding" in r.body), policy = rows.find((r) => "status" in r.body);
  return {
    outstanding: cents(billing.body.outstanding), oldest: billing.body["oldest unpaid due"],
    notices: rows.filter((r) => "period start" in r.body).map((r) => ({ start: r.body["period start"], end: r.body["period end"], total: cents(r.body.total) })).sort((x, y) => x.start.localeCompare(y.start)),
    status: policy.body.status, standing: policy.body.standing,
    // A cover can be in several layers (an indexation increase is a new layer): the sums are added.
    covers: rows.filter((r) => "in force" in r.body).reduce((all, r) => ({ ...all, [r.body.cover]: { inForce: r.body["in force"], sum: (all[r.body.cover] ? all[r.body.cover].sum : 0) + cents(r.body["sum insured"]) } }), {}),
  };
}
function same(what, shown, expected) { if (JSON.stringify(shown) !== JSON.stringify(expected)) throw new Error(`${what}: the screen shows ${JSON.stringify(shown)}, the independent calculation gives ${JSON.stringify(expected)}`); }
const notice = (p, n) => { const { start, end } = oracle.period(p.inception, p.application.frequency, n); return { start, end, total: p.premium.total }; };

// --- The premium at issue ----------------------------------------------------------------------------------------

test({ id: "E2E-233", area: A, persona: "P17", story: "US-P05-1", title: "A term life policy's first premium notice equals the independently calculated premium", matrix: ["LED-005", "BIL-002"], ...date,
  purpose: "Issue the shipped term life policy (life and trauma cover, monthly) and read what is owed and the first notice on the staff console.",
  why: "The first premium is rate x sum insured, modal loading, multi-cover discount and policy fee; a second, separate calculation is the only real check that the engine has them right." }, async (t) => {
  const p = await issued(t, "term_life");
  await known(t, p.id, (v) => v.notices.length === 1);
  const seen = await onScreen(t, p.id);
  t.note(`calculated: life ${oracle.money(p.premium.net.life)}, trauma ${oracle.money(p.premium.net.trauma)}, discount ${oracle.money(p.premium.discount)}, fee ${oracle.money(p.premium.fee)}, total ${oracle.money(p.premium.total)}`);
  same("the first notice", seen.notices, [notice(p, 1)]);
  same("what is owed", seen.outstanding, p.premium.total);
  same("the date the oldest unpaid premium fell due", seen.oldest, p.inception);
  same("cover", seen.covers, { life: { inForce: "yes", sum: 500_000_00 }, trauma: { inForce: "yes", sum: 100_000_00 } });
});

test({ id: "E2E-234", area: A, persona: "P11", story: "US-P11-1", title: "An income protection policy's first premium equals the independently calculated premium", matrix: ["LED-005"], ...date,
  purpose: "Issue the shipped income protection policy (one cover, so no multi-cover discount) and read the first notice.",
  why: "Income protection is rated per thousand of monthly benefit, a different basis from life cover; the same check must hold." }, async (t) => {
  const p = await issued(t, "income_protection");
  await known(t, p.id, (v) => v.notices.length === 1);
  const seen = await onScreen(t, p.id);
  if (p.premium.discount !== 0) throw new Error("the calculator applied a multi-cover discount to a single cover");
  same("the first notice", seen.notices, [notice(p, 1)]);
  same("what is owed", seen.outstanding, p.premium.total);
});

test({ id: "E2E-235", area: A, persona: "P20", story: "US-P20-1", title: "A family health policy's first premium, with GST, equals the independently calculated premium", matrix: ["LED-005", "FIN-014"], ...date,
  purpose: "Issue the shipped health policy on three lives and read the first notice.",
  why: "Health is rated per life by age band and carries 15% GST; the notice must be the three lives' premium plus the tax, to the cent." }, async (t) => {
  const p = await issued(t, "health_family");
  await known(t, p.id, (v) => v.notices.length === 1);
  const seen = await onScreen(t, p.id);
  t.note(`calculated: premium ${oracle.money(p.premium.net.hospital)}, GST ${oracle.money(p.premium.gst.hospital)}`);
  if (p.premium.gst.hospital !== Math.round(p.premium.net.hospital * 0.15)) throw new Error("the calculator's GST is not 15% of the premium");
  same("the first notice", seen.notices, [notice(p, 1)]);
});

test({ id: "E2E-236", area: A, persona: "P1", story: "US-P01-1", title: "Paying fortnightly bills a twenty-sixth of the loaded annual premium every fourteen days", matrix: ["BIL-001"], ...date,
  purpose: "Issue a term life policy paid fortnightly, pay each notice, close fifteen days and read the notices.",
  why: "Fortnightly customers are the most exposed to drift: the instalment and the fourteen-day step must both be exact." }, async (t) => {
  const p = await issued(t, "term_life", (a) => { a.frequency = "fortnightly"; });
  await t.pays(p.number, p.premium.total * 2);
  await t.closes(15);
  await known(t, p.id, (v) => v.notices.length === 2);
  const seen = await onScreen(t, p.id);
  same("the notices", seen.notices, [notice(p, 1), notice(p, 2)]);
  if (seen.notices[1].start !== oracle.addDays(p.inception, 14)) throw new Error("the second fortnight does not start fourteen days after the first");
  same("what is owed after paying both", seen.outstanding, 0);
});

test({ id: "E2E-237", area: A, persona: "P25", story: "US-P25-1", title: "Paying yearly bills the annual premium once, with no modal loading", matrix: ["LED-006"], ...date,
  purpose: "Issue a term life policy paid yearly and read its one notice.",
  why: "The loading is the price of paying by instalment; a yearly payer must not be charged it, and the period must run a full year." }, async (t) => {
  const p = await issued(t, "term_life", (a) => { a.frequency = "yearly"; });
  const monthly = oracle.premium({ ...p.application, frequency: "monthly" });
  if (!(p.premium.total < monthly.total * 12)) throw new Error("the calculator does not make yearly payment cheaper than twelve monthly instalments");
  await known(t, p.id, (v) => v.notices.length === 1);
  const seen = await onScreen(t, p.id);
  same("the notice", seen.notices, [notice(p, 1)]);
  if (seen.notices[0].end !== oracle.addMonths(p.inception, 12)) throw new Error(`the year ends on ${seen.notices[0].end}`);
});

test({ id: "E2E-238", area: A, persona: "P2", story: "US-P02-1", title: "A premium that lands on half a cent or more is rounded as the product says", matrix: ["LED-005"], ...date,
  purpose: "Issue an income protection policy whose fortnightly premium works out at $57.3259504, and read the first notice.",
  why: "The product rounds half-even to the cent, so this premium is $57.33; a system that bills $57.32 is a cent out on about one premium in twenty." }, async (t) => {
  const p = await issued(t, "income_protection", (a) => { a.frequency = "fortnightly"; a.covers[0].sum_insured_minor = 4555_00; Object.assign(a.lives[0].factors, { date_of_birth: `${Number(a.inception.slice(0, 4)) - 29}-01-01`, sex: "male", smoker: true }); });
  await known(t, p.id, (v) => v.notices.length === 1);
  const seen = await onScreen(t, p.id);
  t.note(`calculated: income ${oracle.money(p.premium.net.income)}, fee ${oracle.money(p.premium.fee)}`);
  same("the first notice", seen.notices, [notice(p, 1)]);
});

test({ id: "E2E-239", area: A, persona: "ST15", story: "US-ST15-1", title: "Quotes across ages, sexes, smoker status, frequencies and sums insured match the independent calculation", matrix: [], ...date,
  purpose: "Ask the system for 384 quotes on term life and income protection across the rating grid and compare each cover and fee with the calculator.",
  why: "One policy proves one rate; the grid finds the age-band edges, the smoker and sex columns and the rounding cases a single journey misses." }, async (t) => {
  const shipped = JSON.parse(fs.readFileSync(path.join(t.stack.run, "shipped.json"), "utf8"));
  const today = await t.today(), year = Number(today.slice(0, 4));
  const births = [`${year - 22}-${today.slice(5)}`, oracle.addDays(`${year - 22}-${today.slice(5)}`, 1), `${year - 28}-03-01`, `${year - 39}-06-02`, `${year - 43}-${today.slice(5)}`, oracle.addDays(`${year - 43}-${today.slice(5)}`, 1), `${year - 52}-01-01`, `${year - 61}-07-07`];
  const differ = []; let asked = 0;
  for (const born of births) for (const sex of ["female", "male"]) for (const smoker of [false, true]) for (const frequency of ["monthly", "fortnightly", "yearly"]) {
    for (const [kind, sums] of [["term_life", [500_000_00, 100_000_00]], ["term_life", [333_333_00, 77_777_00]], ["income_protection", [6_000_00]], ["income_protection", [4_555_00]]]) {
      const a = JSON.parse(JSON.stringify(shipped.applications[kind]));
      Object.assign(a, { inception: today, frequency }); Object.assign(a.lives[0].factors, { date_of_birth: born, sex, smoker });
      a.covers.forEach((c, i) => { c.sum_insured_minor = sums[i]; });
      const q = await t.sys.api("POST", "/v1/quotes", a); asked++;
      if (!q.ok) throw new Error(`the quote for ${kind} ${born} ${sex} was refused: ${JSON.stringify(q.data)}`);
      const mine = oracle.premium(a);
      const theirs = Object.fromEntries(Object.entries(q.data.by_cover).map(([c, v]) => [c, v.minor]));
      if (JSON.stringify(theirs) !== JSON.stringify(mine.net) || q.data.policy_fee.minor !== mine.fee) differ.push(`${kind} born ${born} ${sex}${smoker ? " smoker" : ""} ${frequency} ${sums.map(oracle.money).join("+")}: system ${JSON.stringify(theirs)}, calculated ${JSON.stringify(mine.net)}`);
    }
  }
  t.note(`${asked} quotes compared, ${differ.length} differ`);
  if (differ.length) throw new Error(`${differ.length} of ${asked} quotes differ from the independent calculation, each by a cent where the exact premium ends in half a cent or more. First: ${differ[0]}`);
});

// --- Billing and payment -----------------------------------------------------------------------------------------

test({ id: "E2E-240", area: A, persona: "P5", story: "US-P05-1", title: "A premium paid in full clears what is owed, and the next month is billed on the eve of its due date", matrix: ["LED-005", "BIL-002"], ...date,
  purpose: "Pay the first premium, close days to the end of the first month, and read the notices and what is owed.",
  why: "The run-through every paying customer lives: pay, be clear, be billed again for the same amount for the next month." }, async (t) => {
  const p = await issued(t, "term_life");
  await t.pays(p.number, p.premium.total);
  await known(t, p.id, (v) => v.outstanding === 0);
  const paid = await onScreen(t, p.id);
  same("what is owed after payment", paid.outstanding, 0);
  same("the standing after payment", paid.standing, "paid");
  const second = notice(p, 2);
  await t.closesTo(second.start);
  await known(t, p.id, (v) => v.notices.length === 2);
  const seen = await onScreen(t, p.id);
  same("the notices", seen.notices, [notice(p, 1), second]);
  same("what is owed once the second month is billed", seen.outstanding, p.premium.total);
  same("the date the oldest unpaid premium fell due", seen.oldest, second.start);
});

test({ id: "E2E-241", area: A, persona: "P5", story: "US-P05-3", title: "A part payment leaves exactly the balance owing", matrix: ["COL-006"], ...date,
  purpose: "Pay $50.00 against the first premium and read what is owed.",
  why: "A short payment must reduce the debt by exactly what was paid and leave the premium overdue from its original date." }, async (t) => {
  const p = await issued(t, "term_life");
  await t.pays(p.number, 50_00);
  await known(t, p.id, (v) => v.outstanding !== p.premium.total);
  const seen = await onScreen(t, p.id);
  same("what is owed", seen.outstanding, p.premium.total - 50_00);
  same("the date the oldest unpaid premium fell due", seen.oldest, p.inception);
});

test({ id: "E2E-242", area: A, persona: "P11", story: "US-P11-1", title: "Money paid ahead is set against the next premium when it falls due", matrix: ["BIL-007"], ...date,
  purpose: "Pay $200.00 against a smaller first premium, close days until the second month is billed, and read what is owed.",
  why: "A customer who pays ahead must see the credit used: the second notice less what was left over, nothing lost and nothing refunded unasked." }, async (t) => {
  const p = await issued(t, "term_life");
  await t.pays(p.number, 200_00);
  await known(t, p.id, (v) => v.outstanding === 0);
  await t.closesTo(notice(p, 2).start);
  await known(t, p.id, (v) => v.notices.length === 2);
  const seen = await onScreen(t, p.id);
  same("what is owed after the second month is billed", seen.outstanding, p.premium.total - (200_00 - p.premium.total));
});

test({ id: "E2E-243", area: A, persona: "P46", story: "US-P46-1", title: "The same receipt sent twice is applied once", matrix: ["LED-002"], ...date,
  purpose: "Send one bank receipt for part of the premium twice, with the same receipt reference, and read what is owed.",
  why: "Bank files are resent; a receipt applied twice hands a customer credit they never paid." }, async (t) => {
  const p = await issued(t, "term_life");
  const body = { id: t.ref("rc").toLowerCase(), amount: { minor: 40_00, currency: "NZD" }, reference: p.number, payer_account: null, received_on: await t.today() };
  const headers = { "x-dev-subject": "svc-cash-matching", "x-dev-roles": "cash-matching", "x-caller": "cash-matching" };
  const first = await t.sys.http("POST", t.stack.api, "/v1/payments", { headers, body });
  const again = await t.sys.http("POST", t.stack.api, "/v1/payments", { headers, body });
  if (!first.ok || !again.ok || again.data.duplicate !== true) throw new Error(`the second copy answered ${again.status} ${JSON.stringify(again.data)}`);
  await known(t, p.id, (v) => v.outstanding !== p.premium.total);
  same("what is owed", (await onScreen(t, p.id)).outstanding, p.premium.total - 40_00);
});

// --- Arrears -----------------------------------------------------------------------------------------------------

test({ id: "E2E-244", area: A, persona: "P4", story: "US-P04-2", title: "An unpaid premium suspends cover on the day grace ends and not a day before", matrix: ["COL-007"], ...date,
  purpose: "Leave the first premium unpaid, close days to the eve of the end of grace and then one more, reading the standing each time.",
  why: "Grace is a promise of thirty days; the standing must turn on the calculated day exactly." }, async (t) => {
  const p = await issued(t, "term_life");
  const due = oracle.arrears("term-life", p.inception);
  if (due.graceEnds !== oracle.addDays(p.inception, 30) || !due.suspends) throw new Error(`the calculator reads grace as ${JSON.stringify(due)}`);
  await t.closesTo(due.graceEnds);
  const before = await onScreen(t, p.id);
  same(`the standing on ${due.graceEnds}, before that day closes`, before.standing, "in grace");
  await t.closes(1);
  await known(t, p.id, (v) => v.standing === "suspended", "suspended");
  const after = await onScreen(t, p.id);
  same("the standing once the last day of grace has closed", after.standing, "suspended");
  same("what is owed (two months by now)", after.outstanding, p.premium.total * 2);
});

test({ id: "E2E-245", area: A, persona: "P20", story: "US-P20-1", title: "Paying the arrears while suspended restores the policy's standing", matrix: ["COL-018"], ...date,
  purpose: "Let a policy be suspended for non-payment, pay everything owed, and read the standing.",
  why: "Suspension is meant to be recoverable by paying; the customer must be back in good standing the moment the money lands." }, async (t) => {
  const p = await issued(t, "term_life");
  await t.closesTo(oracle.addDays(p.inception, 35));
  const owed = (await known(t, p.id, (v) => v.standing === "suspended", "suspended")).outstanding;
  same("what is owed after 35 days unpaid", owed, p.premium.total * 2);
  await t.pays(p.number, owed);
  await known(t, p.id, (v) => v.outstanding === 0);
  const seen = await onScreen(t, p.id);
  same("the standing after paying", seen.standing, "paid");
  same("what is owed after paying", seen.outstanding, 0);
});

test({ id: "E2E-246", area: A, persona: "P17", story: "US-P17-3", title: "An unpaid policy lapses on the calculated day, its cover ends and its debt is written back", matrix: ["COL-008"], ...date,
  purpose: "Leave a policy unpaid, close days to the eve of the lapse day and then one more, reading the policy each time.",
  why: "Lapse ends someone's cover. It must happen ninety days after the unpaid premium fell due, not before, and must not leave a debt for cover that was never given." }, async (t) => {
  const p = await issued(t, "term_life");
  const due = oracle.arrears("term-life", p.inception);
  await t.closesTo(due.lapses);
  const before = await onScreen(t, p.id);
  same(`the status on ${due.lapses}, before that day closes`, [before.status, before.covers.life.inForce], ["in force", "yes"]);
  // Every month that has started by the lapse day has been billed and none paid (three or four, by the lengths of the months).
  let months = 0; while (oracle.period(p.inception, "monthly", months + 1).start <= due.lapses) months++;
  same(`what is owed on the eve of lapse (${months} months billed)`, before.outstanding, p.premium.total * months);
  await t.closes(1);
  await known(t, p.id, (v) => v.status === "lapsed", "lapsed");
  const after = await onScreen(t, p.id);
  same("the status once the lapse day has closed", after.status, "lapsed");
  same("cover after lapse", [after.covers.life.inForce, after.covers.trauma.inForce], ["no", "no"]);
  same("what is owed after lapse", after.outstanding, 0);
});

test({ id: "E2E-247", area: A, persona: "P17", story: "US-P17-3", title: "A lapsed policy is reinstated only once every premium since it started has been paid", matrix: ["COL-010", "COL-028"], ...date,
  purpose: "Let a policy lapse; ask collections to reinstate it with nothing paid, then pay the calculated arrears and ask again.",
  why: "Reinstatement puts cover back as if it never stopped, so the customer owes every premium for the months in between; the amount asked for must be exactly that." }, async (t) => {
  const p = await issued(t, "term_life");
  const due = oracle.arrears("term-life", p.inception);
  await t.closesTo(oracle.addDays(due.lapses, 5));
  await known(t, p.id, (v) => v.status === "lapsed", "lapsed");
  const today = await t.today();
  // Every month that has started by today is owed.
  let months = 0; while (oracle.period(p.inception, "monthly", months + 1).start <= today) months++;
  const arrears = months * p.premium.total;
  const early = await t.core("collections", { reinstate: { policy: p.id } }, p.id);
  if (early.status !== 422 || !String(early.data.detail).includes(`reinstatement needs ${arrears} in suspense`)) throw new Error(`with nothing paid the system answered ${early.status} ${JSON.stringify(early.data)}; the calculated arrears are ${arrears} cents (${months} months)`);
  await t.pays(p.number, arrears);
  const done = await t.core("collections", { reinstate: { policy: p.id } }, p.id);
  if (!done.ok) throw new Error(`reinstatement with the arrears paid answered ${done.status} ${JSON.stringify(done.data)}`);
  await known(t, p.id, (v) => v.status === "in_force", "in force");
  const seen = await onScreen(t, p.id);
  same("the policy after reinstatement", [seen.status, seen.standing, seen.outstanding, seen.covers.life.inForce], ["in force", "paid", 0, "yes"]);
});

test({ id: "E2E-248", area: A, persona: "P20", story: "US-P20-1", title: "A health policy lapses after sixty days, as its own product says, with no suspension first", matrix: ["COL-008"], ...date,
  purpose: "Leave a health policy unpaid and read it after grace and on either side of its lapse day.",
  why: "Arrears rules are per product: health lapses at sixty days and is never suspended. Applying the life rules to it would end cover a month late." }, async (t) => {
  const p = await issued(t, "health_family");
  const due = oracle.arrears("health-plan", p.inception);
  if (due.lapses !== oracle.addDays(p.inception, 60) || due.suspends) throw new Error(`the calculator reads the health product's arrears as ${JSON.stringify(due)}`);
  await t.closesTo(oracle.addDays(p.inception, 40));
  const mid = await onScreen(t, p.id);
  if (mid.standing === "suspended") throw new Error("the health policy is suspended; its product has no suspension");
  await t.closesTo(due.lapses);
  same("the status on the eve of lapse", (await onScreen(t, p.id)).status, "in force");
  await t.closes(1);
  await known(t, p.id, (v) => v.status === "lapsed", "lapsed");
  same("the status once the lapse day has closed", (await onScreen(t, p.id)).status, "lapsed");
});

// --- The anniversary ---------------------------------------------------------------------------------------------

test({ id: "E2E-252", area: A, persona: "P29", story: "US-P29-1", title: "At the first anniversary cover is indexed and the premium is re-rated at the new age, to the cent", matrix: ["BIL-012", "SVC-024"], lock: "business-date", limit: 1500,
  purpose: "Issue a term life policy on a life aged 42 who turns 43 during the year, pay a year and a month ahead, close days through the first anniversary, and read the cover and the thirteenth notice.",
  why: "The anniversary is where a stepped premium moves: the sum insured rises by the index (2.5% CPI) and the rate steps into the 43 to 47 band. Twelve notices at the old premium and the thirteenth at the new one is the whole promise of the product." }, async (t) => {
  const CPI = 25n; // the activated index rate, in thousandths
  const p = await issued(t, "term_life", (a) => { a.lives[0].factors.date_of_birth = oracle.addMonths(`${Number(a.inception.slice(0, 4)) - 43}-${a.inception.slice(5)}`, 1); });
  if (oracle.age(p.application.lives[0].factors.date_of_birth, p.inception) !== 42) throw new Error("the life is not 42 at inception");
  const anniversary = oracle.addMonths(p.inception, 12);
  if (oracle.age(p.application.lives[0].factors.date_of_birth, anniversary) !== 43) throw new Error("the life is not 43 at the anniversary");
  // Cover after indexation, and the premium for it at the new age: worked out here, not read from the system.
  const indexed = JSON.parse(JSON.stringify(p.application));
  for (const c of indexed.covers) c.sum_insured_minor = Number(BigInt(c.sum_insured_minor) + oracle.halfEven(BigInt(c.sum_insured_minor) * CPI, 1000n));
  const after = oracle.premium(indexed, anniversary);
  if (!(after.total > p.premium.total)) throw new Error("the calculator does not raise the premium at the anniversary");
  t.note(`calculated: ${oracle.money(p.premium.total)} a month in year one, ${oracle.money(after.total)} from ${anniversary}; cover ${indexed.covers.map((c) => `${c.code} ${oracle.money(c.sum_insured_minor)}`).join(", ")}`);
  await t.pays(p.number, p.premium.total * 12 + after.total);
  await t.closesTo(oracle.addDays(anniversary, 1));
  const thirteenth = oracle.period(p.inception, "monthly", 13);
  await known(t, p.id, (v) => v.notices.some((n) => n.period_start === thirteenth.start), "its thirteenth notice");
  const seen = await onScreen(t, p.id);
  // The console keeps the latest notices, not all thirteen: the two either side of the anniversary are found by their dates.
  const at = (start) => seen.notices.find((n) => n.start === start) || null;
  same("the twelfth notice (the last of year one)", at(notice(p, 12).start), notice(p, 12));
  same("the thirteenth notice (the first of year two)", at(thirteenth.start), { ...thirteenth, total: after.total });
  same("cover after indexation", Object.fromEntries(Object.entries(seen.covers).map(([code, c]) => [code, c.sum])), Object.fromEntries(indexed.covers.map((c) => [c.code, c.sum_insured_minor])));
  same("what is owed, having paid thirteen months ahead", seen.outstanding, 0);
});

// --- Leaving -----------------------------------------------------------------------------------------------------

test({ id: "E2E-249", area: A, persona: "P22", story: "US-P22-1", title: "A policy cancelled in the cooling-off period ends at once with nothing owed", matrix: ["BIL-008"], ...date,
  purpose: "Pay the first premium, cancel on day five, and read the policy.",
  why: "Inside the free-look period the customer walks away clean: cover ends and no premium is left owing." }, async (t) => {
  const p = await issued(t, "term_life");
  if (oracle.product("term-life").coolingOffDays !== 14) throw new Error("the calculator does not read a 14-day cooling-off period");
  await t.pays(p.number, p.premium.total);
  await t.closes(5);
  const r = await t.core("policy-servicing", { cancel_policy: { policy: p.id } }, p.id);
  if (!r.ok) throw new Error(`the cancellation answered ${r.status} ${JSON.stringify(r.data)}`);
  await known(t, p.id, (v) => v.status === "cancelled", "cancelled");
  const seen = await onScreen(t, p.id);
  same("the policy after cancelling", [seen.status, seen.outstanding, seen.covers.life.inForce], ["cancelled", 0, "no"]);
});

test({ id: "E2E-250", area: A, persona: "P37", story: "US-P37-1", title: "A policy cancelled mid-term stops being billed", matrix: ["BIL-009"], ...date,
  purpose: "Pay the first premium, cancel on day twenty, close days past the next billing date and read the notices.",
  why: "A cancelled policy that goes on being billed is the complaint every insurer gets; the second month must never be raised." }, async (t) => {
  const p = await issued(t, "term_life");
  await t.pays(p.number, p.premium.total);
  await t.closes(20);
  const r = await t.core("policy-servicing", { cancel_policy: { policy: p.id } }, p.id);
  if (!r.ok) throw new Error(`the cancellation answered ${r.status} ${JSON.stringify(r.data)}`);
  await t.closesTo(oracle.addDays(notice(p, 2).start, 2));
  await known(t, p.id, (v) => v.status === "cancelled", "cancelled");
  const seen = await onScreen(t, p.id);
  same("the notices after cancelling", seen.notices, [notice(p, 1)]);
  same("what is owed", seen.outstanding, 0);
});

// --- The books ---------------------------------------------------------------------------------------------------

test({ id: "E2E-251", area: A, persona: "ST11", story: "US-ST11-1", title: "After days have closed the finance ledger has been fed and every fund's trial balance sums to zero", matrix: ["LED-001", "LED-010"], ...date,
  purpose: "Close a business day, then read the ledger's status and the trial balance of the period as the finance lead.",
  why: "Every premium billed, earned and paid above became journal entries; if debits and credits do not sum to zero in each statutory fund, the books are wrong whatever the screens say." }, async (t) => {
  await t.closes(1);
  const today = await t.today();
  const status = await t.does(cast.st11, "GET", "/staff/v1/finance/status");
  if (!status.last_date || status.entries < 1 || status.partitions.some((p) => p.stalled)) throw new Error(`the ledger's feed reads ${JSON.stringify(status)}`);
  const s = await t.open("/finance", cast.st11);
  await s.never("A feed has stalled", "#status");
  const funds = {};
  for (let m = 0; m < 3; m++) {
    const period = oracle.addMonths(`${today.slice(0, 7)}-01`, -m).slice(0, 7);
    for (const row of await t.does(cast.st11, "GET", `/staff/v1/finance/trial_balance?period=${period}`)) funds[`${period} ${row.fund}`] = (funds[`${period} ${row.fund}`] || 0n) + BigInt(row.balance);
  }
  if (!Object.keys(funds).length) throw new Error("the trial balance of the last three periods is empty");
  const out = Object.entries(funds).filter(([, sum]) => sum !== 0n);
  if (out.length) throw new Error(`debits and credits do not sum to zero: ${out.map(([k, v]) => `${k}: ${v}`).join("; ")}`);
});
